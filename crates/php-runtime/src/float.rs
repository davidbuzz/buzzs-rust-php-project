//! Float → string conversion matching PHP (`zend_gcvt`, `smart_str_append_double`).
//!
//! PHP formats floats through `zend_dtoa` in one of two modes:
//!
//! * mode 0 (used when the precision is `-1`, e.g. the default
//!   `serialize_precision`): the shortest digit string that round-trips;
//! * mode 2 with `n` digits (used for `precision`, default 14): the value
//!   correctly rounded (half-even on exact ties) to `n` significant digits with
//!   trailing zeros removed.
//!
//! Both are obtained here from Rust's float formatting, which implements the
//! same correctly rounded algorithms (Grisu/Ryu-style shortest and exact
//! Dragon4-style fixed precision).
//!
//! Like php-src (`zend_gcvt` writes into a caller's stack buffer, then
//! `zend_double_to_str` makes one exactly sized `zend_string`), the common
//! cases (at most 17 significant digits) are formatted without heap
//! temporaries: digits and output live in stack buffers, integral values
//! skip the float formatter, and mode 2 with at most 15 digits tries the
//! cheap shortest digits before the exact (bignum-backed) ones.

use crate::string::ZStr;
use std::fmt::{self, Write as _};

/// Decimal digits of a float as produced by `zend_dtoa`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DtoaDigits {
    /// Significant digits, without leading/trailing zeros (`"0"` for zero).
    pub digits: String,
    /// Position of the decimal point relative to the digits (`decpt`): the
    /// value is `0.DIGITS × 10^decpt`.
    pub decpt: i32,
    /// Whether the value is negative (including `-0.0`).
    pub negative: bool,
}

/// Up to 17 significant digits of a finite, non-zero float in a stack
/// buffer: the `zend_dtoa` result of mode 0 and of mode 2 with at most 17
/// digits.
#[derive(Debug, Clone, Copy)]
struct SmallDigits {
    buf: [u8; 24],
    len: usize,
    decpt: i32,
}

impl SmallDigits {
    fn digits(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// A fixed-capacity byte buffer that `write!` can format into (the
/// `char buf[ZEND_DOUBLE_MAX_LENGTH]` of `zend_double_to_str`).
#[derive(Debug)]
struct StackBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackBuf<N> {
    const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// The bytes as a `String`, each byte one `char` (what pushing
    /// `char::from(byte)` gives; only the decimal point can be non-ASCII).
    fn to_latin1_string(&self) -> String {
        self.as_bytes().iter().map(|&c| char::from(c)).collect()
    }
}

impl<const N: usize> fmt::Write for StackBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self
            .len
            .checked_add(s.len())
            .filter(|&e| e <= N)
            .ok_or(fmt::Error)?;
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Where [`emit_gcvt`] writes.
trait GcvtOut {
    fn put(&mut self, b: u8);

    fn put_all(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.put(b);
        }
    }
}

impl GcvtOut for String {
    fn put(&mut self, b: u8) {
        self.push(char::from(b));
    }
}

impl<const N: usize> GcvtOut for StackBuf<N> {
    fn put(&mut self, b: u8) {
        // Callers size the buffer for the output (at most 17 digits); a
        // byte past the end would be a bug, and is dropped rather than
        // panicking.
        debug_assert!(self.len < N, "float output buffer overflow");
        if let Some(slot) = self.buf.get_mut(self.len) {
            *slot = b;
            self.len += 1;
        }
    }
}

/// Parses Rust's `{:e}` output (`d.ddde±x`) into significant digits with
/// trailing zeros removed and `decpt`.
fn parse_exp_format(s: &[u8]) -> Option<SmallDigits> {
    let e = s.iter().position(|&c| c == b'e')?;
    let (mantissa, exp) = (&s[..e], &s[e + 1..]);
    let mut out = SmallDigits {
        buf: [0; 24],
        len: 0,
        decpt: 0,
    };
    for &c in mantissa {
        if c.is_ascii_digit() {
            *out.buf.get_mut(out.len)? = c;
            out.len += 1;
        }
    }
    while out.len > 1 && out.buf[out.len - 1] == b'0' {
        out.len -= 1;
    }
    if out.len == 0 {
        return None;
    }
    let (neg, exp_digits) = match exp.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, exp),
    };
    if exp_digits.is_empty() {
        return None;
    }
    let mut x: i32 = 0;
    for &c in exp_digits {
        if !c.is_ascii_digit() {
            return None;
        }
        x = x.checked_mul(10)?.checked_add(i32::from(c - b'0'))?;
    }
    let exp = if neg { -x } else { x };
    out.decpt = exp + 1;
    Some(out)
}

