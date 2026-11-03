//! `BcMath\Number` (PHP 8.4): an immutable arbitrary-precision number whose
//! operations infer the result scale from their operands instead of using
//! `bcmath.scale`.
//!
//! The VM owns the object; this module supplies its state ([`Number`]) and
//! the semantics of every method, operator overload (`do_operation`),
//! comparison handler, property handler and (un)serialization, with PHP's
//! exact error messages.
//!
//! Zend parameter parsing stays in the VM: it rejects wrong operand types
//! (see [`BcError::number_argument_type`]), converts `float`/`bool`
//! arguments to `int` (emitting the usual deprecations) and passes the
//! result as an [`Operand`].

use std::cmp::Ordering;

use crate::error::BcError;
use crate::functions::{
    check_alloc, check_precision, check_round_alloc, check_scale, format_checked, ordering_to_int,
    pow_error, MAX_SCALE,
};
use crate::num::{BcNum, Sign};
use crate::ops::{RaiseModError, RoundingMode};

/// Maximum number of extra digits added when a result's scale must grow,
/// e.g. for a non-terminating division (`BC_MATH_NUMBER_EXPAND_SCALE`).
pub const EXPAND_SCALE: usize = 10;

/// The PHP class name.
pub const CLASS_NAME: &str = "BcMath\\Number";

/// `INT_MAX` as `usize`.
const INT_MAX: usize = 2_147_483_647;

/// An operand accepted by `BcMath\Number` methods and operators
/// (`BcMath\Number|string|int`).
#[derive(Debug, Clone, Copy)]
pub enum Operand<'a> {
    /// A `BcMath\Number` instance.
    Number(&'a Number),
    /// A numeric string (validated like the `bc*()` functions).
    Str(&'a [u8]),
    /// An integer.
    Int(i64),
}

impl<'a> From<&'a Number> for Operand<'a> {
    fn from(n: &'a Number) -> Self {
        Self::Number(n)
    }
}

impl<'a> From<&'a str> for Operand<'a> {
    fn from(s: &'a str) -> Self {
        Self::Str(s.as_bytes())
    }
}

impl<'a> From<&'a [u8]> for Operand<'a> {
    fn from(s: &'a [u8]) -> Self {
        Self::Str(s)
    }
}

impl From<i64> for Operand<'_> {
    fn from(v: i64) -> Self {
        Self::Int(v)
    }
}

/// Arithmetic operators overloaded by `BcMath\Number` (`do_operation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
    /// `**`
    Pow,
}

impl BinaryOp {
    /// The PHP operator token.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Mod => "%",
            Self::Pow => "**",
        }
    }
}

/// The state of a `BcMath\Number` object: the number and its scale (the
/// `scale` property). The `value` property is the number printed at that
/// scale.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Number {
    num: BcNum,
    scale: usize,
}

/// `bc_num_from_obj_or_str_or_long`: the number and its full scale, or
/// `None` for a malformed string.
fn operand_num(op: Operand<'_>) -> Option<(BcNum, usize)> {
    match op {
        Operand::Number(n) => Some((n.num.clone(), n.scale)),
        Operand::Str(s) => BcNum::parse_auto(s).ok().map(|p| (p.num, p.full_scale)),
        Operand::Int(v) => Some((BcNum::from_i64(v), 0)),
    }
}

/// `bc_num_from_obj_or_str_or_long_with_err`.
fn method_operand(
    func: &str,
    op: Operand<'_>,
    arg_num: u32,
    arg_name: &str,
) -> Result<(BcNum, usize), BcError> {
    let (num, full_scale) = operand_num(op)
        .ok_or_else(|| BcError::argument_value(func, arg_num, arg_name, "is not well-formed"))?;
    if full_scale > INT_MAX {
        return Err(BcError::argument_value(
            func,
            arg_num,
            arg_name,
            &format!("must be between 0 and {MAX_SCALE}"),
        ));
    }
    Ok((num, full_scale))
}

fn method_name(name: &str) -> String {
    format!("{CLASS_NAME}::{name}")
}

