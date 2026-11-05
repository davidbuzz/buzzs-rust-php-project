//! `IntlChar` (all static) and `IntlDatePatternGenerator`.

use php_intl::datepatterngenerator::{self as dtpg, IntlDatePatternGenerator};
use php_intl::intlchar::{self as lib, CharOrInt, CodePoint};
use php_runtime::{Obj, Value, ZArray, ZStr};
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core::{self, s};
use crate::locale::text;
use crate::stubs;

/// An owned `int|string` code point argument.
enum Cp {
    Int(i64),
    Str(ZStr),
}

impl Cp {
    fn of(v: &Value) -> Self {
        match v {
            Value::Str(z) => Self::Str(z.clone()),
            Value::Int(i) => Self::Int(*i),
            Value::Bool(b) => Self::Int(i64::from(*b)),
            #[allow(clippy::cast_possible_truncation)]
            Value::Float(f) => Self::Int(*f as i64),
            _ => Self::Int(0),
        }
    }

    fn view(&self) -> CodePoint<'_> {
        match self {
            Self::Int(i) => CodePoint::Int(*i),
            Self::Str(z) => CodePoint::Str(z.as_bytes()),
        }
    }
}

fn finish<T>(
    vm: &mut Vm,
    a: &Args,
    r: php_intl::Result<T>,
    ok: impl FnOnce(T) -> Value,
    fail: Value,
) -> VmResult<Value> {
    match r {
        Ok(v) => Ok(ok(v)),
        Err(e) => core::fail_with(vm, a, None, e, 0, fail),
    }
}

fn char_or_int(c: CharOrInt) -> Value {
    match c {
        CharOrInt::Str(b) => Value::Str(ZStr::from(b)),
        CharOrInt::Int(i) => Value::Int(i),
    }
}

fn method_name(a: &Args) -> String {
    a.func_name()
}

fn bool_method(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cp = Cp::of(&a.at(0));
    let r = lib::bool_method(&method_name(a), cp.view());
    finish(vm, a, r, Value::Bool, Value::Null)
}

fn int_method(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cp = Cp::of(&a.at(0));
    let r = lib::int_method(&method_name(a), cp.view());
    finish(vm, a, r, Value::Int, Value::Null)
}

fn char_method(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cp = Cp::of(&a.at(0));
    let r = lib::char_method(&method_name(a), cp.view());
    finish(vm, a, r, char_or_int, Value::Null)
}

fn chr(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cp = Cp::of(&a.at(0));
    let r = lib::chr(cp.view());
    finish(vm, a, r, |b| Value::Str(ZStr::from(b)), Value::Null)
}

fn ord(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cp = Cp::of(&a.at(0));
    let r = lib::ord(cp.view());
    finish(vm, a, r, Value::Int, Value::Null)
}

fn has_binary_property(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cp = Cp::of(&a.at(0));
    let r = lib::has_binary_property(cp.view(), a.int(1));
    finish(vm, a, r, Value::Bool, Value::Null)
}

fn get_int_property_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cp = Cp::of(&a.at(0));
    let r = lib::get_int_property_value(cp.view(), a.int(1));
    finish(vm, a, r, Value::Int, Value::Null)
}

fn get_int_property_min_value(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    Ok(Value::Int(lib::get_int_property_min_value(a.int(0))))
}

fn get_int_property_max_value(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    Ok(Value::Int(lib::get_int_property_max_value(a.int(0))))
}

fn get_numeric_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cp = Cp::of(&a.at(0));
    let r = lib::get_numeric_value(cp.view());
    finish(vm, a, r, Value::Float, Value::Null)
}

fn fold_case(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cp = Cp::of(&a.at(0));
    let r = lib::fold_case(cp.view(), a.int(1));
    finish(vm, a, r, char_or_int, Value::Null)
}

fn digit(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cp = Cp::of(&a.at(0));
    let r = lib::digit(cp.view(), a.int(1));
    let fail = match &r {
        Err(php_intl::Error::Intl(e)) if e.message.as_deref() == Some("Invalid digit") => {
            Value::Bool(false)
        }
        _ => Value::Null,
    };
    finish(vm, a, r, Value::Int, fail)
}

