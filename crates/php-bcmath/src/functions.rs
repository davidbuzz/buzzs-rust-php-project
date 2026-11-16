// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The procedural `bc*()` functions of ext/bcmath (bcmath.c), with PHP's
//! exact argument validation and error messages.
//!
//! The VM converts arguments with the usual `string`/`?int` coercions and
//! passes the raw bytes here; `None` for a nullable `$scale` means "use the
//! `bcmath.scale` INI setting", which [`BcMath`] holds.

use std::cmp::Ordering;

use crate::error::BcError;
use crate::num::BcNum;
use crate::ops::{RaiseError, RaiseModError, RoundingMode};

/// Largest accepted scale (`INT_MAX`).
pub const MAX_SCALE: i64 = 2_147_483_647;

/// Largest number of digits (≈ bytes) a single result or work buffer may
/// need. PHP would exhaust its memory limit at this size; the VM reports
/// [`crate::ErrorKind::OutOfMemory`] as a fatal error.
pub const MAX_DIGITS: usize = 256 * 1024 * 1024;

/// Fails with [`crate::ErrorKind::OutOfMemory`] when a result of `digits`
/// digits cannot be produced.
pub(crate) fn check_alloc(digits: usize) -> Result<(), BcError> {
    if digits > MAX_DIGITS {
        Err(BcError::out_of_memory(digits.saturating_add(25)))
    } else {
        Ok(())
    }
}

/// Converts a validated scale to `usize`.
fn to_usize(scale: i64) -> usize {
    usize::try_from(scale).unwrap_or(usize::MAX)
}

/// Formats `num` at `scale` after checking the output size.
pub(crate) fn format_checked(num: &BcNum, scale: usize) -> Result<String, BcError> {
    check_alloc(num.int_len().saturating_add(scale).saturating_add(2))?;
    Ok(num.to_string_scaled(scale))
}

/// The error for a `bc_raise` failure; `arg_num == 0` means an operator.
pub(crate) fn pow_error(func: &str, err: RaiseError, arg_num: u32, arg_name: &str) -> BcError {
    const MSG: &str = "exponent is too large, the number of digits overflowed";
    match err {
        RaiseError::DivideByZero => BcError::division_by_zero("Negative power of zero"),
        RaiseError::TooLarge(bytes) => BcError::out_of_memory(bytes),
        RaiseError::LenOverflow | RaiseError::ScaleOverflow | RaiseError::FullLenOverflow => {
            if arg_num == 0 {
                BcError::value(MSG)
            } else {
                BcError::argument_value(func, arg_num, arg_name, MSG)
            }
        }
    }
}

/// Per-request bcmath state: the `bcmath.scale` INI value used when a
/// function's `$scale` argument is `null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BcMath {
    scale: u32,
}

impl BcMath {
    /// State with `bcmath.scale=0`.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current default scale (`bcmath.scale`).
    pub fn default_scale(&self) -> i64 {
        i64::from(self.scale)
    }

    /// INI handler for `bcmath.scale` (`OnUpdateScale`), given the already
    /// quantity-parsed value. Returns `false` (INI update failure) when the
    /// value is outside `0..=INT_MAX`.
    pub fn set_ini_scale(&mut self, value: i64) -> bool {
        match u32::try_from(value) {
            Ok(v) if i64::from(v) <= MAX_SCALE => {
                self.scale = v;
                true
            }
            _ => false,
        }
    }

    /// `bcmath_check_scale` + `null` handling for a function's scale argument.
    fn resolve_scale(self, func: &str, scale: Option<i64>, arg_num: u32) -> Result<usize, BcError> {
        match scale {
            None => Ok(usize::try_from(self.scale).unwrap_or(usize::MAX)),
            Some(s) => {
                check_scale(func, s, arg_num)?;
                Ok(to_usize(s))
            }
        }
    }

    /// `bcscale(?int $scale = null): int` — returns the previous scale and
    /// optionally sets a new one. The VM must mirror the change into the
    /// `bcmath.scale` INI entry (`zend_alter_ini_entry`).
    ///
    /// # Errors
    /// `ValueError` when the new scale is out of range.
    pub fn bcscale(&mut self, scale: Option<i64>) -> Result<i64, BcError> {
        let old = self.default_scale();
        if let Some(s) = scale {
            check_scale("bcscale", s, 1)?;
            self.set_ini_scale(s);
        }
        Ok(old)
    }

    /// `bcadd(string $num1, string $num2, ?int $scale = null): string`.
    ///
    /// # Errors
    /// `ValueError` for a bad scale or malformed operand.
    pub fn bcadd(&self, num1: &[u8], num2: &[u8], scale: Option<i64>) -> Result<String, BcError> {
        let scale = self.resolve_scale("bcadd", scale, 3)?;
        let (a, b) = parse_two("bcadd", num1, num2)?;
        check_alloc(scale)?;
        format_checked(&BcNum::add(&a, &b, scale), scale)
    }