fn scale_too_large() -> BcError {
    BcError::value("scale of the result is too large")
}

/// An inferred result scale, which must not exceed `INT_MAX`.
fn checked_scale(scale: usize) -> Result<usize, BcError> {
    if scale > INT_MAX {
        Err(scale_too_large())
    } else {
        Ok(scale)
    }
}

/// Shrinks an expanded scale back to the digits actually used, by at most
/// [`EXPAND_SCALE`].
fn shrink_expanded(scale: usize, ret: &BcNum) -> usize {
    let diff = scale - ret.scale();
    scale - diff.min(EXPAND_SCALE)
}

/// How `pow` reports errors: as a method (`Argument #1 ($exponent) ...`) or
/// as the `**` operator (plain `ValueError`).
#[derive(Clone, Copy)]
enum PowContext<'a> {
    Method(&'a str),
    Operator,
}

/// `bcmath_number_*_internal`: computes `n1 op n2`. `scale` is the explicit
/// scale or `None` to infer it from the operands' full scales.
fn calc(
    op: BinaryOp,
    n1: &BcNum,
    n2: &BcNum,
    s1: usize,
    s2: usize,
    scale: Option<usize>,
    ctx: PowContext<'_>,
) -> Result<Number, BcError> {
    let auto = scale.is_none();
    match op {
        BinaryOp::Add | BinaryOp::Sub => {
            let scale = scale.unwrap_or(s1.max(s2));
            check_alloc(scale)?;
            let mut ret = if op == BinaryOp::Add {
                BcNum::add(n1, n2, scale)
            } else {
                BcNum::sub(n1, n2, scale)
            };
            ret.truncate_scale(scale);
            if ret.is_zero() {
                ret.sign = Sign::Plus;
            }
            ret.rm_trailing_zeros();
            Ok(Number { num: ret, scale })
        }
        BinaryOp::Mul => {
            let scale = scale.map_or_else(|| checked_scale(s1 + s2), Ok)?;
            check_alloc(scale)?;
            let mut ret = BcNum::multiply(n1, n2, scale);
            ret.truncate_scale(scale);
            if ret.is_zero() {
                ret.sign = Sign::Plus;
            }
            ret.rm_trailing_zeros();
            Ok(Number { num: ret, scale })
        }
        BinaryOp::Div => {
            let mut scale = scale.map_or_else(|| checked_scale(s1 + EXPAND_SCALE), Ok)?;
            check_alloc(scale.saturating_add(n1.int_len()))?;
            let mut ret = BcNum::divide(n1, n2, scale)
                .ok_or_else(|| BcError::division_by_zero("Division by zero"))?;
            ret.rm_trailing_zeros();
            if auto {
                scale = shrink_expanded(scale, &ret);
            }
            Ok(Number { num: ret, scale })
        }
        BinaryOp::Mod => {
            let scale = scale.unwrap_or(s1.max(s2));
            check_alloc(scale.saturating_add(n1.int_len()))?;
            let mut ret = BcNum::modulo(n1, n2, scale)
                .ok_or_else(|| BcError::division_by_zero("Modulo by zero"))?;
            ret.rm_trailing_zeros();
            Ok(Number { num: ret, scale })
        }
        BinaryOp::Pow => pow_internal(n1, n2, s1, scale, ctx),
    }
}

