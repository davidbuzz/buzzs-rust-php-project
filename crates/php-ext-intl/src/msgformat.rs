//! `MessageFormatter` and the `msgfmt_*()` functions.

use php_intl::calendar::{IntlCalendar, PhpDateTime};
use php_intl::msgformat::{self as lib, MessageFormatter, MsgKey, MsgParsed, MsgValue};
use php_runtime::{ArrayKey, Obj, Value, ZArray, ZStr};
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core::{self, s};
use crate::dt;
use crate::locale::text;

fn ctor(vm: &mut Vm, a: &Args, o: &Obj) -> VmResult<bool> {
    core::reset_global();
    let loc = text(&a.str(0));
    let pat = a.str(1);
    match MessageFormatter::new(&loc, pat.as_bytes()) {
        Ok(f) => {
            core::set_data(o, f);
            Ok(true)
        }
        Err(e) => {
            core::fail(vm, a, Some(o), e, 0)?;
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
    let o = core::instantiate(vm, "MessageFormatter")?;
    if ctor(vm, a, &o)? {
        Ok(Value::Object(o))
    } else {
        Ok(Value::Null)
    }
}

fn fetch(vm: &mut Vm, a: &Args) -> VmResult<(Obj, usize)> {
    let (o, off) = core::target(a);
    core::reset_both(&o);
    if !core::is_constructed::<MessageFormatter>(&o) {
        return Err(core::unconstructed(vm, "MessageFormatter"));
    }
    Ok((o, off))
}

/// An owned `$values` element.
enum Owned {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(ZStr),
    DateTime(PhpDateTime),
    Calendar(IntlCalendar),
    Object(String, Option<ZStr>),
    Array(bool),
    Other,
}

fn owned(vm: &mut Vm, v: &Value) -> VmResult<Owned> {
    Ok(match v.deref() {
        Value::Null => Owned::Null,
        Value::Bool(b) => Owned::Bool(b),
        Value::Int(i) => Owned::Int(i),
        Value::Float(f) => Owned::Float(f),
        Value::Str(z) => Owned::Str(z),
        Value::Array(arr) => Owned::Array(!arr.is_empty()),
        Value::Object(o) => {
            if dt::instance_of(&o, "DateTimeInterface") {
                Owned::DateTime(dt::php_datetime(vm, &o)?)
            } else if dt::instance_of(&o, "IntlCalendar") {
                match crate::calendar::require(&o) {
                    Some(c) => Owned::Calendar(c),
                    None => Owned::Object("IntlCalendar".into(), None),
                }
            } else {
                let class = php_vm::object::class_of(&o).name.to_string_lossy();
                let as_string = if crate::collator::has_to_string(&o) {
                    Some(vm.value_to_string(&Value::Object(o))?)
                } else {
                    None
                };
                Owned::Object(class, as_string)
            }
        }
        _ => Owned::Other,
    })
}

fn view(o: &Owned) -> MsgValue<'_> {
    match o {
        Owned::Null => MsgValue::Null,
        Owned::Bool(b) => MsgValue::Bool(*b),
        Owned::Int(i) => MsgValue::Int(*i),
        Owned::Float(f) => MsgValue::Float(*f),
        Owned::Str(z) => MsgValue::Str(z.as_bytes()),
        Owned::DateTime(d) => MsgValue::DateTime(d),
        Owned::Calendar(c) => MsgValue::Calendar(c),
        Owned::Object(class, z) => MsgValue::Object {
            class,
            as_string: z.as_ref().map(ZStr::as_bytes),
        },
        Owned::Array(n) => MsgValue::Array { non_empty: *n },
        Owned::Other => MsgValue::Other,
    }
}

fn owned_args(vm: &mut Vm, arr: &ZArray) -> VmResult<Vec<(ArrayKey, Owned)>> {
    let mut out = Vec::with_capacity(arr.len());
    for (k, v) in arr.iter() {
        out.push((k.clone(), owned(vm, v)?));
    }
    Ok(out)
}

fn views(args: &[(ArrayKey, Owned)]) -> Vec<(MsgKey<'_>, MsgValue<'_>)> {
    args.iter()
        .map(|(k, v)| {
            (
                match k {
                    ArrayKey::Int(i) => MsgKey::Index(*i),
                    ArrayKey::Str(z) => MsgKey::Name(z.as_bytes()),
                },
                view(v),
            )
        })
        .collect()
}

fn parsed_array(v: Vec<MsgParsed>) -> Value {
    Value::Array(ZArray::from_values(v.into_iter().map(|p| match p {
        MsgParsed::Int(i) => Value::Int(i),
        MsgParsed::Float(f) => Value::Float(f),
        MsgParsed::Str(t) => s(t),
    })))
}

fn format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    let args = owned_args(vm, &a.array(off))?;
    let tz = dt::default_tz_name(vm)?;
    let r = core::with::<MessageFormatter, _>(&o, |f| f.format(&views(&args), &tz));
    match r {
        Some(Ok(v)) => Ok(s(v)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

fn format_message(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let args = owned_args(vm, &a.array(2))?;
    let tz = dt::default_tz_name(vm)?;
    let r = lib::format_message(&text(&a.str(0)), a.str(1).as_bytes(), &views(&args), &tz);
    match r {
        Ok(v) => Ok(s(v)),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

fn parse(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    let src = a.str(off);
    match core::with::<MessageFormatter, _>(&o, |f| f.parse(src.as_bytes())) {
        Some(Ok(v)) => Ok(parsed_array(v)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

fn parse_message(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match lib::parse_message(&text(&a.str(0)), a.str(1).as_bytes(), a.str(2).as_bytes()) {
        Ok(v) => Ok(parsed_array(v)),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

fn set_pattern(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, off) = fetch(vm, a)?;
    let p = a.str(off);
    match core::with::<MessageFormatter, _>(&o, |f| f.set_pattern(p.as_bytes())) {
        Some(Ok(())) => Ok(Value::Bool(true)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, off, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

fn get_pattern(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = fetch(vm, a)?;
    Ok(
        core::with::<MessageFormatter, _>(&o, |f| Value::Str(ZStr::from(f.pattern())))
            .unwrap_or(Value::Bool(false)),
    )
}

fn get_locale(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = fetch(vm, a)?;
    Ok(core::with::<MessageFormatter, _>(&o, |f| s(f.locale())).unwrap_or(Value::Bool(false)))
}

/// `clone_obj`.
pub(crate) fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    core::create(vm, new)?;
    match core::with::<MessageFormatter, _>(old, |f| f.try_clone()) {
        Some(Ok(f)) => core::set_data(new, f),
        Some(Err(_)) => return Err(vm.throw("Error", "Failed to clone MessageFormatter")),
        None => return Err(vm.throw("Error", "Cannot clone uninitialized MessageFormatter")),
    }
    Ok(())
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("MessageFormatter::__construct", construct),
        ("MessageFormatter::create", create),
        ("MessageFormatter::format", format),
        ("MessageFormatter::formatMessage", format_message),
        ("MessageFormatter::parse", parse),
        ("MessageFormatter::parseMessage", parse_message),
        ("MessageFormatter::setPattern", set_pattern),
        ("MessageFormatter::getPattern", get_pattern),
        ("MessageFormatter::getLocale", get_locale),
        ("MessageFormatter::getErrorCode", core::get_error_code),
        ("MessageFormatter::getErrorMessage", core::get_error_message),
    ]
}
