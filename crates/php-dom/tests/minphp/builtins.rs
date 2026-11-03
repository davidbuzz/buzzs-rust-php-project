//! Built-in PHP functions used by the DOM tests.

use std::rc::Rc;

use php_dom::dispatch::DomObject;

use super::interp::{compare, loose_eq, strict_eq, to_int, to_key, to_num, Flow, Interp, R};
use super::output::{print_r, to_bytes, var_dump, var_export};
use super::value::{new_obj, Arr, Callable, Key, ObjKind, V};

fn arg(a: &[V], i: usize) -> V {
    a.get(i).cloned().unwrap_or(V::Null)
}

fn s(a: &[V], i: usize) -> Vec<u8> {
    to_bytes(&arg(a, i))
}

fn path(p: &[u8]) -> String {
    String::from_utf8_lossy(p).into_owned()
}

impl Interp {
    #[allow(clippy::too_many_lines)]
    pub fn builtin(&self, name: &str, a: Vec<V>, _named: Vec<(Vec<u8>, V)>) -> R<V> {
        let lname = name.to_ascii_lowercase();
        let lname = lname.trim_start_matches('\\');
        Ok(match lname {
            "var_dump" => {
                for v in &a {
                    let mut out = Vec::new();
                    var_dump(&mut out, v, 0);
                    self.echo(&out);
                }
                V::Null
            }
            "print_r" => {
                let mut out = Vec::new();
                print_r(&mut out, &arg(&a, 0), 0);
                if arg(&a, 1).truthy() {
                    return Ok(V::Str(out));
                }
                self.echo(&out);
                V::Bool(true)
            }
            "var_export" => {
                let mut out = Vec::new();
                var_export(&mut out, &arg(&a, 0), 0);
                if arg(&a, 1).truthy() {
                    return Ok(V::Str(out));
                }
                self.echo(&out);
                V::Null
            }
            "printf" => {
                let s = self.sprintf(&a)?;
                let n = s.len();
                self.echo(&s);
                V::Int(n as i64)
            }
            "sprintf" => V::Str(self.sprintf(&a)?),
            "count" | "sizeof" => match &arg(&a, 0) {
                V::Arr(x) => V::Int(x.len() as i64),
                V::Obj(o) => {
                    if let Some(d) = o.dom() {
                        if let Some(c) = php_dom::dispatch::count(&d) {
                            return Ok(V::Int(c));
                        }
                    }
                    self.call_method(&arg(&a, 0), "count", vec![])?
                }
                other => {
                    return Err(Flow::Throw(self.make_exception("TypeError", format!("count(): Argument #1 ($value) must be of type Countable|array, {} given", other.type_name()).as_bytes(), 0)));
                }
            },
            "strlen" => V::Int(s(&a, 0).len() as i64),
            "str_repeat" => V::Str(s(&a, 0).repeat(to_int(&arg(&a, 1)).max(0) as usize)),
            "strtoupper" => V::Str(s(&a, 0).to_ascii_uppercase()),
            "strtolower" => V::Str(s(&a, 0).to_ascii_lowercase()),
            "ucfirst" => {
                let mut v = s(&a, 0);
                if let Some(c) = v.first_mut() {
                    *c = c.to_ascii_uppercase();
                }
                V::Str(v)
            }
            "trim" | "rtrim" | "ltrim" | "chop" => {
                let v = s(&a, 0);
                let chars: Vec<u8> = if a.len() > 1 { s(&a, 1) } else { b" \t\n\r\0\x0B".to_vec() };
                let mut start = 0;
                let mut end = v.len();
                if lname != "rtrim" && lname != "chop" {
                    while start < end && chars.contains(&v[start]) {
                        start += 1;
                    }
                }
                if lname != "ltrim" {
                    while end > start && chars.contains(&v[end - 1]) {
                        end -= 1;
                    }
                }
                V::Str(v[start..end].to_vec())
            }
            "str_replace" => {
                let subj = s(&a, 2);
                let searches: Vec<Vec<u8>> = match arg(&a, 0) {
                    V::Arr(x) => x.items.iter().map(|(_, v)| to_bytes(v)).collect(),
                    other => vec![to_bytes(&other)],
                };
                let repls: Vec<Vec<u8>> = match arg(&a, 1) {
                    V::Arr(x) => x.items.iter().map(|(_, v)| to_bytes(v)).collect(),
                    other => vec![to_bytes(&other); searches.len()],
                };
                let mut out = subj;
                for (i, se) in searches.iter().enumerate() {
                    if se.is_empty() {
                        continue;
                    }
                    let r = repls.get(i).cloned().unwrap_or_default();
                    out = replace_bytes(&out, se, &r);
                }
                V::Str(out)
            }
            "substr" => {
                let v = s(&a, 0);
                let len = v.len() as i64;
                let mut start = to_int(&arg(&a, 1));
                if start < 0 {
                    start = (len + start).max(0);
                }
                let start = start.min(len) as usize;
                let l = if a.len() > 2 && !matches!(arg(&a, 2), V::Null) {
                    let l = to_int(&arg(&a, 2));
                    if l < 0 {
                        ((len - start as i64) + l).max(0) as usize
                    } else {
                        l as usize
                    }
                } else {
                    v.len() - start
                };
                V::Str(v[start..(start + l).min(v.len())].to_vec())
            }
            "strpos" | "stripos" => {
                let h = s(&a, 0);
                let n = s(&a, 1);
                let (h, n) = if lname == "stripos" { (h.to_ascii_lowercase(), n.to_ascii_lowercase()) } else { (h, n) };
                match h.windows(n.len().max(1)).position(|w| w == n.as_slice()) {
                    Some(p) if !n.is_empty() => V::Int(p as i64),
                    _ => V::Bool(false),
                }
            }
            "str_contains" => {
                let h = s(&a, 0);
                let n = s(&a, 1);
                V::Bool(n.is_empty() || h.windows(n.len()).any(|w| w == n.as_slice()))
            }
            "str_starts_with" => V::Bool(s(&a, 0).starts_with(&s(&a, 1))),
            "str_ends_with" => V::Bool(s(&a, 0).ends_with(&s(&a, 1))),
            "strcmp" => V::Int(match s(&a, 0).cmp(&s(&a, 1)) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            }),
            "strrev" => {
                let mut v = s(&a, 0);
                v.reverse();
                V::Str(v)
            }
            "nl2br" => V::Str(replace_bytes(&s(&a, 0), b"\n", b"<br />\n")),
            "htmlspecialchars" => {
                let mut out = Vec::new();
                for c in s(&a, 0) {
                    match c {
                        b'&' => out.extend_from_slice(b"&amp;"),
                        b'<' => out.extend_from_slice(b"&lt;"),
                        b'>' => out.extend_from_slice(b"&gt;"),
                        b'"' => out.extend_from_slice(b"&quot;"),
                        b'\'' => out.extend_from_slice(b"&#039;"),
                        _ => out.push(c),
                    }
                }
                V::Str(out)
            }
            "bin2hex" => V::Str(s(&a, 0).iter().map(|b| format!("{b:02x}")).collect::<String>().into_bytes()),
            "ord" => V::Int(i64::from(s(&a, 0).first().copied().unwrap_or(0))),
            "chr" => V::Str(vec![(to_int(&arg(&a, 0)) & 0xff) as u8]),
            "dechex" => V::Str(format!("{:x}", to_int(&arg(&a, 0))).into_bytes()),
            "str_pad" => {
                let v = s(&a, 0);
                let len = to_int(&arg(&a, 1)).max(0) as usize;
                let pad = if a.len() > 2 { s(&a, 2) } else { b" ".to_vec() };
                let mut out = v.clone();
                while out.len() < len && !pad.is_empty() {
                    out.push(pad[(out.len() - v.len()) % pad.len()]);
                }
                V::Str(out)
            }
            "implode" | "join" => {
                let (sep, arr) = match (&arg(&a, 0), &arg(&a, 1)) {
                    (V::Arr(x), V::Null) => (Vec::new(), x.clone()),
                    (sep, V::Arr(x)) => (to_bytes(sep), x.clone()),
                    (V::Arr(x), sep) => (to_bytes(sep), x.clone()),
                    _ => (Vec::new(), Rc::new(Arr::default())),
                };
                let mut out = Vec::new();
                for (i, (_, v)) in arr.items.iter().enumerate() {
                    if i > 0 {
                        out.extend_from_slice(&sep);
                    }
                    out.extend_from_slice(&self.to_str(v)?);
                }
                V::Str(out)
            }
            "explode" => {
                let sep = s(&a, 0);
                let v = s(&a, 1);
                let mut arr = Arr::default();
                if sep.is_empty() {
                    return Ok(V::Bool(false));
                }
                let mut start = 0;
                let mut i = 0;
                while i + sep.len() <= v.len() {
                    if v[i..i + sep.len()] == sep[..] {
                        arr.push(V::Str(v[start..i].to_vec()));
                        i += sep.len();
                        start = i;
                    } else {
                        i += 1;
                    }
                }
                arr.push(V::Str(v[start..].to_vec()));
                V::arr(arr)
            }
            "str_split" => {
                let v = s(&a, 0);
                let n = if a.len() > 1 { to_int(&arg(&a, 1)).max(1) as usize } else { 1 };
                let mut arr = Arr::default();
                for c in v.chunks(n) {
                    arr.push(V::Str(c.to_vec()));
                }
                V::arr(arr)
            }
            "array_keys" => {
                let mut arr = Arr::default();
                if let V::Arr(x) = arg(&a, 0) {
                    for (k, _) in &x.items {
                        arr.push(match k {
                            Key::Int(i) => V::Int(*i),
                            Key::Str(s) => V::Str(s.clone()),
                        });
                    }
                }
                V::arr(arr)
            }
            "array_values" => {
                let mut arr = Arr::default();
                if let V::Arr(x) = arg(&a, 0) {
                    for (_, v) in &x.items {
                        arr.push(v.clone());
                    }
                }
                V::arr(arr)
            }
            "array_map" => {
                let f = arg(&a, 0);
                let mut arr = Arr::default();
                if let V::Arr(x) = arg(&a, 1) {
                    for (k, v) in &x.items {
                        let r = if matches!(f, V::Null) { v.clone() } else { self.call_value(&f, vec![v.clone()])? };
                        arr.set(k.clone(), r);
                    }
                }
                V::arr(arr)
            }
            "array_filter" => {
                let f = arg(&a, 1);
                let mut arr = Arr::default();
                if let V::Arr(x) = arg(&a, 0) {
                    for (k, v) in &x.items {
                        let keep = if matches!(f, V::Null) { v.truthy() } else { self.call_value(&f, vec![v.clone()])?.truthy() };
                        if keep {
                            arr.set(k.clone(), v.clone());
                        }
                    }
                }
                V::arr(arr)
            }
            "array_merge" => {
                let mut arr = Arr::default();
                for x in &a {
                    if let V::Arr(x) = x {
                        for (k, v) in &x.items {
                            match k {
                                Key::Int(_) => arr.push(v.clone()),
                                Key::Str(_) => arr.set(k.clone(), v.clone()),
                            }
                        }
                    }
                }
                V::arr(arr)
            }
            "array_reverse" => {
                let mut arr = Arr::default();
                if let V::Arr(x) = arg(&a, 0) {
                    for (k, v) in x.items.iter().rev() {
                        match k {
                            Key::Int(_) => arr.push(v.clone()),
                            Key::Str(_) => arr.set(k.clone(), v.clone()),
                        }
                    }
                }
                V::arr(arr)
            }
            "array_slice" => {
                let mut arr = Arr::default();
                if let V::Arr(x) = arg(&a, 0) {
                    let off = to_int(&arg(&a, 1)).max(0) as usize;
                    let len = if matches!(arg(&a, 2), V::Null) { usize::MAX } else { to_int(&arg(&a, 2)).max(0) as usize };
                    for (_, v) in x.items.iter().skip(off).take(len) {
                        arr.push(v.clone());
                    }
                }
                V::arr(arr)
            }
            "array_search" => {
                if let V::Arr(x) = arg(&a, 1) {
                    for (k, v) in &x.items {
                        if loose_eq(v, &arg(&a, 0)) {
                            return Ok(match k {
                                Key::Int(i) => V::Int(*i),
                                Key::Str(s) => V::Str(s.clone()),
                            });
                        }
                    }
                }
                V::Bool(false)
            }
            "array_push" => V::Null,
            "array_key_exists" | "key_exists" => match arg(&a, 1) {
                V::Arr(x) => V::Bool(x.get(&to_key(&arg(&a, 0))).is_some()),
                _ => V::Bool(false),
            },
            "array_unique" => {
                let mut arr = Arr::default();
                if let V::Arr(x) = arg(&a, 0) {
                    for (k, v) in &x.items {
                        if !arr.items.iter().any(|(_, w)| loose_eq(v, w)) {
                            arr.set(k.clone(), v.clone());
                        }
                    }
                }
                V::arr(arr)
            }
            "array_fill" => {
                let mut arr = Arr::default();
                let start = to_int(&arg(&a, 0));
                for i in 0..to_int(&arg(&a, 1)) {
                    arr.set(Key::Int(start + i), arg(&a, 2));
                }
                V::arr(arr)
            }
            "range" => {
                let mut arr = Arr::default();
                let (x, y) = (to_int(&arg(&a, 0)), to_int(&arg(&a, 1)));
                if x <= y {
                    for i in x..=y {
                        arr.push(V::Int(i));
                    }
                } else {
                    for i in (y..=x).rev() {
                        arr.push(V::Int(i));
                    }
                }
                V::arr(arr)
            }
            "in_array" => match arg(&a, 1) {
                V::Arr(x) => {
                    let strict = arg(&a, 2).truthy();
                    V::Bool(x.items.iter().any(|(_, v)| if strict { strict_eq(v, &arg(&a, 0)) } else { loose_eq(v, &arg(&a, 0)) }))
                }
                _ => V::Bool(false),
            },
            "sort" | "usort" | "ksort" | "asort" | "rsort" => V::Bool(true),
            "is_null" => V::Bool(matches!(arg(&a, 0), V::Null)),
            "is_string" => V::Bool(matches!(arg(&a, 0), V::Str(_))),
            "is_int" | "is_integer" | "is_long" => V::Bool(matches!(arg(&a, 0), V::Int(_))),
            "is_float" | "is_double" => V::Bool(matches!(arg(&a, 0), V::Float(_))),
            "is_bool" => V::Bool(matches!(arg(&a, 0), V::Bool(_))),
            "is_array" => V::Bool(matches!(arg(&a, 0), V::Arr(_))),
            "is_object" => V::Bool(matches!(arg(&a, 0), V::Obj(_))),
            "is_numeric" => V::Bool(match arg(&a, 0) {
                V::Int(_) | V::Float(_) => true,
                V::Str(s) => std::str::from_utf8(&s).ok().is_some_and(|t| t.trim().parse::<f64>().is_ok()),
                _ => false,
            }),
            "is_callable" => V::Bool(matches!(arg(&a, 0), V::Obj(_) | V::Str(_) | V::Arr(_))),
            "intval" => V::Int(to_int(&arg(&a, 0))),
            "floatval" | "doubleval" => match to_num(&arg(&a, 0)) {
                V::Int(i) => V::Float(i as f64),
                other => other,
            },
            "strval" => V::Str(self.to_str(&arg(&a, 0))?),
            "boolval" => V::Bool(arg(&a, 0).truthy()),
            "gettype" => V::str(match arg(&a, 0) {
                V::Null => "NULL",
                V::Bool(_) => "boolean",
                V::Int(_) => "integer",
                V::Float(_) => "double",
                V::Str(_) => "string",
                V::Arr(_) => "array",
                V::Obj(_) => "object",
            }),
            "get_debug_type" => V::Str(arg(&a, 0).type_name().into_bytes()),
            "get_class" => match arg(&a, 0) {
                V::Obj(o) => V::Str(o.class.borrow().clone().into_bytes()),
                _ => V::Bool(false),
            },
            "get_parent_class" => V::Bool(false),
            "spl_object_id" => match arg(&a, 0) {
                V::Obj(o) => V::Int(o.id as i64),
                _ => V::Int(0),
            },
            "spl_object_hash" => match arg(&a, 0) {
                V::Obj(o) => V::Str(format!("{:032x}", o.id).into_bytes()),
                _ => V::Str(Vec::new()),
            },
            "method_exists" => V::Bool(true),
            "property_exists" => V::Bool(match arg(&a, 0) {
                V::Obj(o) => o.prop(&s(&a, 1)).is_some(),
                _ => false,
            }),
            "class_exists" | "interface_exists" | "enum_exists" => V::Bool(self.class_exists(&path(&s(&a, 0)))),
            "function_exists" => V::Bool(true),
            "extension_loaded" => V::Bool(matches!(s(&a, 0).as_slice(), b"dom" | b"libxml" | b"xml")),
            "define" => V::Bool(true),
            "defined" => V::Bool(false),
            "ini_set" | "ini_get" | "error_reporting" | "set_error_handler" | "restore_error_handler" | "gc_collect_cycles" | "gc_enable" | "gc_disable" | "assert_options" | "setlocale" | "mt_srand" | "srand" | "date_default_timezone_set" | "set_exception_handler" => V::Null,
            "assert" => {
                if !arg(&a, 0).truthy() {
                    return Err(Flow::Throw(self.make_exception("AssertionError", b"assert(false)", 0)));
                }
                V::Bool(true)
            }
            "register_shutdown_function" => V::Null,
            "file_get_contents" => {
                let p = self.resolve_path(&s(&a, 0));
                match std::fs::read(&p) {
                    Ok(d) => V::Str(d),
                    Err(_) => {
                        self.emit_diag("Warning", format!("{}: Failed to open stream: No such file or directory", String::from_utf8_lossy(&s(&a, 0))).as_bytes());
                        V::Bool(false)
                    }
                }
            }
            "file_put_contents" => {
                let p = self.resolve_path(&s(&a, 0));
                let d = s(&a, 1);
                match std::fs::write(&p, &d) {
                    Ok(()) => V::Int(d.len() as i64),
                    Err(_) => V::Bool(false),
                }
            }
            "file_exists" | "is_file" => V::Bool(std::path::Path::new(&self.resolve_path(&s(&a, 0))).exists()),
            "unlink" => V::Bool(std::fs::remove_file(self.resolve_path(&s(&a, 0))).is_ok()),
            "is_dir" => V::Bool(std::path::Path::new(&self.resolve_path(&s(&a, 0))).is_dir()),
            "mkdir" => V::Bool(std::fs::create_dir_all(self.resolve_path(&s(&a, 0))).is_ok()),
            "rmdir" => V::Bool(std::fs::remove_dir(self.resolve_path(&s(&a, 0))).is_ok()),
            "dirname" => {
                let p = path(&s(&a, 0));
                V::Str(std::path::Path::new(&p).parent().map(|x| x.to_string_lossy().into_owned()).unwrap_or_else(|| ".".into()).into_bytes())
            }
            "basename" => {
                let p = path(&s(&a, 0));
                V::Str(std::path::Path::new(&p).file_name().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default().into_bytes())
            }
            "realpath" => match std::fs::canonicalize(self.resolve_path(&s(&a, 0))) {
                Ok(p) => V::Str(p.to_string_lossy().into_owned().into_bytes()),
                Err(_) => V::Bool(false),
            },
            "sys_get_temp_dir" => V::str("/tmp"),
            "getenv" => std::env::var(path(&s(&a, 0))).map_or(V::Bool(false), |v| V::Str(v.into_bytes())),
            "getcwd" => V::Str(std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default().into_bytes()),
            "tempnam" => V::Str(format!("/tmp/phpdomtest{}", std::process::id()).into_bytes()),
            "json_encode" => V::Str(json_encode(&arg(&a, 0))),
            "libxml_use_internal_errors" => {
                let e = match a.first() {
                    None | Some(V::Null) => None,
                    Some(v) => Some(v.truthy()),
                };
                V::Bool(php_libxml::error::use_internal_errors(e))
            }
            "libxml_get_errors" => {
                let mut arr = Arr::default();
                for e in php_libxml::error::get_errors() {
                    arr.push(libxml_error_obj(&e));
                }
                V::arr(arr)
            }
            "libxml_get_last_error" => match php_libxml::error::get_last_error() {
                Some(e) => libxml_error_obj(&e),
                None => V::Bool(false),
            },
            "libxml_clear_errors" => {
                php_libxml::error::clear_errors();
                V::Null
            }
            "libxml_disable_entity_loader" => V::Bool(php_libxml::io::disable_entity_loader(arg(&a, 0).truthy())),
            "libxml_set_external_entity_loader" => V::Bool(true),
            "iterator_to_array" => {
                let mut arr = Arr::default();
                let preserve = a.len() < 2 || arg(&a, 1).truthy();
                if let V::Obj(o) = arg(&a, 0) {
                    let it = self.get_iter_pub(&o)?;
                    loop {
                        let (valid, cur, k) = self.iter_state_pub(&it)?;
                        if !valid {
                            break;
                        }
                        if preserve {
                            arr.set(to_key(&k), cur);
                        } else {
                            arr.push(cur);
                        }
                        self.iter_next_pub(&it)?;
                    }
                } else if let V::Arr(x) = arg(&a, 0) {
                    return Ok(V::Arr(x));
                }
                V::arr(arr)
            }
            "iterator_count" => {
                let mut n = 0;
                if let V::Obj(o) = arg(&a, 0) {
                    let it = self.get_iter_pub(&o)?;
                    loop {
                        let (valid, _, _) = self.iter_state_pub(&it)?;
                        if !valid {
                            break;
                        }
                        n += 1;
                        self.iter_next_pub(&it)?;
                    }
                }
                V::Int(n)
            }
            "call_user_func" => {
                let f = arg(&a, 0);
                self.call_value(&f, a[1.min(a.len())..].to_vec())?
            }
            "call_user_func_array" => {
                let f = arg(&a, 0);
                let args = match arg(&a, 1) {
                    V::Arr(x) => x.items.iter().map(|(_, v)| v.clone()).collect(),
                    _ => vec![],
                };
                self.call_value(&f, args)?
            }
            "func_get_args" => V::arr(Arr::default()),
            "max" => {
                let vals: Vec<V> = match &a[..] {
                    [V::Arr(x)] => x.items.iter().map(|(_, v)| v.clone()).collect(),
                    _ => a.clone(),
                };
                vals.into_iter().reduce(|x, y| if compare(&y, &x) > 0 { y } else { x }).unwrap_or(V::Bool(false))
            }
            "min" => {
                let vals: Vec<V> = match &a[..] {
                    [V::Arr(x)] => x.items.iter().map(|(_, v)| v.clone()).collect(),
                    _ => a.clone(),
                };
                vals.into_iter().reduce(|x, y| if compare(&y, &x) < 0 { y } else { x }).unwrap_or(V::Bool(false))
            }
            "abs" => match to_num(&arg(&a, 0)) {
                V::Int(i) => V::Int(i.abs()),
                V::Float(f) => V::Float(f.abs()),
                o => o,
            },
            "round" | "floor" | "ceil" => {
                let f = match to_num(&arg(&a, 0)) {
                    V::Int(i) => i as f64,
                    V::Float(f) => f,
                    _ => 0.0,
                };
                V::Float(match lname {
                    "floor" => f.floor(),
                    "ceil" => f.ceil(),
                    _ => f.round(),
                })
            }
            "random_int" | "mt_rand" | "rand" => V::Int(to_int(&arg(&a, 0))),
            "microtime" | "time" | "hrtime" => V::Int(0),
            "memory_get_usage" | "memory_get_peak_usage" => V::Int(0),
            "dom_import_simplexml" | "simplexml_import_dom" | "simplexml_load_string" => {
                return Err(self.unsupported("simplexml"));
            }
            "stream_wrapper_register" | "stream_context_create" | "fopen" | "fsockopen" | "stream_socket_server" => {
                return Err(self.unsupported("streams"));
            }
            "preg_replace" | "preg_match" | "preg_split" | "preg_quote" | "iconv" | "mb_convert_encoding" | "serialize" | "unserialize" | "json_decode" | "array_walk" => {
                return Err(self.unsupported(lname));
            }
            "debug_zval_refcount" | "debug_zval_dump" => return Err(self.unsupported(lname)),
            "exit" | "die" => {
                if let Some(V::Str(m)) = a.first() {
                    self.echo(m);
                }
                return Err(Flow::Exit);
            }
            "fn" => V::Null,
            "clone" => self.clone_value(&arg(&a, 0))?,
            "get_defined_constants" => {
                let mut lib = Arr::default();
                for n in ["LIBXML_NOENT", "LIBXML_DTDLOAD", "LIBXML_DTDATTR", "LIBXML_DTDVALID", "LIBXML_NOERROR", "LIBXML_NOWARNING", "LIBXML_NOBLANKS", "LIBXML_XINCLUDE", "LIBXML_NSCLEAN", "LIBXML_NOCDATA", "LIBXML_NONET", "LIBXML_PEDANTIC", "LIBXML_COMPACT", "LIBXML_NOXMLDECL", "LIBXML_PARSEHUGE", "LIBXML_BIGLINES", "LIBXML_NOEMPTYTAG", "LIBXML_SCHEMA_CREATE", "LIBXML_HTML_NOIMPLIED", "LIBXML_HTML_NODEFDTD", "LIBXML_ERR_NONE", "LIBXML_ERR_WARNING", "LIBXML_ERR_ERROR", "LIBXML_ERR_FATAL", "LIBXML_RECOVER"] {
                    if let Ok(v) = self.const_by_name(n) {
                        lib.set(Key::Str(n.as_bytes().to_vec()), v);
                    }
                }
                let mut all = Arr::default();
                all.set(Key::Str(b"libxml".to_vec()), V::arr(lib));
                V::arr(all)
            }
            "array_flip" => {
                let mut arr = Arr::default();
                if let V::Arr(x) = arg(&a, 0) {
                    for (k, v) in &x.items {
                        arr.set(to_key(v), match k {
                            Key::Int(i) => V::Int(*i),
                            Key::Str(s) => V::Str(s.clone()),
                        });
                    }
                }
                V::arr(arr)
            }
            "array_intersect_key" => {
                let mut arr = Arr::default();
                if let (V::Arr(x), V::Arr(y)) = (arg(&a, 0), arg(&a, 1)) {
                    for (k, v) in &x.items {
                        if y.get(k).is_some() {
                            arr.set(k.clone(), v.clone());
                        }
                    }
                }
                V::arr(arr)
            }
            "array_sum" => {
                let mut t = V::Int(0);
                if let V::Arr(x) = arg(&a, 0) {
                    for (_, v) in &x.items {
                        t = match (to_num(&t), to_num(v)) {
                            (V::Int(p), V::Int(q)) => V::Int(p + q),
                            (p, q) => V::Float(super::interp::to_f_pub(&p) + super::interp::to_f_pub(&q)),
                        };
                    }
                }
                t
            }
            "array_key_first" => match arg(&a, 0) {
                V::Arr(x) => x.items.first().map_or(V::Null, |(k, _)| match k {
                    Key::Int(i) => V::Int(*i),
                    Key::Str(s) => V::Str(s.clone()),
                }),
                _ => V::Null,
            },
            "array_key_last" => match arg(&a, 0) {
                V::Arr(x) => x.items.last().map_or(V::Null, |(k, _)| match k {
                    Key::Int(i) => V::Int(*i),
                    Key::Str(s) => V::Str(s.clone()),
                }),
                _ => V::Null,
            },
            "array_pop" | "array_shift" => match arg(&a, 0) {
                V::Arr(x) => (if lname == "array_pop" { x.items.last() } else { x.items.first() }).map_or(V::Null, |(_, v)| v.clone()),
                _ => V::Null,
            },
            "chdir" => V::Bool(std::env::set_current_dir(self.resolve_path_pub(&s(&a, 0))).is_ok()),
            "ob_start" => {
                self.ob_start();
                V::Bool(true)
            }
            "ob_get_clean" | "ob_get_contents" => V::Str(self.ob_end(lname == "ob_get_clean")),
            "ob_end_clean" => {
                self.ob_end(true);
                V::Bool(true)
            }
            "ob_end_flush" | "ob_flush" | "flush" => V::Bool(true),
            "array_is_list" => V::Bool(match arg(&a, 0) {
                V::Arr(x) => x.items.iter().enumerate().all(|(i, (k, _))| *k == Key::Int(i as i64)),
                _ => false,
            }),
            "str_contains_any" => V::Null,
            _ => {
                return Err(Flow::Throw(self.make_exception("Error", format!("Call to undefined function {name}()").as_bytes(), 0)));
            }
        })
    }

    pub fn unsupported(&self, what: &str) -> Flow {
        Flow::Throw(self.make_exception("UnsupportedInTestHarness", what.as_bytes(), 0))
    }

    fn resolve_path(&self, p: &[u8]) -> String {
        let s = path(p);
        if s.starts_with('/') || s.contains("://") {
            s
        } else {
            format!("{}/{}", std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(), s)
        }
    }

    fn sprintf(&self, a: &[V]) -> R<Vec<u8>> {
        let fmt = s(a, 0);
        let mut out = Vec::new();
        let mut argi = 1;
        let mut i = 0;
        while i < fmt.len() {
            let c = fmt[i];
            if c != b'%' {
                out.push(c);
                i += 1;
                continue;
            }
            i += 1;
            if i >= fmt.len() {
                break;
            }
            if fmt[i] == b'%' {
                out.push(b'%');
                i += 1;
                continue;
            }
            let mut flags = Vec::new();
            while i < fmt.len() && matches!(fmt[i], b'-' | b'+' | b' ' | b'0' | b'\'') {
                flags.push(fmt[i]);
                i += 1;
            }
            let mut width = 0usize;
            while i < fmt.len() && fmt[i].is_ascii_digit() {
                width = width * 10 + usize::from(fmt[i] - b'0');
                i += 1;
            }
            let mut prec = None;
            if i < fmt.len() && fmt[i] == b'.' {
                i += 1;
                let mut p = 0;
                while i < fmt.len() && fmt[i].is_ascii_digit() {
                    p = p * 10 + usize::from(fmt[i] - b'0');
                    i += 1;
                }
                prec = Some(p);
            }
            let conv = fmt.get(i).copied().unwrap_or(b's');
            i += 1;
            let v = arg(a, argi);
            argi += 1;
            let mut piece = match conv {
                b'd' | b'i' => to_int(&v).to_string().into_bytes(),
                b'u' => (to_int(&v) as u64).to_string().into_bytes(),
                b'x' => format!("{:x}", to_int(&v)).into_bytes(),
                b'X' => format!("{:X}", to_int(&v)).into_bytes(),
                b'f' | b'F' => {
                    let f = match to_num(&v) {
                        V::Int(i) => i as f64,
                        V::Float(f) => f,
                        _ => 0.0,
                    };
                    format!("{:.*}", prec.unwrap_or(6), f).into_bytes()
                }
                b'c' => vec![(to_int(&v) & 0xff) as u8],
                _ => {
                    let mut b = self.to_str(&v)?;
                    if let Some(p) = prec {
                        b.truncate(p);
                    }
                    b
                }
            };
            if piece.len() < width {
                let padc = if flags.contains(&b'0') { b'0' } else { b' ' };
                let pad = vec![padc; width - piece.len()];
                if flags.contains(&b'-') {
                    piece.extend_from_slice(&vec![b' '; width - piece.len()]);
                } else {
                    let mut p = pad;
                    p.extend_from_slice(&piece);
                    piece = p;
                }
            }
            out.extend_from_slice(&piece);
        }
        Ok(out)
    }
}