/// `bcmath_number_pow_internal`.
fn pow_internal(
    n1: &BcNum,
    n2: &BcNum,
    s1: usize,
    scale: Option<usize>,
    ctx: PowContext<'_>,
) -> Result<Number, BcError> {
    let arg_err = |msg: &str| match ctx {
        PowContext::Method(func) => BcError::argument_value(func, 1, "exponent", msg),
        PowContext::Operator => BcError::value(msg),
    };
    if n2.scale() != 0 {
        return Err(arg_err("exponent cannot have a fractional part"));
    }
    let exponent = n2.to_i64();
    let mut scale_expand = false;
    let mut scale = match scale {
        Some(s) => s,
        None => match exponent.cmp(&0) {
            Ordering::Greater => usize::try_from(exponent)
                .ok()
                .and_then(|e| s1.checked_mul(e))
                .filter(|&s| s <= INT_MAX)
                .ok_or_else(scale_too_large)?,
            Ordering::Less => {
                scale_expand = true;
                checked_scale(s1 + EXPAND_SCALE)?
            }
            Ordering::Equal => 0,
        },
    };
    if exponent == 0 && n2.int_part_nonzero() {
        return Err(arg_err("exponent is too large"));
    }
    check_alloc(scale)?;
    let mut ret = BcNum::raise(n1, exponent, scale).map_err(|e| match ctx {
        PowContext::Method(func) => pow_error(func, e, 1, "exponent"),
        PowContext::Operator => pow_error("", e, 0, ""),
    })?;
    ret.rm_trailing_zeros();
    if scale_expand {
        scale = shrink_expanded(scale, &ret);
    }
    Ok(Number { num: ret, scale })
}

impl Number {
    /// `new BcMath\Number(string|int $num)`.
    ///
    /// # Errors
    /// `ValueError: BcMath\Number::__construct(): Argument #1 ($num) is not well-formed`
    /// (or the scale range error for absurdly long fractions).
    pub fn new(num: Operand<'_>) -> Result<Self, BcError> {
        let (num, scale) = method_operand(&method_name("__construct"), num, 1, "num")?;
        Ok(Self { num, scale })
    }

    /// `__construct` called on an object: when it is already initialized
    /// (`$num->__construct(5)`) PHP throws a readonly modification `Error`.
    ///
    /// # Errors
    /// See [`Number::new`].
    pub fn construct(already_initialized: bool, num: Operand<'_>) -> Result<Self, BcError> {
        if already_initialized {
            return Err(BcError::readonly_modification("value"));
        }
        Self::new(num)
    }

    /// Wraps an already computed number with an explicit scale.
    pub fn from_parts(num: BcNum, scale: usize) -> Self {
        Self { num, scale }
    }

    /// The underlying number (trailing zeros trimmed).
    pub fn num(&self) -> &BcNum {
        &self.num
    }

    /// The `value` property: the number printed with `scale` digits.
    pub fn value(&self) -> String {
        self.num.to_string_scaled(self.scale)
    }

    /// The `scale` property.
    pub fn scale(&self) -> i64 {
        i64::try_from(self.scale).unwrap_or(i64::MAX)
    }

    /// Truthiness (`(bool) $num`, `empty($num->value)` is its negation).
    pub fn is_truthy(&self) -> bool {
        !self.num.is_zero()
    }

    /// `isset()`/`empty()` support (`has_property` handler): whether property
    /// `name` exists, and with `check_empty` whether it is also non-empty.
    /// Returns `None` for names other than `value`/`scale` (the VM falls back
    /// to the standard handler).
    pub fn has_property(&self, name: &str, check_empty: bool) -> Option<bool> {
        match name {
            "value" => Some(!check_empty || self.is_truthy()),
            "scale" => Some(!check_empty || self.scale != 0),
            _ => None,
        }
    }

    /// The properties table for `var_dump()`, `(array)` casts and
    /// `get_properties_for`: `["value" => string, "scale" => int]`.
    pub fn debug_info(&self) -> (String, i64) {
        (self.value(), self.scale())
    }

    /// `__toString()`.
    pub fn to_php_string(&self) -> String {
        self.value()
    }

    /// `__serialize()`: the array is `["value" => <this string>]`.
    pub fn serialize(&self) -> String {
        self.value()
    }

