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
    let formatted = if mode == 0 || ndigits <= 0 && mode != 2 {
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
    DtoaDigits {
        digits,
        decpt: exp + 1,
        negative,
    }
}

/// Port of `zend_gcvt(value, ndigit, dec_point, exponent_char, buf)`.
///
/// A negative `ndigit` selects mode 0 (shortest representation, `%.*H` with
/// precision `-1`), in which case the exponential-format threshold is 17
/// digits. `ndigit == 0` must be mapped to `1` by the caller, as PHP does.
#[must_use]
pub fn zend_gcvt(value: f64, ndigit: i32, dec_point: u8, exponent: u8) -> String {
    let (mode, ndigit) = if ndigit >= 0 { (2, ndigit) } else { (0, 17) };
    let d = zend_dtoa(value, mode, ndigit);
    if d.decpt == 9999 {
        // Infinity or NaN.
        return if d.digits.starts_with('I') {
            if d.negative { "-INF" } else { "INF" }.to_string()
        } else {
            "NAN".to_string()
        };
    }
    let dec_point = char::from(dec_point);
    let exponent = char::from(exponent);
    let mut out = String::with_capacity(d.digits.len() + 8);
    if d.negative {
        out.push('-');
    }
    let digits = d.digits.as_bytes();
    let mut decpt = d.decpt;
    if (decpt >= 0 && decpt > ndigit) || decpt < -3 {
        // Exponential format, e.g. 1.2345E+13.
        decpt -= 1;
        let exp_negative = decpt < 0;
        let e = decpt.unsigned_abs();
        out.push(char::from(digits[0]));
        out.push(dec_point);
        if digits.len() == 1 {
            out.push('0');
        } else {
            for &c in &digits[1..] {
                out.push(char::from(c));
            }
        }
        out.push(exponent);
        out.push(if exp_negative { '-' } else { '+' });
        out.push_str(&e.to_string());
    } else if decpt < 0 {
        // Standard format 0.000ddd
        out.push('0');
        out.push(dec_point);
        for _ in decpt..0 {
            out.push('0');
        }
        for &c in digits {
            out.push(char::from(c));
        }
    } else {
        // Standard format.
        let decpt_u = usize::try_from(decpt).unwrap_or(0);
        for i in 0..decpt_u {
            out.push(char::from(*digits.get(i).unwrap_or(&b'0')));
        }
        if digits.len() > decpt_u {
            if decpt_u == 0 {
                out.push('0');
            }
            out.push(dec_point);
            for &c in &digits[decpt_u..] {
                out.push(char::from(c));
            }
        }
    }
    out
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
}
