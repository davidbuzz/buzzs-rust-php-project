//! Multiplication, division, powers, roots and rounding.
//!
//! Ports of `libbcmath/src/{recmul,div,divmod,raise,raisemod,sqrt,
//! floor_or_ceil,round}.c`. The digit-level algorithms are replaced by exact
//! big-integer arithmetic ([`crate::bigint`]); everything that influences the
//! visible result — scales, truncation points, sign handling and the special
//! paths for divisors that are powers of ten — follows the C code line by line.

use std::cmp::Ordering;

use crate::bigint;
use crate::num::{BcNum, Sign};

/// The rounding modes of PHP's `RoundingMode` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RoundingMode {
    /// `RoundingMode::HalfAwayFromZero` (the default; `PHP_ROUND_HALF_UP`).
    #[default]
    HalfAwayFromZero,
    /// `RoundingMode::HalfTowardsZero` (`PHP_ROUND_HALF_DOWN`).
    HalfTowardsZero,
    /// `RoundingMode::HalfEven` (`PHP_ROUND_HALF_EVEN`).
    HalfEven,
    /// `RoundingMode::HalfOdd` (`PHP_ROUND_HALF_ODD`).
    HalfOdd,
    /// `RoundingMode::TowardsZero`.
    TowardsZero,
    /// `RoundingMode::AwayFromZero`.
    AwayFromZero,
    /// `RoundingMode::NegativeInfinity` (floor).
    NegativeInfinity,
    /// `RoundingMode::PositiveInfinity` (ceiling).
    PositiveInfinity,
}

impl RoundingMode {
    /// All cases in declaration order.
    pub const ALL: [Self; 8] = [
        Self::HalfAwayFromZero,
        Self::HalfTowardsZero,
        Self::HalfEven,
        Self::HalfOdd,
        Self::TowardsZero,
        Self::AwayFromZero,
        Self::NegativeInfinity,
        Self::PositiveInfinity,
    ];

    /// The enum case name (`RoundingMode::$name`).
    pub fn name(self) -> &'static str {
        match self {
            Self::HalfAwayFromZero => "HalfAwayFromZero",
            Self::HalfTowardsZero => "HalfTowardsZero",
            Self::HalfEven => "HalfEven",
            Self::HalfOdd => "HalfOdd",
            Self::TowardsZero => "TowardsZero",
            Self::AwayFromZero => "AwayFromZero",
            Self::NegativeInfinity => "NegativeInfinity",
            Self::PositiveInfinity => "PositiveInfinity",
        }
    }

    /// Looks a case up by name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == name)
    }
}

/// Failure modes of [`BcNum::raise`] (`bc_raise_status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaiseError {
    /// `BC_RAISE_STATUS_LEN_IS_OVERFLOW`.
    LenOverflow,
    /// `BC_RAISE_STATUS_SCALE_IS_OVERFLOW`.
    ScaleOverflow,
    /// `BC_RAISE_STATUS_FULLLEN_IS_OVERFLOW`.
    FullLenOverflow,
    /// `BC_RAISE_STATUS_DIVIDE_BY_ZERO`: zero raised to a negative power.
    DivideByZero,
    /// The power would have more than [`crate::MAX_DIGITS`] digits; PHP
    /// exhausts its memory limit computing it. Carries the digit estimate.
    TooLarge(usize),
}

/// Failure modes of [`BcNum::raise_mod`] (`raise_mod_status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaiseModError {
    /// The base has fractional digits.
    BaseHasFractional,
    /// The exponent has fractional digits.
    ExpoHasFractional,
    /// The exponent is negative.
    ExpoIsNegative,
    /// The modulus has fractional digits.
    ModHasFractional,
    /// The modulus is zero.
    ModIsZero,
}

impl BcNum {
    /// `bc_multiply`: the exact product truncated to
    /// `min(scale1 + scale2, max(scale, scale1, scale2))` fractional digits.
    pub fn multiply(n1: &Self, n2: &Self, scale: usize) -> Self {
        let full_scale = n1.scale + n2.scale;
        let prod_scale = full_scale.min(scale.max(n1.scale.max(n2.scale)));
        let len1 = n1.len + n1.scale;
        let len2 = n2.len + n2.scale;
        let prodlen = len1 + len2;
        let product = bigint::mul(
            &bigint::from_digits(&n1.digits),
            &bigint::from_digits(&n2.digits),
        );
        let mut prod = Self::new_zeroed(prodlen - full_scale, full_scale);
        bigint::write_digits(&product, &mut prod.digits);
        prod.sign = if n1.sign == n2.sign {
            Sign::Plus
        } else {
            Sign::Minus
        };
        prod.truncate_scale(prod_scale);
        prod.rm_leading_zeros();
        if prod.is_zero() {
            prod.sign = Sign::Plus;
            prod.truncate_scale(0);
        }
        prod
    }

