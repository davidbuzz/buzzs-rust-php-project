//! `ext/standard/string.c` functions.

use std::cell::RefCell;

use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};
use php_stdstr::analysis::{self, CharCounts, WordCount};
use php_stdstr::error::Notices;
use php_stdstr::search::{self, Strtok};
use php_stdstr::transform::{self, ReplaceArg, TrimMode};
use php_stdstr::{compare, slashes, strip_tags};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::{assoc, bytes, check, deprecated_function, emit, opt_bytes, str_list, to_str};

thread_local! {
    /// `BG(strtok_string)` / `BG(strtok_last)`.
    static STRTOK: RefCell<Strtok> = RefCell::new(Strtok::new());
}

/// `int|false` from an optional position.
fn pos_result(r: Option<i64>) -> Value {
    r.map_or(Value::Bool(false), Value::Int)
}

/// An optional `?int` argument.
fn opt_int(a: &Args, n: usize) -> Option<i64> {
    a.opt(n).map(|_| a.int(n))
}

fn bin2hex(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(transform::bin2hex(a.str(0).as_bytes())))
}

fn hex2bin(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut n = Notices::new();
    let r = transform::hex2bin(a.str(0).as_bytes(), &mut n);
    emit(vm, n)?;
    Ok(opt_bytes(r))
}

fn strspn(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = search::strspn(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.int(2),
        opt_int(a, 3),
    );
    Ok(Value::Int(r))
}

fn strcspn(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = search::strcspn(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.int(2),
        opt_int(a, 3),
    );
    Ok(Value::Int(r))
}

fn strcoll(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    deprecated_function(vm, "strcoll", "8.6", "use Collator::compare() instead")?;
    // LC_COLLATE is "C" unless setlocale() changed it: byte comparison.
    let r = compare::strcmp(a.str(0).as_bytes(), a.str(1).as_bytes());
    Ok(Value::Int(r.signum()))
}

fn trim_impl(vm: &mut Vm, a: &Args, mode: TrimMode) -> VmResult<Value> {
    let s = a.str(0);
    let what = a.has(1).then(|| a.str(1));
    let mut n = Notices::new();
    let r = transform::trim(
        s.as_bytes(),
        what.as_ref().map(ZStr::as_bytes),
        mode,
        &mut n,
    )
    .to_vec();
    emit(vm, n)?;
    Ok(bytes(r))
}

fn trim(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    trim_impl(vm, a, TrimMode::Both)
}

fn ltrim(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    trim_impl(vm, a, TrimMode::Left)
}

fn rtrim(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    trim_impl(vm, a, TrimMode::Right)
}

fn wordwrap(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let width = if a.has(1) { a.int(1) } else { 75 };
    let brk = if a.has(2) { a.str(2) } else { ZStr::from("\n") };
    let r = transform::wordwrap(a.str(0).as_bytes(), width, brk.as_bytes(), a.bool(3));
    Ok(bytes(check(vm, r)?))
}

fn explode(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let limit = if a.has(2) { a.int(2) } else { i64::MAX };
    let r = search::explode(a.str(0).as_bytes(), a.str(1).as_bytes(), limit);
    Ok(str_list(check(vm, r)?))
}

/// `php_implode()`.
pub(crate) fn implode_array(vm: &mut Vm, glue: &[u8], pieces: &ZArray) -> VmResult<Vec<u8>> {
    let mut out = Vec::new();
    for (i, (_, v)) in pieces.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(glue);
        }
        match v.deref() {
            Value::Str(s) => out.extend_from_slice(s.as_bytes()),
            Value::Int(n) => out.extend_from_slice(n.to_string().as_bytes()),
            other => out.extend_from_slice(to_str(vm, &other)?.as_bytes()),
        }
    }
    Ok(out)
}

