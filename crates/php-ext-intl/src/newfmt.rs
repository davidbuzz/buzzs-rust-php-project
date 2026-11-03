//! The ICU "new API" formatters: `IntlListFormatter`,
//! `IntlNumberFormatter`, `IntlNumberRangeFormatter` and
//! `IntlRelativeDateTimeFormatter`.

use php_intl::listformatter::IntlListFormatter;
use php_intl::numberformatter::{FormatValue, IntlNumberFormatter};
use php_intl::numfmt::NumberFormatter;
use php_intl::rangeformatter::{IntlNumberRangeFormatter, RangeValue};
use php_intl::reldateformatter::{Capitalization, IntlRelativeDateTimeFormatter, Style, Unit};
use php_runtime::{Obj, Value, ZStr};
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core::{self, s};
use crate::locale::text;

fn bytes(v: Vec<u8>) -> Value {
    Value::Str(ZStr::from(v))
}

// ----------------------------------------------------------- IntlListFormatter

fn list_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    core::reset_both(&o);
    if core::is_constructed::<IntlListFormatter>(&o) {
        return Err(vm.throw("Error", php_intl::listformatter::ALREADY_CONSTRUCTED));
    }
    let loc = String::from_utf8_lossy(a.str(0).as_bytes()).into_owned();
    match IntlListFormatter::new(&loc, a.int(1), a.int(2)) {
        Ok(f) => {
            core::set_data(&o, f);
            Ok(Value::Null)
        }
        Err(php_intl::Error::IntlException(m)) => {
            core::with_global(|g| g.code = php_intl::UErrorCode::ILLEGAL_ARGUMENT_ERROR);
            Err(vm.throw("IntlException", &m))
        }
        Err(e) => Err(core::throw_engine(vm, a, e, 0)),
    }
}

fn list_format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    core::with_obj_error(&o, php_intl::ErrorState::reset);
    core::reset_global();
    let arr = a.array(0);
    if arr.is_empty() {
        return Ok(s(""));
    }
    let mut items = Vec::with_capacity(arr.len());
    for v in arr.values() {
        items.push(vm.value_to_string(&v.deref())?);
    }
    let r = core::with::<IntlListFormatter, _>(&o, |f| {
        f.format(&items.iter().map(ZStr::as_bytes).collect::<Vec<_>>())
    });
    match r {
        Some(Ok(v)) => Ok(bytes(v)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, 0, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

// --------------------------------------------------------- IntlNumberFormatter

fn nf_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let name = core::active_function(vm).map_or_else(
        || "IntlNumberFormatter".to_owned(),
        |f| f.split("::").next().unwrap_or_default().to_owned(),
    );
    Err(vm.throw(
        "Error",
        &format!("Cannot directly construct {name}, use createFromSkeleton method instead"),
    ))
}

fn nf_create(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let loc = text(&a.str(1));
    match IntlNumberFormatter::create_from_skeleton(a.str(0).as_bytes(), &loc) {
        Ok(f) => {
            let o = core::instantiate(vm, "IntlNumberFormatter")?;
            core::set_data(&o, f);
            Ok(Value::Object(o))
        }
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Null),
    }
}

fn nf_format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    core::with_obj_error(&o, php_intl::ErrorState::reset);
    core::reset_global();
    let v = a.at(0);
    let fv = match &v {
        Value::Str(z) => FormatValue::Str(z.as_bytes()),
        Value::Float(f) => FormatValue::Float(*f),
        Value::Int(i) => FormatValue::Int(*i),
        Value::Bool(b) => FormatValue::Int(i64::from(*b)),
        _ => FormatValue::Int(0),
    };
    match core::with::<IntlNumberFormatter, _>(&o, |f| f.format(fv)) {
        Some(Ok(v)) => Ok(bytes(v)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, 0, Value::Bool(false)),
        None => Err(vm.throw("Error", "Found unconstructed IntlNumberFormatter")),
    }
}

