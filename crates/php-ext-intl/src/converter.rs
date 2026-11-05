//! `UConverter`.

use std::cell::RefCell;

use php_intl::converter::{self as lib, CallbackValue, PhpCallbacks, UConverter};
use php_intl::{IntlError, UErrorCode};
use php_runtime::{Obj, PhpRef, Value, ZArray, ZStr};
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core::{self, s};
use crate::locale::text;
use crate::vmbridge;

thread_local! {
    /// The default callback result of the converter currently converting
    /// (its object payload is taken out while ICU runs), innermost last.
    static DEFAULT_CB: RefCell<Vec<(Obj, DefaultResult)>> = const { RefCell::new(Vec::new()) };
}

/// What `parent::toUCallback()` returns: the value and the new `$error`.
type DefaultResult = (CallbackValue, Option<i64>);

/// The PHP side: calls the object's `toUCallback`/`fromUCallback`.
struct PhpCb(Obj);

impl std::fmt::Debug for PhpCb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PhpCb")
    }
}

fn to_cb_value(v: &Value) -> CallbackValue {
    match v.deref() {
        Value::Null => CallbackValue::Null,
        Value::Int(i) => CallbackValue::Int(i),
        Value::Str(z) => CallbackValue::Str(z.as_bytes().to_vec()),
        Value::Array(arr) => CallbackValue::Array(arr.values().map(to_cb_value).collect()),
        _ => CallbackValue::Other,
    }
}

impl PhpCb {
    fn call(
        &self,
        method: &str,
        args: Vec<Value>,
        err_ref: &PhpRef,
        error: &mut i64,
    ) -> CallbackValue {
        if vmbridge::has_pending() {
            return CallbackValue::Undef;
        }
        let r = vmbridge::with_vm(|vm| vm.call_method_by_name(&self.0, method, args));
        if let Value::Int(e) = err_ref.get() {
            *error = e;
        } else if let Some(Ok(_)) = &r {
            *error =
                vmbridge::with_vm(|vm| vm.value_to_int(&err_ref.get()).unwrap_or(0)).unwrap_or(0);
        }
        match r {
            Some(Ok(v)) => to_cb_value(&v),
            Some(Err(t)) => {
                vmbridge::set_pending(t);
                CallbackValue::Undef
            }
            None => CallbackValue::Undef,
        }
    }
}

impl PhpCallbacks for PhpCb {
    fn to_u_callback(
        &mut self,
        reason: i64,
        source: &[u8],
        code_units: &[u8],
        error: &mut i64,
    ) -> CallbackValue {
        let err = PhpRef::new(Value::Int(*error));
        let args = vec![
            Value::Int(reason),
            Value::Str(ZStr::from(source)),
            Value::Str(ZStr::from(code_units)),
            Value::Reference(err.clone()),
        ];
        self.call("toUCallback", args, &err, error)
    }

    fn from_u_callback(
        &mut self,
        reason: i64,
        source: &[i64],
        code_point: i64,
        error: &mut i64,
    ) -> CallbackValue {
        let err = PhpRef::new(Value::Int(*error));
        let args = vec![
            Value::Int(reason),
            Value::Array(ZArray::from_values(source.iter().map(|&c| Value::Int(c)))),
            Value::Int(code_point),
            Value::Reference(err.clone()),
        ];
        self.call("fromUCallback", args, &err, error)
    }
}

/// The `create_object` hook: the converter exists before the constructor.
pub(crate) fn create(vm: &mut Vm, o: &Obj) -> VmResult<()> {
    core::create(vm, o)?;
    let subclass = php_vm::object::class_of(o).lc_name.as_bytes() != b"uconverter";
    let cb: Option<Box<dyn PhpCallbacks>> = if subclass {
        Some(Box::new(PhpCb(o.clone())))
    } else {
        None
    };
    core::set_data(o, UConverter::new_empty(cb));
    Ok(())
}

/// Records the converter's logged errors on `o` (`intl_errors_set`).
fn flush(vm: &mut Vm, o: Option<&Obj>, errs: Vec<IntlError>) -> VmResult<()> {
    for e in errs {
        core::record(vm, o, &e)?;
    }
    Ok(())
}

fn take_errors(o: &Obj) -> Vec<IntlError> {
    core::with::<UConverter, _>(o, |c| c.take_errors()).unwrap_or_default()
}

fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    core::reset_global();
    let dest = a.opt(0).map(|v| {
        if let Value::Str(z) = v {
            text(&z)
        } else {
            String::new()
        }
    });
    let src = a.opt(1).map(|v| {
        if let Value::Str(z) = v {
            text(&z)
        } else {
            String::new()
        }
    });
    let r = core::with::<UConverter, _>(&o, |c| c.construct(dest.as_deref(), src.as_deref()));
    let _g = core::CtorGuard::new();
    match r {
        Some(Err(errs)) => flush(vm, Some(&o), errs)?,
        _ => flush(vm, Some(&o), take_errors(&o))?,
    }
    Ok(Value::Null)
}