fn for_digit(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    Ok(Value::Int(lib::for_digit(a.int(0), a.int(1))))
}

fn version_array(v: [i64; 4]) -> Value {
    Value::Array(ZArray::from_values(v.into_iter().map(Value::Int)))
}

fn char_age(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cp = Cp::of(&a.at(0));
    let r = lib::char_age(cp.view());
    finish(vm, a, r, version_array, Value::Null)
}

fn get_unicode_version(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    Ok(version_array(lib::get_unicode_version()))
}

fn get_fc_nfkc_closure(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cp = Cp::of(&a.at(0));
    let r = lib::get_fc_nfkc_closure(cp.view());
    let fail = if lib::convert_cp(cp.view()).is_err() {
        Value::Null
    } else {
        Value::Bool(false)
    };
    finish(vm, a, r, s, fail)
}

fn char_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cp = Cp::of(&a.at(0));
    let r = lib::char_name(cp.view(), a.int(1));
    finish(vm, a, r, |b| Value::Str(ZStr::from(b)), Value::Null)
}

fn char_from_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = lib::char_from_name(a.str(0).as_bytes(), a.int(1));
    finish(vm, a, r, Value::Int, Value::Null)
}

/// `Z_PARAM_FUNC`: the callback is resolved once, in the caller's scope
/// (`"self::m"` raises its deprecation once and reaches private methods).
fn callback(vm: &mut Vm, cb: &Value) -> VmResult<Value> {
    Ok(vm
        .resolve_callable_value(cb)?
        .unwrap_or_else(|_| cb.clone()))
}

fn enum_char_types(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let cb = callback(vm, &a.at(0))?;
    let mut err = None;
    lib::enum_char_types(&mut |start, limit, ty| match vm.call_value(
        &cb,
        vec![Value::Int(start), Value::Int(limit), Value::Int(ty)],
    ) {
        Ok(_) => true,
        Err(t) => {
            err = Some(t);
            false
        }
    });
    match err {
        Some(t) => Err(t),
        None => Ok(Value::Null),
    }
}

fn enum_char_names(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let (start, end) = (Cp::of(&a.at(0)), Cp::of(&a.at(1)));
    let cb = callback(vm, &a.at(2))?;
    let mut err = None;
    let r = lib::enum_char_names(
        start.view(),
        end.view(),
        a.int(3),
        &mut |c, n, name| match vm.call_value(
            &cb,
            vec![Value::Int(c), Value::Int(n), Value::Str(ZStr::from(name))],
        ) {
            Ok(_) => true,
            Err(t) => {
                err = Some(t);
                false
            }
        },
    );
    if let Some(t) = err {
        return Err(t);
    }
    finish(vm, a, r, |()| Value::Bool(true), Value::Bool(false))
}

fn get_property_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = lib::get_property_name(a.int(0), a.int(1));
    finish(vm, a, r, s, Value::Bool(false))
}

fn get_property_enum(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    Ok(Value::Int(lib::get_property_enum(a.str(0).as_bytes())))
}

fn get_property_value_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = lib::get_property_value_name(a.int(0), a.int(1), a.int(2));
    finish(vm, a, r, s, Value::Bool(false))
}

fn get_property_value_enum(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    Ok(Value::Int(lib::get_property_value_enum(
        a.int(0),
        a.str(1).as_bytes(),
    )))
}

// ------------------------------------------------- IntlDatePatternGenerator

fn dtpg_ctor(vm: &mut Vm, a: &Args, o: &Obj) -> VmResult<bool> {
    core::reset_global();
    if core::is_constructed::<IntlDatePatternGenerator>(o) {
        core::errors_set(
            vm,
            o,
            php_intl::UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "Cannot call constructor twice",
        )?;
        return Ok(false);
    }
    let loc = a.opt(0).map(|v| {
        if let Value::Str(z) = v {
            text(&z)
        } else {
            String::new()
        }
    });
    match IntlDatePatternGenerator::new(loc.as_deref()) {
        Ok(g) => {
            core::set_data(o, g);
            Ok(true)
        }
        Err(e) => {
            core::fail(vm, a, Some(o), e, 0)?;
            Ok(false)
        }
    }
}

