//! The `bc_num` representation and its basic operations.
//!
//! Ports of `libbcmath/src/{init,str2num,num2str,compare,zero,nearzero,rmzero,
//! doaddsub,add,sub,long2num,num2long,int2num}.c`.

use std::cmp::Ordering;

/// Sign of a [`BcNum`] (`sign` in libbcmath).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sign {
    /// Non-negative.
    Plus,
    /// Negative.
    Minus,
}

/// An arbitrary-precision decimal number laid out exactly like libbcmath's
/// `bc_struct`: a sign, `len` integer digits and `scale` fractional digits,
/// one decimal digit (`0..=9`) per byte, most significant first.
///
/// The integer part always has at least one digit. Numbers produced by the
/// arithmetic in this crate never have redundant leading integer zeros, but
/// fractional trailing zeros are significant for formatting and scale
/// propagation exactly as in PHP.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BcNum {
    pub(crate) sign: Sign,
    pub(crate) len: usize,
    pub(crate) scale: usize,
    pub(crate) digits: Vec<u8>,
}

/// Result of parsing a numeric string with [`BcNum::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// The parsed number (trailing fractional zeros removed).
    pub num: BcNum,
    /// Number of fractional digits written in the string, *including*
    /// trailing zeros (`full_scale` in libbcmath).
    pub full_scale: usize,
}

impl Default for BcNum {
    fn default() -> Self {
        Self::zero()
    }
}

impl BcNum {
    /// Allocates a zero-filled number (`bc_new_num`).
    pub(crate) fn new_zeroed(len: usize, scale: usize) -> Self {
        Self {
            sign: Sign::Plus,
            len,
            scale,
            digits: vec![0; len + scale],
        }
    }

    /// The constant `0` (`BCG(_zero_)`).
    pub fn zero() -> Self {
        Self::new_zeroed(1, 0)
    }

    /// The constant `1` (`BCG(_one_)`).
    pub fn one() -> Self {
        let mut n = Self::new_zeroed(1, 0);
        n.digits[0] = 1;
        n
    }

    /// The constant `2` (`BCG(_two_)`).
    pub(crate) fn two() -> Self {
        let mut n = Self::new_zeroed(1, 0);
        n.digits[0] = 2;
        n
    }

    /// The sign.
    pub fn sign(&self) -> Sign {
        self.sign
    }

    /// Whether the sign is negative (`bc_is_neg`). Note that libbcmath can
    /// carry a negative sign on a value that is zero at a given scale.
    pub fn is_neg(&self) -> bool {
        self.sign == Sign::Minus
    }

    /// Number of integer digits (`n_len`).
    pub fn int_len(&self) -> usize {
        self.len
    }

    /// Number of fractional digits stored (`n_scale`).
    pub fn scale(&self) -> usize {
        self.scale
    }

    /// The stored digits (`n_len + n_scale` of them, values `0..=9`).
    pub fn digits(&self) -> &[u8] {
        &self.digits
    }

    /// Lowers the stored scale, discarding (truncating) fractional digits.
    /// Requests for a larger scale are ignored, mirroring the
    /// `n->n_scale = MIN(scale, n->n_scale)` idiom of the C code.
    pub(crate) fn truncate_scale(&mut self, scale: usize) {
        if scale < self.scale {
            self.scale = scale;
            self.digits.truncate(self.len + scale);
        }
    }

    /// Returns a copy with the sign flipped.
    #[must_use]
    pub fn negate(&self) -> Self {
        let mut n = self.clone();
        n.sign = match n.sign {
            Sign::Plus => Sign::Minus,
            Sign::Minus => Sign::Plus,
        };
        n
    }

    /// Returns a copy with a positive sign.
    #[must_use]
    pub fn abs(&self) -> Self {
        let mut n = self.clone();
        n.sign = Sign::Plus;
        n
    }

    // ----------------------------------------------------------------------
    // str2num / num2str
    // ----------------------------------------------------------------------

