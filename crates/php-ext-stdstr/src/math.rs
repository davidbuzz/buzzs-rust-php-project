//! `ext/standard/math.c` (except `pow()`, which stays in the VM because it
//! is the `**` operator, and `min`/`max`/`clamp`, which are array
//! functions).

use php_runtime::Value;
use php_stdstr::error::Notices;
use php_stdstr::math::{self, Number, RoundingMode};
use php_support::fmt as pfmt;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::{bytes, check, emit};

/// A `Z_PARAM_NUMBER` argument (already `int|float` after the VM's ZPP).
fn number(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Number> {
    Ok(match a.at(n) {
        Value::Int(i) => Number::Int(i),
        Value::Float(f) => Number::Float(f),
        Value::Bool(b) => Number::Int(i64::from(b)),
        other => Number::Float(vm.value_to_float(&other)?),
    })
}

fn number_value(n: Number) -> Value {
    match n {
        Number::Int(i) => Value::Int(i),
        Number::Float(f) => Value::Float(f),
    }
}

fn abs(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(number_value(math::abs(number(vm, a, 0)?)))
}

fn ceil(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Float(math::ceil(number(vm, a, 0)?)))
}

fn floor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Float(math::floor(number(vm, a, 0)?)))
}

fn support_mode(m: RoundingMode) -> pfmt::RoundingMode {
    match m {
        RoundingMode::HalfUp => pfmt::RoundingMode::HalfUp,
        RoundingMode::HalfDown => pfmt::RoundingMode::HalfDown,
        RoundingMode::HalfEven => pfmt::RoundingMode::HalfEven,
        RoundingMode::HalfOdd => pfmt::RoundingMode::HalfOdd,
        RoundingMode::Ceiling => pfmt::RoundingMode::PositiveInfinity,
        RoundingMode::Floor => pfmt::RoundingMode::NegativeInfinity,
        RoundingMode::TowardZero => pfmt::RoundingMode::TowardsZero,
        RoundingMode::AwayFromZero => pfmt::RoundingMode::AwayFromZero,
    }
}

/// `Z_PARAM_OBJ_OF_CLASS_OR_LONG(mode_object, rounding_mode_ce, mode)`.
fn round_mode(vm: &mut Vm, a: &Args, v: &Value) -> VmResult<RoundingMode> {
    let long = match v {
        Value::Object(o)
            if o.class_name()
                .as_bytes()
                .eq_ignore_ascii_case(b"RoundingMode") =>
        {
            let name = vm.get_property(o, "name")?;
            let name = crate::to_str(vm, &name)?;
            return Ok(RoundingMode::from_enum_case(&name.to_string_lossy())
                .unwrap_or(RoundingMode::HalfUp));
        }
        Value::Int(i) => Some(*i),
        Value::Bool(b) => Some(i64::from(*b)),
        #[allow(clippy::cast_possible_truncation, clippy::float_cmp)]
        Value::Float(f) if f.is_finite() && f.trunc() == *f => Some(*f as i64),
        Value::Str(s) => match php_runtime::numeric::is_numeric_string_ex(s.as_bytes(), false) {
            Some(n) => match n.value {
                php_runtime::numeric::Number::Int(i) => Some(i),
                #[allow(clippy::cast_possible_truncation, clippy::float_cmp)]
                php_runtime::numeric::Number::Float(f) if f.trunc() == f => Some(f as i64),
                php_runtime::numeric::Number::Float(_) => None,
            },
            None => None,
        },
        _ => None,
    };
    let Some(m) = long else {
        let msg = format!(
            "must be of type RoundingMode|int, {} given",
            crate::type_name(v)
        );
        return Err(a.type_error(vm, 3, &msg));
    };
    check(vm, RoundingMode::from_long(m))
}

