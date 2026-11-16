// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The few `spprintf()` conversions exif uses, with PHP's exact output.

/// PHP's `%.<prec>F` conversion (`php_conv_fp` + `php_fcvt`): correctly
/// rounded fixed notation, `"INF"`/`"NAN"` for non-finite values (without a
/// sign), and no sign for zero.
pub fn php_f(value: f64, prec: usize) -> String {
    if value.is_nan() {
        return "NAN".to_owned();
    }
    if value.is_infinite() {
        return "INF".to_owned();
    }
    if value == 0.0 {
        return format!("{:.prec$}", 0.0_f64);
    }
    format!("{value:.prec$}")
}

/// `%0<width>X` of an `unsigned int` argument (what `%X` reads from the
/// varargs).
pub fn hex_u32(value: u32, width: usize) -> String {
    format!("{value:0width$X}")
}

/// `%0<width>zX` of a `size_t` argument.
pub fn hex_u64(value: u64, width: usize) -> String {
    format!("{value:0width$X}")
}

/// `%<width>s`: right-aligns `s` in a field of `width` bytes.
pub fn pad_left(s: &str, width: usize) -> String {
    format!("{s:>width$}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_point() {
        assert_eq!(php_f(2.8, 1), "2.8");
        assert_eq!(php_f(0.25, 1), "0.2");
        assert_eq!(php_f(-0.0, 2), "0.00");
        assert_eq!(php_f(f64::INFINITY, 1), "INF");
        assert_eq!(php_f(f64::NEG_INFINITY, 1), "INF");
        assert_eq!(php_f(f64::NAN, 3), "NAN");
        assert_eq!(php_f(0.016_666_668_f32.into(), 3), "0.017");
        assert_eq!(php_f(-0.0001, 3), "-0.000");
    }

    #[test]
    fn hex() {
        assert_eq!(hex_u32(0x2a, 4), "002A");
        assert_eq!(hex_u32(0x1_2345, 4), "12345");
        assert_eq!(hex_u64(0x829A_0004, 4), "829A0004");
        assert_eq!(pad_left("abc", 5), "  abc");
    }
}