    /// Parses a numeric string exactly like `bc_str2num`.
    ///
    /// Accepted syntax: an optional `+`/`-`, decimal digits, and an optional
    /// `.` followed by digits. Empty integer and/or fractional parts are
    /// allowed (`""`, `"-"`, `"."`, `"1."`, `".5"` are all valid). Anything
    /// else — whitespace, exponents, embedded NUL bytes — is rejected.
    ///
    /// With `auto_scale` the full fractional precision is kept; otherwise the
    /// fraction is truncated to `scale` digits. Trailing fractional zeros are
    /// always dropped from the stored number but counted in
    /// [`Parsed::full_scale`].
    ///
    /// # Errors
    /// Returns `Err(())` for a string that is not well-formed.
    #[allow(clippy::result_unit_err)]
    pub fn parse(s: &[u8], scale: usize, auto_scale: bool) -> Result<Parsed, ()> {
        // The C code relies on the terminating NUL of zend_string.
        let at = |i: usize| -> u8 { s.get(i).copied().unwrap_or(0) };
        let end = s.len();
        let count_digits = |mut i: usize| -> usize {
            while at(i).is_ascii_digit() {
                i += 1;
            }
            i
        };
        let skip_zero_reverse = |mut i: usize, stop: usize| -> usize {
            while i > stop && at(i - 1) == b'0' {
                i -= 1;
            }
            i
        };

        let mut ptr = 0;
        if at(0) == b'+' || at(0) == b'-' {
            ptr += 1;
        }
        while at(ptr) == b'0' {
            ptr += 1;
        }
        let integer_ptr = ptr;
        ptr = count_digits(ptr);
        let mut digits = ptr - integer_ptr;
        let decimal_point = at(ptr) == b'.' && ptr < end;

        if !decimal_point && ptr != end {
            return Err(());
        }

        let mut full_scale = 0;
        let mut str_scale = 0;
        let mut fractional_ptr = 0;
        let mut fractional_end = 0;
        if decimal_point {
            fractional_ptr = ptr + 1;
            fractional_end = fractional_ptr;
            if fractional_ptr != end {
                fractional_end = count_digits(fractional_ptr);
                if fractional_end != end {
                    return Err(());
                }
                full_scale = fractional_end - fractional_ptr;
                fractional_end = skip_zero_reverse(fractional_end, fractional_ptr);
                str_scale = fractional_end - fractional_ptr;
                if str_scale > scale && !auto_scale {
                    fractional_end -= str_scale - scale;
                    str_scale = scale;
                    if str_scale > 0 {
                        let new_end = skip_zero_reverse(fractional_end, fractional_ptr);
                        str_scale -= fractional_end - new_end;
                        fractional_end = new_end;
                    }
                }
            }
        }

        if digits + str_scale == 0 {
            return Ok(Parsed {
                num: Self::zero(),
                full_scale,
            });
        }

        let zero_int = digits == 0;
        if zero_int {
            digits = 1;
        }
        let mut out = Vec::with_capacity(digits + str_scale);
        if zero_int {
            out.push(0);
        } else {
            out.extend(
                s[integer_ptr..integer_ptr + digits]
                    .iter()
                    .map(|b| b - b'0'),
            );
        }
        if str_scale > 0 {
            out.extend(s[fractional_ptr..fractional_end].iter().map(|b| b - b'0'));
        }
        Ok(Parsed {
            num: Self {
                sign: if at(0) == b'-' {
                    Sign::Minus
                } else {
                    Sign::Plus
                },
                len: digits,
                scale: str_scale,
                digits: out,
            },
            full_scale,
        })
    }

    /// Parses with automatic scale (the `php_str2num` helper of bcmath.c).
    ///
    /// # Errors
    /// Returns `Err(())` for a string that is not well-formed.
    #[allow(clippy::result_unit_err)]
    pub fn parse_auto(s: &[u8]) -> Result<Parsed, ()> {
        Self::parse(s, 0, true)
    }

    /// Formats the number with exactly `scale` fractional digits
    /// (`bc_num2str_ex`): extra digits are truncated, missing ones padded
    /// with zeros, and a negative sign is dropped when the printed value is
    /// zero.
    pub fn to_string_scaled(&self, scale: usize) -> String {
        let min_scale = self.scale.min(scale);
        let signch = self.sign != Sign::Plus && !self.is_zero_for_scale(min_scale);
        let mut out = String::with_capacity(self.len + scale + 2);
        if signch {
            out.push('-');
        }
        out.extend(
            self.digits[..self.len]
                .iter()
                .map(|&d| char::from(b'0' + d)),
        );
        if scale > 0 {
            out.push('.');
            out.extend(
                self.digits[self.len..self.len + min_scale]
                    .iter()
                    .map(|&d| char::from(b'0' + d)),
            );
            out.extend(std::iter::repeat_n('0', scale - min_scale));
        }
        out
    }

