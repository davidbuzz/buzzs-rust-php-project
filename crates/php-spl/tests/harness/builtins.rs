//! Built-in classes, constants and functions of the test interpreter.

use super::interp::*;
use php_parser::ast::{Expr, ExprKind, Span};
use php_runtime::{
    ops, ArrayKey, ErrorLevel, HashTable, Obj, PhpRef, RtResult, RuntimeContext, RuntimeError, Value, ZArray, ZStr,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

fn lit(v: Value) -> Expr {
    let sp = Span::new(0, 0, 0);
    let kind = match v {
        Value::Str(s) => ExprKind::String(s.as_bytes().to_vec()),
        Value::Int(i) => ExprKind::Int(i),
        Value::Array(_) => ExprKind::Array { items: Vec::new(), syntax: php_parser::ast::ArraySyntax::Short },
        _ => ExprKind::Const(php_parser::ast::Name {
            kind: php_parser::ast::NameKind::Unqualified,
            name: b"null".to_vec(),
            span: sp,
        }),
    };
    Expr::new(kind, sp)
}

fn native_class(
    it: &mut Interp,
    name: &str,
    parent: Option<&str>,
    interfaces: &[&str],
    kind: CKind,
    props: Vec<PropDecl>,
    methods: &[(&str, NativeFn, bool)],
) {
    let parent = parent.and_then(|p| it.find_class(p.as_bytes()));
    let mut all_ifaces: Vec<String> = Vec::new();
    for i in interfaces {
        if let Some(ic) = it.find_class(i.as_bytes()) {
            all_ifaces.push(ic.name.clone());
            for x in &ic.interfaces {
                if !all_ifaces.contains(x) {
                    all_ifaces.push(x.clone());
                }
            }
        } else {
            all_ifaces.push((*i).to_string());
        }
    }
    let mut mm = HashMap::new();
    for (n, f, st) in methods {
        mm.insert(
            n.to_ascii_lowercase(),
            Rc::new(MethodH { name: (*n).to_string(), class: name.to_string(), kind: MKind::Native(*f), is_static: *st, visibility: 0 }),
        );
    }
    let spl_base = parent.as_ref().and_then(|p| p.spl_base);
    let c = Rc::new(Class {
        name: name.to_string(),
        parent,
        interfaces: all_ifaces,
        kind,
        consts: RefCell::new(Vec::new()),
        statics: RefCell::new(HashMap::new()),
        props,
        methods: mm,
        spl_base,
        internal: true,
        allow_dynamic: name == "stdClass" || name == "__PHP_Incomplete_Class",
    });
    it.classes.insert(name.to_ascii_lowercase(), c);
}

fn exc_props(base: &str) -> Vec<PropDecl> {
    vec![
        PropDecl { name: "message".into(), visibility: 1, default: Some(lit(Value::from(""))), is_static: false, typed: false },
        PropDecl { name: "string".into(), visibility: 2, default: Some(lit(Value::from(""))), is_static: false, typed: true },
        PropDecl { name: "code".into(), visibility: 1, default: Some(lit(Value::Int(0))), is_static: false, typed: false },
        PropDecl { name: "file".into(), visibility: 1, default: Some(lit(Value::from(""))), is_static: false, typed: true },
        PropDecl { name: "line".into(), visibility: 1, default: Some(lit(Value::Int(0))), is_static: false, typed: true },
        PropDecl { name: "trace".into(), visibility: 2, default: Some(lit(Value::empty_array())), is_static: false, typed: true },
        PropDecl { name: "previous".into(), visibility: 2, default: Some(lit(Value::Null)), is_static: false, typed: true },
    ]
    .into_iter()
    .map(|mut p| {
        let _ = base;
        p.typed = false;
        p
    })
    .collect()
}

fn exc_construct(it: &mut Interp, this: Option<&Obj>, args: &[Value]) -> RtResult<Value> {
    let o = this.unwrap();
    if let Some(m) = args.first() {
        let m = m.to_zstr(it)?;
        it.prop_set_raw(o, "message", Value::Str(m));
    }
    if let Some(c) = args.get(1) {
        it.prop_set_raw(o, "code", c.clone());
    }
    if let Some(p) = args.get(2) {
        it.prop_set_raw(o, "previous", p.clone());
    }
    Ok(Value::Null)
}

fn exc_get(it: &mut Interp, this: Option<&Obj>, name: &str) -> RtResult<Value> {
    Ok(it.prop_get_raw(this.unwrap(), name))
}

fn register_exceptions(it: &mut Interp) {
    let methods: &[(&str, NativeFn, bool)] = &[
        ("__construct", exc_construct, false),
        ("getMessage", |it, t, _| exc_get(it, t, "message"), false),
        ("getCode", |it, t, _| exc_get(it, t, "code"), false),
        ("getPrevious", |it, t, _| exc_get(it, t, "previous"), false),
        ("getFile", |it, t, _| exc_get(it, t, "file"), false),
        ("getLine", |it, t, _| exc_get(it, t, "line"), false),
        ("getTrace", |_, _, _| Ok(Value::empty_array()), false),
        ("getTraceAsString", |_, _, _| Ok(Value::from("#0 {main}")), false),
        ("__toString", |it, t, _| Ok(Value::from(it.exception_to_string(t.unwrap())?)), false),
    ];
    native_class(it, "Exception", None, &["Throwable"], CKind::Class, exc_props("Exception"), methods);
    native_class(it, "Error", None, &["Throwable"], CKind::Class, exc_props("Error"), methods);
    for (n, p) in [
        ("ErrorException", "Exception"),
        ("TypeError", "Error"),
        ("ValueError", "Error"),
        ("ArithmeticError", "Error"),
        ("DivisionByZeroError", "ArithmeticError"),
        ("ArgumentCountError", "TypeError"),
        ("UnhandledMatchError", "Error"),
        ("JsonException", "Exception"),
    ] {
        native_class(it, n, Some(p), &[], CKind::Class, Vec::new(), &[]);
    }
}

pub fn register_classes(it: &mut Interp) {
    for (n, parents) in [
        ("Traversable", &[][..]),
        ("Iterator", &["Traversable"][..]),
        ("IteratorAggregate", &["Traversable"][..]),
        ("ArrayAccess", &[][..]),
        ("Countable", &[][..]),
        ("Serializable", &[][..]),
        ("Stringable", &[][..]),
        ("JsonSerializable", &[][..]),
        ("Throwable", &["Stringable"][..]),
        ("UnitEnum", &[][..]),
    ] {
        native_class(it, n, None, parents, CKind::Interface, Vec::new(), &[]);
    }
    native_class(it, "stdClass", None, &[], CKind::Class, Vec::new(), &[]);
    native_class(it, "__PHP_Incomplete_Class", None, &[], CKind::Class, Vec::new(), &[]);
    native_class(
        it,
        "Closure",
        None,
        &[],
        CKind::Class,
        Vec::new(),
        &[
            ("__invoke", |it, t, a| it.call_value(&Value::Object(t.unwrap().clone()), a), false),
            ("bindTo", closure_bind_to, false),
            ("call", closure_call, false),
            ("bind", |it, _, a| closure_bind_to(it, a.first().and_then(Value::as_object), &a[1..]), true),
            ("fromCallable", |it, _, a| {
                let v = a.first().cloned().unwrap_or_default();
                if let Value::Object(o) = &v {
                    if matches!(&*tobj(o).extra.borrow(), Extra::Closure(_)) {
                        return Ok(v.clone());
                    }
                }
                let def = Rc::new(FuncDef { return_type: None, name: "{closure}".into(), params: Vec::new(), body: Rc::new(Vec::new()), arrow: None, by_ref: false, is_generator: false, statics: Rc::new(RefCell::new(HashMap::new())) });
                let cb = v.clone();
                let holder = it.make_closure(ClosureData { func: def, bound: vec![(b"__cb".to_vec(), cb)], this: None, scope: None, builtin: Some("__call_bound".into()), method: None })?;
                Ok(holder)
            }, true),
        ],
    );
    native_class(
        it,
        "InternalIterator",
        None,
        &["Iterator"],
        CKind::Class,
        Vec::new(),
        &[
            ("current", |it, t, _| super::host::internal_iter_call(it, t.unwrap(), "current"), false),
            ("key", |it, t, _| super::host::internal_iter_call(it, t.unwrap(), "key"), false),
            ("next", |it, t, _| super::host::internal_iter_call(it, t.unwrap(), "next"), false),
            ("valid", |it, t, _| super::host::internal_iter_call(it, t.unwrap(), "valid"), false),
            ("rewind", |it, t, _| super::host::internal_iter_call(it, t.unwrap(), "rewind"), false),
        ],
    );
    register_exceptions(it);
    // SPL classes, parents and interfaces first
    let mut defs: Vec<&php_spl::classes::ClassDef> = php_spl::classes::class_table().iter().collect();
    let mut ordered = Vec::new();
    while !defs.is_empty() {
        let before = defs.len();
        defs.retain(|d| {
            let deps_ok = d.parent.iter().chain(d.interfaces.iter()).all(|n| {
                it.find_class(n.as_bytes()).is_some() || ordered.iter().any(|o: &&php_spl::classes::ClassDef| o.name == *n)
            });
            if deps_ok {
                ordered.push(*d);
                false
            } else {
                true
            }
        });
        if defs.len() == before {
            ordered.extend(defs.drain(..));
        }
    }
    for def in ordered {
        let parent = def.parent.and_then(|p| it.find_class(p.as_bytes()));
        let mut ifaces: Vec<String> = Vec::new();
        for i in def.interfaces {
            if let Some(ic) = it.find_class(i.as_bytes()) {
                ifaces.push(ic.name.clone());
                for x in &ic.interfaces {
                    if !ifaces.contains(x) {
                        ifaces.push(x.clone());
                    }
                }
            }
        }
        let mut methods = HashMap::new();
        for m in def.methods {
            let abstract_ = m.flags & php_spl::classes::MethodFlags::ABSTRACT != 0
                || def.kind == php_spl::classes::ClassKind::Interface;
            methods.insert(
                m.name.to_ascii_lowercase(),
                Rc::new(MethodH {
                    name: m.name.to_string(),
                    class: def.name.to_string(),
                    kind: if abstract_ { MKind::Abstract } else { MKind::Spl },
                    is_static: m.flags & php_spl::classes::MethodFlags::STATIC != 0,
                    visibility: if m.flags & php_spl::classes::MethodFlags::PROTECTED != 0 { 1 } else { 0 },
                }),
            );
        }
        let spl_base = php_spl::object::canonical_class(def.name).or_else(|| parent.as_ref().and_then(|p| p.spl_base));
        let kind = match def.kind {
            php_spl::classes::ClassKind::Interface => CKind::Interface,
            php_spl::classes::ClassKind::Abstract => CKind::Abstract,
            php_spl::classes::ClassKind::Class => CKind::Class,
        };
        let props = def
            .properties
            .iter()
            .map(|p| PropDecl { name: p.name.to_string(), visibility: 0, default: Some(lit(Value::Null)), is_static: false, typed: false })
            .collect();
        let c = Rc::new(Class {
            name: def.name.to_string(),
            parent,
            interfaces: ifaces,
            kind,
            consts: RefCell::new(def.constants.iter().map(|(n, v)| ((*n).to_string(), Ok(Value::Int(*v)))).collect()),
            statics: RefCell::new(HashMap::new()),
            props,
            methods,
            spl_base,
            internal: true,
            allow_dynamic: false,
        });
        it.classes.insert(def.name.to_ascii_lowercase(), c);
    }
}

fn closure_bind_to(it: &mut Interp, this: Option<&Obj>, args: &[Value]) -> RtResult<Value> {
    let o = this.unwrap();
    let Extra::Closure(c) = &*tobj(o).extra.borrow() else { return Ok(Value::Null) };
    let c = c.clone();
    let new_this = match args.first() {
        Some(Value::Object(t)) => Some(t.clone()),
        _ => None,
    };
    let scope = match args.get(1) {
        Some(Value::Object(s)) => Some(tobj(s).class.clone()),
        Some(Value::Str(s)) => it.find_class(s.as_bytes()),
        _ => new_this.as_ref().map(|t| tobj(t).class.clone()).or_else(|| c.scope.clone()),
    };
    it.make_closure(ClosureData {
        func: c.func.clone(),
        bound: c.bound.clone(),
        this: new_this,
        scope,
        builtin: c.builtin.clone(),
        method: c.method.clone(),
    })
}

fn closure_call(it: &mut Interp, this: Option<&Obj>, args: &[Value]) -> RtResult<Value> {
    let o = this.unwrap();
    let Extra::Closure(c) = &*tobj(o).extra.borrow() else { return Ok(Value::Null) };
    let c = c.clone();
    let Some(Value::Object(nt)) = args.first() else { return Ok(Value::Null) };
    let scope = Some(tobj(nt).class.clone());
    it.call_user(&c.func, &args[1..], Some(nt.clone()), scope.clone(), scope, &c.bound)
}

pub fn register_constants(it: &mut Interp) {
    let ints: &[(&str, i64)] = &[
        ("E_ALL", 32767),
        ("E_ERROR", 1),
        ("E_WARNING", 2),
        ("E_NOTICE", 8),
        ("E_DEPRECATED", 8192),
        ("E_USER_ERROR", 256),
        ("E_USER_WARNING", 512),
        ("E_USER_NOTICE", 1024),
        ("E_USER_DEPRECATED", 16384),
        ("E_STRICT", 2048),
        ("E_RECOVERABLE_ERROR", 4096),
        ("SORT_REGULAR", 0),
        ("SORT_NUMERIC", 1),
        ("SORT_STRING", 2),
        ("SORT_DESC", 3),
        ("SORT_ASC", 4),
        ("SORT_LOCALE_STRING", 5),
        ("SORT_NATURAL", 6),
        ("SORT_FLAG_CASE", 8),
        ("COUNT_NORMAL", 0),
        ("COUNT_RECURSIVE", 1),
        ("PHP_INT_MAX", i64::MAX),
        ("PHP_INT_MIN", i64::MIN),
        ("PHP_INT_SIZE", 8),
        ("PREG_SPLIT_NO_EMPTY", 1),
        ("PREG_PATTERN_ORDER", 1),
        ("PREG_SET_ORDER", 2),
        ("PREG_OFFSET_CAPTURE", 256),
        ("PREG_SPLIT_DELIM_CAPTURE", 2),
        ("PREG_SPLIT_OFFSET_CAPTURE", 4),
        ("PREG_UNMATCHED_AS_NULL", 512),
        ("SEEK_SET", 0),
        ("PATHINFO_DIRNAME", 1),
        ("PATHINFO_BASENAME", 2),
        ("PATHINFO_EXTENSION", 4),
        ("PATHINFO_FILENAME", 8),
        ("SEEK_CUR", 1),
        ("SEEK_END", 2),
        ("LOCK_SH", 1),
        ("LOCK_EX", 2),
        ("LOCK_UN", 3),
        ("GLOB_BRACE", 1024),
        ("GLOB_ONLYDIR", 8192),
        ("GLOB_MARK", 2),
        ("GLOB_NOSORT", 4),
        ("JSON_PRETTY_PRINT", 128),
        ("FILE_APPEND", 8),
        ("FILE_IGNORE_NEW_LINES", 2),
        ("FILE_SKIP_EMPTY_LINES", 4),
        ("ARRAY_FILTER_USE_KEY", 2),
        ("ARRAY_FILTER_USE_BOTH", 1),
        ("PHP_VERSION_ID", 80600),
        ("PHP_MAJOR_VERSION", 8),
    ];
    for (n, v) in ints {
        it.constants.insert((*n).to_string(), Value::Int(*v));
    }
    it.constants.insert("PHP_EOL".into(), Value::from("\n"));
    it.constants.insert("PHP_OS".into(), Value::from("Linux"));
    it.constants.insert("PHP_OS_FAMILY".into(), Value::from("Linux"));
    it.constants.insert("DIRECTORY_SEPARATOR".into(), Value::from("/"));
    it.constants.insert("PATH_SEPARATOR".into(), Value::from(":"));
    it.constants.insert("PHP_VERSION".into(), Value::from("8.6.0-dev"));
    it.constants.insert("NAN".into(), Value::Float(f64::NAN));
    it.constants.insert("INF".into(), Value::Float(f64::INFINITY));
    it.constants.insert("PHP_FLOAT_EPSILON".into(), Value::Float(f64::EPSILON));
}

/// Whether argument `i` of builtin `name` is by reference.
pub fn by_ref(name: &str, i: usize) -> bool {
    match name {
        "sort" | "rsort" | "usort" | "uasort" | "uksort" | "ksort" | "krsort" | "asort" | "arsort" | "natsort"
        | "natcasesort" | "shuffle" | "array_push" | "array_pop" | "array_shift" | "array_unshift" | "array_splice"
        | "end" | "reset" | "next" | "prev" | "array_walk" | "array_walk_recursive" | "settype" => i == 0,
        "preg_match" | "preg_match_all" => i == 2,
        "array_multisort" => true,
        "str_replace" => i == 3,
        _ => false,
    }
}

pub fn exists(name: &str) -> bool {
    matches!(
        name,
        "var_dump" | "print_r" | "var_export" | "printf" | "sprintf" | "count" | "sizeof" | "strlen" | "str_repeat"
            | "implode" | "join" | "explode" | "array_keys" | "array_values" | "in_array" | "range" | "array_map"
            | "array_merge" | "array_slice" | "array_reverse" | "array_sum" | "gettype" | "get_class" | "is_array"
            | "is_object" | "is_int" | "is_string" | "is_bool" | "is_null" | "is_numeric" | "is_callable" | "intval"
            | "strval" | "serialize" | "unserialize" | "strtoupper" | "strtolower" | "ucfirst" | "trim" | "str_pad"
            | "max" | "min" | "abs" | "chr" | "ord" | "call_user_func" | "call_user_func_array" | "func_get_args"
            | "array_key_exists" | "method_exists" | "class_exists" | "function_exists" | "substr" | "str_replace"
            | "array_filter" | "usort" | "uasort" | "uksort" | "sort" | "ksort" | "strrev" | "is_float" | "floatval"
            | "array_push" | "array_pop" | "array_shift" | "array_unshift" | "array_search" | "array_flip"
            | "array_combine" | "array_fill" | "array_key_last" | "array_key_first" | "json_encode" | "dirname"
            | "basename" | "file_exists" | "unlink" | "spl_object_id" | "spl_object_hash" | "iterator_to_array"
            | "iterator_count" | "iterator_apply" | "strcmp" | "ucwords" | "lcfirst" | "str_contains"
            | "str_starts_with" | "str_ends_with" | "array_unique" | "rsort" | "asort" | "arsort" | "krsort"
            | "natsort" | "natcasesort" | "is_iterable" | "array_walk" | "strpos" | "array_diff" | "array_intersect"
            | "array_column" | "array_chunk" | "array_fill_keys" | "array_rand" | "nl2br" | "number_format"
            | "round" | "floor" | "ceil" | "sqrt" | "pow" | "dechex" | "hexdec" | "bin2hex" | "md5" | "crc32"
            | "is_a" | "is_subclass_of" | "get_parent_class" | "get_object_vars" | "property_exists"
            | "interface_exists" | "array_reduce" | "printf_" | "end" | "reset" | "next" | "prev" | "current"
            | "key" | "array_product" | "str_split" | "mb_strlen" | "boolval" | "settype"
    ) || php_spl::classes::function_table().iter().any(|f| f.name == name)
}

fn arr(v: &Value) -> RtResult<ZArray> {
    match v.deref() {
        Value::Array(a) => Ok(a),
        other => Err(RuntimeError::TypeError(format!("expected array, {} given", other.value_name()))),
    }
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).map(Value::deref).unwrap_or_default()
}

