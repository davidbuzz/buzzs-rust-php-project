// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `DateTimeInterface`, `DateTime`, `DateTimeImmutable` and the procedural
//! `date_*()` functions operating on them.

use std::rc::Rc;

use php_date::api;
use php_date::parse::TimezoneSpec;
use php_date::Time;
use php_runtime::{ArrayKey, Obj, PropertyPurpose, Value, ZArray, ZStr};
use php_vm::class::{cflags, AttributeInfo, Class, ConstExpr};
use php_vm::ext::{
    acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, Throw, VmResult,
};
use php_vm::func::flags;
use php_vm::object;
use php_vm::Vm;

use crate::common::{default_tz, get_str, lossy, now, to_php, update_errors};
use crate::functions::date_get_last_errors;
use crate::objects::{
    class, clone_raw, instantiate, new_date, new_interval, new_tz, require_interval, require_time,
    restore_property, set_time, std_props, time_of, tz_of, uninit_error, with_time, DateObj,
};

/// The `DATE_*` / `DateTimeInterface::*` format constants.
pub(crate) const FORMATS: [(&str, &str); 14] = [
    ("ATOM", "Y-m-d\\TH:i:sP"),
    ("COOKIE", "l, d-M-Y H:i:s T"),
    ("ISO8601", "Y-m-d\\TH:i:sO"),
    ("ISO8601_EXPANDED", "X-m-d\\TH:i:sP"),
    ("RFC822", "D, d M y H:i:s O"),
    ("RFC850", "l, d-M-y H:i:s T"),
    ("RFC1036", "D, d M y H:i:s O"),
    ("RFC1123", "D, d M Y H:i:s O"),
    ("RFC7231", "D, d M Y H:i:s \\G\\M\\T"),
    ("RFC2822", "D, d M Y H:i:s O"),
    ("RFC3339", "Y-m-d\\TH:i:sP"),
    ("RFC3339_EXTENDED", "Y-m-d\\TH:i:s.vP"),
    ("RSS", "D, d M Y H:i:s O"),
    ("W3C", "Y-m-d\\TH:i:sP"),
];

/// Which of the two concrete date classes a method belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Mutable,
    Immutable,
}

impl Kind {
    fn class_name(self) -> &'static str {
        match self {
            Self::Mutable => "DateTime",
            Self::Immutable => "DateTimeImmutable",
        }
    }
}

fn this(a: &Args) -> Obj {
    a.this_obj()
}

/// Object argument `n`, if it is one.
pub(crate) fn obj_arg(a: &Args, n: usize) -> Option<Obj> {
    match a.at(n) {
        Value::Object(o) => Some(o),
        _ => None,
    }
}

/// The zone of an optional `?DateTimeZone` argument; `Err(())` when the
/// object was never initialised.
fn zone_arg(a: &Args, n: usize) -> Result<Option<TimezoneSpec>, ()> {
    match a.opt(n) {
        Some(Value::Object(o)) => tz_of(&o).map(Some).ok_or(()),
        _ => Ok(None),
    }
}

/// `php_date_initialize()`: parses `time_str` (optionally with `format`)
/// into `o`. With `ctor` a parse error throws `DateMalformedStringException`.
fn initialize(
    vm: &mut Vm,
    o: &Obj,
    time_str: &[u8],
    format: Option<&[u8]>,
    zone: &Result<Option<TimezoneSpec>, ()>,
    ctor: bool,
) -> VmResult<bool> {
    let dtz = default_tz(vm)?;
    let (sec, usec) = now();
    let zone_ok = zone.clone().unwrap_or(None);
    let (t, errors) = api::date_initialize(time_str, format, zone_ok.as_ref(), &dtz, sec, usec);
    update_errors(&errors);
    if !errors.errors.is_empty() {
        set_time(o, None);
        if ctor {
            let s: &[u8] = if format.is_none() && time_str.is_empty() {
                b"now"
            } else {
                time_str
            };
            let msg = api::parse_failure_message(s, &errors);
            return Err(vm.throw("DateMalformedStringException", &lossy(&msg)));
        }
        return Ok(false);
    }
    if zone.is_err() {
        return Err(vm.throw("Error", api::messages::ZONE_NOT_INITIALIZED));
    }
    set_time(o, t);
    Ok(true)
}

fn date_create_impl(vm: &mut Vm, a: &Args, cls: &Rc<Class>) -> VmResult<Value> {
    let s = if a.has(0) { a.str(0) } else { ZStr::from("") };
    let o = instantiate(vm, cls)?;
    if initialize(vm, &o, s.as_bytes(), None, &zone_arg(a, 1), false)? {
        Ok(Value::Object(o))
    } else {
        Ok(Value::Bool(false))
    }
}