    /// `bcsub(string $num1, string $num2, ?int $scale = null): string`.
    ///
    /// # Errors
    /// `ValueError` for a bad scale or malformed operand.
    pub fn bcsub(&self, num1: &[u8], num2: &[u8], scale: Option<i64>) -> Result<String, BcError> {
        let scale = self.resolve_scale("bcsub", scale, 3)?;
        let (a, b) = parse_two("bcsub", num1, num2)?;
        check_alloc(scale)?;
        format_checked(&BcNum::sub(&a, &b, scale), scale)
    }

    /// `bcmul(string $num1, string $num2, ?int $scale = null): string`.
    ///
    /// # Errors
    /// `ValueError` for a bad scale or malformed operand.
    pub fn bcmul(&self, num1: &[u8], num2: &[u8], scale: Option<i64>) -> Result<String, BcError> {
        let scale = self.resolve_scale("bcmul", scale, 3)?;
        let (a, b) = parse_two("bcmul", num1, num2)?;
        check_alloc(scale)?;
        format_checked(&BcNum::multiply(&a, &b, scale), scale)
    }

    /// `bcdiv(string $num1, string $num2, ?int $scale = null): string`.
    ///
    /// # Errors
    /// `ValueError` for a bad scale or malformed operand,
    /// `DivisionByZeroError` ("Division by zero").
    pub fn bcdiv(&self, num1: &[u8], num2: &[u8], scale: Option<i64>) -> Result<String, BcError> {
        let scale = self.resolve_scale("bcdiv", scale, 3)?;
        let (a, b) = parse_two("bcdiv", num1, num2)?;
        check_alloc(scale.saturating_add(a.int_len()))?;
        let q = BcNum::divide(&a, &b, scale)
            .ok_or_else(|| BcError::division_by_zero("Division by zero"))?;
        format_checked(&q, scale)
    }

    /// `bcmod(string $num1, string $num2, ?int $scale = null): string`.
    ///
    /// # Errors
    /// `ValueError` for a bad scale or malformed operand,
    /// `DivisionByZeroError` ("Modulo by zero").
    pub fn bcmod(&self, num1: &[u8], num2: &[u8], scale: Option<i64>) -> Result<String, BcError> {
        let scale = self.resolve_scale("bcmod", scale, 3)?;
        let (a, b) = parse_two("bcmod", num1, num2)?;
        check_alloc(scale.saturating_add(a.int_len()))?;
        let r = BcNum::modulo(&a, &b, scale)
            .ok_or_else(|| BcError::division_by_zero("Modulo by zero"))?;
        format_checked(&r, scale)
    }

    /// `bcdivmod(string $num1, string $num2, ?int $scale = null): array` —
    /// returns `[quotient (scale 0), remainder (scale $scale)]`.
    ///
    /// # Errors
    /// `ValueError` for a bad scale or malformed operand,
    /// `DivisionByZeroError` ("Division by zero").
    pub fn bcdivmod(
        &self,
        num1: &[u8],
        num2: &[u8],
        scale: Option<i64>,
    ) -> Result<(String, String), BcError> {
        let scale = self.resolve_scale("bcdivmod", scale, 3)?;
        let (a, b) = parse_two("bcdivmod", num1, num2)?;
        check_alloc(scale.saturating_add(a.int_len()))?;
        let (q, r) = BcNum::divmod(&a, &b, scale)
            .ok_or_else(|| BcError::division_by_zero("Division by zero"))?;
        Ok((q.to_string_scaled(0), format_checked(&r, scale)?))
    }

    /// `bcpowmod(string $num, string $exponent, string $modulus, ?int $scale = null): string`.
    ///
    /// # Errors
    /// `ValueError` for a bad scale, malformed operand, fractional operand or
    /// negative exponent; `DivisionByZeroError` ("Modulo by zero").
    pub fn bcpowmod(
        &self,
        num: &[u8],
        exponent: &[u8],
        modulus: &[u8],
        scale: Option<i64>,
    ) -> Result<String, BcError> {
        const F: &str = "bcpowmod";
        let scale = self.resolve_scale(F, scale, 4)?;
        let base = parse_arg(F, num, 1, "num")?;
        let expo = parse_arg(F, exponent, 2, "exponent")?;
        let modulus = parse_arg(F, modulus, 3, "modulus")?;
        check_alloc(scale)?;
        let r = BcNum::raise_mod(&base, &expo, &modulus, scale).map_err(|e| match e {
            RaiseModError::BaseHasFractional => {
                BcError::argument_value(F, 1, "num", "cannot have a fractional part")
            }
            RaiseModError::ExpoHasFractional => {
                BcError::argument_value(F, 2, "exponent", "cannot have a fractional part")
            }
            RaiseModError::ExpoIsNegative => {
                BcError::argument_value(F, 2, "exponent", "must be greater than or equal to 0")
            }
            RaiseModError::ModHasFractional => {
                BcError::argument_value(F, 3, "modulus", "cannot have a fractional part")
            }
            RaiseModError::ModIsZero => BcError::division_by_zero("Modulo by zero"),
        })?;
        format_checked(&r, scale)
    }