    /// `bc_divide`: the quotient truncated (not rounded) to `scale` digits.
    /// Returns `None` when `divisor` is zero.
    pub fn divide(numerator: &Self, divisor: &Self, scale: usize) -> Option<Self> {
        if divisor.is_zero() {
            return None;
        }
        let quot_scale = scale;
        if numerator.is_zero() {
            return Some(Self::zero());
        }

        // Divisor is 1 or -1: copy the numerator.
        if Self::do_compare(divisor, &Self::one(), divisor.scale, false) == Ordering::Equal {
            let qs = numerator.scale.min(quot_scale);
            let mut quot = Self::new_zeroed(numerator.len, qs);
            quot.digits
                .copy_from_slice(&numerator.digits[..numerator.len + qs]);
            quot.sign = if quot.is_zero() || numerator.sign == divisor.sign {
                Sign::Plus
            } else {
                Sign::Minus
            };
            return Some(quot);
        }

        let num_digit = |i: usize| -> u8 { numerator.digits.get(i).copied().unwrap_or(0) };
        let mut numerator_size = numerator.len + quot_scale + divisor.scale;
        let mut divisor_size = divisor.len + divisor.scale;

        // Strip numerator leading zeros.
        let mut numerator_leading_zeros = 0;
        while num_digit(numerator_leading_zeros) == 0 {
            numerator_leading_zeros += 1;
            if numerator_leading_zeros == numerator_size {
                return Some(Self::zero());
            }
        }
        numerator_size -= numerator_leading_zeros;

        // Strip divisor leading zeros.
        let mut divisor_start = 0;
        while divisor.digits[divisor_start] == 0 {
            divisor_start += 1;
            divisor_size -= 1;
        }

        if divisor_size > numerator_size {
            return Some(Self::zero());
        }

        // Strip divisor trailing zeros (at least one digit remains).
        let divisor_digits = &divisor.digits[divisor_start..divisor_start + divisor_size];
        let mut divisor_trailing_zeros = 0;
        for i in (1..divisor_size).rev() {
            if divisor_digits[i] != 0 {
                break;
            }
            divisor_trailing_zeros += 1;
        }
        divisor_size -= divisor_trailing_zeros;
        numerator_size -= divisor_trailing_zeros;
        let divisor_digits = &divisor_digits[..divisor_size];

        let quot_size = numerator_size - divisor_size + 1;
        let mut quot = if quot_size > quot_scale {
            Self::new_zeroed(quot_size - quot_scale, quot_scale)
        } else {
            Self::new_zeroed(1, quot_scale)
        };

        let numerator_readable_size = numerator.len + numerator.scale - numerator_leading_zeros;
        let numerator_digits = &numerator.digits[numerator_leading_zeros..];

        let result_sign = if numerator.sign == divisor.sign {
            Sign::Plus
        } else {
            Sign::Minus
        };

        if divisor_size == 1 && divisor_digits[0] == 1 {
            divide_by_pow_10(
                numerator_digits,
                numerator_readable_size,
                &mut quot,
                quot_size,
                quot_scale,
            );
            quot.sign = if quot.is_zero() {
                Sign::Plus
            } else {
                result_sign
            };
            return Some(quot);
        }

        // General case: integer division of the leading `numerator_size`
        // digits (zero padded) by the stripped divisor.
        let use_size = numerator_size.min(numerator_readable_size);
        let mut n_digits = numerator_digits[..use_size].to_vec();
        n_digits.resize(numerator_size, 0);
        let q = bigint::div(
            &bigint::from_digits(&n_digits),
            &bigint::from_digits(divisor_digits),
        );
        bigint::write_digits(&q, &mut quot.digits);

        quot.rm_leading_zeros();
        if quot.is_zero() {
            quot.sign = Sign::Plus;
            quot.truncate_scale(0);
        } else {
            quot.sign = result_sign;
        }
        Some(quot)
    }