    // ----------------------------------------------------------------------
    // zero tests
    // ----------------------------------------------------------------------

    /// `bc_is_zero_for_scale`: whether the integer part and the first
    /// `scale` fractional digits are all zero.
    pub fn is_zero_for_scale(&self, scale: usize) -> bool {
        let count = self.len + scale.min(self.scale);
        self.digits[..count].iter().all(|&d| d == 0)
    }

    /// `bc_is_zero`.
    pub fn is_zero(&self) -> bool {
        self.is_zero_for_scale(self.scale)
    }

    /// `bc_is_near_zero`: all digits up to `scale` are zero except possibly a
    /// final `1`.
    pub fn is_near_zero(&self, scale: usize) -> bool {
        let count = self.len + scale.min(self.scale);
        let d = &self.digits[..count];
        match d.iter().position(|&x| x != 0) {
            None => true,
            Some(p) => p == count - 1 && d[p] == 1,
        }
    }

    /// `_bc_rm_leading_zeros`.
    pub(crate) fn rm_leading_zeros(&mut self) {
        let mut n = 0;
        while self.len - n > 1 && self.digits[n] == 0 {
            n += 1;
        }
        if n > 0 {
            self.digits.drain(..n);
            self.len -= n;
        }
    }

    /// `bc_rm_trailing_zeros`: lowers the scale past trailing fractional zeros.
    pub fn rm_trailing_zeros(&mut self) {
        while self.scale > 0 && self.digits[self.len + self.scale - 1] == 0 {
            self.scale -= 1;
        }
        self.digits.truncate(self.len + self.scale);
    }

    // ----------------------------------------------------------------------
    // comparison
    // ----------------------------------------------------------------------