fn date_create(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cls = class(vm, "DateTime");
    date_create_impl(vm, a, &cls)
}

fn date_create_immutable(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cls = class(vm, "DateTimeImmutable");
    date_create_impl(vm, a, &cls)
}

/// `zend_argument_value_error()` with the full `Class::method` name.
pub(crate) fn arg_value_error(vm: &mut Vm, func: &str, n: usize, pname: &str, msg: &str) -> Throw {
    vm.throw(
        "ValueError",
        &format!("{func}(): Argument #{n} (${pname}) {msg}"),
    )
}

/// The `Class::method` (or function) name of the running native call.
pub(crate) fn func_display(a: &Args) -> String {
    let name = a.func_name();
    match (&a.this, &a.called) {
        (Some(o), _) => {
            let cls = object::class_of(o);
            let decl = declaring_class(cls, &name);
            format!("{decl}::{name}")
        }
        (None, Some(c)) => {
            let decl = declaring_class(c, &name);
            format!("{decl}::{name}")
        }
        _ => name,
    }
}

/// The internal class declaring method `name` (walking up from `cls`).
fn declaring_class(cls: &Rc<Class>, name: &str) -> String {
    let lc = name.to_ascii_lowercase();
    match cls.methods.get(&ZStr::from(lc.as_str())) {
        Some(m) => m
            .scope
            .upgrade()
            .map_or_else(|| cls.name.to_string_lossy(), |c| c.name.to_string_lossy()),
        None => cls.name.to_string_lossy(),
    }
}

/// `Z_PARAM_PATH`: rejects strings with NUL bytes.
pub(crate) fn null_byte_check(vm: &mut Vm, a: &Args, n: usize, s: &ZStr) -> VmResult<()> {
    if s.as_bytes().contains(&0) {
        let pname = a
            .func
            .as_ref()
            .and_then(|f| f.params.get(n - 1).map(|p| p.name.to_string_lossy()))
            .unwrap_or_default();
        let func = func_display(a);
        return Err(arg_value_error(
            vm,
            &func,
            n,
            &pname,
            "must not contain any null bytes",
        ));
    }
    Ok(())
}

fn from_format_impl(vm: &mut Vm, a: &Args, cls: &Rc<Class>) -> VmResult<Value> {
    let format = a.str(0);
    let s = a.str(1);
    null_byte_check(vm, a, 2, &s)?;
    let o = instantiate(vm, cls)?;
    if initialize(
        vm,
        &o,
        s.as_bytes(),
        Some(format.as_bytes()),
        &zone_arg(a, 2),
        false,
    )? {
        Ok(Value::Object(o))
    } else {
        Ok(Value::Bool(false))
    }
}

fn date_create_from_format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cls = class(vm, "DateTime");
    from_format_impl(vm, a, &cls)
}

fn date_create_immutable_from_format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cls = class(vm, "DateTimeImmutable");
    from_format_impl(vm, a, &cls)
}

fn called_or(vm: &Vm, a: &Args, kind: Kind) -> Rc<Class> {
    a.called
        .clone()
        .unwrap_or_else(|| class(vm, kind.class_name()))
}

fn create_from_format(vm: &mut Vm, a: &Args, kind: Kind) -> VmResult<Value> {
    let cls = called_or(vm, a, kind);
    from_format_impl(vm, a, &cls)
}

fn dt_create_from_format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    create_from_format(vm, a, Kind::Mutable)
}

fn dti_create_from_format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    create_from_format(vm, a, Kind::Immutable)
}

fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = this(a);
    let s = if a.has(0) { a.str(0) } else { ZStr::from("") };
    initialize(vm, &o, s.as_bytes(), None, &zone_arg(a, 1), true)?;
    Ok(Value::Null)
}

/// `createFromImmutable` / `createFromMutable` / `createFromInterface`.
fn create_from_other(vm: &mut Vm, a: &Args, kind: Kind) -> VmResult<Value> {
    let Some(src) = obj_arg(a, 0) else {
        return Ok(Value::Null);
    };
    let t = require_time(vm, &src)?;
    let cls = called_or(vm, a, kind);
    Ok(Value::Object(new_date(vm, &cls, t)?))
}

fn dt_create_from_other(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    create_from_other(vm, a, Kind::Mutable)
}

fn dti_create_from_other(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    create_from_other(vm, a, Kind::Immutable)
}