    /// `bc_divmod`: truncated quotient (scale 0) and remainder
    /// `num1 - quot * num2` with `scale` fractional digits.
    /// Returns `None` when `num2` is zero.
    pub fn divmod(num1: &Self, num2: &Self, scale: usize) -> Option<(Self, Self)> {
        if num2.is_zero() {
            return None;
        }
        let rscale = num1.scale.max(num2.scale + scale);
        let quotient = Self::divide(num1, num2, 0)?;
        let temp = Self::multiply(&quotient, num2, rscale);
        let mut rem = Self::sub(num1, &temp, rscale);
        rem.truncate_scale(scale);
        if rem.is_zero() {
            rem.sign = Sign::Plus;
            rem.truncate_scale(0);
        }
        Some((quotient, rem))
    }

    /// `bc_modulo`. Returns `None` when `num2` is zero.
    pub fn modulo(num1: &Self, num2: &Self, scale: usize) -> Option<Self> {
        Self::divmod(num1, num2, scale).map(|(_, rem)| rem)
    }

    /// `bc_raise`: `base ** exponent`.
    ///
    /// For positive exponents the exact power is truncated to
    /// `min(scale, base_scale * exponent)` digits; negative exponents compute
    /// `1 / base ** |exponent|` truncated to `scale` digits.
    ///
    /// # Errors
    /// See [`RaiseError`].
    pub fn raise(base: &Self, exponent: i64, scale: usize) -> Result<Self, RaiseError> {
        if exponent == 0 {
            return Ok(Self::one());
        }
        let is_neg = exponent < 0;
        let exp = exponent.unsigned_abs();
        if base.is_zero() {
            return if is_neg {
                Err(RaiseError::DivideByZero)
            } else {
                Ok(Self::zero())
            };
        }
        let exp_usize = usize::try_from(exp).unwrap_or(usize::MAX);
        let power_len = base
            .len
            .checked_mul(exp_usize)
            .ok_or(RaiseError::LenOverflow)?;
        let power_scale = base
            .scale
            .checked_mul(exp_usize)
            .ok_or(RaiseError::ScaleOverflow)?;
        power_len
            .checked_add(power_scale)
            .ok_or(RaiseError::FullLenOverflow)?;
        let hint = power_size_hint(base, exp);
        if hint > crate::MAX_DIGITS {
            return Err(RaiseError::TooLarge(hint));
        }

        let power_sign = if base.sign == Sign::Minus && exp & 1 == 1 {
            Sign::Minus
        } else {
            Sign::Plus
        };

        let mut power = if base.digits.iter().filter(|&&d| d != 0).count() == 1
            && base.digits.iter().any(|&d| d == 1)
        {
            // |base| is a power of ten: avoid exponentiating digit by digit.
            let one_pos = base.digits.iter().position(|&d| d == 1).unwrap_or(0);
            // value = 10^(len - 1 - one_pos); the power as an integer with
            // power_scale implied fractional digits.
            let zeros_after = (base.digits.len() - 1 - one_pos)
                .checked_mul(exp_usize)
                .ok_or(RaiseError::FullLenOverflow)?;
            build_power_from_pow10(zeros_after, power_scale)
        } else {
            let p = bigint::pow(&bigint::from_digits(&base.digits), exp);
            let count = bigint::digit_count(&p);
            let total = count.max(power_scale + 1);
            let mut power = Self::new_zeroed(total - power_scale, power_scale);
            bigint::write_digits(&p, &mut power.digits);
            power
        };
        power.rm_leading_zeros();
        if power.is_zero() {
            power.sign = Sign::Plus;
            power.truncate_scale(0);
        } else {
            power.sign = power_sign;
        }

        if is_neg {
            Self::divide(&Self::one(), &power, scale).ok_or(RaiseError::DivideByZero)
        } else {
            power.truncate_scale(scale);
            if power.is_zero() {
                power.sign = Sign::Plus;
            }
            Ok(power)
        }
    }

    /// `bc_raisemod`: `base ** expo % modulus` for integers.
    ///
    /// # Errors
    /// See [`RaiseModError`].
    pub fn raise_mod(
        base: &Self,
        expo: &Self,
        modulus: &Self,
        scale: usize,
    ) -> Result<Self, RaiseModError> {
        if base.scale != 0 {
            return Err(RaiseModError::BaseHasFractional);
        }
        if expo.scale != 0 {
            return Err(RaiseModError::ExpoHasFractional);
        }
        if expo.is_neg() {
            return Err(RaiseModError::ExpoIsNegative);
        }
        if modulus.scale != 0 {
            return Err(RaiseModError::ModHasFractional);
        }
        if modulus.is_zero() {
            return Err(RaiseModError::ModIsZero);
        }
        if Self::do_compare(modulus, &Self::one(), modulus.scale, false) == Ordering::Equal {
            return Ok(Self::new_zeroed(1, scale));
        }
        let two = Self::two();
        let mut power = base.clone();
        let mut exponent = expo.clone();
        let mut temp = Self::one();
        while !exponent.is_zero() {
            let (q, parity) = Self::divmod(&exponent, &two, 0).unwrap_or_default();
            exponent = q;
            if !parity.is_zero() {
                temp = Self::multiply(&temp, &power, scale);
                temp = Self::modulo(&temp, modulus, scale).unwrap_or_default();
            }
            power = Self::multiply(&power, &power, scale);
            power = Self::modulo(&power, modulus, scale).unwrap_or_default();
        }
        Ok(temp)
    }

