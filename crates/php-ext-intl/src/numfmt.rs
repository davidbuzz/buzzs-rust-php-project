//! `NumberFormatter` and the `numfmt_*()` functions.

use php_intl::numfmt::{Number, NumberFormatter, Parsed};
use php_runtime::{Obj, Value};
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core::{self, s};
use crate::locale::text;

fn ctor(vm: &mut Vm, a: &Args, o: &Obj) -> VmResult<bool> {
    core::reset_global();
    let loc = text(&a.str(0));
    if loc.len() <= php_intl::common::INTL_MAX_LOCALE_LEN && core::is_constructed::<NumberFormatter>(o) {
        return Err(vm.throw("Error", "NumberFormatter object is already constructed"));
    }
    let pattern = match a.opt(2) {
        Some(Value::Str(z)) => Some(z),
        _ => None,
    };
    match NumberFormatter::new(&loc, a.int(1), pattern.as_ref().map(|z| z.as_bytes())) {
        Ok(f) => {
            core::set_data(o, f);
            Ok(true)
        }
        Err(e) => {
            let scope_global = matches!(&e, php_intl::Error::Intl(ie) if ie.message.as_deref().is_some_and(|m| m.starts_with("Locale string too long")));
            core::fail(vm, a, if scope_global { None } else { Some(o) }, e, 0)?;
            Ok(false)
        }
    }
}

fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    core::in_ctor(|| ctor(vm, a, &o))?;
    Ok(Value::Null)
}

fn create(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = core::instantiate(vm, "NumberFormatter")?;
    if ctor(vm, a, &o)? {
        Ok(Value::Object(o))
    } else {
        Ok(Value::Null)
    }
}

fn fetch(vm: &mut Vm, a: &Args) -> VmResult<(Obj, usize)> {
    let (o, off) = core::target(a);
    core::reset_both(&o);
    if !core::is_constructed::<NumberFormatter>(&o) {
        return Err(core::unconstructed(vm, "NumberFormatter"));
    }
    Ok((o, off))
}