fn create_from_timestamp(vm: &mut Vm, a: &Args, kind: Kind) -> VmResult<Value> {
    let cls = called_or(vm, a, kind);
    let t = match a.at(0) {
        Value::Int(i) => api::from_timestamp(i, 0),
        Value::Float(f) => match api::from_timestamp_float(f) {
            Ok(t) => t,
            Err(m) => {
                // C's `%g` prints non-finite values in upper case.
                let m = m
                    .replace(", nan given", ", NAN given")
                    .replace(", -inf given", ", -INF given")
                    .replace(", inf given", ", INF given");
                let msg = format!(
                    "{}::createFromTimestamp(): Argument #1 ($timestamp) {m}",
                    kind.class_name()
                );
                return Err(vm.throw("DateRangeError", &msg));
            }
        },
        _ => return Ok(Value::Null),
    };
    Ok(Value::Object(new_date(vm, &cls, t)?))
}

fn dt_create_from_timestamp(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    create_from_timestamp(vm, a, Kind::Mutable)
}

fn dti_create_from_timestamp(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    create_from_timestamp(vm, a, Kind::Immutable)
}

// ---------------------------------------------------------------------------
// Serialisation
// ---------------------------------------------------------------------------

/// `date_object_to_hash()` appended to `props`.
pub(crate) fn add_date_props(props: &mut ZArray, t: &Time) {
    if let Value::Array(d) = to_php(&api::date_props(t)) {
        for (k, v) in d.iter() {
            props.make_mut().insert(k.clone(), v.clone());
        }
    }
}

/// `php_date_initialize_from_hash()`.
fn init_from_hash(vm: &mut Vm, o: &Obj, h: &ZArray) -> VmResult<bool> {
    let (Some(Value::Str(date)), Some(Value::Int(tt)), Some(Value::Str(tz))) = (
        get_str(h, "date").map(Value::deref),
        get_str(h, "timezone_type").map(Value::deref),
        get_str(h, "timezone").map(Value::deref),
    ) else {
        return Ok(false);
    };
    match tt {
        1 | 2 => {
            let mut s = date.as_bytes().to_vec();
            s.push(b' ');
            s.extend_from_slice(tz.as_bytes());
            initialize(vm, o, &s, None, &Ok(None), false)
        }
        3 => {
            let dtz = default_tz(vm)?;
            let (sec, usec) = now();
            let Some(t) = api::date_from_hash(date.as_bytes(), tt, tz.as_bytes(), &dtz, sec, usec)
            else {
                return Ok(false);
            };
            set_time(o, Some(t));
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// `Invalid serialization data for X object`.
pub(crate) fn invalid_serialization(vm: &mut Vm, cls: &str) -> Throw {
    vm.throw(
        "Error",
        &format!("Invalid serialization data for {cls} object"),
    )
}

fn set_state(vm: &mut Vm, a: &Args, kind: Kind) -> VmResult<Value> {
    let h = a.array(0);
    let cls = class(vm, kind.class_name());
    let o = instantiate(vm, &cls)?;
    if !init_from_hash(vm, &o, &h)? {
        return Err(invalid_serialization(vm, kind.class_name()));
    }
    Ok(Value::Object(o))
}

fn dt_set_state(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    set_state(vm, a, Kind::Mutable)
}

fn dti_set_state(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    set_state(vm, a, Kind::Immutable)
}

/// `add_common_properties()`: std properties not already present.
pub(crate) fn add_common_properties(out: &mut ZArray, o: &Obj) {
    for (k, v) in std_props(o).iter() {
        if !out.contains_key(k) {
            out.make_mut().insert(k.clone(), v.clone());
        }
    }
}

fn serialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = this(a);
    let t = require_time(vm, &o)?;
    let mut out = ZArray::new();
    add_date_props(&mut out, &t);
    add_common_properties(&mut out, &o);
    Ok(Value::Array(out))
}

/// Restores the non-internal entries of a serialisation hash as properties.
pub(crate) fn restore_custom(vm: &mut Vm, o: &Obj, h: &ZArray, internal: &[&str]) -> VmResult<()> {
    for (k, v) in h.iter() {
        let ArrayKey::Str(name) = k else { continue };
        if matches!(v, Value::Reference(_))
            || internal.iter().any(|i| name.as_bytes() == i.as_bytes())
        {
            continue;
        }
        restore_property(vm, o, name, v)?;
    }
    Ok(())
}

fn unserialize(vm: &mut Vm, a: &Args, kind: Kind) -> VmResult<Value> {
    let o = this(a);
    let h = a.array(0);
    if !init_from_hash(vm, &o, &h)? {
        return Err(invalid_serialization(vm, kind.class_name()));
    }
    restore_custom(vm, &o, &h, &["date", "timezone_type", "timezone"])?;
    Ok(Value::Null)
}

fn dt_unserialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    unserialize(vm, a, Kind::Mutable)
}

fn dti_unserialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    unserialize(vm, a, Kind::Immutable)
}