    /// `bc_int2num`.
    pub(crate) fn from_int(v: i32) -> Self {
        Self::from_i64(i64::from(v))
    }

    /// `bc_sqrt`: Newton's iteration producing libbcmath's exact digits.
    /// Returns `None` for negative input.
    pub fn sqrt(num: &Self, scale: usize) -> Option<Self> {
        if num.is_neg() {
            return None;
        }
        if num.is_zero() {
            return Some(Self::zero());
        }
        let one = Self::one();
        let num_cmp_one = Self::compare(num, &one, num.scale);
        if num_cmp_one == Ordering::Equal {
            return Some(one);
        }
        let rscale = scale.max(num.scale);
        let mut point5 = Self::new_zeroed(1, 1);
        point5.digits[1] = 5;

        let (mut guess, mut cscale) = if num_cmp_one == Ordering::Less {
            (one.clone(), num.scale)
        } else {
            let ten = Self::from_int(10);
            let len = i32::try_from(num.len).unwrap_or(i32::MAX);
            let mut guess1 = Self::multiply(&Self::from_int(len), &point5, 0);
            guess1.truncate_scale(0);
            // bc_raise_bc_exponent(guess=10, guess1, 0)
            let guess = Self::raise(&ten, guess1.to_i64(), 0).unwrap_or_else(|_| one.clone());
            (guess, 3)
        };

        loop {
            let guess1 = guess.clone();
            guess = Self::divide(num, &guess, cscale).unwrap_or_default();
            guess = Self::add(&guess, &guess1, 0);
            guess = Self::multiply(&guess, &point5, cscale);
            let diff = Self::sub(&guess, &guess1, cscale + 1);
            if diff.is_near_zero(cscale) {
                if cscale < rscale + 1 {
                    cscale = (cscale * 3).min(rscale + 1);
                } else {
                    break;
                }
            }
        }
        Self::divide(&guess, &one, rscale)
    }

    /// `bc_floor_or_ceil`: the integer part rounded towards negative
    /// (`is_floor`) or positive infinity.
    pub fn floor_or_ceil(num: &Self, is_floor: bool) -> Self {
        let mut result = Self::new_zeroed(num.len, 0);
        result.sign = num.sign;
        result.digits.copy_from_slice(&num.digits[..num.len]);
        let skip = num.scale == 0 || result.sign == if is_floor { Sign::Plus } else { Sign::Minus };
        if !skip && num.digits[num.len..].iter().any(|&d| d != 0) {
            let mut tmp = Self::do_add(&result, &Self::one());
            tmp.sign = result.sign;
            result = tmp;
        }
        if result.is_zero() {
            result.sign = Sign::Plus;
        }
        result
    }