    /// `__unserialize(array $data)`. `value` is `$data["value"]` when it
    /// exists and is a string, `None` otherwise.
    ///
    /// # Errors
    /// `Exception: Invalid serialization data for BcMath\Number object`, or
    /// the readonly `Error` when called on an initialized object.
    pub fn unserialize(already_initialized: bool, value: Option<&[u8]>) -> Result<Self, BcError> {
        let invalid = || {
            BcError::new(
                crate::ErrorKind::Exception,
                format!("Invalid serialization data for {CLASS_NAME} object"),
            )
        };
        let value = value.filter(|v| !v.is_empty()).ok_or_else(invalid)?;
        if already_initialized {
            return Err(BcError::readonly_modification("value"));
        }
        let parsed = BcNum::parse_auto(value).map_err(|()| invalid())?;
        if parsed.full_scale > INT_MAX {
            return Err(invalid());
        }
        Ok(Self {
            num: parsed.num,
            scale: parsed.full_scale,
        })
    }

    fn calc_method(
        &self,
        op: BinaryOp,
        name: &str,
        arg_name: &str,
        num: Operand<'_>,
        scale: Option<i64>,
    ) -> Result<Self, BcError> {
        let func = method_name(name);
        let (n2, s2) = method_operand(&func, num, 1, arg_name)?;
        let scale = resolve_method_scale(&func, scale, 2)?;
        calc(
            op,
            &self.num,
            &n2,
            self.scale,
            s2,
            scale,
            PowContext::Method(&func),
        )
    }

    /// `add(BcMath\Number|string|int $num, ?int $scale = null)`; the inferred
    /// scale is the larger operand scale.
    ///
    /// # Errors
    /// `ValueError` for a malformed operand or bad scale.
    pub fn add(&self, num: Operand<'_>, scale: Option<i64>) -> Result<Self, BcError> {
        self.calc_method(BinaryOp::Add, "add", "num", num, scale)
    }

    /// `sub(BcMath\Number|string|int $num, ?int $scale = null)`.
    ///
    /// # Errors
    /// `ValueError` for a malformed operand or bad scale.
    pub fn sub(&self, num: Operand<'_>, scale: Option<i64>) -> Result<Self, BcError> {
        self.calc_method(BinaryOp::Sub, "sub", "num", num, scale)
    }

    /// `mul(BcMath\Number|string|int $num, ?int $scale = null)`; the inferred
    /// scale is the sum of the operand scales.
    ///
    /// # Errors
    /// `ValueError` for a malformed operand, bad or too large scale.
    pub fn mul(&self, num: Operand<'_>, scale: Option<i64>) -> Result<Self, BcError> {
        self.calc_method(BinaryOp::Mul, "mul", "num", num, scale)
    }

    /// `div(BcMath\Number|string|int $num, ?int $scale = null)`; the inferred
    /// scale is this scale plus up to [`EXPAND_SCALE`] digits.
    ///
    /// # Errors
    /// `ValueError` for a malformed operand or bad scale,
    /// `DivisionByZeroError`.
    pub fn div(&self, num: Operand<'_>, scale: Option<i64>) -> Result<Self, BcError> {
        self.calc_method(BinaryOp::Div, "div", "num", num, scale)
    }

    /// `mod(BcMath\Number|string|int $num, ?int $scale = null)`.
    ///
    /// # Errors
    /// `ValueError` for a malformed operand or bad scale,
    /// `DivisionByZeroError`.
    pub fn modulo(&self, num: Operand<'_>, scale: Option<i64>) -> Result<Self, BcError> {
        self.calc_method(BinaryOp::Mod, "mod", "num", num, scale)
    }

    /// `pow(BcMath\Number|string|int $exponent, ?int $scale = null)`.
    ///
    /// # Errors
    /// `ValueError` for a malformed, fractional or too large exponent or a
    /// bad scale, `DivisionByZeroError` for a negative power of zero.
    pub fn pow(&self, exponent: Operand<'_>, scale: Option<i64>) -> Result<Self, BcError> {
        self.calc_method(BinaryOp::Pow, "pow", "exponent", exponent, scale)
    }