fn reset_obj(o: &Obj) {
    core::with_obj_error(o, php_intl::ErrorState::reset);
    core::reset_global();
}

fn set_encoding(vm: &mut Vm, a: &Args, dest: bool) -> VmResult<Value> {
    let o = a.this_obj();
    reset_obj(&o);
    let enc = text(&a.str(0));
    let ok = core::with::<UConverter, _>(&o, |c| {
        if dest {
            c.set_destination_encoding(&enc)
        } else {
            c.set_source_encoding(&enc)
        }
    })
    .unwrap_or(false);
    flush(vm, Some(&o), take_errors(&o))?;
    Ok(Value::Bool(ok))
}

fn set_source_encoding(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    set_encoding(vm, a, false)
}

fn set_destination_encoding(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    set_encoding(vm, a, true)
}

fn get_encoding(vm: &mut Vm, a: &Args, dest: bool) -> VmResult<Value> {
    let o = a.this_obj();
    reset_obj(&o);
    let r = core::with::<UConverter, _>(&o, |c| {
        if dest {
            c.destination_encoding()
        } else {
            c.source_encoding()
        }
    })
    .flatten();
    flush(vm, Some(&o), take_errors(&o))?;
    Ok(match r {
        None => Value::Null,
        Some(Ok(n)) => s(n),
        Some(Err(())) => Value::Bool(false),
    })
}

fn get_source_encoding(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    get_encoding(vm, a, false)
}

fn get_destination_encoding(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    get_encoding(vm, a, true)
}

fn get_type(a: &Args, dest: bool) -> Value {
    let o = a.this_obj();
    reset_obj(&o);
    core::with::<UConverter, _>(&o, |c| {
        if dest {
            c.destination_type()
        } else {
            c.source_type()
        }
    })
    .flatten()
    .map_or(Value::Null, Value::Int)
}

fn get_source_type(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(get_type(a, false))
}

fn get_destination_type(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(get_type(a, true))
}

fn set_subst_chars(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    reset_obj(&o);
    let chars = a.str(0);
    let ok =
        core::with::<UConverter, _>(&o, |c| c.set_subst_chars(chars.as_bytes())).unwrap_or(false);
    flush(vm, Some(&o), take_errors(&o))?;
    Ok(Value::Bool(ok))
}

fn get_subst_chars(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    reset_obj(&o);
    let r = core::with::<UConverter, _>(&o, |c| c.subst_chars()).flatten();
    flush(vm, Some(&o), take_errors(&o))?;
    Ok(match r {
        None => Value::Null,
        Some(Ok(v)) => Value::Str(ZStr::from(v)),
        Some(Err(())) => Value::Bool(false),
    })
}

fn cb_value(v: CallbackValue) -> Value {
    match v {
        CallbackValue::Undef | CallbackValue::Null | CallbackValue::Other => Value::Null,
        CallbackValue::Int(i) => Value::Int(i),
        CallbackValue::Str(b) => Value::Str(ZStr::from(b)),
        CallbackValue::Array(items) => {
            Value::Array(ZArray::from_values(items.into_iter().map(cb_value)))
        }
    }
}

/// `toUCallback`/`fromUCallback` default implementation.
fn default_callback(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let reason = a.int(0);
    let r = match core::with::<UConverter, _>(&o, |c| c.default_callback(reason)) {
        Some(r) => r,
        None => DEFAULT_CB
            .with(|d| {
                d.borrow()
                    .iter()
                    .rev()
                    .find(|(x, _)| x == &o)
                    .map(|(_, r)| r.clone())
            })
            .unwrap_or((CallbackValue::Null, None)),
    };
    flush(vm, Some(&o), take_errors(&o))?;
    if let Some(e) = r.1 {
        a.set_ref(3, Value::Int(e));
    }
    Ok(cb_value(r.0))
}

fn convert(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    reset_obj(&o);
    let st = a.str(0);
    let reverse = a.bool(1);
    // The intermediate UTF-16 buffer.
    if !core::reserve(2 * st.len()) {
        return Ok(Value::Bool(false));
    }
    // The payload leaves the object while ICU (and PHP callbacks) run.
    let Some(mut boxed) = core::take_data(&o) else {
        return Ok(Value::Bool(false));
    };
    let Some(cnv) = boxed.downcast_mut::<UConverter>() else {
        core::set_boxed(&o, boxed);
        return Ok(Value::Bool(false));
    };
    let dflt = cnv.default_callback(lib::reason::UNASSIGNED);
    let _ = cnv.take_errors();
    DEFAULT_CB.with(|d| d.borrow_mut().push((o.clone(), dflt)));
    let r = vmbridge::enter(vm, || cnv.convert(st.as_bytes(), reverse));
    DEFAULT_CB.with(|d| {
        d.borrow_mut().pop();
    });
    let errs = cnv.take_errors();
    core::set_boxed(&o, boxed);
    let r = r?;
    if r.as_ref().is_some_and(|v| !core::reserve(v.len())) {
        return Ok(Value::Bool(false));
    }
    flush(vm, Some(&o), errs)?;
    Ok(r.map_or(Value::Bool(false), |v| Value::Str(ZStr::from(v))))
}