fn dtpg_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    core::in_ctor(|| dtpg_ctor(vm, a, &o))?;
    Ok(Value::Null)
}

fn dtpg_create(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = core::instantiate(vm, "IntlDatePatternGenerator")?;
    if dtpg_ctor(vm, a, &o)? {
        Ok(Value::Object(o))
    } else {
        Ok(Value::Null)
    }
}

fn dtpg_best(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    core::reset_both(&o);
    let sk = a.str(0);
    match core::with::<IntlDatePatternGenerator, _>(&o, |g| g.best_pattern(sk.as_bytes())) {
        Some(Ok(p)) => Ok(s(p)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, 0, Value::Bool(false)),
        None => Err(core::unconstructed(vm, "IntlDatePatternGenerator")),
    }
}

fn dtpg_skeleton(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = dtpg::skeleton(a.str(0).as_bytes());
    finish(vm, a, r, s, Value::Bool(false))
}

fn dtpg_base_skeleton(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let r = dtpg::base_skeleton(a.str(0).as_bytes());
    finish(vm, a, r, s, Value::Bool(false))
}

/// `clone_obj`.
pub(crate) fn dtpg_clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    core::create(vm, new)?;
    match core::with::<IntlDatePatternGenerator, _>(old, |g| g.try_clone()) {
        Some(Ok(g)) => core::set_data(new, g),
        Some(Err(_)) => return Err(vm.throw("Error", "Failed to clone IntlDatePatternGenerator")),
        None => {
            return Err(vm.throw(
                "Error",
                "Cannot clone uninitialized IntlDatePatternGenerator",
            ))
        }
    }
    Ok(())
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    let mut v: Vec<(&'static str, NativeFn)> = vec![
        ("IntlChar::hasBinaryProperty", has_binary_property),
        ("IntlChar::charAge", char_age),
        ("IntlChar::charFromName", char_from_name),
        ("IntlChar::charName", char_name),
        ("IntlChar::chr", chr),
        ("IntlChar::digit", digit),
        ("IntlChar::enumCharNames", enum_char_names),
        ("IntlChar::enumCharTypes", enum_char_types),
        ("IntlChar::foldCase", fold_case),
        ("IntlChar::forDigit", for_digit),
        ("IntlChar::getFC_NFKC_Closure", get_fc_nfkc_closure),
        (
            "IntlChar::getIntPropertyMaxValue",
            get_int_property_max_value,
        ),
        (
            "IntlChar::getIntPropertyMinValue",
            get_int_property_min_value,
        ),
        ("IntlChar::getIntPropertyValue", get_int_property_value),
        ("IntlChar::getNumericValue", get_numeric_value),
        ("IntlChar::getPropertyEnum", get_property_enum),
        ("IntlChar::getPropertyName", get_property_name),
        ("IntlChar::getPropertyValueEnum", get_property_value_enum),
        ("IntlChar::getPropertyValueName", get_property_value_name),
        ("IntlChar::getUnicodeVersion", get_unicode_version),
        ("IntlChar::ord", ord),
        ("IntlDatePatternGenerator::__construct", dtpg_construct),
        ("IntlDatePatternGenerator::create", dtpg_create),
        ("IntlDatePatternGenerator::getBestPattern", dtpg_best),
        ("IntlDatePatternGenerator::getSkeleton", dtpg_skeleton),
        (
            "IntlDatePatternGenerator::getBaseSkeleton",
            dtpg_base_skeleton,
        ),
    ];
    // The generic predicates / int / mapping methods, by name.
    for c in stubs::CLASSES.iter().filter(|c| c.name == "IntlChar") {
        for m in c.methods {
            if v.iter().any(|(k, _)| *k == m.key) {
                continue;
            }
            let name = m.sig.split('(').next().unwrap_or_default();
            let h: NativeFn = match name {
                "charDirection" | "charType" | "getCombiningClass" | "charDigitValue"
                | "getBlockCode" => int_method,
                "charMirror" | "tolower" | "toupper" | "totitle" | "getBidiPairedBracket" => {
                    char_method
                }
                _ => bool_method,
            };
            v.push((m.key, h));
        }
    }
    v
}