    /// `divmod(BcMath\Number|string|int $num, ?int $scale = null): array` —
    /// `[quotient (scale 0), remainder]`.
    ///
    /// # Errors
    /// `ValueError` for a malformed operand or bad scale,
    /// `DivisionByZeroError`.
    pub fn divmod(&self, num: Operand<'_>, scale: Option<i64>) -> Result<(Self, Self), BcError> {
        let func = method_name("divmod");
        let (n2, s2) = method_operand(&func, num, 1, "num")?;
        let scale = resolve_method_scale(&func, scale, 2)?.unwrap_or(self.scale.max(s2));
        check_alloc(scale.saturating_add(self.num.int_len()))?;
        let (mut quot, mut rem) = BcNum::divmod(&self.num, &n2, scale)
            .ok_or_else(|| BcError::division_by_zero("Division by zero"))?;
        quot.rm_trailing_zeros();
        rem.rm_trailing_zeros();
        Ok((
            Self {
                num: quot,
                scale: 0,
            },
            Self { num: rem, scale },
        ))
    }

    /// `powmod(BcMath\Number|string|int $exponent, BcMath\Number|string|int $modulus, ?int $scale = null)`.
    /// A `null` scale means 0.
    ///
    /// # Errors
    /// `ValueError` for malformed or fractional operands, a negative exponent
    /// or bad scale; `DivisionByZeroError` for a zero modulus.
    pub fn powmod(
        &self,
        exponent: Operand<'_>,
        modulus: Operand<'_>,
        scale: Option<i64>,
    ) -> Result<Self, BcError> {
        let func = method_name("powmod");
        let (e, _) = method_operand(&func, exponent, 1, "exponent")?;
        let (m, _) = method_operand(&func, modulus, 2, "modulus")?;
        let scale = resolve_method_scale(&func, scale, 3)?.unwrap_or(0);
        check_alloc(scale)?;
        let mut ret = BcNum::raise_mod(&self.num, &e, &m, scale).map_err(|err| match err {
            RaiseModError::BaseHasFractional => {
                BcError::value("Base number cannot have a fractional part")
            }
            RaiseModError::ExpoHasFractional => {
                BcError::argument_value(&func, 1, "exponent", "cannot have a fractional part")
            }
            RaiseModError::ExpoIsNegative => {
                BcError::argument_value(&func, 1, "exponent", "must be greater than or equal to 0")
            }
            RaiseModError::ModHasFractional => {
                BcError::argument_value(&func, 2, "modulus", "cannot have a fractional part")
            }
            RaiseModError::ModIsZero => BcError::division_by_zero("Modulo by zero"),
        })?;
        ret.rm_trailing_zeros();
        Ok(Self { num: ret, scale })
    }

    /// `sqrt(?int $scale = null)`; the inferred scale is this scale plus up
    /// to [`EXPAND_SCALE`] digits.
    ///
    /// # Errors
    /// `ValueError` for a bad scale or a negative number.
    pub fn sqrt(&self, scale: Option<i64>) -> Result<Self, BcError> {
        let func = method_name("sqrt");
        let explicit = resolve_method_scale(&func, scale, 1)?;
        let mut scale = explicit.map_or_else(|| checked_scale(self.scale + EXPAND_SCALE), Ok)?;
        check_alloc(scale.saturating_add(self.num.int_len()))?;
        let mut ret = BcNum::sqrt(&self.num, scale)
            .ok_or_else(|| BcError::value("Base number must be greater than or equal to 0"))?;
        ret.truncate_scale(scale);
        ret.rm_trailing_zeros();
        if explicit.is_none() {
            scale = shrink_expanded(scale, &ret);
        }
        Ok(Self { num: ret, scale })
    }

    /// `floor()`.
    #[must_use]
    pub fn floor(&self) -> Self {
        Self {
            num: BcNum::floor_or_ceil(&self.num, true),
            scale: 0,
        }
    }

    /// `ceil()`.
    #[must_use]
    pub fn ceil(&self) -> Self {
        Self {
            num: BcNum::floor_or_ceil(&self.num, false),
            scale: 0,
        }
    }