fn implode(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let first = a.at(0);
    let fname = a.func_name();
    let (glue, pieces) = match a.opt(1) {
        None => match first {
            Value::Array(p) => (ZStr::empty(), p),
            _ => {
                return Err(vm.throw(
                    "TypeError",
                    &format!(
                        "{fname}(): If argument #1 ($separator) is of type string, argument #2 ($array) must be of type array, null given"
                    ),
                ))
            }
        },
        Some(Value::Array(p)) => match first {
            Value::Array(_) => {
                return Err(a.type_error(vm, 1, "must be of type string, array given"));
            }
            other => (to_str(vm, &other)?, p),
        },
        Some(other) => {
            let msg = format!("must be of type ?array, {} given", crate::type_name(&other));
            return Err(a.type_error(vm, 2, &msg));
        }
    };
    Ok(bytes(implode_array(vm, glue.as_bytes(), &pieces)?))
}

fn strtok(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut n = Notices::new();
    let r = match a.opt(1) {
        Some(_) => {
            let (s, tok) = (a.str(0), a.str(1));
            STRTOK.with(|t| t.borrow_mut().start(s.as_bytes(), tok.as_bytes(), &mut n))
        }
        None => {
            let tok = a.str(0);
            STRTOK.with(|t| t.borrow_mut().next(tok.as_bytes(), &mut n))
        }
    };
    emit(vm, n)?;
    Ok(opt_bytes(r))
}

fn strtoupper(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(transform::strtoupper(a.str(0).as_bytes())))
}

fn strtolower(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(transform::strtolower(a.str(0).as_bytes())))
}

fn str_increment(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = transform::str_increment(a.str(0).as_bytes());
    Ok(bytes(check(vm, r)?))
}

fn str_decrement(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = transform::str_decrement(a.str(0).as_bytes());
    Ok(bytes(check(vm, r)?))
}

fn strstr(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(opt_bytes(search::strstr(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.bool(2),
    )))
}

fn stristr(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(opt_bytes(search::stristr(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.bool(2),
    )))
}

fn strrchr(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(opt_bytes(search::strrchr(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.bool(2),
    )))
}

fn strpos(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = search::strpos(a.str(0).as_bytes(), a.str(1).as_bytes(), a.int(2));
    Ok(pos_result(check(vm, r)?))
}

fn stripos(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = search::stripos(a.str(0).as_bytes(), a.str(1).as_bytes(), a.int(2));
    Ok(pos_result(check(vm, r)?))
}

fn strrpos(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = search::strrpos(a.str(0).as_bytes(), a.str(1).as_bytes(), a.int(2));
    Ok(pos_result(check(vm, r)?))
}

fn strripos(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = search::strripos(a.str(0).as_bytes(), a.str(1).as_bytes(), a.int(2));
    Ok(pos_result(check(vm, r)?))
}

fn str_contains(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(search::str_contains(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
    )))
}

fn str_starts_with(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(search::str_starts_with(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
    )))
}

fn str_ends_with(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(search::str_ends_with(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
    )))
}

fn chunk_split(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let len = if a.has(1) { a.int(1) } else { 76 };
    let end = if a.has(2) {
        a.str(2)
    } else {
        ZStr::from("\r\n")
    };
    let r = transform::chunk_split(a.str(0).as_bytes(), len, end.as_bytes());
    Ok(bytes(check(vm, r)?))
}

fn substr(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    Ok(bytes(search::substr(s.as_bytes(), a.int(1), opt_int(a, 2))))
}

/// Walks the values of an `array|int` argument of `substr_replace()` in
/// order (`HashPosition` over the defined slots).
struct Cursor {
    values: Vec<Value>,
    pos: usize,
}

impl Cursor {
    fn new(v: &Value) -> Option<Self> {
        match v {
            Value::Array(arr) => Some(Self {
                values: arr.iter().map(|(_, v)| v.deref()).collect(),
                pos: 0,
            }),
            _ => None,
        }
    }

    fn next(&mut self) -> Option<Value> {
        let v = self.values.get(self.pos).cloned();
        if v.is_some() {
            self.pos += 1;
        }
        v
    }
}