fn round(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let num = number(vm, a, 0)?;
    let places = if a.has(1) {
        math::round_places(a.int(1))
    } else {
        0
    };
    let mode = match a.get(2) {
        None => RoundingMode::HalfUp,
        Some(v) => round_mode(vm, a, &v)?,
    };
    let r = match num {
        Number::Int(i) => math::round_long(i, places)
            .unwrap_or_else(|| pfmt::php_round(num.to_f64(), places, support_mode(mode))),
        Number::Float(f) => pfmt::php_round(f, places, support_mode(mode)),
    };
    Ok(Value::Float(r))
}

macro_rules! float_fns {
    ($($name:ident => $f:expr),* $(,)?) => {
        $(fn $name(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let g: fn(f64) -> f64 = $f;
            Ok(Value::Float(g(a.float(0))))
        })*
    };
}

float_fns! {
    sin => f64::sin, cos => f64::cos, tan => f64::tan,
    asin => f64::asin, acos => f64::acos, atan => f64::atan,
    sinh => f64::sinh, cosh => f64::cosh, tanh => f64::tanh,
    asinh => f64::asinh, acosh => f64::acosh, atanh => f64::atanh,
    expm1 => f64::exp_m1, log1p => f64::ln_1p, exp => f64::exp,
    log10 => f64::log10, sqrt => f64::sqrt,
    deg2rad => math::deg2rad, rad2deg => math::rad2deg,
}

fn atan2(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Float(a.float(0).atan2(a.float(1))))
}

fn pi(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Float(std::f64::consts::PI))
}

fn is_finite(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(a.float(0).is_finite()))
}

fn is_nan(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(a.float(0).is_nan()))
}

fn is_infinite(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(a.float(0).is_infinite()))
}

fn intdiv(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(check(vm, math::intdiv(a.int(0), a.int(1)))?))
}

fn log(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let base = a.has(1).then(|| a.float(1));
    Ok(Value::Float(check(vm, math::log(a.float(0), base))?))
}

fn hypot(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Float(math::hypot(a.float(0), a.float(1))))
}

fn fmod(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Float(math::fmod(a.float(0), a.float(1))))
}

fn fdiv(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Float(math::fdiv(a.float(0), a.float(1))))
}

fn fpow(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Float(math::fpow(a.float(0), a.float(1))))
}

fn base_to_number(vm: &mut Vm, a: &Args, f: fn(&[u8], &mut Notices) -> Number) -> VmResult<Value> {
    let mut n = Notices::new();
    let r = f(a.str(0).as_bytes(), &mut n);
    emit(vm, n)?;
    Ok(number_value(r))
}

fn bindec(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    base_to_number(vm, a, math::bindec)
}

fn hexdec(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    base_to_number(vm, a, math::hexdec)
}

fn octdec(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    base_to_number(vm, a, math::octdec)
}

fn decbin(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(math::decbin(a.int(0))))
}

fn decoct(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(math::decoct(a.int(0))))
}

fn dechex(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(math::dechex(a.int(0))))
}

fn base_convert(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut n = Notices::new();
    let r = math::base_convert(a.str(0).as_bytes(), a.int(1), a.int(2), &mut n);
    emit(vm, n)?;
    Ok(bytes(check(vm, r)?))
}

fn number_format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let num = number(vm, a, 0)?;
    let dec = a.int(1);
    let dec_point = a
        .opt(2)
        .map_or_else(|| b".".to_vec(), |_| a.str(2).as_bytes().to_vec());
    let thousands = a
        .opt(3)
        .map_or_else(|| b",".to_vec(), |_| a.str(3).as_bytes().to_vec());
    let Ok(dec) = i32::try_from(dec) else {
        let msg = format!("must be between {} and {}", i32::MIN, i32::MAX);
        return Err(a.value_error(vm, 2, &msg));
    };
    let num = match num {
        Number::Int(i) => pfmt::Number::Int(i),
        Number::Float(f) => pfmt::Number::Float(f),
    };
    Ok(bytes(pfmt::number_format(num, dec, &dec_point, &thousands)))
}