    /// `round(int $precision = 0, RoundingMode $mode = RoundingMode::HalfAwayFromZero)`.
    ///
    /// # Errors
    /// `ValueError` for a precision above `INT_MAX`.
    pub fn round(&self, precision: i64, mode: RoundingMode) -> Result<Self, BcError> {
        check_precision(&method_name("round"), precision, 1)?;
        check_round_alloc(&self.num, precision, mode)?;
        let (mut ret, scale) = BcNum::round(&self.num, precision, mode);
        ret.rm_trailing_zeros();
        Ok(Self { num: ret, scale })
    }

    /// `compare(BcMath\Number|string|int $num, ?int $scale = null): int`; by
    /// default all significant digits of both numbers are compared.
    ///
    /// # Errors
    /// `ValueError` for a malformed operand or bad scale.
    pub fn compare(&self, num: Operand<'_>, scale: Option<i64>) -> Result<i64, BcError> {
        let func = method_name("compare");
        let (n2, _) = method_operand(&func, num, 1, "num")?;
        let scale =
            resolve_method_scale(&func, scale, 2)?.unwrap_or(self.num.scale().max(n2.scale()));
        Ok(ordering_to_int(BcNum::compare(&self.num, &n2, scale)))
    }

    /// The `do_operation` handler: `$a <op> $b` where at least one side is a
    /// `BcMath\Number`. The VM must only call this for operands of type
    /// `BcMath\Number`, `string` or `int` (after converting `float`/`bool` to
    /// `int`); for `null`, arrays and other objects it reports
    /// "Unsupported operand types" itself.
    ///
    /// # Errors
    /// `ValueError` ("Left/Right string operand cannot be converted to
    /// BcMath\Number", pow exponent errors, scale overflow) and
    /// `DivisionByZeroError`.
    pub fn do_operation(op: BinaryOp, lhs: Operand<'_>, rhs: Operand<'_>) -> Result<Self, BcError> {
        let (n1, s1) = operand_num(lhs).ok_or_else(|| {
            BcError::value("Left string operand cannot be converted to BcMath\\Number")
        })?;
        let (n2, s2) = operand_num(rhs).ok_or_else(|| {
            BcError::value("Right string operand cannot be converted to BcMath\\Number")
        })?;
        if s1 > INT_MAX || s2 > INT_MAX {
            return Err(BcError::value(format!(
                "scale must be between 0 and {MAX_SCALE}"
            )));
        }
        calc(op, &n1, &n2, s1, s2, None, PowContext::Operator)
    }

    /// The `compare` handler (`==`, `<`, `<=>`, ...). Returns `Ok(None)` when
    /// a string operand is not well-formed (`ZEND_UNCOMPARABLE`, which makes
    /// every comparison false).
    ///
    /// # Errors
    /// `ValueError` for an operand scale above `INT_MAX`.
    pub fn compare_operands(
        lhs: Operand<'_>,
        rhs: Operand<'_>,
    ) -> Result<Option<Ordering>, BcError> {
        let (Some((n1, s1)), Some((n2, s2))) = (operand_num(lhs), operand_num(rhs)) else {
            return Ok(None);
        };
        if s1 > INT_MAX || s2 > INT_MAX {
            return Err(BcError::value(format!(
                "scale must be between 0 and {MAX_SCALE}"
            )));
        }
        Ok(Some(BcNum::compare(&n1, &n2, n1.scale().max(n2.scale()))))
    }

    /// Formats with an explicit scale (helper for callers needing the output
    /// size check).
    ///
    /// # Errors
    /// [`crate::ErrorKind::OutOfMemory`] for impossible sizes.
    pub fn value_checked(&self) -> Result<String, BcError> {
        format_checked(&self.num, self.scale)
    }
}

/// Validates an optional method scale argument (`null` → `None`).
fn resolve_method_scale(
    func: &str,
    scale: Option<i64>,
    arg_num: u32,
) -> Result<Option<usize>, BcError> {
    match scale {
        None => Ok(None),
        Some(s) => {
            check_scale(func, s, arg_num)?;
            Ok(Some(usize::try_from(s).unwrap_or(usize::MAX)))
        }
    }
}