fn substr_replace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let subject = a.at(0);
    let repl = a.at(1);
    let from = a.at(2);
    let len = a.opt(3);
    let mut repl_cur = Cursor::new(&repl);
    let Value::Array(subjects) = subject else {
        let s = to_str(vm, &subject)?;
        if matches!(from, Value::Array(_)) {
            return Err(crate::throw(vm, &search::substr_replace_array_error(3)));
        }
        if matches!(len, Some(Value::Array(_))) {
            return Err(crate::throw(vm, &search::substr_replace_array_error(4)));
        }
        let r = match &mut repl_cur {
            Some(c) => match c.next() {
                Some(v) => to_str(vm, &v)?,
                None => ZStr::empty(),
            },
            None => to_str(vm, &repl)?,
        };
        let f = vm.value_to_int(&from)?;
        let l = match &len {
            Some(v) => Some(vm.value_to_int(v)?),
            None => None,
        };
        return Ok(bytes(search::substr_replace(
            s.as_bytes(),
            r.as_bytes(),
            f,
            l,
        )));
    };
    let repl_str = if repl_cur.is_none() {
        to_str(vm, &repl)?
    } else {
        ZStr::empty()
    };
    let mut from_cur = Cursor::new(&from);
    let from_long = if from_cur.is_none() {
        vm.value_to_int(&from)?
    } else {
        0
    };
    let mut len_cur = len.as_ref().and_then(Cursor::new);
    let len_long = match (&len, &len_cur) {
        (Some(v), None) => Some(vm.value_to_int(v)?),
        _ => None,
    };
    let mut out = HashTable::new();
    for (k, v) in subjects.iter() {
        let orig = to_str(vm, &v.deref())?;
        let f = match &mut from_cur {
            Some(c) => match c.next() {
                Some(x) => vm.value_to_int(&x)?,
                None => 0,
            },
            None => from_long,
        };
        let l = match &mut len_cur {
            Some(c) => match c.next() {
                Some(x) => Some(vm.value_to_int(&x)?),
                None => None,
            },
            None => len_long,
        };
        let r = match &mut repl_cur {
            Some(c) => match c.next() {
                Some(x) => to_str(vm, &x)?,
                None => ZStr::empty(),
            },
            None => repl_str.clone(),
        };
        out.insert(
            k.clone(),
            bytes(search::substr_replace(orig.as_bytes(), r.as_bytes(), f, l)),
        );
    }
    Ok(Value::Array(ZArray::from_table(out)))
}

fn quotemeta(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(slashes::quotemeta(a.str(0).as_bytes())))
}

fn ord(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut n = Notices::new();
    let r = transform::ord(a.str(0).as_bytes(), &mut n);
    emit(vm, n)?;
    Ok(Value::Int(r))
}

fn chr(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut n = Notices::new();
    let c = transform::chr(a.int(0), &mut n);
    emit(vm, n)?;
    Ok(Value::Str(ZStr::from_byte(c)))
}

fn ucfirst(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(transform::ucfirst(a.str(0).as_bytes())))
}

fn lcfirst(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(transform::lcfirst(a.str(0).as_bytes())))
}

fn ucwords(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let delims = if a.has(1) {
        a.str(1)
    } else {
        ZStr::from(transform::UCWORDS_DEFAULT_DELIMITERS)
    };
    let mut n = Notices::new();
    let r = transform::ucwords(a.str(0).as_bytes(), delims.as_bytes(), &mut n);
    emit(vm, n)?;
    Ok(bytes(r))
}

fn strtr(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    let from = a.at(1);
    if a.has(2) {
        if let Value::Array(_) = from {
            return Err(a.type_error(vm, 2, "must be of type string, array given"));
        }
        let from = to_str(vm, &from)?;
        let to = a.str(2);
        if s.is_empty() {
            return Ok(bytes(ZStr::empty()));
        }
        return Ok(bytes(transform::strtr(
            s.as_bytes(),
            from.as_bytes(),
            to.as_bytes(),
        )));
    }
    let Value::Array(pairs) = from else {
        let msg = format!("must be of type array, {} given", crate::type_name(&from));
        return Err(a.type_error(vm, 2, &msg));
    };
    if s.is_empty() {
        return Ok(bytes(ZStr::empty()));
    }
    let mut ps: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(pairs.len());
    for (k, v) in pairs.iter() {
        let key = match k {
            ArrayKey::Int(i) => i.to_string().into_bytes(),
            ArrayKey::Str(s) => s.as_bytes().to_vec(),
        };
        let val = to_str(vm, &v.deref())?;
        ps.push((key, val.as_bytes().to_vec()));
    }
    let mut n = Notices::new();
    let r = transform::strtr_pairs(s.as_bytes(), &ps, &mut n);
    emit(vm, n)?;
    Ok(bytes(r))
}