/// The deprecation every `__wakeup()` of ext/date emits (PHP 8.5).
pub(crate) fn wakeup_deprecation(vm: &mut Vm, cls: &str) -> VmResult<()> {
    vm.raise(
        crate::common::E_DEPRECATED,
        &format!("Method {cls}::__wakeup() is deprecated since 8.5, this method is obsolete, as serialization hooks are provided by __unserialize() and __serialize()"),
    )
}

fn wakeup(vm: &mut Vm, a: &Args, kind: Kind) -> VmResult<Value> {
    wakeup_deprecation(vm, kind.class_name())?;
    let o = this(a);
    let h = std_props(&o);
    if !init_from_hash(vm, &o, &h)? {
        return Err(invalid_serialization(vm, kind.class_name()));
    }
    Ok(Value::Null)
}

fn dt_wakeup(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    wakeup(vm, a, Kind::Mutable)
}

fn dti_wakeup(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    wakeup(vm, a, Kind::Immutable)
}

// ---------------------------------------------------------------------------
// Getters
// ---------------------------------------------------------------------------

/// The object operand: `$this` for methods, argument 0 for functions, and
/// the index of the next argument.
fn subject(a: &Args) -> (Obj, usize) {
    match &a.this {
        Some(o) => (o.clone(), 0),
        None => (obj_arg(a, 0).unwrap_or_else(|| a.this_obj()), 1),
    }
}

fn format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, n) = subject(a);
    let f = a.str(n);
    let s = with_time(vm, &o, |t| api::format(t, f.as_bytes()))?;
    Ok(Value::Str(ZStr::from(s)))
}

fn get_timezone(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = subject(a);
    let t = require_time(vm, &o)?;
    match api::timezone_of(&t) {
        Some(z) => Ok(Value::Object(new_tz(vm, z)?)),
        None => Ok(Value::Bool(false)),
    }
}

fn get_offset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = subject(a);
    let off = with_time(vm, &o, |t| api::offset(t))?;
    Ok(Value::Int(off))
}

fn get_timestamp(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, _) = subject(a);
    let ts = with_time(vm, &o, api::timestamp)?;
    Ok(Value::Int(ts))
}

fn get_microsecond(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = this(a);
    let t = require_time(vm, &o)?;
    Ok(Value::Int(t.us))
}

fn diff(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, n) = subject(a);
    let Some(other) = obj_arg(a, n) else {
        return Ok(Value::Null);
    };
    let absolute = a.has(n + 1) && a.bool(n + 1);
    with_time(vm, &o, |_| ())?;
    let t2 = require_time(vm, &other)?;
    let iv = with_time(vm, &o, |t1| api::diff(t1, &t2, absolute))?;
    Ok(Value::Object(new_interval(vm, iv)?))
}

// ---------------------------------------------------------------------------
// Mutators
// ---------------------------------------------------------------------------

/// `php_date_modify()`. Returns `false` after a warning in warning mode.
fn op_modify(vm: &mut Vm, o: &Obj, s: &[u8], throw_as: Option<&str>) -> VmResult<bool> {
    if time_of(o).is_none() {
        return Err(uninit_error(vm, object::class_of(o)));
    }
    let r = with_time(vm, o, |t| api::modify(t, s))?;
    match r {
        Ok(e) => {
            update_errors(&e);
            Ok(true)
        }
        Err(e) => {
            update_errors(&e);
            let msg = lossy(&api::parse_failure_message(s, &e));
            if let Some(func) = throw_as {
                return Err(vm.throw("DateMalformedStringException", &format!("{func}(): {msg}")));
            }
            vm.php_warning(&msg)?;
            Ok(false)
        }
    }
}

fn op_add(vm: &mut Vm, o: &Obj, iv: &Obj) -> VmResult<()> {
    require_time(vm, o)?;
    let iv = require_interval(vm, iv)?;
    with_time(vm, o, |t| *t = api::add(t, &iv))
}

fn op_sub(vm: &mut Vm, o: &Obj, iv: &Obj, throw_as: Option<&str>) -> VmResult<()> {
    let t = require_time(vm, o)?;
    let iv = require_interval(vm, iv)?;
    match api::sub(&t, &iv) {
        Ok(n) => {
            set_time(o, Some(n));
            Ok(())
        }
        Err(m) => match throw_as {
            Some(func) => Err(vm.throw("DateInvalidOperationException", &format!("{func}(): {m}"))),
            None => vm.php_warning(m),
        },
    }
}