fn run<T>(
    vm: &mut Vm,
    a: &Args,
    o: &Obj,
    off: usize,
    f: impl FnOnce(&mut NumberFormatter) -> php_intl::Result<T>,
    ok: impl FnOnce(T) -> Value,
) -> VmResult<Value> {
    match core::with::<NumberFormatter, _>(o, f) {
        Some(Ok(v)) => Ok(ok(v)),
        Some(Err(e)) => core::fail_with(vm, a, Some(o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

/// An `int|float` argument.
pub(crate) fn number(v: &Value) -> Number {
    match v {
        Value::Float(f) => Number::Float(*f),
        Value::Int(i) => Number::Int(*i),
        Value::Bool(b) => Number::Int(i64::from(*b)),
        _ => Number::Int(0),
    }
}

fn num_value(n: Number) -> Value {
    match n {
        Number::Int(i) => Value::Int(i),
        Number::Float(f) => Value::Float(f),
    }
}

macro_rules! simple {
    ($name:ident, |$f:ident, $a:ident, $off:ident| $body:expr, $ok:expr) => {
        fn $name(vm: &mut Vm, $a: &mut Args) -> VmResult<Value> {
            let (o, $off) = fetch(vm, $a)?;
            let _ = $off;
            run(vm, $a, &o, $off, |$f| $body, $ok)
        }
    };
}

simple!(format, |f, a, off| f.format(number(&a.at(off)), a.int(off + 1), off == 1), s);
simple!(format_currency, |f, a, off| f.format_currency(a.float(off), a.str(off + 1).as_bytes()), s);
simple!(set_attribute, |f, a, off| f.set_attribute(a.int(off), number(&a.at(off + 1))), |()| Value::Bool(true));
simple!(get_attribute, |f, a, off| f.get_attribute(a.int(off)), num_value);
simple!(set_text_attribute, |f, a, off| f.set_text_attribute(a.int(off), a.str(off + 1).as_bytes()), |()| Value::Bool(true));
simple!(get_text_attribute, |f, a, off| f.get_text_attribute(a.int(off)), s);
simple!(set_symbol, |f, a, off| f.set_symbol(a.int(off), a.str(off + 1).as_bytes()), |()| Value::Bool(true));
simple!(get_symbol, |f, a, off| f.get_symbol(a.int(off)), s);
simple!(set_pattern, |f, a, off| f.set_pattern(a.str(off).as_bytes()), |()| Value::Bool(true));
simple!(get_pattern, |f, a, off| f.get_pattern(), s);
simple!(get_locale, |f, a, off| f.get_locale(a.int(off)), s);

/// The `&$offset` argument (`zval_get_long`), `None` when absent/null.
fn offset_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Option<i64>> {
    if !a.has(n) {
        return Ok(None);
    }
    match a.at(n) {
        Value::Null => Ok(None),
        v => Ok(Some(vm.value_to_int(&v)?)),
    }
}

fn parse(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = core::target(a);
    core::reset_global();
    let pos = offset_arg(vm, a, off + 2)?;
    if let Some(p) = pos {
        if i32::try_from(p).is_err() {
            return Err(core::value_error(vm, a, off + 3, &format!("must be between {} and {}", i32::MIN, i32::MAX)));
        }
    }
    let (o, off) = {
        let _ = (o, off);
        fetch(vm, a)?
    };
    let st = a.str(off);
    let ty = a.int(off + 1);
    let r = core::with::<NumberFormatter, _>(&o, |f| f.parse(st.as_bytes(), ty, pos, off == 1));
    match r {
        Some(Ok((v, newpos))) => {
            if let Some(p) = newpos {
                a.set_ref(off + 2, Value::Int(p));
            }
            Ok(match v {
                Parsed::Int(i) => Value::Int(i),
                Parsed::Float(f) => Value::Float(f),
            })
        }
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

fn parse_currency(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    let pos = offset_arg(vm, a, off + 2)?;
    let st = a.str(off);
    let r = core::with::<NumberFormatter, _>(&o, |f| f.parse_currency(st.as_bytes(), pos));
    match r {
        Some(Ok((v, cur, newpos))) => {
            if let Some(p) = newpos {
                a.set_ref(off + 2, Value::Int(p));
            }
            a.set_ref(off + 1, s(cur));
            Ok(Value::Float(v))
        }
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

/// `clone_obj`.
pub(crate) fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    core::create(vm, new)?;
    match core::with::<NumberFormatter, _>(old, |f| f.try_clone()) {
        Some(Ok(f)) => core::set_data(new, f),
        Some(Err(_)) => return Err(vm.throw("Error", "Failed to clone NumberFormatter")),
        None => return Err(vm.throw("Error", "Cannot clone uninitialized NumberFormatter")),
    }
    Ok(())
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("NumberFormatter::__construct", construct),
        ("NumberFormatter::create", create),
        ("NumberFormatter::format", format),
        ("NumberFormatter::parse", parse),
        ("NumberFormatter::formatCurrency", format_currency),
        ("NumberFormatter::parseCurrency", parse_currency),
        ("NumberFormatter::setAttribute", set_attribute),
        ("NumberFormatter::getAttribute", get_attribute),
        ("NumberFormatter::setTextAttribute", set_text_attribute),
        ("NumberFormatter::getTextAttribute", get_text_attribute),
        ("NumberFormatter::setSymbol", set_symbol),
        ("NumberFormatter::getSymbol", get_symbol),
        ("NumberFormatter::setPattern", set_pattern),
        ("NumberFormatter::getPattern", get_pattern),
        ("NumberFormatter::getLocale", get_locale),
        ("NumberFormatter::getErrorCode", core::get_error_code),
        ("NumberFormatter::getErrorMessage", core::get_error_message),
    ]
}