fn refarg(args: &[Value], i: usize) -> PhpRef {
    match args.get(i) {
        Some(Value::Reference(r)) => r.clone(),
        Some(v) => PhpRef::new(v.clone()),
        None => PhpRef::new(Value::Null),
    }
}

fn zstr(it: &mut Interp, v: &Value) -> RtResult<ZStr> {
    v.to_zstr(it)
}

struct Sink;
impl php_support::diag::DiagnosticSink for Sink {
    fn report(&mut self, _d: php_support::Diagnostic) {}
}

struct FArg(Value, Vec<u8>, i64, f64);
impl php_support::fmt::printf::FormatArg for FArg {
    fn as_int(&self) -> i64 {
        self.2
    }
    fn as_float(&self) -> f64 {
        self.3
    }
    fn as_str(&self) -> std::borrow::Cow<'_, [u8]> {
        std::borrow::Cow::Borrowed(&self.1)
    }
    fn exact_int(&self) -> Option<i64> {
        self.0.as_int()
    }
}

fn do_sprintf(it: &mut Interp, args: &[Value]) -> RtResult<Vec<u8>> {
    let fmt = zstr(it, &arg(args, 0))?;
    let mut fa = Vec::new();
    for a in &args[1..] {
        let v = a.deref();
        let st = match &v {
            Value::Array(_) => b"Array".to_vec(),
            _ => v.to_zstr(it)?.as_bytes().to_vec(),
        };
        let i = v.to_long(it)?;
        let f = v.to_double(it)?;
        fa.push(FArg(v, st, i, f));
    }
    let refs: Vec<&dyn php_support::fmt::printf::FormatArg> = fa.iter().map(|a| a as &dyn php_support::fmt::printf::FormatArg).collect();
    php_support::fmt::printf::sprintf(fmt.as_bytes(), &refs, &mut Sink).map_err(|e| RuntimeError::ValueError(e.message().to_string()))
}