fn op_set_timezone(vm: &mut Vm, o: &Obj, z: &Obj) -> VmResult<()> {
    require_time(vm, o)?;
    let zone = tz_of(z);
    with_time(vm, o, |t| {
        if let Some(z) = zone {
            api::set_timezone(t, &z);
        } else {
            let sse = t.sse;
            t.unixtime2local(sse);
        }
    })
}

fn instance_of_immutable(o: &Obj) -> bool {
    object::class_of(o).instance_of_name(b"datetimeimmutable")
}

/// Applies `op` to `$this` (`DateTime`) or to a raw clone of it
/// (`DateTimeImmutable`), returning the affected object.
fn apply(vm: &mut Vm, a: &Args, op: impl FnOnce(&mut Vm, &Obj) -> VmResult<()>) -> VmResult<Value> {
    let o = this(a);
    let target = if instance_of_immutable(&o) {
        clone_raw(&o)
    } else {
        o
    };
    op(vm, &target)?;
    Ok(Value::Object(target))
}

fn method_name(o: &Obj, m: &str) -> String {
    let base = if instance_of_immutable(o) {
        "DateTimeImmutable"
    } else {
        "DateTime"
    };
    format!("{base}::{m}")
}

fn modify(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    apply(vm, a, |vm, o| {
        let name = method_name(o, "modify");
        op_modify(vm, o, s.as_bytes(), Some(&name)).map(|_| ())
    })
}

fn date_modify(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(o) = obj_arg(a, 0) else {
        return Ok(Value::Null);
    };
    let s = a.str(1);
    if op_modify(vm, &o, s.as_bytes(), None)? {
        Ok(Value::Object(o))
    } else {
        Ok(Value::Bool(false))
    }
}

fn add(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(iv) = obj_arg(a, 0) else {
        return Ok(Value::Null);
    };
    apply(vm, a, |vm, o| op_add(vm, o, &iv))
}

fn date_add(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (Some(o), Some(iv)) = (obj_arg(a, 0), obj_arg(a, 1)) else {
        return Ok(Value::Null);
    };
    op_add(vm, &o, &iv)?;
    Ok(Value::Object(o))
}

fn sub(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(iv) = obj_arg(a, 0) else {
        return Ok(Value::Null);
    };
    apply(vm, a, |vm, o| {
        let name = method_name(o, "sub");
        op_sub(vm, o, &iv, Some(&name))
    })
}

fn date_sub(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (Some(o), Some(iv)) = (obj_arg(a, 0), obj_arg(a, 1)) else {
        return Ok(Value::Null);
    };
    op_sub(vm, &o, &iv, None)?;
    Ok(Value::Object(o))
}

fn set_timezone(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(z) = obj_arg(a, 0) else {
        return Ok(Value::Null);
    };
    apply(vm, a, |vm, o| op_set_timezone(vm, o, &z))
}

fn date_timezone_set(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (Some(o), Some(z)) = (obj_arg(a, 0), obj_arg(a, 1)) else {
        return Ok(Value::Null);
    };
    op_set_timezone(vm, &o, &z)?;
    Ok(Value::Object(o))
}

fn int_or(a: &Args, n: usize, d: i64) -> i64 {
    if a.has(n) {
        a.int(n)
    } else {
        d
    }
}

fn set_time_m(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (h, i, s, us) = (a.int(0), a.int(1), int_or(a, 2, 0), int_or(a, 3, 0));
    apply(vm, a, |vm, o| {
        with_time(vm, o, |t| api::set_time(t, h, i, s, us))
    })
}

fn date_time_set(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(o) = obj_arg(a, 0) else {
        return Ok(Value::Null);
    };
    let (h, i, s, us) = (a.int(1), a.int(2), int_or(a, 3, 0), int_or(a, 4, 0));
    with_time(vm, &o, |t| api::set_time(t, h, i, s, us))?;
    Ok(Value::Object(o))
}

fn set_date_m(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (y, m, d) = (a.int(0), a.int(1), a.int(2));
    apply(vm, a, |vm, o| {
        with_time(vm, o, |t| api::set_date(t, y, m, d))
    })
}

fn date_date_set(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(o) = obj_arg(a, 0) else {
        return Ok(Value::Null);
    };
    let (y, m, d) = (a.int(1), a.int(2), a.int(3));
    with_time(vm, &o, |t| api::set_date(t, y, m, d))?;
    Ok(Value::Object(o))
}

fn set_isodate_m(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (y, w, d) = (a.int(0), a.int(1), int_or(a, 2, 1));
    apply(vm, a, |vm, o| {
        with_time(vm, o, |t| api::set_isodate(t, y, w, d))
    })
}