/// `zend_dtoa` digits of a finite, positive, non-zero `abs` without heap
/// allocation: the shortest round-trip digits (`shortest`, mode 0) or the
/// value correctly rounded to `max(ndigits, 1)` digits (mode 2). `None`
/// when more than 17 digits are asked for.
///
/// Two shortcuts precede the exact formatter:
/// * an integral value below 10^15 has its own digits as both the shortest
///   form and, when it has at most `n` digits, the exact `n`-digit form;
/// * for a normal value and `n <= 15`, the shortest digits, when there are
///   at most `n` of them, ARE the `n`-digit rounding: they lie within half
///   an ulp of the value (< 1.2e-16 relative), closer than half a unit in
///   the 15th digit (>= 5e-16 relative), so they are the unique nearest
///   `n`-digit number. This avoids the exact formatter's bignum fallback
///   for short values such as `1.5` at `precision=14`.
fn dtoa_small(abs: f64, shortest: bool, ndigits: i32) -> Option<SmallDigits> {
    debug_assert!(abs.is_finite() && abs > 0.0);
    let n = if shortest { 17 } else { ndigits.max(1) };
    if n > 17 {
        return None;
    }
    let n_digits = usize::try_from(n).ok()?;
    if (1.0..1e15).contains(&abs) && abs.fract() == 0.0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let mut m = abs as u64;
        let mut tmp = [0u8; 16];
        let mut pos = tmp.len();
        while m > 0 {
            pos -= 1;
            #[allow(clippy::cast_possible_truncation)]
            let digit = (m % 10) as u8;
            tmp[pos] = b'0' + digit;
            m /= 10;
        }
        let count = tmp.len() - pos;
        if shortest || count <= n_digits {
            let mut end = tmp.len();
            while end - pos > 1 && tmp[end - 1] == b'0' {
                end -= 1;
            }
            let mut out = SmallDigits {
                buf: [0; 24],
                len: end - pos,
                decpt: i32::try_from(count).ok()?,
            };
            out.buf[..end - pos].copy_from_slice(&tmp[pos..end]);
            return Some(out);
        }
    }
    let mut w = StackBuf::<32>::new();
    if shortest || (n <= 15 && abs >= f64::MIN_POSITIVE) {
        write!(w, "{abs:e}").ok()?;
        let d = parse_exp_format(w.as_bytes())?;
        if shortest || d.len <= n_digits {
            return Some(d);
        }
        w.len = 0;
    }
    let prec = n_digits - 1;
    write!(w, "{abs:.prec$e}").ok()?;
    parse_exp_format(w.as_bytes())
}