fn with_callback<R>(
    it: &mut Interp,
    cb: Value,
    f: impl FnOnce(&mut Interp, &mut php_stdarr::Callback<'_, Interp>) -> RtResult<R>,
) -> RtResult<R> {
    let mut call = move |it: &mut Interp, a: &[Value]| it.call_value(&cb, a);
    f(it, &mut call)
}

#[allow(clippy::too_many_lines)]
pub fn call(it: &mut Interp, name: &str, args: &[Value]) -> RtResult<Value> {
    if let Some(msg) = php_spl::classes::function_deprecation_message(name) {
        it.emit(ErrorLevel::Deprecated, &msg)?;
    }
    if let Some(r) = php_spl::call_function(it, name, args) {
        return r;
    }
    let a0 = || arg(args, 0);
    Ok(match name {
        "__call_bound" => {
            let cb = it.frame().vars.get(b"__cb".as_slice()).cloned().unwrap_or_default();
            return it.call_value(&cb, args);
        }
        "var_dump" => {
            for a in args {
                let mut out = Vec::new();
                let r = php_runtime::dump::var_dump(&mut out, &a.deref(), it);
                it.echo(&out);
                r?;
            }
            Value::Null
        }
        "print_r" => {
            let mut out = Vec::new();
            php_runtime::dump::print_r_into(&mut out, &a0(), it)?;
            if arg(args, 1).to_bool(it)? {
                Value::Str(ZStr::from(out))
            } else {
                it.echo(&out);
                Value::Bool(true)
            }
        }
        "var_export" => {
            let out = php_runtime::dump::var_export(&a0(), it)?;
            if arg(args, 1).to_bool(it)? {
                Value::Str(ZStr::from(out))
            } else {
                it.echo(&out);
                Value::Null
            }
        }
        "printf" => {
            let out = do_sprintf(it, args)?;
            it.echo(&out);
            Value::Int(out.len() as i64)
        }
        "sprintf" => Value::Str(ZStr::from(do_sprintf(it, args)?)),
        "vsprintf" | "vprintf" => {
            let mut v = vec![a0()];
            v.extend(arr(&arg(args, 1))?.values().cloned());
            let out = do_sprintf(it, &v)?;
            if name == "vprintf" {
                it.echo(&out);
                Value::Int(out.len() as i64)
            } else {
                Value::Str(ZStr::from(out))
            }
        }
        "count" | "sizeof" => {
            let mode = arg(args, 1).to_long(it)?;
            Value::Int(php_stdarr::basic::count(it, name, &a0(), mode)?)
        }
        "strlen" => Value::Int(zstr(it, &a0())?.len() as i64),
        "str_repeat" => {
            let s = zstr(it, &a0())?;
            let n = arg(args, 1).to_long(it)?.max(0) as usize;
            Value::Str(ZStr::from(s.as_bytes().repeat(n)))
        }
        "implode" | "join" => {
            let (sep, a) = match (a0(), arg(args, 1)) {
                (Value::Array(a), Value::Null) => (ZStr::empty(), a),
                (Value::Array(a), s) => (zstr(it, &s)?, a),
                (s, a) => (zstr(it, &s)?, arr(&a)?),
            };
            let mut out = Vec::new();
            for (i, v) in a.values().enumerate() {
                if i > 0 {
                    out.extend_from_slice(sep.as_bytes());
                }
                out.extend_from_slice(zstr(it, v)?.as_bytes());
            }
            Value::Str(ZStr::from(out))
        }
        "explode" => {
            let sep = zstr(it, &a0())?;
            let s = zstr(it, &arg(args, 1))?;
            let parts: Vec<Value> = split_bytes(s.as_bytes(), sep.as_bytes()).into_iter().map(|p| Value::Str(ZStr::from(p))).collect();
            Value::Array(ZArray::from_values(parts))
        }
        "str_split" => {
            let s = zstr(it, &a0())?;
            let n = if args.len() > 1 { arg(args, 1).to_long(it)?.max(1) as usize } else { 1 };
            Value::Array(ZArray::from_values(s.as_bytes().chunks(n).map(|c| Value::Str(ZStr::from(c)))))
        }
        "array_keys" => Value::Array(php_stdarr::basic::array_keys(it, &*arr(&a0())?, args.get(1), false)?),
        "array_values" => Value::Array(php_stdarr::basic::array_values(&arr(&a0())?)),
        "in_array" => {
            let strict = arg(args, 2).to_bool(it)?;
            Value::Bool(php_stdarr::basic::in_array(it, &a0(), &*arr(&arg(args, 1))?, strict)?)
        }
        "array_search" => {
            let strict = arg(args, 2).to_bool(it)?;
            php_stdarr::basic::array_search(it, &a0(), &*arr(&arg(args, 1))?, strict)?
        }
        "range" => Value::Array(php_stdarr::range::range(it, &a0(), &arg(args, 1), args.get(2))?),
        "array_map" => {
            let cb = a0();
            let arrays: Vec<Value> = args[1..].iter().map(Value::deref).collect();
            if cb.is_null() {
                Value::Array(php_stdarr::callback::array_map(it, None, &arrays)?)
            } else {
                with_callback(it, cb, |it, c| php_stdarr::callback::array_map(it, Some(c), &arrays))?.into()
            }
        }
        "array_filter" => {
            let a = arr(&a0())?;
            let mode = arg(args, 2).to_long(it)?;
            if arg(args, 1).is_null() {
                Value::Array(php_stdarr::callback::array_filter(it, &a, None, mode)?)
            } else {
                with_callback(it, arg(args, 1), |it, c| php_stdarr::callback::array_filter(it, &a, Some(c), mode))?.into()
            }
        }
        "array_reduce" => {
            let a = arr(&a0())?;
            let init = arg(args, 2);
            with_callback(it, arg(args, 1), |it, c| php_stdarr::callback::array_reduce(it, &a, c, Some(&init)))?
        }
        "array_walk" => {
            let r = refarg(args, 0);
            let ud = args.get(2).map(Value::deref);
            with_callback(it, arg(args, 1), |it, c| php_stdarr::callback::array_walk(it, &r, c, ud.as_ref()))?;
            Value::Bool(true)
        }
        "array_merge" => {
            let v: Vec<Value> = args.iter().map(Value::deref).collect();
            Value::Array(php_stdarr::merge::array_merge(it, &v)?)
        }
        "array_slice" => {
            let a = arr(&a0())?;
            let off = arg(args, 1).to_long(it)?;
            let len = match arg(args, 2) {
                Value::Null => None,
                v => Some(v.to_long(it)?),
            };
            let pk = arg(args, 3).to_bool(it)?;
            Value::Array(php_stdarr::basic::array_slice(&a, off, len, pk))
        }
        "array_reverse" => Value::Array(php_stdarr::basic::array_reverse(&*arr(&a0())?, arg(args, 1).to_bool(it)?)),
        "array_sum" => php_stdarr::basic::array_sum(it, &*arr(&a0())?)?,
        "array_product" => php_stdarr::basic::array_product(it, &*arr(&a0())?)?,
        "array_flip" => Value::Array(php_stdarr::basic::array_flip(it, &*arr(&a0())?)?),
        "array_combine" => Value::Array(php_stdarr::basic::array_combine(it, &*arr(&a0())?, &*arr(&arg(args, 1))?)?),
        "array_fill" => Value::Array(php_stdarr::basic::array_fill(a0().to_long(it)?, arg(args, 1).to_long(it)?, &arg(args, 2))?),
        "array_fill_keys" => Value::Array(php_stdarr::basic::array_fill_keys(it, &*arr(&a0())?, &arg(args, 1))?),
        "array_key_first" => php_stdarr::basic::array_key_first(&*arr(&a0())?),
        "array_key_last" => php_stdarr::basic::array_key_last(&*arr(&a0())?),
        "array_key_exists" | "key_exists" => {
            let k = a0();
            match arg(args, 1) {
                Value::Array(a) => {
                    let key = super::interp_expr::to_key(it, &k)?;
                    Value::Bool(a.contains_key(&key))
                }
                _ => Value::Bool(false),
            }
        }
        "array_unique" => {
            let fl = if args.len() > 1 { arg(args, 1).to_long(it)? } else { 2 };
            Value::Array(php_stdarr::setops::array_unique(it, &arr(&a0())?, fl)?)
        }
        "array_diff" => Value::Array(php_stdarr::setops::array_diff(it, &args.iter().map(Value::deref).collect::<Vec<_>>())?),
        "array_intersect" => Value::Array(php_stdarr::setops::array_intersect(it, &args.iter().map(Value::deref).collect::<Vec<_>>())?),
        "array_chunk" => Value::Array(php_stdarr::basic::array_chunk(&*arr(&a0())?, arg(args, 1).to_long(it)?, arg(args, 2).to_bool(it)?)?),
        "array_push" => Value::Int(php_stdarr::stack::array_push(&refarg(args, 0), &args[1..].iter().map(Value::deref).collect::<Vec<_>>())?),
        "array_pop" => php_stdarr::stack::array_pop(&refarg(args, 0))?,
        "array_shift" => php_stdarr::stack::array_shift(&refarg(args, 0))?,
        "array_unshift" => Value::Int(php_stdarr::stack::array_unshift(&refarg(args, 0), &args[1..].iter().map(Value::deref).collect::<Vec<_>>())?),
        "sort" => {
            let fl = arg(args, 1).to_long(it)?;
            php_stdarr::sort::sort(it, &refarg(args, 0), fl)?;
            Value::Bool(true)
        }
        "rsort" => {
            let fl = arg(args, 1).to_long(it)?;
            php_stdarr::sort::rsort(it, &refarg(args, 0), fl)?;
            Value::Bool(true)
        }
        "ksort" => {
            let fl = arg(args, 1).to_long(it)?;
            php_stdarr::sort::ksort(it, &refarg(args, 0), fl)?;
            Value::Bool(true)
        }
        "krsort" => {
            let fl = arg(args, 1).to_long(it)?;
            php_stdarr::sort::krsort(it, &refarg(args, 0), fl)?;
            Value::Bool(true)
        }
        "asort" => {
            let fl = arg(args, 1).to_long(it)?;
            php_stdarr::sort::asort(it, &refarg(args, 0), fl)?;
            Value::Bool(true)
        }
        "arsort" => {
            let fl = arg(args, 1).to_long(it)?;
            php_stdarr::sort::arsort(it, &refarg(args, 0), fl)?;
            Value::Bool(true)
        }
        "natsort" => {
            php_stdarr::sort::natsort(it, &refarg(args, 0))?;
            Value::Bool(true)
        }
        "natcasesort" => {
            php_stdarr::sort::natcasesort(it, &refarg(args, 0))?;
            Value::Bool(true)
        }
        "usort" | "uasort" | "uksort" => {
            let r = refarg(args, 0);
            let cb = arg(args, 1);
            with_callback(it, cb, |it, c| match name {
                "usort" => php_stdarr::sort::usort(it, &r, c),
                "uasort" => php_stdarr::sort::uasort(it, &r, c),
                _ => php_stdarr::sort::uksort(it, &r, c),
            })?;
            Value::Bool(true)
        }
        "end" => php_stdarr::pointer::end(it, &refarg(args, 0))?,
        "reset" => php_stdarr::pointer::reset(it, &refarg(args, 0))?,
        "next" => php_stdarr::pointer::next(it, &refarg(args, 0))?,
        "prev" => php_stdarr::pointer::prev(it, &refarg(args, 0))?,
        "current" | "pos" => php_stdarr::pointer::current(it, name, &a0())?,
        "key" => php_stdarr::pointer::key(it, &a0())?,
        "gettype" => Value::from(a0().legacy_type_name()),
        "get_debug_type" => Value::from(match a0() {
            Value::Object(o) => o.class_name().to_string_lossy(),
            v => v.simple_type_name().to_string(),
        }),
        "get_class" => match a0() {
            Value::Object(o) => Value::Str(o.class_name()),
            _ => Value::from(it.frame().scope.as_ref().map(|c| c.name.clone()).unwrap_or_default()),
        },
        "get_parent_class" => {
            let c = match a0() {
                Value::Object(o) => Some(tobj(&o).class.clone()),
                Value::Str(s) => it.find_class(s.as_bytes()),
                _ => it.frame().scope.clone(),
            };
            match c.and_then(|c| c.parent.clone()) {
                Some(p) => Value::from(p.name.clone()),
                None => Value::Bool(false),
            }
        }
        "get_mangled_object_vars" => match a0() {
            Value::Object(o) => Value::Array(ZArray::from_table(tobj(&o).props.borrow().to_symtable())),
            _ => Value::Null,
        },
        "get_object_vars" => match a0() {
            Value::Object(o) => Value::Array(it.visible_props(&o)),
            _ => Value::Null,
        },
        "is_a" | "is_subclass_of" => {
            let c = match a0() {
                Value::Object(o) => Some(tobj(&o).class.clone()),
                Value::Str(s) if name == "is_subclass_of" || arg(args, 2).to_bool(it)? => it.find_class(s.as_bytes()),
                _ => None,
            };
            let target = zstr(it, &arg(args, 1))?.to_string_lossy();
            Value::Bool(c.is_some_and(|c| {
                if name == "is_subclass_of" && c.name.eq_ignore_ascii_case(&target) {
                    false
                } else {
                    c.is_a(&target)
                }
            }))
        }
        "is_array" => Value::Bool(matches!(a0(), Value::Array(_))),
        "is_object" => Value::Bool(matches!(a0(), Value::Object(_))),
        "is_int" | "is_integer" | "is_long" => Value::Bool(matches!(a0(), Value::Int(_))),
        "is_float" | "is_double" => Value::Bool(matches!(a0(), Value::Float(_))),
        "is_string" => Value::Bool(matches!(a0(), Value::Str(_))),
        "is_bool" => Value::Bool(matches!(a0(), Value::Bool(_))),
        "is_null" => Value::Bool(a0().is_null()),
        "is_resource" => Value::Bool(matches!(a0(), Value::Resource(r) if !r.is_closed())),
        "is_scalar" => Value::Bool(a0().is_scalar()),
        "is_numeric" => Value::Bool(match a0() {
            Value::Int(_) | Value::Float(_) => true,
            Value::Str(s) => php_runtime::numeric::is_numeric(s.as_bytes()),
            _ => false,
        }),
        "is_iterable" => Value::Bool(match a0() {
            Value::Array(_) => true,
            Value::Object(o) => tobj(&o).class.is_a("Traversable"),
            _ => false,
        }),
        "is_countable" => Value::Bool(match a0() {
            Value::Array(_) => true,
            Value::Object(o) => tobj(&o).class.is_a("Countable"),
            _ => false,
        }),
        "is_callable" => Value::Bool(it.is_callable_value(&a0()).is_ok()),
        "intval" => Value::Int(a0().to_long(it)?),
        "floatval" | "doubleval" => Value::Float(a0().to_double(it)?),
        "boolval" => Value::Bool(a0().to_bool(it)?),
        "strval" => Value::Str(zstr(it, &a0())?),
        "settype" => {
            let r = refarg(args, 0);
            let t = zstr(it, &arg(args, 1))?.to_string_lossy();
            let v = r.get();
            let nv = match t.as_str() {
                "int" | "integer" => Value::Int(v.to_long(it)?),
                "string" => Value::Str(v.to_zstr(it)?),
                "array" => Value::Array(v.to_array(it)?),
                "bool" | "boolean" => Value::Bool(v.to_bool(it)?),
                "float" | "double" => Value::Float(v.to_double(it)?),
                _ => Value::Null,
            };
            r.set(nv);
            Value::Bool(true)
        }
        "serialize" => Value::Str(ZStr::from(php_runtime::serialize::serialize(&a0(), it)?)),
        "unserialize" => {
            let s = zstr(it, &a0())?;
            match php_runtime::serialize::unserialize(s.as_bytes(), &Default::default(), it)? {
                Some(v) => v,
                None => Value::Bool(false),
            }
        }
        "json_encode" => {
            let v = a0();
            let flags = arg(args, 1).to_long(it)?;
            match json_encode(it, &v, flags) {
                Ok(s) => Value::Str(ZStr::from(s)),
                Err(e) => return Err(e),
            }
        }
        "strtoupper" => Value::Str(zstr(it, &a0())?.to_ascii_uppercase()),
        "strtolower" => Value::Str(zstr(it, &a0())?.to_ascii_lowercase()),
        "ucfirst" | "lcfirst" => {
            let mut b = zstr(it, &a0())?.as_bytes().to_vec();
            if let Some(c) = b.first_mut() {
                *c = if name == "ucfirst" { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() };
            }
            Value::Str(ZStr::from(b))
        }
        "ucwords" => {
            let mut b = zstr(it, &a0())?.as_bytes().to_vec();
            let mut up = true;
            for c in &mut b {
                if up {
                    *c = c.to_ascii_uppercase();
                }
                up = matches!(*c, b' ' | b'\t' | b'\n' | b'\r');
            }
            Value::Str(ZStr::from(b))
        }
        "strrev" => {
            let mut b = zstr(it, &a0())?.as_bytes().to_vec();
            b.reverse();
            Value::Str(ZStr::from(b))
        }
        "trim" | "rtrim" | "ltrim" | "chop" => {
            let s = zstr(it, &a0())?;
            let chars: Vec<u8> = if args.len() > 1 { zstr(it, &arg(args, 1))?.as_bytes().to_vec() } else { b" \t\n\r\0\x0B".to_vec() };
            let b = s.as_bytes();
            let mut st = 0;
            let mut en = b.len();
            if name != "rtrim" && name != "chop" {
                while st < en && chars.contains(&b[st]) {
                    st += 1;
                }
            }
            if name != "ltrim" {
                while en > st && chars.contains(&b[en - 1]) {
                    en -= 1;
                }
            }
            Value::Str(ZStr::from(&b[st..en]))
        }
        "str_pad" => {
            let s = zstr(it, &a0())?;
            let len = arg(args, 1).to_long(it)?.max(0) as usize;
            let pad = if args.len() > 2 { zstr(it, &arg(args, 2))?.as_bytes().to_vec() } else { b" ".to_vec() };
            let ty = arg(args, 3).to_long(it)?;
            let mut b = s.as_bytes().to_vec();
            if b.len() < len && !pad.is_empty() {
                let n = len - b.len();
                let fill = |k: usize| pad.iter().cycle().take(k).copied().collect::<Vec<u8>>();
                match ty {
                    0 => b = [fill(n), b].concat(),
                    2 => {
                        let l = n / 2;
                        b = [fill(l), b, fill(n - l)].concat();
                    }
                    _ => b.extend(fill(n)),
                }
            }
            Value::Str(ZStr::from(b))
        }
        "substr" => {
            let s = zstr(it, &a0())?;
            let b = s.as_bytes();
            let len = b.len() as i64;
            let mut start = arg(args, 1).to_long(it)?;
            if start < 0 {
                start = (len + start).max(0);
            }
            let start = start.min(len);
            let l = match arg(args, 2) {
                Value::Null => len - start,
                v => {
                    let l = v.to_long(it)?;
                    if l < 0 { (len - start + l).max(0) } else { l.min(len - start) }
                }
            };
            Value::Str(ZStr::from(&b[start as usize..(start + l) as usize]))
        }
        "strpos" => {
            let h = zstr(it, &a0())?;
            let n = zstr(it, &arg(args, 1))?;
            match h.as_bytes().windows(n.len().max(1)).position(|w| w == n.as_bytes()) {
                Some(p) if !n.is_empty() => Value::Int(p as i64),
                _ => Value::Bool(false),
            }
        }
        "str_contains" => {
            let h = zstr(it, &a0())?;
            let n = zstr(it, &arg(args, 1))?;
            Value::Bool(n.is_empty() || h.as_bytes().windows(n.len()).any(|w| w == n.as_bytes()))
        }
        "str_starts_with" => Value::Bool(zstr(it, &a0())?.as_bytes().starts_with(zstr(it, &arg(args, 1))?.as_bytes())),
        "str_ends_with" => Value::Bool(zstr(it, &a0())?.as_bytes().ends_with(zstr(it, &arg(args, 1))?.as_bytes())),
        "str_replace" => {
            let search = a0();
            let rep = arg(args, 1);
            let subj = zstr(it, &arg(args, 2))?;
            let pairs: Vec<(Vec<u8>, Vec<u8>)> = match (&search, &rep) {
                (Value::Array(s), Value::Array(r)) => s
                    .values()
                    .zip(r.values().map(Some).chain(std::iter::repeat(None)))
                    .map(|(a, b)| (a.to_zstr(it).map(|z| z.as_bytes().to_vec()).unwrap_or_default(), b.map(|b| b.to_zstr(it).map(|z| z.as_bytes().to_vec()).unwrap_or_default()).unwrap_or_default()))
                    .collect(),
                (Value::Array(s), r) => {
                    let r = zstr(it, r)?.as_bytes().to_vec();
                    s.values().map(|a| (a.to_zstr(it).map(|z| z.as_bytes().to_vec()).unwrap_or_default(), r.clone())).collect()
                }
                (s, r) => vec![(zstr(it, s)?.as_bytes().to_vec(), zstr(it, r)?.as_bytes().to_vec())],
            };
            let mut out = subj.as_bytes().to_vec();
            for (s, r) in pairs {
                if s.is_empty() {
                    continue;
                }
                out = replace_bytes(&out, &s, &r);
            }
            Value::Str(ZStr::from(out))
        }
        "strcmp" => Value::Int(i64::from(php_runtime::string::binary_strcmp(zstr(it, &a0())?.as_bytes(), zstr(it, &arg(args, 1))?.as_bytes()).signum())),
        "nl2br" => Value::Str(ZStr::from(replace_bytes(zstr(it, &a0())?.as_bytes(), b"\n", b"<br />\n"))),
        "max" => php_stdarr::basic::max(it, &args.iter().map(Value::deref).collect::<Vec<_>>())?,
        "min" => php_stdarr::basic::min(it, &args.iter().map(Value::deref).collect::<Vec<_>>())?,
        "abs" => match a0() {
            Value::Int(n) => Value::Int(n.abs()),
            v => Value::Float(v.to_double(it)?.abs()),
        },
        "floor" => Value::Float(a0().to_double(it)?.floor()),
        "ceil" => Value::Float(a0().to_double(it)?.ceil()),
        "round" => {
            let p = arg(args, 1).to_long(it)? as i32;
            Value::Float(php_support::fmt::number::php_round(a0().to_double(it)?, p, php_support::fmt::number::RoundingMode::HalfUp))
        }
        "sqrt" => Value::Float(a0().to_double(it)?.sqrt()),
        "pow" => ops::pow(&a0(), &arg(args, 1), it)?,
        "chr" => Value::Str(ZStr::from(vec![(a0().to_long(it)?.rem_euclid(256)) as u8])),
        "ord" => Value::Int(i64::from(zstr(it, &a0())?.as_bytes().first().copied().unwrap_or(0))),
        "dechex" => Value::from(format!("{:x}", a0().to_long(it)?)),
        "hexdec" => Value::Int(i64::from_str_radix(&zstr(it, &a0())?.to_string_lossy(), 16).unwrap_or(0)),
        "bin2hex" => Value::from(zstr(it, &a0())?.as_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>()),
        "number_format" => {
            let d = a0().to_double(it)?;
            let dec = arg(args, 1).to_long(it)? as i32;
            Value::Str(ZStr::from(php_support::fmt::number::number_format_double(d, dec, b".", b",")))
        }
        "call_user_func" => {
            let f = a0();
            return it.call_value(&f, &args[1..]);
        }
        "call_user_func_array" => {
            let f = a0();
            let a: Vec<Value> = arr(&arg(args, 1))?.values().cloned().collect();
            return it.call_value(&f, &a);
        }
        "func_get_args" => Value::Array(ZArray::from_values(it.frame().args.clone())),
        "func_num_args" => Value::Int(it.frame().args.len() as i64),
        "method_exists" => {
            let c = match a0() {
                Value::Object(o) => Some(tobj(&o).class.clone()),
                Value::Str(s) => it.find_class(s.as_bytes()),
                _ => None,
            };
            let m = zstr(it, &arg(args, 1))?.to_string_lossy().to_ascii_lowercase();
            Value::Bool(c.is_some_and(|c| c.find_method(&m).is_some()))
        }
        "property_exists" => {
            let n = zstr(it, &arg(args, 1))?.to_string_lossy();
            Value::Bool(match a0() {
                Value::Object(o) => it.find_prop_key(&o, &n).is_some(),
                Value::Str(c) => it.find_class(c.as_bytes()).is_some_and(|c| c.all_props().iter().any(|(k, i)| k.props[*i].name == n)),
                _ => false,
            })
        }
        "class_exists" | "interface_exists" => {
            let n = zstr(it, &a0())?;
            let autoload = args.len() < 2 || arg(args, 1).to_bool(it)?;
            let c = if autoload { it.lookup_class(n.as_bytes()).ok() } else { it.find_class(n.as_bytes()) };
            Value::Bool(c.is_some_and(|c| (c.kind == CKind::Interface) == (name == "interface_exists")))
        }
        "function_exists" => {
            let n = zstr(it, &a0())?.to_string_lossy().to_ascii_lowercase();
            Value::Bool(it.functions.contains_key(&n) || exists(&n))
        }
        "dirname" => {
            let s = zstr(it, &a0())?.to_string_lossy();
            Value::from(match s.rfind('/') {
                Some(0) => "/".to_string(),
                Some(p) => s[..p].to_string(),
                None => ".".to_string(),
            })
        }
        "basename" => {
            let s = zstr(it, &a0())?.to_string_lossy();
            let s = s.trim_end_matches('/');
            let mut b = s.rsplit('/').next().unwrap_or("").to_string();
            if args.len() > 1 {
                let suf = zstr(it, &arg(args, 1))?.to_string_lossy();
                if b.ends_with(&suf) && b != suf {
                    b.truncate(b.len() - suf.len());
                }
            }
            Value::from(b)
        }
        "file_exists" | "is_file" | "is_dir" => {
            let p = zstr(it, &a0())?.to_string_lossy();
            let path = std::path::Path::new(&p);
            Value::Bool(match name {
                "is_file" => path.is_file(),
                "is_dir" => path.is_dir(),
                _ => path.exists(),
            })
        }
        "unlink" => Value::Bool(std::fs::remove_file(zstr(it, &a0())?.to_string_lossy()).is_ok()),
        "rmdir" => Value::Bool(std::fs::remove_dir(zstr(it, &a0())?.to_string_lossy()).is_ok()),
        "mkdir" => {
            let p = zstr(it, &a0())?.to_string_lossy();
            let rec = arg(args, 2).to_bool(it)?;
            Value::Bool(if rec { std::fs::create_dir_all(p).is_ok() } else { std::fs::create_dir(p).is_ok() })
        }
        "touch" => {
            let p = zstr(it, &a0())?.to_string_lossy();
            Value::Bool(std::fs::OpenOptions::new().create(true).append(true).open(p).is_ok())
        }
        "file_put_contents" => {
            let p = zstr(it, &a0())?.to_string_lossy();
            let data = match arg(args, 1) {
                Value::Array(a) => {
                    let mut o = Vec::new();
                    for v in a.values() {
                        o.extend_from_slice(zstr(it, v)?.as_bytes());
                    }
                    o
                }
                v => zstr(it, &v)?.as_bytes().to_vec(),
            };
            let append = arg(args, 2).to_long(it)? & 8 != 0;
            let r = if append {
                use std::io::Write;
                std::fs::OpenOptions::new().create(true).append(true).open(&p).and_then(|mut f| f.write_all(&data))
            } else {
                std::fs::write(&p, &data)
            };
            match r {
                Ok(()) => Value::Int(data.len() as i64),
                Err(_) => Value::Bool(false),
            }
        }
        "file_get_contents" => match std::fs::read(zstr(it, &a0())?.to_string_lossy()) {
            Ok(d) => Value::Str(ZStr::from(d)),
            Err(_) => Value::Bool(false),
        },
        "strstr" | "stristr" | "strrchr" => {
            let h = zstr(it, &a0())?;
            let n = zstr(it, &arg(args, 1))?;
            let before = arg(args, 2).to_bool(it)?;
            let hb = h.as_bytes();
            let pos = if name == "strrchr" {
                n.as_bytes().first().and_then(|c| hb.iter().rposition(|x| x == c))
            } else if name == "stristr" {
                let hl = hb.to_ascii_lowercase();
                let nl = n.as_bytes().to_ascii_lowercase();
                hl.windows(nl.len().max(1)).position(|w| w == nl.as_slice())
            } else {
                hb.windows(n.len().max(1)).position(|w| w == n.as_bytes())
            };
            match pos {
                Some(p) if before => Value::Str(ZStr::from(&hb[..p])),
                Some(p) => Value::Str(ZStr::from(&hb[p..])),
                None => Value::Bool(false),
            }
        }
        "str_rot13" => Value::Str(ZStr::from(
            zstr(it, &a0())?
                .as_bytes()
                .iter()
                .map(|&c| match c {
                    b'a'..=b'z' => (c - b'a' + 13) % 26 + b'a',
                    b'A'..=b'Z' => (c - b'A' + 13) % 26 + b'A',
                    _ => c,
                })
                .collect::<Vec<u8>>(),
        )),
        "pathinfo" => {
            let p = zstr(it, &a0())?;
            let pi = php_streams::pathname::pathinfo(p.as_bytes());
            let opt = if args.len() > 1 { arg(args, 1).to_long(it)? } else { 15 };
            let mut t = HashTable::new();
            let mut put = |k: &str, v: Option<&Vec<u8>>| {
                if let Some(v) = v {
                    t.insert(ArrayKey::from(k), Value::Str(ZStr::from(v.as_slice())));
                }
            };
            let dir = pi.dirname.clone();
            let base = Some(pi.basename.clone());
            let ext = pi.extension.clone();
            let fname = Some(pi.filename.clone());
            match opt {
                1 => return Ok(dir.map_or(Value::from(""), |d| Value::Str(ZStr::from(d)))),
                2 => return Ok(Value::Str(ZStr::from(pi.basename))),
                4 => return Ok(ext.map_or(Value::from(""), |d| Value::Str(ZStr::from(d)))),
                8 => return Ok(Value::Str(ZStr::from(pi.filename))),
                _ => {}
            }
            put("dirname", dir.as_ref());
            put("basename", base.as_ref());
            put("extension", ext.as_ref());
            put("filename", fname.as_ref());
            Value::Array(ZArray::from_table(t))
        }
        "getenv" => Value::Bool(false),
        "set_include_path" | "get_include_path" => Value::from("."),
        "realpath" => match std::fs::canonicalize(zstr(it, &a0())?.to_string_lossy()) {
            Ok(p) => Value::from(p.to_string_lossy().into_owned()),
            Err(_) => Value::Bool(false),
        },
        "symlink" => {
            let t = zstr(it, &a0())?.to_string_lossy();
            let l = zstr(it, &arg(args, 1))?.to_string_lossy();
            Value::Bool(std::os::unix::fs::symlink(t, l).is_ok())
        }
        "is_link" => Value::Bool(std::fs::symlink_metadata(zstr(it, &a0())?.to_string_lossy()).is_ok_and(|m| m.file_type().is_symlink())),
        "fileowner" | "filegroup" | "fileperms" | "filesize" | "fileinode" | "filemtime" | "fileatime" | "filectime" | "filetype" | "is_readable" | "is_writable" | "is_writeable" | "is_executable" => {
            use php_streams::filestat::{php_stat, FsType, StatResult};
            let p = zstr(it, &a0())?;
            let ty = match name {
                "fileowner" => FsType::Owner,
                "filegroup" => FsType::Group,
                "fileperms" => FsType::Perms,
                "filesize" => FsType::Size,
                "fileinode" => FsType::Inode,
                "filemtime" => FsType::Mtime,
                "fileatime" => FsType::Atime,
                "filectime" => FsType::Ctime,
                "filetype" => FsType::Type,
                "is_readable" => FsType::IsR,
                "is_executable" => FsType::IsX,
                _ => FsType::IsW,
            };
            match php_stat(p.as_bytes(), ty) {
                StatResult::Bool(b) => Value::Bool(b),
                StatResult::Int(i) => Value::Int(i),
                StatResult::Str(s) => Value::from(s),
                StatResult::Stat(_) => Value::Null,
            }
        }
        "fopen" => {
            let p = zstr(it, &a0())?;
            let mode = zstr(it, &arg(args, 1))?.to_string_lossy();
            let prev = php_streams::diag::set_handler(None);
            let s = php_streams::open::fopen(p.as_bytes(), &mode, false, None);
            let diags = php_streams::diag::take();
            php_streams::diag::set_handler(prev);
            for d in diags {
                it.emit(ErrorLevel::Warning, &format!("fopen({}): {}", p.to_string_lossy(), d.text()))?;
            }
            match s {
                Some(s) => Value::Resource(php_runtime::Resource::new("stream", Some(Box::new(s)))),
                None => Value::Bool(false),
            }
        }
        "fclose" | "fwrite" | "fputs" | "fgets" | "feof" | "fread" | "rewind" | "ftell" | "fflush" => {
            let Value::Resource(r) = a0() else { return Err(rt_err(format!("{name}(): Argument #1 must be of type resource"))) };
            if name == "fclose" {
                r.close();
                return Ok(Value::Bool(true));
            }
            let data2 = if args.len() > 1 { Some(zstr(it, &arg(args, 1))?) } else { None };
            let mut d = r.data_mut();
            let Some(s) = d.as_mut().and_then(|b| b.downcast_mut::<php_streams::stream::Stream>()) else {
                return Ok(Value::Bool(false));
            };
            match name {
                "fwrite" | "fputs" => match s.write(data2.unwrap_or_default().as_bytes()) {
                    Ok(n) => Value::Int(n as i64),
                    Err(_) => Value::Bool(false),
                },
                "fgets" => s.get_line(None).map_or(Value::Bool(false), |l| Value::Str(ZStr::from(l))),
                "feof" => Value::Bool(s.eof()),
                "fread" => {
                    let n = data2.and_then(|z| z.to_string_lossy().parse::<usize>().ok()).unwrap_or(0);
                    s.read_vec(n).map_or(Value::from(""), |v| Value::Str(ZStr::from(v)))
                }
                "rewind" => Value::Bool(s.rewind() == 0),
                "ftell" => Value::Int(s.tell()),
                _ => Value::Bool(s.flush() == 0),
            }
        }
        "file" => {
            let p = zstr(it, &a0())?;
            let flags = arg(args, 1).to_long(it)?;
            match std::fs::read(p.to_string_lossy()) {
                Ok(d) => Value::Array(ZArray::from_values(
                    php_streams::file::split_lines(&d, b'\n', flags & 2 == 0, flags & 4 != 0)
                        .into_iter()
                        .map(|l| Value::Str(ZStr::from(l))),
                )),
                Err(_) => Value::Bool(false),
            }
        }
        "preg_match" | "preg_match_all" | "preg_replace" | "preg_split" | "preg_quote" => return preg(it, name, args),
        "getmypid" => Value::Int(i64::from(std::process::id())),
        "sys_get_temp_dir" => Value::from(std::env::temp_dir().to_string_lossy().into_owned()),
        "tempnam" => {
            let dir = zstr(it, &a0())?.to_string_lossy();
            let pre = zstr(it, &arg(args, 1))?.to_string_lossy();
            let p = format!("{dir}/{pre}{}", std::process::id() as u64 * 1000 + it.tmpfiles.len() as u64);
            let _ = std::fs::write(&p, b"");
            Value::from(p)
        }
        "getcwd" => Value::from(std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()),
        "chdir" => Value::Bool(true),
        "error_reporting" => {
            let old = it.error_reporting;
            if !args.is_empty() && !a0().is_null() {
                it.error_reporting = a0().to_long(it)?;
            }
            Value::Int(old)
        }
        "ini_set" | "ini_get" | "set_time_limit" | "gc_collect_cycles" | "ob_implicit_flush" | "setlocale" | "date_default_timezone_set" => Value::Bool(false),
        "set_error_handler" => {
            let old = it.error_handlers.last().cloned().unwrap_or_default();
            it.error_handlers.push(a0());
            old
        }
        "restore_error_handler" => {
            it.error_handlers.pop();
            Value::Bool(true)
        }
        "set_exception_handler" => {
            let old = it.exception_handler.take().unwrap_or_default();
            it.exception_handler = Some(a0());
            old
        }
        "register_shutdown_function" => {
            it.shutdown.push((a0(), args[1..].to_vec()));
            Value::Null
        }
        "trigger_error" | "user_error" => {
            let m = zstr(it, &a0())?.to_string_lossy();
            let lvl = if args.len() > 1 { arg(args, 1).to_long(it)? } else { 1024 };
            let level = match lvl {
                256 => ErrorLevel::UserError,
                512 => ErrorLevel::UserWarning,
                16384 => ErrorLevel::UserDeprecated,
                _ => ErrorLevel::UserNotice,
            };
            it.emit(level, &m)?;
            if lvl == 256 {
                return Err(RuntimeError::Custom { class: ZStr::from(EXIT_MARK), message: String::new() });
            }
            Value::Bool(true)
        }
        "clone" => match a0() {
            Value::Object(o) => Value::Object(it.clone_object(&o)?),
            other => return Err(rt_err(format!("__clone method called on non-object ({})", other.type_name()))),
        },
        "shuffle" => {
            let r = refarg(args, 0);
            let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
            let mut rng = |min: i64, max: i64| -> RtResult<i64> {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                Ok(min + (seed % ((max - min + 1) as u64)) as i64)
            };
            php_stdarr::random::shuffle(&r, &mut rng)?;
            Value::Bool(true)
        }
        "exit" | "die" => {
            if let Value::Str(s) = a0() {
                it.echo(s.as_bytes());
            }
            return Err(RuntimeError::Custom { class: ZStr::from(EXIT_MARK), message: String::new() });
        }
        "ob_start" => {
            it.ob_stack.push(Vec::new());
            Value::Bool(true)
        }
        "ob_get_clean" | "ob_get_contents" => match name {
            "ob_get_clean" => it.ob_stack.pop().map_or(Value::Bool(false), |b| Value::Str(ZStr::from(b))),
            _ => it.ob_stack.last().map_or(Value::Bool(false), |b| Value::Str(ZStr::from(b.clone()))),
        },
        "ob_end_clean" => Value::Bool(it.ob_stack.pop().is_some()),
        "ob_end_flush" | "ob_get_flush" => {
            if let Some(b) = it.ob_stack.pop() {
                it.echo(&b);
            }
            Value::Bool(true)
        }
        "md5" => Value::from(format!("{:x}", md5_hash(zstr(it, &a0())?.as_bytes()))),
        "crc32" => Value::Int(i64::from(crc32(zstr(it, &a0())?.as_bytes()))),
        "mb_strlen" => Value::Int(String::from_utf8_lossy(zstr(it, &a0())?.as_bytes()).chars().count() as i64),
        "array_column" | "array_rand" => return Err(rt_err(format!("builtin {name} unsupported"))),
        "spl_autoload_register_" => Value::Null,
        "debug_zval_refcount" | "debug_zval_dump" => Value::Null,
        "assert" => Value::Bool(true),
        "array_is_list" => Value::Bool(arr(&a0())?.is_list()),
        "array_any" | "array_all" => {
            let a = arr(&a0())?;
            with_callback(it, arg(args, 1), |it, c| if name == "array_any" { php_stdarr::callback::array_any(it, &a, c) } else { php_stdarr::callback::array_all(it, &a, c) })?.into()
        }
        "date" => Value::from("2024-01-01"),
        "microtime" | "time" | "hrtime" => Value::Int(0),
        "usleep" | "sleep" | "flush" | "clearstatcache" => Value::Null,
        "version_compare" => Value::Bool(true),
        "extension_loaded" => Value::Bool(true),
        "str_word_count" => Value::Int(0),
        "spl_object_hash_" => Value::Null,
        _ => return Err(rt_err(format!("Call to undefined function {name}()"))),
    })
}

fn split_bytes(s: &[u8], sep: &[u8]) -> Vec<Vec<u8>> {
    if sep.is_empty() {
        return vec![s.to_vec()];
    }
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i + sep.len() <= s.len() {
        if &s[i..i + sep.len()] == sep {
            out.push(s[start..i].to_vec());
            i += sep.len();
            start = i;
        } else {
            i += 1;
        }
    }
    out.push(s[start..].to_vec());
    out
}

fn replace_bytes(s: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let parts = split_bytes(s, from);
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(to);
        }
        out.extend_from_slice(p);
    }
    out
}