fn date_isodate_set(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(o) = obj_arg(a, 0) else {
        return Ok(Value::Null);
    };
    let (y, w, d) = (a.int(1), a.int(2), int_or(a, 3, 1));
    with_time(vm, &o, |t| api::set_isodate(t, y, w, d))?;
    Ok(Value::Object(o))
}

fn set_timestamp_m(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ts = a.int(0);
    apply(vm, a, |vm, o| {
        with_time(vm, o, |t| api::set_timestamp(t, ts))
    })
}

fn date_timestamp_set(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(o) = obj_arg(a, 0) else {
        return Ok(Value::Null);
    };
    let ts = a.int(1);
    with_time(vm, &o, |t| api::set_timestamp(t, ts))?;
    Ok(Value::Object(o))
}

fn set_microsecond(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let us = a.int(0);
    let o = this(a);
    if let Some(m) = api::microsecond_range_error(us) {
        let name = method_name(&o, "setMicrosecond");
        return Err(vm.throw(
            "DateRangeError",
            &format!("{name}(): Argument #1 ($microsecond) {m}"),
        ));
    }
    require_time(vm, &o)?;
    apply(vm, a, |vm, o| with_time(vm, o, |t| t.us = us))
}

// ---------------------------------------------------------------------------
// Object handlers
// ---------------------------------------------------------------------------

fn create_hook(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    object::set_internal(o, DateObj::default());
    Ok(())
}

/// The `clone_obj` handler shared by all date classes.
pub(crate) fn clone_hook(_vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    crate::objects::copy_state(old, new);
    Ok(())
}

/// `date_object_get_properties_for()`.
fn properties_for(_vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    // php-src: only DatePeriod answers ZEND_PROP_PURPOSE_GET_OBJECT_VARS;
    // everything else falls back to `get_properties`.
    if purpose == PropertyPurpose::GetObjectVars {
        return Ok(None);
    }
    if purpose == PropertyPurpose::Debug && object::class_of(o).magic.debug_info.is_some() {
        return Ok(None);
    }
    let mut props = std_props(o);
    if let Some(t) = time_of(o) {
        add_date_props(&mut props, &t);
    }
    Ok(Some(props))
}

/// `date_object_compare_date()`.
fn compare(vm: &mut Vm, a: &Value, b: &Value) -> VmResult<Option<i32>> {
    let (Value::Object(x), Value::Object(y)) = (a, b) else {
        return Ok(None);
    };
    if !object::has_internal::<DateObj>(x) || !object::has_internal::<DateObj>(y) {
        return Ok(None);
    }
    let (Some(mut t1), Some(mut t2)) = (time_of(x), time_of(y)) else {
        return Err(vm.throw("DateObjectError", api::messages::INCOMPLETE_DATE_COMPARE));
    };
    Ok(Some(match api::compare(&mut t1, &mut t2) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }))
}

fn hooks() -> ClassHooks {
    ClassHooks {
        create: Some(create_hook),
        clone: Some(clone_hook),
        properties_for: Some(properties_for),
        compare: Some(compare),
        ..ClassHooks::default()
    }
}

/// `implement_date_interface_handler`: only `DateTime` and
/// `DateTimeImmutable` (and their subclasses) may implement
/// `DateTimeInterface`.
fn implement_date_interface(vm: &mut Vm, cls: &Rc<Class>) -> VmResult<()> {
    if cls.flags & cflags::INTERNAL == 0
        && !cls.instance_of_name(b"datetime")
        && !cls.instance_of_name(b"datetimeimmutable")
    {
        return Err(vm.fatal("DateTimeInterface can't be implemented by user classes"));
    }
    Ok(())
}

/// Registers the interface and both classes.
#[allow(clippy::too_many_lines)]
/// The `DateTimeImmutable` methods declared `#[\NoDiscard]`.
const NODISCARD_METHODS: [&str; 9] = [
    "modify",
    "add",
    "sub",
    "setTimezone",
    "setTime",
    "setDate",
    "setISODate",
    "setTimestamp",
    "setMicrosecond",
];

pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    for (name, v) in FORMATS {
        let cname = format!("DATE_{name}");
        if name == "RFC7231" {
            m.constant_deprecated(
                &cname,
                Value::Str(ZStr::from(v)),
                "8.5",
                "as this format ignores the associated timezone and always uses GMT",
            );
        } else {
            m.constant(&cname, Value::Str(ZStr::from(v)));
        }
    }

    let vm = m.vm();
    let mut iface = ClassBuilder::new("DateTimeInterface").flags(class_flags::INTERFACE);
    for (name, v) in FORMATS {
        iface = if name == "RFC7231" {
            iface.deprecated_constant(
                name,
                Value::Str(ZStr::from(v)),
                "8.5",
                "as this format ignores the associated timezone and always uses GMT",
            )
        } else {
            iface.constant(name, Value::Str(ZStr::from(v)))
        };
    }
    iface
        .hooks(ClassHooks {
            interface_gets_implemented: Some(implement_date_interface),
            ..ClassHooks::default()
        })
        .abstract_method("format(string $format): string", acc::PUBLIC)
        .abstract_method("getTimezone(): DateTimeZone|false", acc::PUBLIC)
        .abstract_method("getOffset(): int", acc::PUBLIC)
        .abstract_method("getTimestamp(): int", acc::PUBLIC)
        .abstract_method("getMicrosecond(): int", acc::PUBLIC)
        .abstract_method(
            "diff(DateTimeInterface $targetObject, bool $absolute = false): DateInterval",
            acc::PUBLIC,
        )
        .abstract_method("__wakeup(): void", acc::PUBLIC)
        .abstract_method("__serialize(): array", acc::PUBLIC)
        .abstract_method("__unserialize(array $data): void", acc::PUBLIC)
        .register(vm);

    let p = acc::PUBLIC;
    let ps = acc::PUBLIC | acc::STATIC;
    ClassBuilder::new("DateTime")
        .implements(vm, &["DateTimeInterface"])
        .method(
            "__construct(string $datetime = \"now\", ?DateTimeZone $timezone = null)",
            p,
            construct,
        )
        .method("__serialize(): array", p, serialize)
        .method("__unserialize(array $data): void", p, dt_unserialize)
        .method("__wakeup(): void", p, dt_wakeup)
        .method("__set_state(array $array): DateTime", ps, dt_set_state)
        .method(
            "createFromImmutable(DateTimeImmutable $object): static",
            ps,
            dt_create_from_other,
        )
        .method(
            "createFromInterface(DateTimeInterface $object): static",
            ps,
            dt_create_from_other,
        )
        .method(
            "createFromFormat(string $format, string $datetime, ?DateTimeZone $timezone = null): DateTime|false",
            ps,
            dt_create_from_format,
        )
        .method(
            "createFromTimestamp(int|float $timestamp): static",
            ps,
            dt_create_from_timestamp,
        )
        .method("getLastErrors(): array|false", ps, date_get_last_errors)
        .method("format(string $format): string", p, format)
        .method("modify(string $modifier): DateTime", p, modify)
        .method("add(DateInterval $interval): DateTime", p, add)
        .method("sub(DateInterval $interval): DateTime", p, sub)
        .method("getTimezone(): DateTimeZone|false", p, get_timezone)
        .method("setTimezone(DateTimeZone $timezone): DateTime", p, set_timezone)
        .method("getOffset(): int", p, get_offset)
        .method("getMicrosecond(): int", p, get_microsecond)
        .method(
            "setTime(int $hour, int $minute, int $second = 0, int $microsecond = 0): DateTime",
            p,
            set_time_m,
        )
        .method("setDate(int $year, int $month, int $day): DateTime", p, set_date_m)
        .method(
            "setISODate(int $year, int $week, int $dayOfWeek = 1): DateTime",
            p,
            set_isodate_m,
        )
        .method("setTimestamp(int $timestamp): DateTime", p, set_timestamp_m)
        .method("setMicrosecond(int $microsecond): static", p, set_microsecond)
        .method("getTimestamp(): int", p, get_timestamp)
        .method(
            "diff(DateTimeInterface $targetObject, bool $absolute = false): DateInterval",
            p,
            diff,
        )
        .hooks(hooks())
        .register(vm);

    // `#[\NoDiscard(message: "as DateTimeImmutable::X() does not modify
    // the object itself")]` on the withers (php_date.stub.php).
    let nd = p | flags::NODISCARD;
    for n in NODISCARD_METHODS {
        vm.builtin_attributes.insert(
            ZStr::from(format!("datetimeimmutable::{}", n.to_ascii_lowercase())),
            Rc::new(vec![AttributeInfo {
                name: ZStr::from("NoDiscard"),
                args: vec![(
                    Some(ZStr::from("message")),
                    ConstExpr::Value(Value::Str(ZStr::from(format!(
                        "as DateTimeImmutable::{n}() does not modify the object itself"
                    )))),
                )],
                line: 0,
                strict: false,
                source: None,
                class_cache: php_vm::class::AttrClassCache::default(),
            }]),
        );
    }
    ClassBuilder::new("DateTimeImmutable")
        .implements(vm, &["DateTimeInterface"])
        .method(
            "__construct(string $datetime = \"now\", ?DateTimeZone $timezone = null)",
            p,
            construct,
        )
        .method("__serialize(): array", p, serialize)
        .method("__unserialize(array $data): void", p, dti_unserialize)
        .method("__wakeup(): void", p, dti_wakeup)
        .method("__set_state(array $array): static", ps, dti_set_state)
        .method(
            "createFromFormat(string $format, string $datetime, ?DateTimeZone $timezone = null): DateTimeImmutable|false",
            ps,
            dti_create_from_format,
        )
        .method(
            "createFromTimestamp(int|float $timestamp): static",
            ps,
            dti_create_from_timestamp,
        )
        .method("getLastErrors(): array|false", ps, date_get_last_errors)
        .method("format(string $format): string", p, format)
        .method("getTimezone(): DateTimeZone|false", p, get_timezone)
        .method("getOffset(): int", p, get_offset)
        .method("getTimestamp(): int", p, get_timestamp)
        .method("getMicrosecond(): int", p, get_microsecond)
        .method(
            "diff(DateTimeInterface $targetObject, bool $absolute = false): DateInterval",
            p,
            diff,
        )
        .method("modify(string $modifier): static", nd, modify)
        .method("add(DateInterval $interval): static", nd, add)
        .method("sub(DateInterval $interval): static", nd, sub)
        .method("setTimezone(DateTimeZone $timezone): static", nd, set_timezone)
        .method(
            "setTime(int $hour, int $minute, int $second = 0, int $microsecond = 0): static",
            nd,
            set_time_m,
        )
        .method("setDate(int $year, int $month, int $day): static", nd, set_date_m)
        .method(
            "setISODate(int $year, int $week, int $dayOfWeek = 1): static",
            nd,
            set_isodate_m,
        )
        .method("setTimestamp(int $timestamp): static", nd, set_timestamp_m)
        .method("setMicrosecond(int $microsecond): static", nd, set_microsecond)
        .method("createFromMutable(DateTime $object): static", ps, dti_create_from_other)
        .method(
            "createFromInterface(DateTimeInterface $object): static",
            ps,
            dti_create_from_other,
        )
        .hooks(hooks())
        .register(vm);
}