    /// `_bc_do_compare`.
    pub(crate) fn do_compare(n1: &Self, n2: &Self, scale: usize, use_sign: bool) -> Ordering {
        let flip = |ord: Ordering| -> Ordering {
            if !use_sign || n1.sign == Sign::Plus {
                ord
            } else {
                ord.reverse()
            }
        };
        if use_sign && n1.sign != n2.sign {
            if (n1.scale > scale || n2.scale > scale)
                && n1.len == 1
                && n2.len == 1
                && n1.digits[0] == 0
                && n2.digits[0] == 0
                && n1.is_zero_for_scale(scale)
                && n2.is_zero_for_scale(scale)
            {
                return Ordering::Equal;
            }
            return if n1.sign == Sign::Plus {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        if n1.len != n2.len {
            return flip(n1.len.cmp(&n2.len));
        }
        let n1_scale = n1.scale.min(scale);
        let n2_scale = n2.scale.min(scale);
        let count = n1.len + n1_scale.min(n2_scale);
        let ord = n1.digits[..count].cmp(&n2.digits[..count]);
        if ord != Ordering::Equal {
            return flip(ord);
        }
        if n1_scale > n2_scale {
            if n1.digits[count..n1.len + n1_scale].iter().any(|&d| d != 0) {
                return flip(Ordering::Greater);
            }
        } else if n2_scale > n1_scale && n2.digits[count..n2.len + n2_scale].iter().any(|&d| d != 0)
        {
            return flip(Ordering::Less);
        }
        Ordering::Equal
    }

    /// `bc_compare`: signed comparison considering `scale` fractional digits.
    pub fn compare(n1: &Self, n2: &Self, scale: usize) -> Ordering {
        Self::do_compare(n1, n2, scale, true)
    }

    // ----------------------------------------------------------------------
    // addition and subtraction
    // ----------------------------------------------------------------------

    /// `_bc_do_add`: magnitude addition, result scale `max(scale1, scale2)`.
    pub(crate) fn do_add(n1: &Self, n2: &Self) -> Self {
        let sum_scale = n1.scale.max(n2.scale);
        let sum_len = n1.len.max(n2.len) + 1;
        let mut sum = Self::new_zeroed(sum_len, sum_scale);
        let mut carry = 0u8;
        // Walk positions from the least significant fractional digit.
        for k in 0..sum_len + sum_scale {
            // Position relative to the decimal point: frac index or int index.
            let a = digit_at(n1, sum_scale, k);
            let b = digit_at(n2, sum_scale, k);
            let mut v = a + b + carry;
            if v >= 10 {
                v -= 10;
                carry = 1;
            } else {
                carry = 0;
            }
            let idx = sum_len + sum_scale - 1 - k;
            sum.digits[idx] = v;
        }
        sum.rm_leading_zeros();
        sum
    }

    /// `_bc_do_sub`: magnitude subtraction `|n1| - |n2|`, requires `|n1| >= |n2|`.
    pub(crate) fn do_sub(n1: &Self, n2: &Self) -> Self {
        let diff_scale = n1.scale.max(n2.scale);
        let diff_len = n1.len.max(n2.len);
        let mut diff = Self::new_zeroed(diff_len, diff_scale);
        let mut borrow = 0i8;
        for k in 0..diff_len + diff_scale {
            let a = i8::try_from(digit_at(n1, diff_scale, k)).unwrap_or(0);
            let b = i8::try_from(digit_at(n2, diff_scale, k)).unwrap_or(0);
            let mut v = a - b - borrow;
            if v < 0 {
                v += 10;
                borrow = 1;
            } else {
                borrow = 0;
            }
            diff.digits[diff_len + diff_scale - 1 - k] = u8::try_from(v).unwrap_or(0);
        }
        debug_assert_eq!(borrow, 0, "_bc_do_sub requires |n1| >= |n2|");
        diff.rm_leading_zeros();
        diff
    }

    /// `bc_add`: signed addition; `scale_min` is the minimum result scale
    /// (only used when the operands cancel out).
    pub fn add(n1: &Self, n2: &Self, scale_min: usize) -> Self {
        if n1.sign == n2.sign {
            let mut sum = Self::do_add(n1, n2);
            sum.sign = n1.sign;
            return sum;
        }
        match Self::do_compare(n1, n2, scale_min, false) {
            Ordering::Less => {
                let mut sum = Self::do_sub(n2, n1);
                sum.sign = n2.sign;
                sum
            }
            Ordering::Equal => Self::new_zeroed(1, scale_min.max(n1.scale.max(n2.scale))),
            Ordering::Greater => {
                let mut sum = Self::do_sub(n1, n2);
                sum.sign = n1.sign;
                sum
            }
        }
    }

    /// `bc_sub`: signed subtraction `n1 - n2`.
    pub fn sub(n1: &Self, n2: &Self, scale_min: usize) -> Self {
        if n1.sign != n2.sign {
            let mut diff = Self::do_add(n1, n2);
            diff.sign = n1.sign;
            return diff;
        }
        match Self::do_compare(n1, n2, scale_min, false) {
            Ordering::Less => {
                let mut diff = Self::do_sub(n2, n1);
                diff.sign = if n2.sign == Sign::Plus {
                    Sign::Minus
                } else {
                    Sign::Plus
                };
                diff
            }
            Ordering::Equal => Self::new_zeroed(1, scale_min.max(n1.scale.max(n2.scale))),
            Ordering::Greater => {
                let mut diff = Self::do_sub(n1, n2);
                diff.sign = n1.sign;
                diff
            }
        }
    }

    // ----------------------------------------------------------------------
    // integer conversions
    // ----------------------------------------------------------------------

    /// `bc_long2num`.
    pub fn from_i64(v: i64) -> Self {
        if v == 0 {
            return Self::zero();
        }
        let mag = v.unsigned_abs();
        let digits: Vec<u8> = mag.to_string().bytes().map(|b| b - b'0').collect();
        Self {
            sign: if v < 0 { Sign::Minus } else { Sign::Plus },
            len: digits.len(),
            scale: 0,
            digits,
        }
    }

    /// `bc_num2long`: the integer part as an `i64`, or `0` when it does not
    /// fit (callers detect overflow by checking for a non-zero number).
    pub fn to_i64(&self) -> i64 {
        let mut val: i64 = 0;
        for &d in &self.digits[..self.len] {
            let d = i64::from(d);
            if val > i64::MAX / 10 {
                return 0;
            }
            val *= 10;
            if val > i64::MAX - d {
                return 0;
            }
            val += d;
        }
        if self.sign == Sign::Plus {
            val
        } else {
            -val
        }
    }

    /// Whether `to_i64()` returning 0 means "too large" rather than zero:
    /// the `exponent == 0 && (n_len > 1 || n_value[0] != 0)` test of bcmath.c.
    pub(crate) fn int_part_nonzero(&self) -> bool {
        self.len > 1 || self.digits[0] != 0
    }
}

/// Digit of `n` at position `k` counted from the least significant digit of
/// a layout with `scale` fractional digits (zero outside `n`).
fn digit_at(n: &BcNum, scale: usize, k: usize) -> u8 {
    // Index of the digit relative to the decimal point: positions
    // 0..scale are fractional (from the right), then integer digits.
    if k < scale {
        let frac_idx = scale - 1 - k; // 0-based fractional digit index
        if frac_idx < n.scale {
            n.digits[n.len + frac_idx]
        } else {
            0
        }
    } else {
        let int_from_right = k - scale; // 0 = units digit
        if int_from_right < n.len {
            n.digits[n.len - 1 - int_from_right]
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> BcNum {
        BcNum::parse_auto(s.as_bytes()).expect("valid").num
    }

    #[test]
    fn parse_accepts_php_syntax() {
        for (s, out, full) in [
            ("", "0", 0),
            ("-", "0", 0),
            ("+", "0", 0),
            (".", "0", 0),
            ("1.", "1", 0),
            (".5", "0.5", 1),
            ("-.50", "-0.5", 2),
            ("007.100", "7.1", 3),
            ("-0.000", "0", 3),
            ("+12", "12", 0),
        ] {
            let parsed = BcNum::parse_auto(s.as_bytes()).expect(s);
            assert_eq!(parsed.num.to_string_scaled(parsed.num.scale()), out, "{s}");
            assert_eq!(parsed.full_scale, full, "{s}");
        }
    }

    #[test]
    fn parse_rejects_garbage() {
        for s in [
            " 1", "1 ", "1e5", "1.2.3", "--1", "+-1", "0x1A", "1\0", "1.\0", "abc", "1,5", ". 5",
        ] {
            assert!(BcNum::parse_auto(s.as_bytes()).is_err(), "{s:?}");
        }
    }

    #[test]
    fn parse_with_scale_truncates() {
        let n = BcNum::parse(b"123.0001", 2, false).unwrap().num;
        assert_eq!(n.scale(), 0);
        let n = BcNum::parse(b"-0.0001", 2, false).unwrap().num;
        assert!(n.is_zero());
        assert_eq!(n.sign(), Sign::Plus);
        let n = BcNum::parse(b"1.2345", 3, false).unwrap().num;
        assert_eq!(n.to_string_scaled(3), "1.234");
    }

    #[test]
    fn formatting() {
        assert_eq!(p("-0.001").to_string_scaled(2), "0.00");
        assert_eq!(p("-0.001").to_string_scaled(3), "-0.001");
        assert_eq!(p("12.5").to_string_scaled(0), "12");
        assert_eq!(p("12.5").to_string_scaled(4), "12.5000");
    }

    #[test]
    fn compare() {
        assert_eq!(BcNum::compare(&p("1.001"), &p("1"), 2), Ordering::Equal);
        assert_eq!(BcNum::compare(&p("1.001"), &p("1"), 3), Ordering::Greater);
        assert_eq!(BcNum::compare(&p("-1"), &p("1"), 0), Ordering::Less);
        assert_eq!(BcNum::compare(&p("-2"), &p("-1"), 0), Ordering::Less);
        assert_eq!(
            BcNum::compare(&p("-0.001"), &p("0.001"), 2),
            Ordering::Equal
        );
        assert_eq!(BcNum::compare(&p("10"), &p("9.99"), 5), Ordering::Greater);
    }

    #[test]
    fn add_sub() {
        assert_eq!(
            BcNum::add(&p("999.99"), &p("0.01"), 0).to_string_scaled(2),
            "1000.00"
        );
        assert_eq!(
            BcNum::add(&p("1.5"), &p("-2.25"), 3).to_string_scaled(3),
            "-0.750"
        );
        assert_eq!(
            BcNum::sub(&p("1"), &p("1.0001"), 2).to_string_scaled(4),
            "0.0000"
        );
        assert_eq!(BcNum::sub(&p("-5"), &p("3"), 0).to_string_scaled(0), "-8");
    }

    #[test]
    fn long_conversion() {
        for v in [0, 1, -1, 1234, i64::MAX, i64::MIN] {
            assert_eq!(BcNum::from_i64(v).to_string_scaled(0), v.to_string());
        }
        assert_eq!(p("123.9").to_i64(), 123);
        assert_eq!(p("-9223372036854775807").to_i64(), -i64::MAX);
        assert_eq!(p("9223372036854775808").to_i64(), 0);
    }
}