fn strrev(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(transform::strrev(a.str(0).as_bytes())))
}

fn similar_text(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (sim, pct) = analysis::similar_text(a.str(0).as_bytes(), a.str(1).as_bytes());
    if a.has(2) {
        a.set_ref(2, Value::Float(pct));
    }
    Ok(Value::Int(sim))
}

fn addcslashes(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut n = Notices::new();
    let r = slashes::addcslashes(a.str(0).as_bytes(), a.str(1).as_bytes(), &mut n);
    emit(vm, n)?;
    Ok(bytes(r))
}

fn addslashes(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(slashes::addslashes(a.str(0).as_bytes())))
}

fn stripcslashes(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(slashes::stripcslashes(a.str(0).as_bytes())))
}

fn stripslashes(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(slashes::stripslashes(a.str(0).as_bytes())))
}

/// `php_str_replace_in_subject()`: applies `search`/`replace` to one
/// subject, converting array entries lazily exactly like PHP (so
/// conversion warnings repeat per subject and stop once the subject
/// becomes empty).
fn replace_in_subject(
    vm: &mut Vm,
    search: &Value,
    replace: &Value,
    subject: ZStr,
    ci: bool,
) -> VmResult<(ZStr, i64)> {
    if subject.is_empty() {
        return Ok((subject, 0));
    }
    let Value::Array(searches) = search else {
        let s = to_str(vm, search)?;
        let r = to_str(vm, replace)?;
        let res = transform::str_replace(
            subject.as_bytes(),
            ReplaceArg::One::<&[u8]>(s.as_bytes()),
            ReplaceArg::One::<&[u8]>(r.as_bytes()),
            ci,
        );
        let (out, n) = check(vm, res)?;
        return Ok((if n == 0 { subject } else { ZStr::from(out) }, n));
    };
    let mut repl_cur = Cursor::new(replace);
    let repl_one = if repl_cur.is_none() {
        to_str(vm, replace)?
    } else {
        ZStr::empty()
    };
    let mut subject = subject;
    let mut count = 0;
    for (_, entry) in searches.iter() {
        let s = to_str(vm, &entry.deref())?;
        let r = match &mut repl_cur {
            Some(c) => match c.next() {
                Some(v) => to_str(vm, &v)?,
                None => ZStr::empty(),
            },
            None => repl_one.clone(),
        };
        if s.is_empty() {
            continue;
        }
        let res = transform::str_replace(
            subject.as_bytes(),
            ReplaceArg::One::<&[u8]>(s.as_bytes()),
            ReplaceArg::One::<&[u8]>(r.as_bytes()),
            ci,
        );
        let (out, n) = check(vm, res)?;
        if n > 0 {
            count += n;
            subject = ZStr::from(out);
            if subject.is_empty() {
                break;
            }
        }
    }
    Ok((subject, count))
}

fn str_replace_common(vm: &mut Vm, a: &Args, ci: bool) -> VmResult<Value> {
    let search = a.at(0);
    let replace = a.at(1);
    let subject = a.at(2);
    if !matches!(search, Value::Array(_)) && matches!(replace, Value::Array(_)) {
        return Err(a.type_error(
            vm,
            2,
            "must be of type string when argument #1 ($search) is a string",
        ));
    }
    let mut count = 0;
    let result = if let Value::Array(subjects) = subject {
        let mut out = HashTable::new();
        for (k, v) in subjects.iter() {
            let s = to_str(vm, &v.deref())?;
            let (r, n) = replace_in_subject(vm, &search, &replace, s, ci)?;
            count += n;
            out.insert(k.clone(), Value::Str(r));
        }
        Value::Array(ZArray::from_table(out))
    } else {
        let s = to_str(vm, &subject)?;
        let (r, n) = replace_in_subject(vm, &search, &replace, s, ci)?;
        count = n;
        Value::Str(r)
    };
    if a.has(3) {
        a.set_ref(3, Value::Int(count));
    }
    Ok(result)
}

fn str_replace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    str_replace_common(vm, a, false)
}

fn str_ireplace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    str_replace_common(vm, a, true)
}