fn reason_text(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match lib::reason_text(a.int(0)) {
        Ok(t) => Ok(s(t)),
        Err(e) => Err(core::throw_engine(vm, a, e, 0)),
    }
}

fn transcode(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let opts = a.opt(3);
    let get = |k: &str| -> Option<ZStr> {
        match &opts {
            Some(Value::Array(arr)) => match arr.get_str(k.as_bytes()).map(Value::deref) {
                Some(Value::Str(z)) => Some(z),
                _ => None,
            },
            _ => None,
        }
    };
    let (fs, ts) = (get("from_subst"), get("to_subst"));
    if !core::reserve(2 * a.str(0).len()) {
        return Ok(Value::Bool(false));
    }
    let r = lib::transcode(
        a.str(0).as_bytes(),
        &text(&a.str(1)),
        &text(&a.str(2)),
        fs.as_ref().map(ZStr::as_bytes),
        ts.as_ref().map(ZStr::as_bytes),
    );
    match r {
        Ok((v, notes)) => {
            if !core::reserve(v.len()) {
                return Ok(Value::Bool(false));
            }
            flush(vm, None, notes)?;
            Ok(Value::Str(ZStr::from(v)))
        }
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

fn get_error_message(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    Ok(s(core::obj_error(&o).get_message()))
}

fn get_available(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    Ok(Value::Array(ZArray::from_values(
        lib::get_available().into_iter().map(s),
    )))
}

fn get_aliases(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match lib::get_aliases(&text(&a.str(0))) {
        Ok(v) => Ok(Value::Array(ZArray::from_values(v.into_iter().map(s)))),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

fn get_standards(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match lib::get_standards() {
        Ok(v) => Ok(Value::Array(ZArray::from_values(v.into_iter().map(s)))),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Null),
    }
}

/// `php_converter_free_object`: closing the ICU converters (source, then
/// destination) fires a subclass's callbacks with `REASON_CLOSE`. The
/// payload holds the object itself (the callback target), so this also
/// breaks that cycle.
pub(crate) fn destruct(vm: &mut Vm, o: &Obj) -> VmResult<()> {
    let Some(payload) = core::take_data(o) else {
        return Ok(());
    };
    vmbridge::enter(vm, || drop(payload))
}

/// `clone_obj`: a fresh converter with the same encodings.
pub(crate) fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    create(vm, new)?;
    let enc = core::with::<UConverter, _>(old, |c| {
        (
            c.source_encoding(),
            c.destination_encoding(),
            c.subst_chars(),
        )
    });
    if let Some((src, dest, subst)) = enc {
        core::with::<UConverter, _>(new, |c| {
            if let Some(Ok(sname)) = src {
                c.set_source_encoding(&sname);
            }
            if let Some(Ok(dname)) = dest {
                c.set_destination_encoding(&dname);
            }
            if let Some(Ok(sub)) = subst {
                c.set_subst_chars(&sub);
            }
            let _ = c.take_errors();
        });
    }
    let _ = UErrorCode::ZERO;
    Ok(())
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("UConverter::__construct", construct),
        ("UConverter::convert", convert),
        ("UConverter::fromUCallback", default_callback),
        ("UConverter::toUCallback", default_callback),
        ("UConverter::getAliases", get_aliases),
        ("UConverter::getAvailable", get_available),
        (
            "UConverter::getDestinationEncoding",
            get_destination_encoding,
        ),
        ("UConverter::getDestinationType", get_destination_type),
        ("UConverter::getErrorCode", core::get_error_code),
        ("UConverter::getErrorMessage", get_error_message),
        ("UConverter::getSourceEncoding", get_source_encoding),
        ("UConverter::getSourceType", get_source_type),
        ("UConverter::getStandards", get_standards),
        ("UConverter::getSubstChars", get_subst_chars),
        ("UConverter::reasonText", reason_text),
        (
            "UConverter::setDestinationEncoding",
            set_destination_encoding,
        ),
        ("UConverter::setSourceEncoding", set_source_encoding),
        ("UConverter::setSubstChars", set_subst_chars),
        ("UConverter::transcode", transcode),
    ]
}