    /// `bc_round`: rounds to `precision` fractional digits (negative values
    /// round to tens, hundreds, ...). Returns the rounded number and the
    /// scale it must be printed with.
    pub fn round(num: &Self, precision: i64, mode: RoundingMode) -> (Self, usize) {
        use RoundingMode as M;
        // Rounding to a place left of the most significant digit.
        if precision < 0 && (num.len as u64) < precision.unsigned_abs() {
            match mode {
                M::HalfAwayFromZero
                | M::HalfTowardsZero
                | M::HalfEven
                | M::HalfOdd
                | M::TowardsZero => return (Self::zero(), 0),
                M::PositiveInfinity if num.sign == Sign::Minus => return (Self::zero(), 0),
                M::NegativeInfinity if num.sign == Sign::Plus => return (Self::zero(), 0),
                _ => {}
            }
            if num.is_zero() {
                return (Self::zero(), 0);
            }
            let magnitude = usize::try_from(precision.unsigned_abs()).unwrap_or(usize::MAX);
            let mut result = Self::new_zeroed(magnitude + 1, 0);
            result.digits[0] = 1;
            result.sign = num.sign;
            return (result, 0);
        }

        if precision >= 0 {
            let p = usize::try_from(precision).unwrap_or(usize::MAX);
            if num.scale <= p {
                let mut result = Self::new_zeroed(num.len, p);
                result.sign = num.sign;
                result.digits[..num.len + num.scale].copy_from_slice(&num.digits);
                return (result, p);
            }
        }

        // rounded_len >= 0 is guaranteed by the early returns above.
        let rounded_len = if precision >= 0 {
            num.len + usize::try_from(precision).unwrap_or(0)
        } else {
            num.len - usize::try_from(precision.unsigned_abs()).unwrap_or(0)
        };
        let mut result = if rounded_len == 0 {
            Self::new_zeroed(1, 0)
        } else {
            let mut r = Self::new_zeroed(num.len, usize::try_from(precision.max(0)).unwrap_or(0));
            r.digits[..rounded_len].copy_from_slice(&num.digits[..rounded_len]);
            r
        };
        result.sign = num.sign;

        let next = num.digits[rounded_len];
        let rest_nonzero = || num.digits[rounded_len + 1..].iter().any(|&d| d != 0);
        let prev_digit = || {
            if rounded_len == 0 {
                None
            } else {
                Some(num.digits[rounded_len - 1])
            }
        };

        let up = match mode {
            M::HalfAwayFromZero => next >= 5,
            M::HalfTowardsZero | M::HalfEven | M::HalfOdd => match next.cmp(&5) {
                Ordering::Greater => true,
                Ordering::Less => false,
                Ordering::Equal => {
                    rest_nonzero()
                        || match mode {
                            M::HalfEven => !(prev_digit().is_none_or(|d| d % 2 == 0)),
                            M::HalfOdd => !(prev_digit().is_some_and(|d| d % 2 == 1)),
                            _ => false,
                        }
                }
            },
            M::PositiveInfinity => num.sign == Sign::Plus && (next > 0 || rest_nonzero()),
            M::NegativeInfinity => num.sign == Sign::Minus && (next > 0 || rest_nonzero()),
            M::TowardsZero => false,
            M::AwayFromZero => next > 0 || rest_nonzero(),
        };

        if up {
            let tmp = if rounded_len == 0 {
                let mut t = Self::new_zeroed(num.len + 1, 0);
                t.digits[0] = 1;
                t.sign = num.sign;
                t
            } else {
                let mut scaled_one = Self::new_zeroed(result.len, result.scale);
                scaled_one.digits[rounded_len - 1] = 1;
                let mut t = Self::do_add(&result, &scaled_one);
                t.sign = result.sign;
                t
            };
            result = tmp;
        }

        let scale = result.scale;
        if result.is_zero() {
            result.sign = Sign::Plus;
            result.truncate_scale(0);
        }
        (result, scale)
    }
}

/// Estimated number of digits of `base ** exponent` (`exponent > 0`), used
/// to refuse computations PHP could never complete.
fn power_size_hint(base: &BcNum, exponent: u64) -> usize {
    let exp = usize::try_from(exponent).unwrap_or(usize::MAX);
    let digits = base.digits();
    let scale_digits = base.scale().saturating_mul(exp);
    let Some(first) = digits.iter().position(|&d| d != 0) else {
        return 1;
    };
    let is_pow10 = digits[first] == 1 && digits[first + 1..].iter().all(|&d| d == 0);
    let grow = if is_pow10 {
        digits.len() - 1 - first
    } else {
        digits.len() - first
    };
    grow.saturating_mul(exp).max(scale_digits).saturating_add(1)
}

/// Builds `10^zeros` as a number with `power_scale` fractional digits
/// (before leading-zero removal), matching the layout `bc_raise` produces.
fn build_power_from_pow10(zeros: usize, power_scale: usize) -> BcNum {
    let count = zeros + 1;
    let total = count.max(power_scale + 1);
    let mut power = BcNum::new_zeroed(total - power_scale, power_scale);
    let idx = total - count;
    power.digits[idx] = 1;
    power
}