fn hebrev(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(transform::hebrev(a.str(0).as_bytes(), a.int(1))))
}

fn nl2br(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let xhtml = !a.has(1) || a.bool(1);
    Ok(bytes(transform::nl2br(a.str(0).as_bytes(), xhtml)))
}

fn strip_tags_fn(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    let allowed = match a.opt(1) {
        None => None,
        Some(Value::Array(list)) => {
            let mut tags = Vec::with_capacity(list.len());
            for (_, v) in list.iter() {
                tags.push(to_str(vm, &v.deref())?.as_bytes().to_vec());
            }
            strip_tags::allowed_tags_from_list(&tags)
        }
        Some(v) => Some(to_str(vm, &v)?.as_bytes().to_vec()),
    };
    Ok(bytes(strip_tags::strip_tags(
        s.as_bytes(),
        allowed.as_deref(),
    )))
}

fn str_repeat(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = transform::str_repeat(a.str(0).as_bytes(), a.int(1));
    Ok(bytes(check(vm, r)?))
}

fn count_chars(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = analysis::count_chars(a.str(0).as_bytes(), a.int(1));
    Ok(match check(vm, r)? {
        CharCounts::Array(pairs) => assoc(
            pairs
                .into_iter()
                .map(|(b, c)| (i64::from(b), Value::Int(c))),
        ),
        CharCounts::Bytes(b) => bytes(b),
    })
}

fn strnatcmp(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(compare::strnatcmp(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        false,
    )))
}

fn strnatcasecmp(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(compare::strnatcmp(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        true,
    )))
}

fn strcmp(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(compare::strcmp(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
    )))
}

fn strcasecmp(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(compare::strcasecmp(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
    )))
}

fn strncmp(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = compare::strncmp(a.str(0).as_bytes(), a.str(1).as_bytes(), a.int(2));
    Ok(Value::Int(check(vm, r)?))
}

fn strncasecmp(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = compare::strncasecmp(a.str(0).as_bytes(), a.str(1).as_bytes(), a.int(2));
    Ok(Value::Int(check(vm, r)?))
}

fn substr_count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = search::substr_count(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.int(2),
        opt_int(a, 3),
    );
    Ok(Value::Int(check(vm, r)?))
}

fn str_pad(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let pad = if a.has(2) { a.str(2) } else { ZStr::from(" ") };
    let ty = if a.has(3) {
        a.int(3)
    } else {
        transform::STR_PAD_RIGHT
    };
    let r = transform::str_pad(a.str(0).as_bytes(), a.int(1), pad.as_bytes(), ty);
    Ok(bytes(check(vm, r)?))
}

fn str_rot13(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(transform::str_rot13(a.str(0).as_bytes())))
}

fn str_shuffle(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut s = a.str(0).as_bytes().to_vec();
    if s.len() <= 1 {
        return Ok(bytes(s));
    }
    // php_string_shuffle(): Fisher-Yates over the default (MT) engine,
    // which mt_rand() shares.
    let rand = Value::string("mt_rand");
    let mut left = s.len() - 1;
    while left > 0 {
        let max = i64::try_from(left).unwrap_or(i64::MAX);
        let r = vm.call_value(&rand, vec![Value::Int(0), Value::Int(max)])?;
        let idx = usize::try_from(vm.value_to_int(&r)?).unwrap_or(0).min(left);
        s.swap(left, idx);
        left -= 1;
    }
    Ok(bytes(s))
}

fn str_word_count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let chars = a.opt(2).map(|_| a.str(2));
    let mut n = Notices::new();
    let r = analysis::str_word_count(
        a.str(0).as_bytes(),
        a.int(1),
        chars.as_ref().map(ZStr::as_bytes),
        &mut n,
    );
    emit(vm, n)?;
    Ok(match check(vm, r)? {
        WordCount::Count(c) => Value::Int(c),
        WordCount::Words(w) => str_list(w),
        WordCount::Positions(p) => assoc(p.into_iter().map(|(pos, w)| (pos, bytes(w)))),
    })
}

fn str_split(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let len = if a.has(1) { a.int(1) } else { 1 };
    let r = transform::str_split(a.str(0).as_bytes(), len);
    Ok(str_list(check(vm, r)?))
}