fn replace_bytes(h: &[u8], n: &[u8], r: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < h.len() {
        if h[i..].starts_with(n) {
            out.extend_from_slice(r);
            i += n.len();
        } else {
            out.push(h[i]);
            i += 1;
        }
    }
    out
}

fn json_encode(v: &V) -> Vec<u8> {
    match v {
        V::Null => b"null".to_vec(),
        V::Bool(b) => if *b { b"true".to_vec() } else { b"false".to_vec() },
        V::Int(i) => i.to_string().into_bytes(),
        V::Float(f) => super::output::float_repr(*f).into_bytes(),
        V::Str(s) => {
            let mut o = vec![b'"'];
            for &c in s {
                match c {
                    b'"' => o.extend_from_slice(b"\\\""),
                    b'\\' => o.extend_from_slice(b"\\\\"),
                    b'/' => o.extend_from_slice(b"\\/"),
                    b'\n' => o.extend_from_slice(b"\\n"),
                    _ => o.push(c),
                }
            }
            o.push(b'"');
            o
        }
        V::Arr(a) => {
            let is_list = a.items.iter().enumerate().all(|(i, (k, _))| *k == Key::Int(i as i64));
            let mut o = Vec::new();
            o.push(if is_list { b'[' } else { b'{' });
            for (i, (k, v)) in a.items.iter().enumerate() {
                if i > 0 {
                    o.push(b',');
                }
                if !is_list {
                    let kb = match k {
                        Key::Int(i) => i.to_string().into_bytes(),
                        Key::Str(s) => s.clone(),
                    };
                    o.extend_from_slice(&json_encode(&V::Str(kb)));
                    o.push(b':');
                }
                o.extend_from_slice(&json_encode(v));
            }
            o.push(if is_list { b']' } else { b'}' });
            o
        }
        V::Obj(_) => b"{}".to_vec(),
    }
}

fn libxml_error_obj(e: &php_libxml::error::LibXmlError) -> V {
    let o = new_obj("LibXMLError", ObjKind::Plain);
    o.set_prop(b"level", V::Int(i64::from(e.level)));
    o.set_prop(b"code", V::Int(i64::from(e.code)));
    o.set_prop(b"column", V::Int(i64::from(e.column)));
    o.set_prop(b"message", V::Str(e.message.clone()));
    o.set_prop(b"file", V::Str(e.file_or_empty().to_vec()));
    o.set_prop(b"line", V::Int(i64::from(e.line)));
    V::Obj(o)
}

#[allow(dead_code)]
fn unused(_: DomObject, _: Callable) {}