/// `bc_divide_by_pow_10`.
fn divide_by_pow_10(
    numerator: &[u8],
    numerator_readable_size: usize,
    quot: &mut BcNum,
    quot_size: usize,
    quot_scale: usize,
) {
    let mut q = 0;
    if quot_size <= quot_scale {
        let zeros = quot_scale - quot_size + 1;
        quot.digits[..zeros].fill(0);
        q = zeros;
    }
    let use_size = quot_size.min(numerator_readable_size);
    quot.digits[q..q + use_size].copy_from_slice(&numerator[..use_size]);
    q += use_size;
    if use_size < quot.len {
        // e.g. 12.3 / 0.01 <=> 1230
        quot.digits[q..quot.len].fill(0);
        quot.truncate_scale(0);
    } else {
        let qend = quot.len + quot.scale;
        quot.truncate_scale(quot.scale - (qend - q));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> BcNum {
        BcNum::parse_auto(s.as_bytes()).expect("valid").num
    }

    #[test]
    fn multiply_scale_rules() {
        let r = BcNum::multiply(&p("1.25"), &p("1.5"), 0);
        assert_eq!(r.scale(), 2);
        assert_eq!(r.to_string_scaled(3), "1.870");
        let r = BcNum::multiply(&p("1.25"), &p("1.5"), 5);
        assert_eq!(r.to_string_scaled(5), "1.87500");
        assert_eq!(
            BcNum::multiply(&p("-0.01"), &p("0.01"), 2).to_string_scaled(2),
            "0.00"
        );
    }

    #[test]
    fn division() {
        assert_eq!(
            BcNum::divide(&p("1"), &p("3"), 5)
                .unwrap()
                .to_string_scaled(5),
            "0.33333"
        );
        assert_eq!(
            BcNum::divide(&p("-7"), &p("2"), 0)
                .unwrap()
                .to_string_scaled(0),
            "-3"
        );
        assert_eq!(
            BcNum::divide(&p("12.3"), &p("0.01"), 0)
                .unwrap()
                .to_string_scaled(0),
            "1230"
        );
        assert_eq!(
            BcNum::divide(&p("1"), &p("1000"), 4)
                .unwrap()
                .to_string_scaled(4),
            "0.0010"
        );
        assert!(BcNum::divide(&p("1"), &p("0.000"), 4).is_none());
    }

    #[test]
    fn modulo() {
        assert_eq!(
            BcNum::modulo(&p("10"), &p("3"), 0)
                .unwrap()
                .to_string_scaled(0),
            "1"
        );
        assert_eq!(
            BcNum::modulo(&p("-10"), &p("3"), 0)
                .unwrap()
                .to_string_scaled(0),
            "-1"
        );
        assert_eq!(
            BcNum::modulo(&p("5.7"), &p("1.3"), 1)
                .unwrap()
                .to_string_scaled(1),
            "0.5"
        );
    }

    #[test]
    fn powers() {
        assert_eq!(
            BcNum::raise(&p("2"), 64, 0).unwrap().to_string_scaled(0),
            "18446744073709551616"
        );
        assert_eq!(
            BcNum::raise(&p("1.5"), -3, 10)
                .unwrap()
                .to_string_scaled(10),
            "0.2962962962"
        );
        assert_eq!(BcNum::raise(&p("0"), -1, 0), Err(RaiseError::DivideByZero));
        assert_eq!(
            BcNum::raise(&p("-0.1"), 3, 3).unwrap().to_string_scaled(3),
            "-0.001"
        );
        assert_eq!(
            BcNum::raise(&p("100"), 3, 0).unwrap().to_string_scaled(0),
            "1000000"
        );
    }

    #[test]
    fn square_roots() {
        assert_eq!(
            BcNum::sqrt(&p("2"), 30).unwrap().to_string_scaled(30),
            "1.414213562373095048801688724209"
        );
        assert_eq!(
            BcNum::sqrt(&p("0.25"), 2).unwrap().to_string_scaled(2),
            "0.50"
        );
        assert!(BcNum::sqrt(&p("-4"), 2).is_none());
    }

    #[test]
    fn rounding() {
        let r = |s: &str, prec: i64, m: RoundingMode| {
            let (n, scale) = BcNum::round(&p(s), prec, m);
            n.to_string_scaled(scale)
        };
        assert_eq!(r("-1235.5", 0, RoundingMode::HalfEven), "-1236");
        assert_eq!(r("1234.5", 0, RoundingMode::HalfEven), "1234");
        assert_eq!(r("0.05", -1, RoundingMode::AwayFromZero), "10");
        assert_eq!(r("1.5", 3, RoundingMode::HalfAwayFromZero), "1.500");
        assert_eq!(r("0.01", -3, RoundingMode::PositiveInfinity), "1000");
        assert_eq!(r("-0.5", 0, RoundingMode::HalfTowardsZero), "0");
    }
}
