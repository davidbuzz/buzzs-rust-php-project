// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The current `LC_NUMERIC` decimal point, as PHP's formatters read it
//! through `LCONV_DECIMAL_POINT` (`main/snprintf.c`, `main/spprintf.c`,
//! `ext/standard/formatted_print.c`; `localeconv_decimal_point()` in
//! `ext/standard/string.c`).
//!
//! The C library's locale is process-global and only changes through
//! `setlocale()`, so instead of calling `localeconv()` on every `%f` this
//! crate keeps a process-wide copy of `*localeconv()->decimal_point` that
//! the `setlocale()` binding refreshes after each call (and at request
//! shutdown, when PHP restores the "C" locale). Reading it is a single
//! relaxed atomic load; in the C locale it is `b'.'`.

use std::sync::atomic::{AtomicU8, Ordering};

static DECIMAL_POINT: AtomicU8 = AtomicU8::new(b'.');

/// `LCONV_DECIMAL_POINT`: the first byte of the current locale's
/// `localeconv()->decimal_point` (`b'.'` until [`set_decimal_point`] is
/// called with something else).
#[inline]
#[must_use]
pub fn decimal_point() -> u8 {
    DECIMAL_POINT.load(Ordering::Relaxed)
}

/// Records the C library's current `*localeconv()->decimal_point`. Called
/// by the `setlocale()` binding after the process locale changed.
#[inline]
pub fn set_decimal_point(dp: u8) {
    DECIMAL_POINT.store(dp, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_restore() {
        let before = decimal_point();
        assert_eq!(before, b'.');
        set_decimal_point(b',');
        assert_eq!(decimal_point(), b',');
        set_decimal_point(before);
        assert_eq!(decimal_point(), b'.');
    }
}
