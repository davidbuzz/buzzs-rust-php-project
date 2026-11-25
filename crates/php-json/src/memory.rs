// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Sizes php-src allocates for the encoder's and the decoder's strings, and
//! the growth of the encoder's output buffer under `memory_limit`
//! (ref: `Zend/zend_string.h` `_ZSTR_STRUCT_SIZE`, `Zend/zend_smart_str.c`
//! `SMART_STR_NEW_LEN`, `Zend/zend_alloc.c` `zend_mm_realloc_huge`).
//!
//! php-src's allocator checks `memory_limit` on every allocation and
//! raises the fatal error from inside it. A Rust allocation cannot fail
//! that way, so the host's accountant records a violation and the VM raises
//! the fatal at its next safe point, killing the process only at a hard
//! ceiling above the limit. A single large allocation or growth could jump
//! from under the limit to over that ceiling, so the JSON code asks first
//! ([`JsonHost::json_try_reserve`](crate::JsonHost::json_try_reserve),
//! [`JsonHost::json_reserve`](crate::JsonHost::json_reserve)).

/// `_ZSTR_HEADER_SIZE`: the `zend_string` header before the bytes.
const ZSTR_HEADER: usize = 24;

/// `SMART_STR_OVERHEAD`: `ZEND_MM_OVERHEAD` (0) + the header + the NUL.
const SMART_STR_OVERHEAD: usize = ZSTR_HEADER + 1;

/// `SMART_STR_PAGE`.
const SMART_STR_PAGE: usize = 4096;

/// `ZEND_MM_ALIGNED_SIZE(_ZSTR_STRUCT_SIZE(len))`: the bytes php-src
/// allocates for a `zend_string` of `len` bytes (the header, the bytes and
/// the terminating NUL, 8-byte aligned). Saturates instead of overflowing.
#[must_use]
pub fn zend_string_size(len: usize) -> usize {
    len.saturating_add(ZSTR_HEADER + 1 + 7) & !7
}

/// The block size of a `smart_str` grown to hold `len` bytes
/// (`SMART_STR_NEW_LEN(len) + _ZSTR_HEADER_SIZE + 1`, a whole number of
/// pages): what php-src's out-of-memory error reports as "tried to
/// allocate" when the growth of its output buffer does not fit.
/// Saturates instead of overflowing.
#[must_use]
pub fn smart_str_size(len: usize) -> usize {
    len.saturating_add(SMART_STR_OVERHEAD + SMART_STR_PAGE - 1) & !(SMART_STR_PAGE - 1)
}

/// The capacity to grow an output buffer of capacity `cap` holding `len`
/// bytes to, so that `additional` more fit, given `fits(bytes)`: whether
/// `bytes` more can be allocated under `memory_limit`.
///
/// The buffer doubles, as a vector does, while the growth fits. Near the
/// limit it grows by less (halving the extra room until it fits, down to
/// what is needed), so that an output php-src's page-by-page growth would
/// hold still fits here. `Err` carries the size php-src reports for the
/// growth that does not fit ([`smart_str_size`] of the needed length):
/// the caller records the violation with that figure and stops.
///
/// # Errors
///
/// When even the needed growth does not fit (or the needed length
/// overflows `usize`).
pub fn grow_output(
    cap: usize,
    len: usize,
    additional: usize,
    mut fits: impl FnMut(usize) -> bool,
) -> Result<usize, usize> {
    let Some(needed) = len.checked_add(additional) else {
        return Err(usize::MAX);
    };
    if needed <= cap {
        return Ok(cap);
    }
    let mut target = needed.max(cap.saturating_mul(2));
    loop {
        if fits(target - cap) {
            return Ok(target);
        }
        if target == needed {
            return Err(smart_str_size(needed));
        }
        target = needed.max(cap + (target - cap) / 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_src_sizes() {
        assert_eq!(zend_string_size(0), 32);
        assert_eq!(zend_string_size(7), 32);
        assert_eq!(zend_string_size(8), 40);
        // 10 MiB of "\x80" with JSON_INVALID_UTF8_SUBSTITUTE, less the
        // quotes, is 3 * (10 MiB - 2) bytes.
        assert_eq!(zend_string_size(31_457_274), 31_457_304);
        assert_eq!(zend_string_size(usize::MAX), usize::MAX & !7);
        // php8.5 -d memory_limit=16M: json_encode(str_repeat("a", 7 << 20))
        // "tried to allocate 7344128 bytes" for the 7 MiB + 2 quotes.
        assert_eq!(smart_str_size((7 << 20) + 2), 7_344_128);
        assert_eq!(smart_str_size(4096 - 25), 4096);
        assert_eq!(smart_str_size(4096 - 24), 8192);
        assert_eq!(smart_str_size(usize::MAX), usize::MAX & !4095);
    }

    #[test]
    fn growth_doubles_while_it_fits() {
        let any = |_: usize| true;
        assert_eq!(grow_output(0, 0, 10, any), Ok(10));
        assert_eq!(grow_output(16, 10, 6, any), Ok(16));
        assert_eq!(grow_output(16, 10, 7, any), Ok(32));
        assert_eq!(grow_output(16, 16, 100, any), Ok(116));
        assert_eq!(grow_output(0, usize::MAX, 1, any), Err(usize::MAX));
    }

    #[test]
    fn growth_shrinks_near_the_limit() {
        // 1000 bytes of room: doubling 1024 does not fit, 512 more does.
        let mut asked = Vec::new();
        let r = grow_output(1024, 1024, 1, |b| {
            asked.push(b);
            b <= 1000
        });
        assert_eq!(r, Ok(1024 + 512));
        assert_eq!(asked, [1024, 512]);
        // Down to exactly what is needed.
        assert_eq!(grow_output(1024, 1024, 3, |b| b <= 3), Ok(1027));
        // Not even that: php-src's figure for the needed length.
        assert_eq!(
            grow_output(1 << 20, 1 << 20, 10, |b| b < 10),
            Err(smart_str_size((1 << 20) + 10))
        );
    }
}