/// Registers the `math.c` functions and constants.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function("abs(int|float $num): int|float", abs)
        .function("ceil(int|float $num): float", ceil)
        .function("floor(int|float $num): float", floor)
        .function("round(int|float $num, int $precision = 0, mixed $mode = PHP_ROUND_HALF_UP): float", round)
        .function("sin(float $num): float", sin)
        .function("cos(float $num): float", cos)
        .function("tan(float $num): float", tan)
        .function("asin(float $num): float", asin)
        .function("acos(float $num): float", acos)
        .function("atan(float $num): float", atan)
        .function("atanh(float $num): float", atanh)
        .function("atan2(float $y, float $x): float", atan2)
        .function("sinh(float $num): float", sinh)
        .function("cosh(float $num): float", cosh)
        .function("tanh(float $num): float", tanh)
        .function("asinh(float $num): float", asinh)
        .function("acosh(float $num): float", acosh)
        .function("expm1(float $num): float", expm1)
        .function("log1p(float $num): float", log1p)
        .function("pi(): float", pi)
        .function("is_finite(float $num): bool", is_finite)
        .function("is_nan(float $num): bool", is_nan)
        .function("intdiv(int $num1, int $num2): int", intdiv)
        .function("is_infinite(float $num): bool", is_infinite)
        .function("exp(float $num): float", exp)
        .function("log(float $num, float $base = M_E): float", log)
        .function("log10(float $num): float", log10)
        .function("sqrt(float $num): float", sqrt)
        .function("hypot(float $x, float $y): float", hypot)
        .function("deg2rad(float $num): float", deg2rad)
        .function("rad2deg(float $num): float", rad2deg)
        .function("bindec(string $binary_string): int|float", bindec)
        .function("hexdec(string $hex_string): int|float", hexdec)
        .function("octdec(string $octal_string): int|float", octdec)
        .function("decbin(int $num): string", decbin)
        .function("decoct(int $num): string", decoct)
        .function("dechex(int $num): string", dechex)
        .function("base_convert(string $num, int $from_base, int $to_base): string", base_convert)
        .function("number_format(int|float $num, int $decimals = 0, ?string $decimal_separator = \".\", ?string $thousands_separator = \",\"): string", number_format)
        .function("fmod(float $num1, float $num2): float", fmod)
        .function("fdiv(float $num1, float $num2): float", fdiv)
        .function("fpow(float $num, float $exponent): float", fpow);
    for (name, v) in [
        ("M_E", std::f64::consts::E),
        ("M_LOG2E", std::f64::consts::LOG2_E),
        ("M_LOG10E", std::f64::consts::LOG10_E),
        ("M_LN2", std::f64::consts::LN_2),
        ("M_LN10", std::f64::consts::LN_10),
        ("M_PI", std::f64::consts::PI),
        ("M_PI_2", std::f64::consts::FRAC_PI_2),
        ("M_PI_4", std::f64::consts::FRAC_PI_4),
        ("M_1_PI", std::f64::consts::FRAC_1_PI),
        ("M_2_PI", std::f64::consts::FRAC_2_PI),
        ("M_SQRTPI", 1.772_453_850_905_516_f64),
        ("M_2_SQRTPI", std::f64::consts::FRAC_2_SQRT_PI),
        ("M_LNPI", 1.144_729_885_849_400_2_f64),
        ("M_EULER", 0.577_215_664_901_532_9_f64),
        ("M_SQRT2", std::f64::consts::SQRT_2),
        ("M_SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
        ("M_SQRT3", 1.732_050_807_568_877_2_f64),
    ] {
        m.constant(name, Value::Float(v));
    }
    for (name, v) in [
        ("PHP_ROUND_HALF_UP", RoundingMode::HalfUp),
        ("PHP_ROUND_HALF_DOWN", RoundingMode::HalfDown),
        ("PHP_ROUND_HALF_EVEN", RoundingMode::HalfEven),
        ("PHP_ROUND_HALF_ODD", RoundingMode::HalfOdd),
    ] {
        m.constant(name, Value::Int(v.as_long()));
    }
}