fn strpbrk(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = search::strpbrk(a.str(0).as_bytes(), a.str(1).as_bytes());
    Ok(opt_bytes(check(vm, r)?))
}

fn substr_compare(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = compare::substr_compare(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.int(2),
        opt_int(a, 3),
        a.bool(4),
    );
    Ok(Value::Int(check(vm, r)?))
}

const UTF8_DEPRECATION: &str = "visit the php.net documentation for various alternatives";

fn utf8_encode(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    deprecated_function(vm, "utf8_encode", "8.2", UTF8_DEPRECATION)?;
    Ok(bytes(transform::utf8_encode(a.str(0).as_bytes())))
}

fn utf8_decode(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    deprecated_function(vm, "utf8_decode", "8.2", UTF8_DEPRECATION)?;
    Ok(bytes(transform::utf8_decode(a.str(0).as_bytes())))
}

fn str_getcsv(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    use php_stdstr::csv;
    let sep = a.has(1).then(|| a.str(1));
    let enc = a.has(2).then(|| a.str(2));
    let esc = a.has(3).then(|| a.str(3));
    let mut n = Notices::new();
    let opts = csv::csv_options(
        "str_getcsv",
        2,
        sep.as_ref().map(ZStr::as_bytes),
        enc.as_ref().map(ZStr::as_bytes),
        esc.as_ref().map(ZStr::as_bytes),
        &mut n,
    );
    emit(vm, n)?;
    let opts = check(vm, opts)?;
    Ok(match csv::str_getcsv(a.str(0).as_bytes(), &opts) {
        Some(fields) => str_list(fields),
        None => Value::Array(ZArray::from_values([Value::Null])),
    })
}