// ---------------------------------------------------- IntlNumberRangeFormatter

fn range_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw("Error", php_intl::rangeformatter::DIRECT_CONSTRUCTION))
}

fn range_create(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let loc = text(&a.str(1));
    match IntlNumberRangeFormatter::create_from_skeleton(
        a.str(0).as_bytes(),
        &loc,
        a.int(2),
        a.int(3),
    ) {
        Ok(f) => {
            let o = core::instantiate(vm, "IntlNumberRangeFormatter")?;
            core::set_data(&o, f);
            Ok(Value::Object(o))
        }
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Null),
    }
}

fn range_value(v: &Value) -> RangeValue {
    match v {
        Value::Float(f) => RangeValue::Float(*f),
        Value::Int(i) => RangeValue::Int(*i),
        Value::Bool(b) => RangeValue::Int(i64::from(*b)),
        _ => RangeValue::Int(0),
    }
}

fn range_format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    core::reset_both(&o);
    let (x, y) = (range_value(&a.at(0)), range_value(&a.at(1)));
    match core::with::<IntlNumberRangeFormatter, _>(&o, |f| f.format(x, y)) {
        Some(Ok(v)) => Ok(bytes(v)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, 0, Value::Bool(false)),
        None => Err(vm.throw("Error", "Found unconstructed IntlNumberRangeFormatter")),
    }
}

// ---------------------------------------------- IntlRelativeDateTimeFormatter

/// The payload: the formatter and its bound `NumberFormatter` object.
struct RelDate {
    f: IntlRelativeDateTimeFormatter,
    nf: Option<Obj>,
}

fn case_name(vm: &mut Vm, v: &Value) -> VmResult<Option<String>> {
    match v {
        Value::Object(o) => {
            let n = vm.get_property(o, "name")?;
            Ok(Some(vm.value_to_string(&n)?.to_string_lossy()))
        }
        _ => Ok(None),
    }
}

fn rel_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    core::with_obj_error(&o, php_intl::ErrorState::reset);
    core::reset_global();
    if core::is_constructed::<RelDate>(&o) {
        return Err(vm.throw("Error", php_intl::reldateformatter::ALREADY_CONSTRUCTED));
    }
    let loc = a.opt(0).map(|v| match v {
        Value::Str(z) => text(&z),
        _ => String::new(),
    });
    let style = match case_name(vm, &a.at(1))? {
        Some(n) => Style::from_case(&n).unwrap_or(Style::Long),
        None => Style::Long,
    };
    let cap = match case_name(vm, &a.at(2))? {
        Some(n) => Capitalization::from_case(&n).unwrap_or(Capitalization::None),
        None => Capitalization::None,
    };
    let nf_obj = match a.opt(3) {
        Some(Value::Object(n)) => Some(n),
        _ => None,
    };
    // Validate the locale before looking at the formatter (php-src order).
    let probe = IntlRelativeDateTimeFormatter::new(loc.as_deref(), style, cap, None);
    if let Err(e @ (php_intl::Error::ArgumentValue { .. } | php_intl::Error::ArgumentType { .. })) =
        probe
    {
        return Err(core::throw_engine(vm, a, e, 0));
    }
    let r = match &nf_obj {
        Some(n) => match core::with::<NumberFormatter, _>(n, |nf| {
            IntlRelativeDateTimeFormatter::new(loc.as_deref(), style, cap, Some(nf))
        }) {
            Some(r) => r,
            None => {
                return Err(vm.throw(
                    "Error",
                    php_intl::reldateformatter::UNCONSTRUCTED_NUMBER_FORMATTER,
                ))
            }
        },
        None => probe,
    };
    match r {
        Ok(f) => {
            core::set_data(&o, RelDate { f, nf: nf_obj });
            Ok(Value::Null)
        }
        Err(e) => {
            let _g = core::CtorGuard::new();
            core::fail(vm, a, Some(&o), e, 0)?;
            Ok(Value::Null)
        }
    }
}