fn json_encode(it: &mut Interp, v: &Value, flags: i64) -> RtResult<Vec<u8>> {
    let _ = flags;
    Ok(match v.deref() {
        Value::Null => b"null".to_vec(),
        Value::Bool(b) => if b { b"true".to_vec() } else { b"false".to_vec() },
        Value::Int(i) => i.to_string().into_bytes(),
        Value::Float(f) => php_runtime::float::zend_gcvt(f, 17, b'.', b'e').into_bytes(),
        Value::Str(s) => {
            let mut o = vec![b'"'];
            for &c in s.as_bytes() {
                match c {
                    b'"' => o.extend_from_slice(b"\\\""),
                    b'\\' => o.extend_from_slice(b"\\\\"),
                    b'/' => o.extend_from_slice(b"\\/"),
                    b'\n' => o.extend_from_slice(b"\\n"),
                    c => o.push(c),
                }
            }
            o.push(b'"');
            o
        }
        Value::Array(a) => {
            let list = a.is_list();
            let mut o = vec![if list { b'[' } else { b'{' }];
            for (i, (k, v)) in a.iter().enumerate() {
                if i > 0 {
                    o.push(b',');
                }
                if !list {
                    o.extend(json_encode(it, &Value::Str(k.to_zstr()), 0)?);
                    o.push(b':');
                }
                o.extend(json_encode(it, v, 0)?);
            }
            o.push(if list { b']' } else { b'}' });
            o
        }
        Value::Object(ob) => {
            if tobj(&ob).class.is_a("JsonSerializable") {
                let r = it.call_method(&ob, "jsonSerialize", &[])?;
                return json_encode(it, &r, flags);
            }
            let props = it.visible_props(&ob);
            if props.is_empty() {
                return Ok(b"{}".to_vec());
            }
            let mut o = vec![b'{'];
            for (i, (k, v)) in props.iter().enumerate() {
                if i > 0 {
                    o.push(b',');
                }
                o.extend(json_encode(it, &Value::Str(k.to_zstr()), 0)?);
                o.push(b':');
                o.extend(json_encode(it, v, 0)?);
            }
            o.push(b'}');
            o
        }
        Value::Resource(_) | Value::Reference(_) => b"null".to_vec(),
    })
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn md5_hash(_data: &[u8]) -> u128 {
    0
}


pub fn unused(_: &HashTable, _: ArrayKey, _: Obj) {}

fn pcre_value(v: &php_pcre::Value) -> Value {
    match v {
        php_pcre::Value::Str(s) => Value::Str(ZStr::from(s.as_slice())),
        php_pcre::Value::Null => Value::Null,
        php_pcre::Value::Int(i) => Value::Int(*i),
        php_pcre::Value::Array(a) => Value::Array(pcre_array(a)),
    }
}

fn pcre_array(a: &php_pcre::MatchArray) -> ZArray {
    let mut t = HashTable::new();
    for (k, v) in a.iter() {
        let key = match k {
            php_pcre::Key::Index(i) => ArrayKey::Int(*i),
            php_pcre::Key::Name(n) => ArrayKey::from_bytes(n),
        };
        t.insert(key, pcre_value(v));
    }
    ZArray::from_table(t)
}

fn preg(it: &mut Interp, name: &str, args: &[Value]) -> RtResult<Value> {
    if name == "preg_quote" {
        let s = zstr(it, &arg(args, 0))?;
        let d = if args.len() > 1 { Some(zstr(it, &arg(args, 1))?) } else { None };
        return Ok(Value::Str(ZStr::from(php_pcre::quote(s.as_bytes(), d.as_ref().map(ZStr::as_bytes)))));
    }
    let pat = zstr(it, &arg(args, 0))?;
    let re = match it.pcre.compile(pat.as_bytes()) {
        Ok(r) => r,
        Err(e) => {
            it.emit(ErrorLevel::Warning, &format!("{name}(): {}", String::from_utf8_lossy(&e.message())))?;
            return Ok(Value::Bool(false));
        }
    };
    let cfg = it.pcre.config();
    match name {
        "preg_match" | "preg_match_all" => {
            let subj = zstr(it, &arg(args, 1))?;
            let flags = arg(args, 3).to_long(it)?;
            let out = if name == "preg_match" {
                php_pcre::match_one(&re, &cfg, subj.as_bytes(), args.len() > 2, flags, 0)
            } else {
                php_pcre::match_all(&re, &cfg, subj.as_bytes(), args.len() > 2, flags, 0)
            };
            if let (Some(Value::Reference(r)), Some(m)) = (args.get(2), &out.matches) {
                r.set(Value::Array(pcre_array(m)));
            }
            Ok(match out.result {
                Ok(n) => Value::Int(n as i64),
                Err(_) => Value::Bool(false),
            })
        }
        "preg_replace" => {
            let rep = zstr(it, &arg(args, 1))?;
            let subj = zstr(it, &arg(args, 2))?;
            let out = php_pcre::replace(&re, &cfg, subj.as_bytes(), &php_pcre::Replacement::parse(rep.as_bytes()), php_pcre::limit_from_php(-1));
            Ok(out.result.map_or(Value::Null, |v| Value::Str(ZStr::from(v))))
        }
        _ => {
            let subj = zstr(it, &arg(args, 1))?;
            let limit = if args.len() > 2 { arg(args, 2).to_long(it)? } else { -1 };
            let flags = arg(args, 3).to_long(it)?;
            Ok(php_pcre::split(&re, &cfg, subj.as_bytes(), limit, flags).map_or(Value::Bool(false), |a| Value::Array(pcre_array(&a))))
        }
    }
}