/// Registers the `string.c` functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function("bin2hex(string $string): string", bin2hex)
        .function("hex2bin(string $string): string|false", hex2bin)
        .function("strspn(string $string, string $characters, int $offset = 0, ?int $length = null): int", strspn)
        .function("strcspn(string $string, string $characters, int $offset = 0, ?int $length = null): int", strcspn)
        .function("strcoll(string $string1, string $string2): int", strcoll)
        .function("trim(string $string, string $characters = \" \\f\\n\\r\\t\\v\\0\"): string", trim)
        .function("rtrim(string $string, string $characters = \" \\f\\n\\r\\t\\v\\0\"): string", rtrim)
        .function("chop(string $string, string $characters = \" \\f\\n\\r\\t\\v\\0\"): string", rtrim)
        .function("ltrim(string $string, string $characters = \" \\f\\n\\r\\t\\v\\0\"): string", ltrim)
        .function("wordwrap(string $string, int $width = 75, string $break = \"\\n\", bool $cut_long_words = false): string", wordwrap)
        .function("explode(string $separator, string $string, int $limit = PHP_INT_MAX): array", explode)
        .function("implode(array|string $separator, ?array $array = null): string", implode)
        .function("join(array|string $separator, ?array $array = null): string", implode)
        .function("strtok(string $string, ?string $token = null): string|false", strtok)
        .function("strtoupper(string $string): string", strtoupper)
        .function("strtolower(string $string): string", strtolower)
        .function("str_increment(string $string): string", str_increment)
        .function("str_decrement(string $string): string", str_decrement)
        .function("stristr(string $haystack, string $needle, bool $before_needle = false): string|false", stristr)
        .function("strstr(string $haystack, string $needle, bool $before_needle = false): string|false", strstr)
        .function("strchr(string $haystack, string $needle, bool $before_needle = false): string|false", strstr)
        .function("strpos(string $haystack, string $needle, int $offset = 0): int|false", strpos)
        .function("stripos(string $haystack, string $needle, int $offset = 0): int|false", stripos)
        .function("strrpos(string $haystack, string $needle, int $offset = 0): int|false", strrpos)
        .function("strripos(string $haystack, string $needle, int $offset = 0): int|false", strripos)
        .function("strrchr(string $haystack, string $needle, bool $before_needle = false): string|false", strrchr)
        .function("str_contains(string $haystack, string $needle): bool", str_contains)
        .function("str_starts_with(string $haystack, string $needle): bool", str_starts_with)
        .function("str_ends_with(string $haystack, string $needle): bool", str_ends_with)
        .function("chunk_split(string $string, int $length = 76, string $separator = \"\\r\\n\"): string", chunk_split)
        .function("substr(string $string, int $offset, ?int $length = null): string", substr)
        .function("substr_replace(array|string $string, array|string $replace, array|int $offset, array|int|null $length = null): string|array", substr_replace)
        .function("quotemeta(string $string): string", quotemeta)
        .function("ord(string $character): int", ord)
        .function("chr(int $codepoint): string", chr)
        .function("ucfirst(string $string): string", ucfirst)
        .function("lcfirst(string $string): string", lcfirst)
        .function("ucwords(string $string, string $separators = \" \\t\\r\\n\\f\\v\"): string", ucwords)
        .function("strtr(string $string, array|string $from, ?string $to = null): string", strtr)
        .function("strrev(string $string): string", strrev)
        .function("similar_text(string $string1, string $string2, &$percent = null): int", similar_text)
        .function("addcslashes(string $string, string $characters): string", addcslashes)
        .function("addslashes(string $string): string", addslashes)
        .function("stripcslashes(string $string): string", stripcslashes)
        .function("stripslashes(string $string): string", stripslashes)
        .function("str_replace(array|string $search, array|string $replace, array|string $subject, &$count = null): array|string", str_replace)
        .function("str_ireplace(array|string $search, array|string $replace, array|string $subject, &$count = null): array|string", str_ireplace)
        .function("hebrev(string $string, int $max_chars_per_line = 0): string", hebrev)
        .function("nl2br(string $string, bool $use_xhtml = true): string", nl2br)
        .function("strip_tags(string $string, array|string|null $allowed_tags = null): string", strip_tags_fn)
        .function("str_getcsv(string $string, string $separator = \",\", string $enclosure = \"\\\"\", string $escape = \"\\\\\"): array", str_getcsv)
        .function("str_repeat(string $string, int $times): string", str_repeat)
        .function("count_chars(string $string, int $mode = 0): array|string", count_chars)
        .function("strnatcmp(string $string1, string $string2): int", strnatcmp)
        .function("strnatcasecmp(string $string1, string $string2): int", strnatcasecmp)
        .function("strcmp(string $string1, string $string2): int", strcmp)
        .function("strcasecmp(string $string1, string $string2): int", strcasecmp)
        .function("strncmp(string $string1, string $string2, int $length): int", strncmp)
        .function("strncasecmp(string $string1, string $string2, int $length): int", strncasecmp)
        .function("substr_count(string $haystack, string $needle, int $offset = 0, ?int $length = null): int", substr_count)
        .function("str_pad(string $string, int $length, string $pad_string = \" \", int $pad_type = STR_PAD_RIGHT): string", str_pad)
        .function("str_rot13(string $string): string", str_rot13)
        .function("str_shuffle(string $string): string", str_shuffle)
        .function("str_word_count(string $string, int $format = 0, ?string $characters = null): array|int", str_word_count)
        .function("str_split(string $string, int $length = 1): array", str_split)
        .function("strpbrk(string $string, string $characters): string|false", strpbrk)
        .function("substr_compare(string $haystack, string $needle, int $offset, ?int $length = null, bool $case_insensitive = false): int", substr_compare)
        .function("utf8_encode(string $string): string", utf8_encode)
        .function("utf8_decode(string $string): string", utf8_decode);
    for (name, v) in [
        ("STR_PAD_LEFT", transform::STR_PAD_LEFT),
        ("STR_PAD_RIGHT", transform::STR_PAD_RIGHT),
        ("STR_PAD_BOTH", transform::STR_PAD_BOTH),
        ("PATHINFO_DIRNAME", transform::PATHINFO_DIRNAME),
        ("PATHINFO_BASENAME", transform::PATHINFO_BASENAME),
        ("PATHINFO_EXTENSION", transform::PATHINFO_EXTENSION),
        ("PATHINFO_FILENAME", transform::PATHINFO_FILENAME),
        ("PATHINFO_ALL", transform::PATHINFO_ALL),
        ("CHAR_MAX", 127),
    ] {
        m.constant(name, Value::Int(v));
    }
}