fn rel_fetch(vm: &mut Vm, a: &Args) -> VmResult<Obj> {
    let o = a.this_obj();
    core::with_obj_error(&o, php_intl::ErrorState::reset);
    core::reset_global();
    if !core::is_constructed::<RelDate>(&o) {
        return Err(vm.throw("Error", php_intl::reldateformatter::UNCONSTRUCTED));
    }
    Ok(o)
}

fn rel_format_impl(vm: &mut Vm, a: &Args, numeric: bool) -> VmResult<Value> {
    let o = rel_fetch(vm, a)?;
    let offset = match a.at(0) {
        Value::Float(f) => f,
        #[allow(clippy::cast_precision_loss)]
        Value::Int(i) => i as f64,
        _ => 0.0,
    };
    let unit = match case_name(vm, &a.at(1))? {
        Some(n) => Unit::from_case(&n).unwrap_or(Unit::Year),
        None => Unit::Year,
    };
    let nf = core::with::<RelDate, _>(&o, |r| r.nf.clone()).flatten();
    let r = core::with::<RelDate, _>(&o, |r| {
        let go = |f: &mut IntlRelativeDateTimeFormatter, nf: Option<&NumberFormatter>| {
            if numeric {
                f.format_numeric(offset, unit, nf)
            } else {
                f.format(offset, unit, nf)
            }
        };
        match &nf {
            Some(n) => core::with::<NumberFormatter, _>(n, |nfv| go(&mut r.f, Some(nfv)))
                .unwrap_or_else(|| go(&mut r.f, None)),
            None => go(&mut r.f, None),
        }
    });
    match r {
        Some(Ok(v)) => Ok(bytes(v)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, 0, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

fn rel_format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    rel_format_impl(vm, a, false)
}

fn rel_format_numeric(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    rel_format_impl(vm, a, true)
}

fn rel_combine(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = rel_fetch(vm, a)?;
    let (d, t) = (a.str(0), a.str(1));
    match core::with::<RelDate, _>(&o, |r| {
        r.f.combine_date_and_time(d.as_bytes(), t.as_bytes())
    }) {
        Some(Ok(v)) => Ok(bytes(v)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, 0, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("IntlListFormatter::__construct", list_construct),
        ("IntlListFormatter::format", list_format),
        ("IntlListFormatter::getErrorCode", core::get_error_code),
        (
            "IntlListFormatter::getErrorMessage",
            core::get_error_message,
        ),
        ("IntlNumberFormatter::__construct", nf_construct),
        ("IntlNumberFormatter::createFromSkeleton", nf_create),
        ("IntlNumberFormatter::format", nf_format),
        ("IntlNumberFormatter::getErrorCode", core::get_error_code),
        (
            "IntlNumberFormatter::getErrorMessage",
            core::get_error_message,
        ),
        ("IntlNumberRangeFormatter::__construct", range_construct),
        ("IntlNumberRangeFormatter::createFromSkeleton", range_create),
        ("IntlNumberRangeFormatter::format", range_format),
        (
            "IntlNumberRangeFormatter::getErrorCode",
            core::get_error_code,
        ),
        (
            "IntlNumberRangeFormatter::getErrorMessage",
            core::get_error_message,
        ),
        ("IntlRelativeDateTimeFormatter::__construct", rel_construct),
        ("IntlRelativeDateTimeFormatter::format", rel_format),
        (
            "IntlRelativeDateTimeFormatter::formatNumeric",
            rel_format_numeric,
        ),
        (
            "IntlRelativeDateTimeFormatter::combineDateAndTime",
            rel_combine,
        ),
        (
            "IntlRelativeDateTimeFormatter::getErrorCode",
            core::get_error_code,
        ),
        (
            "IntlRelativeDateTimeFormatter::getErrorMessage",
            core::get_error_message,
        ),
    ]
}