impl std::fmt::Display for Number {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.value())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Number {
        Number::new(Operand::from(s)).unwrap()
    }

    #[test]
    fn construction() {
        let x = n("12.0004");
        assert_eq!(x.value(), "12.0004");
        assert_eq!(x.scale(), 4);
        assert_eq!(n("-0.0").value(), "0.0");
        assert_eq!(Number::new(Operand::Int(-12345)).unwrap().value(), "-12345");
        assert_eq!(
            Number::new(Operand::from("a")).unwrap_err().message,
            "BcMath\\Number::__construct(): Argument #1 ($num) is not well-formed"
        );
        assert_eq!(
            Number::construct(true, Operand::Int(5))
                .unwrap_err()
                .message,
            "Cannot modify readonly property BcMath\\Number::$value"
        );
    }

    #[test]
    fn scale_inference() {
        let one = n("1");
        let d = one.div(Operand::from("3"), None).unwrap();
        assert_eq!((d.value().as_str(), d.scale()), ("0.3333333333", 10));
        let d = one.div(Operand::from("1000"), None).unwrap();
        assert_eq!((d.value().as_str(), d.scale()), ("0.001", 3));
        let m = n("1.25").mul(Operand::from("1.5"), None).unwrap();
        assert_eq!((m.value().as_str(), m.scale()), ("1.875", 3));
        let a = n("1.25").add(Operand::from("1.75"), None).unwrap();
        assert_eq!((a.value().as_str(), a.scale()), ("3.00", 2));
        let s = n("2").sqrt(None).unwrap();
        assert_eq!((s.value().as_str(), s.scale()), ("1.4142135623", 10));
        let s = n("4").sqrt(None).unwrap();
        assert_eq!((s.value().as_str(), s.scale()), ("2", 0));
    }

    #[test]
    fn operators() {
        let x = n("100.0000");
        let r = Number::do_operation(BinaryOp::Add, Operand::from(&x), Operand::Int(1)).unwrap();
        assert_eq!(r.value(), "101.0000");
        let e =
            Number::do_operation(BinaryOp::Add, Operand::from(&x), Operand::from("a")).unwrap_err();
        assert_eq!(
            e.message,
            "Right string operand cannot be converted to BcMath\\Number"
        );
        let e = Number::do_operation(BinaryOp::Pow, Operand::from(&x), Operand::from("1.5"))
            .unwrap_err();
        assert_eq!(e.message, "exponent cannot have a fractional part");
        assert_eq!(
            Number::compare_operands(Operand::from(&x), Operand::Int(100)).unwrap(),
            Some(Ordering::Equal)
        );
        assert_eq!(
            Number::compare_operands(Operand::from(&x), Operand::from("x")).unwrap(),
            None
        );
    }

    #[test]
    fn serialization() {
        assert_eq!(n("0.1230").serialize(), "0.1230");
        let u = Number::unserialize(false, Some(b"1.50")).unwrap();
        assert_eq!((u.value().as_str(), u.scale()), ("1.50", 2));
        let msg = "Invalid serialization data for BcMath\\Number object";
        assert_eq!(
            Number::unserialize(false, Some(b"a")).unwrap_err().message,
            msg
        );
        assert_eq!(
            Number::unserialize(false, Some(b"")).unwrap_err().message,
            msg
        );
        assert_eq!(Number::unserialize(true, None).unwrap_err().message, msg);
        assert_eq!(
            Number::unserialize(true, Some(b"5")).unwrap_err().message,
            "Cannot modify readonly property BcMath\\Number::$value"
        );
    }

    #[test]
    fn properties() {
        let zero = n("0");
        assert_eq!(zero.has_property("value", false), Some(true));
        assert_eq!(zero.has_property("value", true), Some(false));
        assert_eq!(n("5.67").has_property("scale", true), Some(true));
        assert_eq!(zero.has_property("foo", false), None);
        assert!(!zero.is_truthy());
        assert_eq!(n("-0.1230").debug_info(), ("-0.1230".to_string(), 4));
    }
}