    /// `bcpow(string $num, string $exponent, ?int $scale = null): string`.
    ///
    /// # Errors
    /// `ValueError` for a bad scale, malformed operand, fractional or
    /// too-large exponent; `DivisionByZeroError` ("Negative power of zero").
    pub fn bcpow(
        &self,
        num: &[u8],
        exponent: &[u8],
        scale: Option<i64>,
    ) -> Result<String, BcError> {
        const F: &str = "bcpow";
        let scale = self.resolve_scale(F, scale, 3)?;
        let base = parse_arg(F, num, 1, "num")?;
        let expo = parse_arg(F, exponent, 2, "exponent")?;
        if expo.scale() != 0 {
            return Err(BcError::argument_value(
                F,
                2,
                "exponent",
                "cannot have a fractional part",
            ));
        }
        let e = expo.to_i64();
        if e == 0 && expo.int_part_nonzero() {
            return Err(BcError::argument_value(F, 2, "exponent", "is too large"));
        }
        check_alloc(scale)?;
        let r = BcNum::raise(&base, e, scale).map_err(|err| pow_error(F, err, 2, "exponent"))?;
        format_checked(&r, scale)
    }

    /// `bcsqrt(string $num, ?int $scale = null): string`.
    ///
    /// # Errors
    /// `ValueError` for a bad scale, malformed or negative operand.
    pub fn bcsqrt(&self, num: &[u8], scale: Option<i64>) -> Result<String, BcError> {
        const F: &str = "bcsqrt";
        let scale = self.resolve_scale(F, scale, 2)?;
        let n = parse_arg(F, num, 1, "num")?;
        check_alloc(scale.saturating_add(n.int_len()))?;
        let r = BcNum::sqrt(&n, scale).ok_or_else(|| {
            BcError::argument_value(F, 1, "num", "must be greater than or equal to 0")
        })?;
        format_checked(&r, scale)
    }

    /// `bccomp(string $num1, string $num2, ?int $scale = null): int` —
    /// compares after *truncating* both operands to `$scale` digits.
    ///
    /// # Errors
    /// `ValueError` for a bad scale or malformed operand.
    pub fn bccomp(&self, num1: &[u8], num2: &[u8], scale: Option<i64>) -> Result<i64, BcError> {
        const F: &str = "bccomp";
        let scale = self.resolve_scale(F, scale, 3)?;
        let a = BcNum::parse(num1, scale, false)
            .map_err(|()| BcError::argument_value(F, 1, "num1", "is not well-formed"))?
            .num;
        let b = BcNum::parse(num2, scale, false)
            .map_err(|()| BcError::argument_value(F, 2, "num2", "is not well-formed"))?
            .num;
        Ok(ordering_to_int(BcNum::compare(&a, &b, scale)))
    }

    /// `bcfloor(string $num): string`.
    ///
    /// # Errors
    /// `ValueError` for a malformed operand.
    pub fn bcfloor(&self, num: &[u8]) -> Result<String, BcError> {
        let n = parse_arg("bcfloor", num, 1, "num")?;
        Ok(BcNum::floor_or_ceil(&n, true).to_string_scaled(0))
    }

    /// `bcceil(string $num): string`.
    ///
    /// # Errors
    /// `ValueError` for a malformed operand.
    pub fn bcceil(&self, num: &[u8]) -> Result<String, BcError> {
        let n = parse_arg("bcceil", num, 1, "num")?;
        Ok(BcNum::floor_or_ceil(&n, false).to_string_scaled(0))
    }

    /// `bcround(string $num, int $precision = 0, RoundingMode $mode = RoundingMode::HalfAwayFromZero): string`.
    ///
    /// # Errors
    /// `ValueError` for a precision above `INT_MAX` or a malformed operand.
    pub fn bcround(
        &self,
        num: &[u8],
        precision: i64,
        mode: RoundingMode,
    ) -> Result<String, BcError> {
        const F: &str = "bcround";
        check_precision(F, precision, 2)?;
        let n = parse_arg(F, num, 1, "num")?;
        check_round_alloc(&n, precision, mode)?;
        let (r, scale) = BcNum::round(&n, precision, mode);
        format_checked(&r, scale)
    }
}

