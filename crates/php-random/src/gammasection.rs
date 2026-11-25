// Derived from php-src ext/random/gammasection.c. Similarity metric to original: 44%
//   Copyright © The PHP Group and Contributors.
//   Authors: Tim Düsterhus <timwolla@php.net>
//            Based on code from: Frédéric Goualard
//            Corrected to handled appropriately random integers larger than 2^53
//            converted to double precision floats, and to avoid overflows for
//            large gammas.
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The γ-section method for drawing floats from an interval
//! (ext/random/gammasection.c, after F. Goualard, ACM TOMACS 32:3, 2022).
//!
//! Each function returns `NaN` when the interval is empty, like the C code.

use crate::engine::Engine;
use crate::error::Result;
use crate::range::range64;

/// C `nextafter(x, y)`.
#[allow(clippy::float_cmp)]
fn next_after(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if x == y {
        return y;
    }
    if x == 0.0 {
        let tiny = f64::from_bits(1);
        return if y > 0.0 { tiny } else { -tiny };
    }
    let bits = x.to_bits();
    let up = (y > x) == (x > 0.0);
    f64::from_bits(if up { bits + 1 } else { bits - 1 })
}

fn gamma_low(x: f64) -> f64 {
    x - next_after(x, -f64::MAX)
}

fn gamma_high(x: f64) -> f64 {
    next_after(x, f64::MAX) - x
}

fn gamma_max(x: f64, y: f64) -> f64 {
    if x.abs() > y.abs() {
        gamma_high(x)
    } else {
        gamma_low(y)
    }
}

#[allow(clippy::cast_precision_loss)]
fn splitint64(v: u64) -> (f64, f64) {
    ((v >> 2) as f64, (v & 3) as f64)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::many_single_char_names
)]
fn ceilint(a: f64, b: f64, g: f64) -> u64 {
    let s = b / g - a / g;
    let e = if a.abs() <= b.abs() {
        -a / g - (s - b / g)
    } else {
        b / g - (s + a / g)
    };
    let si = s.ceil();
    if s == si {
        (si as u64) + u64::from(e > 0.0)
    } else {
        si as u64
    }
}

fn from_max(max: f64, k: u64, g: f64) -> f64 {
    let (k_hi, k_lo) = splitint64(k);
    4.0 * (max * 0.25 - k_hi * g) - k_lo * g
}

fn from_min(min: f64, k: u64, g: f64) -> f64 {
    let (k_hi, k_lo) = splitint64(k);
    4.0 * (min * 0.25 + k_hi * g) + k_lo * g
}

/// `php_random_gammasection_closed_open`: a float in `[min, max)`.
pub fn closed_open<E: Engine + ?Sized>(engine: &mut E, min: f64, max: f64) -> Result<f64> {
    let g = gamma_max(min, max);
    let hi = ceilint(min, max, g);
    if max <= min || hi < 1 {
        return Ok(f64::NAN);
    }
    let k = 1 + range64(engine, hi - 1)?;
    Ok(if min.abs() <= max.abs() {
        if k == hi {
            min
        } else {
            from_max(max, k, g)
        }
    } else {
        from_min(min, k - 1, g)
    })
}

/// `php_random_gammasection_closed_closed`: a float in `[min, max]`.
pub fn closed_closed<E: Engine + ?Sized>(engine: &mut E, min: f64, max: f64) -> Result<f64> {
    let g = gamma_max(min, max);
    let hi = ceilint(min, max, g);
    if max < min {
        return Ok(f64::NAN);
    }
    let k = range64(engine, hi)?;
    Ok(if min.abs() <= max.abs() {
        if k == hi {
            min
        } else {
            from_max(max, k, g)
        }
    } else if k == hi {
        max
    } else {
        from_min(min, k, g)
    })
}

/// `php_random_gammasection_open_closed`: a float in `(min, max]`.
pub fn open_closed<E: Engine + ?Sized>(engine: &mut E, min: f64, max: f64) -> Result<f64> {
    let g = gamma_max(min, max);
    let hi = ceilint(min, max, g);
    if max <= min || hi < 1 {
        return Ok(f64::NAN);
    }
    let k = range64(engine, hi - 1)?;
    Ok(if min.abs() <= max.abs() {
        from_max(max, k, g)
    } else if k == hi - 1 {
        max
    } else {
        from_min(min, k + 1, g)
    })
}

/// `php_random_gammasection_open_open`: a float in `(min, max)`.
pub fn open_open<E: Engine + ?Sized>(engine: &mut E, min: f64, max: f64) -> Result<f64> {
    let g = gamma_max(min, max);
    let hi = ceilint(min, max, g);
    if max <= min || hi < 2 {
        return Ok(f64::NAN);
    }
    let k = 1 + range64(engine, hi - 2)?;
    Ok(if min.abs() <= max.abs() {
        from_max(max, k, g)
    } else {
        from_min(min, k, g)
    })
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn nextafter_matches_c() {
        assert_eq!(next_after(1.0, 2.0), 1.0 + f64::EPSILON);
        assert_eq!(next_after(1.0, 0.0), 1.0 - f64::EPSILON / 2.0);
        assert_eq!(next_after(-1.0, -2.0), -1.0 - f64::EPSILON);
        assert_eq!(next_after(0.0, 1.0), f64::from_bits(1));
        assert_eq!(next_after(-0.0, -1.0), -f64::from_bits(1));
        assert_eq!(next_after(f64::MAX, f64::MAX), f64::MAX);
        assert!(next_after(f64::NAN, 1.0).is_nan());
        assert_eq!(gamma_low(-f64::MAX), 0.0);
        assert_eq!(gamma_high(1.0), f64::EPSILON);
    }

    #[test]
    fn ceilint_cases() {
        let g = gamma_max(1.0, 2.0);
        assert_eq!(ceilint(1.0, 2.0, g), 1u64 << 52);
        assert_eq!(ceilint(0.0, 0.0, gamma_max(0.0, 0.0)), 0);
    }
}