/// Registers the procedural `date_*()` functions over date objects.
pub(crate) fn register_functions(m: &mut ModuleBuilder<'_>) {
    m.function_static(
        "date_create(string $datetime = \"now\", ?DateTimeZone $timezone = null): DateTime|false",
        date_create,
    )
    .function_static(
        "date_create_immutable(string $datetime = \"now\", ?DateTimeZone $timezone = null): DateTimeImmutable|false",
        date_create_immutable,
    )
    .function_static(
        "date_create_from_format(string $format, string $datetime, ?DateTimeZone $timezone = null): DateTime|false",
        date_create_from_format,
    )
    .function_static(
        "date_create_immutable_from_format(string $format, string $datetime, ?DateTimeZone $timezone = null): DateTimeImmutable|false",
        date_create_immutable_from_format,
    )
    .function_static("date_format(DateTimeInterface $object, string $format): string", format)
    .function_static("date_modify(DateTime $object, string $modifier): DateTime|false", date_modify)
    .function_static("date_add(DateTime $object, DateInterval $interval): DateTime", date_add)
    .function_static("date_sub(DateTime $object, DateInterval $interval): DateTime", date_sub)
    .function_static("date_timezone_get(DateTimeInterface $object): DateTimeZone|false", get_timezone)
    .function_static(
        "date_timezone_set(DateTime $object, DateTimeZone $timezone): DateTime",
        date_timezone_set,
    )
    .function_static("date_offset_get(DateTimeInterface $object): int", get_offset)
    .function_static(
        "date_diff(DateTimeInterface $baseObject, DateTimeInterface $targetObject, bool $absolute = false): DateInterval",
        diff,
    )
    .function_static(
        "date_time_set(DateTime $object, int $hour, int $minute, int $second = 0, int $microsecond = 0): DateTime",
        date_time_set,
    )
    .function_static(
        "date_date_set(DateTime $object, int $year, int $month, int $day): DateTime",
        date_date_set,
    )
    .function_static(
        "date_isodate_set(DateTime $object, int $year, int $week, int $dayOfWeek = 1): DateTime",
        date_isodate_set,
    )
    .function_static("date_timestamp_set(DateTime $object, int $timestamp): DateTime", date_timestamp_set)
    .function_static("date_timestamp_get(DateTimeInterface $object): int", get_timestamp);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_constants() {
        assert_eq!(FORMATS.len(), 14);
        assert!(FORMATS
            .iter()
            .any(|(n, f)| *n == "ATOM" && *f == "Y-m-d\\TH:i:sP"));
    }
}