/// The general (heap) digit generation behind [`zend_dtoa`], for any
/// number of digits.
fn dtoa_slow(abs: f64, shortest: bool, ndigits: i32) -> (String, i32) {
    let formatted = if shortest {
        format!("{abs:e}")
    } else {
        let prec = usize::try_from(ndigits.max(1) - 1).unwrap_or(0);
        format!("{abs:.prec$e}")
    };
    let (mantissa, exp) = formatted.split_once('e').unwrap_or((&formatted, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let mut digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    (digits, exp + 1)
}

/// Port of `zend_dtoa` for finite values in mode 0 (`ndigits <= 0`, shortest
/// round-trip) or mode 2 (`ndigits` significant digits).
///
/// # Panics
/// Never for finite input; non-finite input yields digits `"Infinity"`/`"NaN"`
/// with `decpt == 9999` like the C implementation.
#[must_use]
pub fn zend_dtoa(value: f64, mode: u8, ndigits: i32) -> DtoaDigits {
    let negative = value.is_sign_negative();
    if !value.is_finite() {
        return DtoaDigits {
            digits: if value.is_nan() { "NaN" } else { "Infinity" }.to_string(),
            decpt: 9999,
            negative: negative && !value.is_nan(),
        };
    }
    let abs = value.abs();
    if abs == 0.0 {
        return DtoaDigits {
            digits: "0".to_string(),
            decpt: 1,
            negative,
        };
    }
    let shortest = mode == 0 || ndigits <= 0 && mode != 2;
    if let Some(d) = dtoa_small(abs, shortest, ndigits) {
        return DtoaDigits {
            digits: d.digits().iter().map(|&c| char::from(c)).collect(),
            decpt: d.decpt,
            negative,
        };
    }
    let (digits, decpt) = dtoa_slow(abs, shortest, ndigits);
    DtoaDigits {
        digits,
        decpt,
        negative,
    }
}

/// Writes a decimal exponent (`zend_gcvt`'s exponent digits).
fn put_exponent(out: &mut impl GcvtOut, mut e: u32) {
    let mut tmp = [0u8; 10];
    let mut pos = tmp.len();
    loop {
        pos -= 1;
        #[allow(clippy::cast_possible_truncation)]
        let digit = (e % 10) as u8;
        tmp[pos] = b'0' + digit;
        e /= 10;
        if e == 0 {
            break;
        }
    }
    out.put_all(&tmp[pos..]);
}

/// The layout half of `zend_gcvt`: digits and `decpt` to text.
fn emit_gcvt(
    out: &mut impl GcvtOut,
    digits: &[u8],
    decpt: i32,
    negative: bool,
    ndigit: i32,
    dec_point: u8,
    exponent: u8,
) {
    if negative {
        out.put(b'-');
    }
    let mut decpt = decpt;
    if (decpt >= 0 && decpt > ndigit) || decpt < -3 {
        // Exponential format, e.g. 1.2345E+13.
        decpt -= 1;
        let exp_negative = decpt < 0;
        let e = decpt.unsigned_abs();
        out.put(digits[0]);
        out.put(dec_point);
        if digits.len() == 1 {
            out.put(b'0');
        } else {
            out.put_all(&digits[1..]);
        }
        out.put(exponent);
        out.put(if exp_negative { b'-' } else { b'+' });
        put_exponent(out, e);
    } else if decpt < 0 {
        // Standard format 0.000ddd
        out.put(b'0');
        out.put(dec_point);
        for _ in decpt..0 {
            out.put(b'0');
        }
        out.put_all(digits);
    } else {
        // Standard format.
        let decpt_u = usize::try_from(decpt).unwrap_or(0);
        for i in 0..decpt_u {
            out.put(*digits.get(i).unwrap_or(&b'0'));
        }
        if digits.len() > decpt_u {
            if decpt_u == 0 {
                out.put(b'0');
            }
            out.put(dec_point);
            out.put_all(&digits[decpt_u..]);
        }
    }
}

/// `zend_gcvt` into `out` (see [`zend_gcvt`]).
fn gcvt_with(out: &mut impl GcvtOut, value: f64, ndigit: i32, dec_point: u8, exponent: u8) {
    let (mode, ndigit) = if ndigit >= 0 { (2, ndigit) } else { (0, 17) };
    if value.is_nan() {
        out.put_all(b"NAN");
        return;
    }
    if value.is_infinite() {
        out.put_all(if value < 0.0 {
            &b"-INF"[..]
        } else {
            &b"INF"[..]
        });
        return;
    }
    let negative = value.is_sign_negative();
    let abs = value.abs();
    if abs == 0.0 {
        emit_gcvt(out, b"0", 1, negative, ndigit, dec_point, exponent);
        return;
    }
    let shortest = mode == 0;
    if let Some(d) = dtoa_small(abs, shortest, ndigit) {
        emit_gcvt(
            out,
            d.digits(),
            d.decpt,
            negative,
            ndigit,
            dec_point,
            exponent,
        );
        return;
    }
    let (digits, decpt) = dtoa_slow(abs, shortest, ndigit);
    emit_gcvt(
        out,
        digits.as_bytes(),
        decpt,
        negative,
        ndigit,
        dec_point,
        exponent,
    );
}

/// Room for `zend_gcvt` output of at most 17 significant digits (at most
/// 24 bytes in exponential form, 36 in standard form) plus `".0"`.
const SMALL_OUT: usize = 64;

/// Port of `zend_gcvt(value, ndigit, dec_point, exponent_char, buf)`.
///
/// A negative `ndigit` selects mode 0 (shortest representation, `%.*H` with
/// precision `-1`), in which case the exponential-format threshold is 17
/// digits. `ndigit == 0` must be mapped to `1` by the caller, as PHP does.
#[must_use]
pub fn zend_gcvt(value: f64, ndigit: i32, dec_point: u8, exponent: u8) -> String {
    if ndigit <= 17 {
        let mut b = StackBuf::<SMALL_OUT>::new();
        gcvt_with(&mut b, value, ndigit, dec_point, exponent);
        return b.to_latin1_string();
    }
    let mut s = String::new();
    gcvt_with(&mut s, value, ndigit, dec_point, exponent);
    s
}

/// `smart_str_append_double(num, precision, zero_fraction)`: formats with
/// `zend_gcvt(num, precision ? precision : 1, '.', 'E')` and optionally appends
/// `".0"` to finite values that have no decimal point.
///
/// This is the formatting used by `var_export` (`serialize_precision`, zero
/// fraction), `print_r`/`echo`-style `precision` formatting (no zero fraction),
/// `serialize` and `json_encode`.
#[must_use]
pub fn format_double(num: f64, precision: i64, zero_fraction: bool) -> String {
    let p = i32::try_from(precision.clamp(-1, 1000)).unwrap_or(17);
    let mut s = zend_gcvt(num, if p == 0 { 1 } else { p }, b'.', b'E');
    if zero_fraction && num.is_finite() && !s.contains('.') {
        s.push_str(".0");
    }
    s
}

/// [`format_double`] straight into a [`ZStr`] (`zend_double_to_str`:
/// `zend_gcvt` into a stack buffer, then one exactly sized string), with
/// no heap temporaries for precisions up to 17.
#[must_use]
pub fn format_double_zstr(num: f64, precision: i64, zero_fraction: bool) -> ZStr {
    let p = i32::try_from(precision.clamp(-1, 1000)).unwrap_or(17);
    let ndigit = if p == 0 { 1 } else { p };
    if ndigit > 17 {
        return ZStr::from(format_double(num, precision, zero_fraction));
    }
    let mut b = StackBuf::<SMALL_OUT>::new();
    gcvt_with(&mut b, num, ndigit, b'.', b'E');
    if zero_fraction && num.is_finite() && !b.as_bytes().contains(&b'.') {
        b.put_all(b".0");
    }
    ZStr::from(b.as_bytes())
}

/// `%.*H` formatting as used by `var_dump` / `debug_zval_dump`
/// (`php_printf("%.*H", serialize_precision, d)`).
///
/// Unlike [`format_double`], NaN/±Inf are printed by the printf engine as
/// `NAN`, `INF` and `-INF`, and a precision of 0 is treated as 1.
#[must_use]
pub fn format_double_h(num: f64, precision: i64) -> String {
    if num.is_nan() {
        return "NAN".to_string();
    }
    if num.is_infinite() {
        return if num > 0.0 { "INF" } else { "-INF" }.to_string();
    }
    let p = i32::try_from(precision.clamp(-1, 1000)).unwrap_or(17);
    zend_gcvt(num, if p == 0 { 1 } else { p }, b'.', b'E')
}

/// `zend_double_to_str` without the NaN warning: the string conversion of a
/// float under the given `precision` ini value (default 14).
#[must_use]
pub fn double_to_string(num: f64, precision: i64) -> String {
    format_double(num, precision, false)
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn shortest() {
        let cases: &[(f64, &str)] = &[
            (0.1 + 0.2, "0.30000000000000004"),
            (1e15, "1000000000000000"),
            (0.0, "0"),
            (-0.0, "-0"),
            (1.5, "1.5"),
            (100_000.0, "100000"),
            (1e17, "1.0E+17"),
            (123_456_789_012_345_678.0, "1.2345678901234568E+17"),
            (0.0001, "0.0001"),
            (0.00001, "1.0E-5"),
            (1.5e300, "1.5E+300"),
            (-1.25e-10, "-1.25E-10"),
        ];
        for &(v, want) in cases {
            assert_eq!(format_double_h(v, -1), want, "{v}");
        }
        assert_eq!(format_double_h(1e16, -1), "10000000000000000");
        assert_eq!(format_double_h(f64::NAN, -1), "NAN");
        assert_eq!(format_double_h(f64::NEG_INFINITY, -1), "-INF");
    }

    #[test]
    fn precision_14() {
        let cases: &[(f64, &str)] = &[
            (1e15, "1.0E+15"),
            (1e14, "1.0E+14"),
            (1e13, "10000000000000"),
            (0.1 + 0.2, "0.3"),
            (123_456_789_012_345.67, "1.2345678901235E+14"),
            (-0.0, "-0"),
            (1e100, "1.0E+100"),
            (0.00001, "1.0E-5"),
            (1.0 / 3.0, "0.33333333333333"),
            (f64::INFINITY, "INF"),
            (f64::NEG_INFINITY, "-INF"),
            (f64::NAN, "NAN"),
            (7.0, "7"),
        ];
        for &(v, want) in cases {
            assert_eq!(double_to_string(v, 14), want, "{v}");
        }
    }

    #[test]
    fn half_even_ties() {
        assert_eq!(double_to_string(0.125, 2), "0.12");
        assert_eq!(double_to_string(0.375, 2), "0.38");
        assert_eq!(double_to_string(0.25, 1), "0.2");
        assert_eq!(double_to_string(2.5, 1), "2");
        assert_eq!(double_to_string(3.5, 1), "4");
        assert_eq!(double_to_string(0.75, 1), "0.8");
        assert_eq!(double_to_string(2.5e-5, 2), "2.5E-5");
        assert_eq!(double_to_string(0.1, 17), "0.10000000000000001");
        assert_eq!(double_to_string(1.5, 0), "2");
    }

    #[test]
    fn zero_fraction() {
        assert_eq!(format_double(1.0, -1, true), "1.0");
        assert_eq!(format_double(0.1, -1, true), "0.1");
        assert_eq!(format_double(1e20, -1, true), "1.0E+20");
        assert_eq!(format_double(f64::INFINITY, -1, true), "INF");
        assert_eq!(format_double(-0.0, -1, true), "-0.0");
    }

    /// Values for the fast-path cross-checks: edge cases plus a
    /// deterministic pseudo-random sample of bit patterns and of short
    /// decimals.
    fn sample_values() -> Vec<f64> {
        let mut vals = vec![
            1.5,
            0.1,
            0.5,
            7.0,
            -7.0,
            1e14,
            1e15,
            1e15 - 1.0,
            999_999_999_999_999.0,
            123_456_789.0,
            1_200_000.0,
            0.1 + 0.2,
            1.0 / 3.0,
            2.5,
            0.125,
            0.375,
            1e-300,
            5e-324,
            f64::MIN_POSITIVE,
            f64::MAX,
            9_007_199_254_740_993.0,
            2.5e-5,
            99.995,
            0.000_123_45,
        ];
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for i in 0..20_000u32 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            vals.push(f64::from_bits(x));
            #[allow(clippy::cast_precision_loss)]
            let short = (x % 1_000_000) as f64 / f64::from(10u32.pow(i % 7));
            vals.push(short);
        }
        vals
    }

    #[test]
    fn small_digits_match_exact_digits() {
        for v in sample_values() {
            if !v.is_finite() || v == 0.0 {
                continue;
            }
            let a = v.abs();
            for nd in [-1, 0, 1, 2, 3, 6, 10, 14, 15, 16, 17] {
                let shortest = nd < 0;
                let small = dtoa_small(a, shortest, nd).expect("at most 17 digits");
                let (digits, decpt) = dtoa_slow(a, shortest, nd);
                assert_eq!(
                    (small.digits(), small.decpt),
                    (digits.as_bytes(), decpt),
                    "{v:e} ndigits {nd}"
                );
            }
            assert!(dtoa_small(a, false, 18).is_none());
        }
    }

    #[test]
    fn zstr_and_string_formatting_agree() {
        for v in sample_values().into_iter().chain([
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.0,
            -0.0,
        ]) {
            for p in [-1, 0, 1, 5, 14, 17, 18, 40] {
                for zf in [false, true] {
                    let s = format_double(v, p, zf);
                    let z = format_double_zstr(v, p, zf);
                    assert_eq!(z.as_bytes(), s.as_bytes(), "{v:e} precision {p}");
                }
            }
        }
        assert_eq!(format_double_zstr(1.5, 14, false), "1.5");
        assert_eq!(format_double_zstr(-2.0, 14, false), "-2");
        assert_eq!(format_double_zstr(1e25, 14, false), "1.0E+25");
        assert_eq!(format_double_zstr(3.0, -1, true), "3.0");
        assert_eq!(zend_gcvt(1.5, 2, b',', b'e'), "1,5");
        assert_eq!(zend_gcvt(0.1, 20, b'.', b'E'), "0.10000000000000000555");
        let d = zend_dtoa(1.5, 2, 14);
        assert_eq!((d.digits.as_str(), d.decpt, d.negative), ("15", 1, false));
        let d = zend_dtoa(-0.1, 0, 0);
        assert_eq!((d.digits.as_str(), d.decpt, d.negative), ("1", 0, true));
    }
}