/// Rejects roundings whose result could never be allocated by PHP.
pub(crate) fn check_round_alloc(
    n: &BcNum,
    precision: i64,
    mode: RoundingMode,
) -> Result<(), BcError> {
    if precision >= 0 {
        check_alloc(n.int_len().saturating_add(to_usize(precision)))
    } else {
        let magnitude = usize::try_from(precision.unsigned_abs()).unwrap_or(usize::MAX);
        let tiny = n.int_len() < magnitude;
        let goes_up = !n.is_zero()
            && match mode {
                RoundingMode::AwayFromZero => true,
                RoundingMode::PositiveInfinity => !n.is_neg(),
                RoundingMode::NegativeInfinity => n.is_neg(),
                _ => false,
            };
        if tiny && goes_up {
            check_alloc(magnitude.saturating_add(1))
        } else {
            Ok(())
        }
    }
}

/// `bcmath_check_scale`.
pub(crate) fn check_scale(func: &str, scale: i64, arg_num: u32) -> Result<(), BcError> {
    if (0..=MAX_SCALE).contains(&scale) {
        Ok(())
    } else {
        Err(BcError::argument_value(
            func,
            arg_num,
            "scale",
            &format!("must be between 0 and {MAX_SCALE}"),
        ))
    }
}

/// `bcmath_check_precision`.
pub(crate) fn check_precision(func: &str, precision: i64, arg_num: u32) -> Result<(), BcError> {
    if precision > MAX_SCALE {
        Err(BcError::argument_value(
            func,
            arg_num,
            "precision",
            &format!("must be between {} and {MAX_SCALE}", i64::MIN),
        ))
    } else {
        Ok(())
    }
}

/// `php_str2num` + "is not well-formed".
fn parse_arg(func: &str, s: &[u8], arg_num: u32, name: &str) -> Result<BcNum, BcError> {
    BcNum::parse_auto(s)
        .map(|p| p.num)
        .map_err(|()| BcError::argument_value(func, arg_num, name, "is not well-formed"))
}

fn parse_two(func: &str, num1: &[u8], num2: &[u8]) -> Result<(BcNum, BcNum), BcError> {
    Ok((
        parse_arg(func, num1, 1, "num1")?,
        parse_arg(func, num2, 2, "num2")?,
    ))
}

/// `bcmath_compare_result` as a PHP int.
pub(crate) fn ordering_to_int(o: Ordering) -> i64 {
    match o {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scale_and_bcscale() {
        let mut bc = BcMath::new();
        assert_eq!(bc.bcadd(b"1", b"2.5", None).unwrap(), "3");
        assert_eq!(bc.bcscale(Some(3)).unwrap(), 0);
        assert_eq!(bc.bcadd(b"1", b"2.5", None).unwrap(), "3.500");
        assert_eq!(bc.bcscale(None).unwrap(), 3);
        let e = bc.bcscale(Some(-1)).unwrap_err();
        assert_eq!(
            e.message,
            "bcscale(): Argument #1 ($scale) must be between 0 and 2147483647"
        );
        assert!(!bc.set_ini_scale(-1));
        assert!(!bc.set_ini_scale(MAX_SCALE + 1));
        assert!(bc.set_ini_scale(5));
        assert_eq!(bc.default_scale(), 5);
    }

    #[test]
    fn error_messages() {
        let bc = BcMath::new();
        assert_eq!(
            bc.bcadd(b" 1", b"1", None).unwrap_err().message,
            "bcadd(): Argument #1 ($num1) is not well-formed"
        );
        assert_eq!(
            bc.bcsub(b"1", b"1e5", None).unwrap_err().message,
            "bcsub(): Argument #2 ($num2) is not well-formed"
        );
        assert_eq!(
            bc.bcdiv(b"1", b"0", None).unwrap_err().message,
            "Division by zero"
        );
        assert_eq!(
            bc.bcmod(b"1", b"0.00", None).unwrap_err().message,
            "Modulo by zero"
        );
        assert_eq!(
            bc.bcpow(b"1", b"1.5", None).unwrap_err().message,
            "bcpow(): Argument #2 ($exponent) cannot have a fractional part"
        );
        assert_eq!(
            bc.bcsqrt(b"-1", None).unwrap_err().message,
            "bcsqrt(): Argument #1 ($num) must be greater than or equal to 0"
        );
        assert_eq!(
            bc.bcround(b"1", i64::MAX, RoundingMode::HalfEven).unwrap_err().message,
            "bcround(): Argument #2 ($precision) must be between -9223372036854775808 and 2147483647"
        );
        assert_eq!(
            bc.bcmul(b"0", b"0", Some(MAX_SCALE)).unwrap_err().kind,
            crate::ErrorKind::OutOfMemory
        );
    }
}
