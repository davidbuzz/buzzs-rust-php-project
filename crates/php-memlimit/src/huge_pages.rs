// buzzs-rust-php-project - Copyright (c) 2026 Aleks <al-git001@none.at>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Transparent huge pages, as php-src decides them: only with
//! `USE_ZEND_ALLOC_HUGE_PAGES` set to a non-zero number
//! (`alloc_globals_ctor`, `zend_mm_use_huge_pages`). mimalloc marks its
//! arenas `MADV_HUGEPAGE` by default; a process that forks workers then
//! copies a whole 2 MiB page into a worker that writes 8 bytes of it
//! (measured with 10 php-fpm workers: 4.8 MB private per worker after a
//! request with huge pages, 2.9 MB without).

#[cfg(target_os = "linux")]
use std::ffi::c_void;
use std::ffi::{c_int, c_long};

extern "C" {
    fn mi_option_set(option: c_int, value: c_long);
}

/// `mi_option_allow_thp` in mimalloc's `mi_option_e`, which the Rust
/// binding does not name; read from `mimalloc.h` of version 3.3.2 (a test
/// fails when the linked mimalloc is another).
const MI_OPTION_ALLOW_THP: c_int = 43;

/// `USE_ZEND_ALLOC_HUGE_PAGES` as php-src reads it: huge pages when the
/// value parses (like `ZEND_ATOL`) to non-zero.
#[must_use]
pub fn huge_pages_from_env(value: Option<&[u8]>) -> bool {
    value.is_some_and(|v| crate::atol(v) != 0)
}

/// Lets the allocator use transparent huge pages (`on`), or not, as
/// php-src without `USE_ZEND_ALLOC_HUGE_PAGES`. Call it first in `main`:
/// it turns mimalloc's option off for the arenas still to come and takes
/// `MADV_HUGEPAGE` back from the arena reserved before `main` (the mapping
/// that holds the heap). Unlike `prctl(PR_SET_THP_DISABLE)` this does not
/// reach the programs PHP starts with `exec`.
pub fn set_huge_pages(on: bool) {
    if on {
        return;
    }
    // SAFETY: mimalloc's option setter takes any option and value.
    unsafe { mi_option_set(MI_OPTION_ALLOW_THP, 0) };
    unmark_heap_mapping();
}

/// Takes `MADV_HUGEPAGE` back from the mapping that holds the heap (Linux:
/// `/proc/self/maps`, `MADV_NOHUGEPAGE`).
#[cfg(target_os = "linux")]
fn unmark_heap_mapping() {
    let probe = Box::new(0u64);
    let addr = std::ptr::from_ref::<u64>(&probe) as usize;
    let Some((start, end)) = std::fs::read("/proc/self/maps")
        .ok()
        .and_then(|maps| mapping_of(&maps, addr))
    else {
        return;
    };
    // SAFETY: madvise only changes the advice of this process's own
    // mapping that holds `probe`; MADV_NOHUGEPAGE does not touch its data.
    unsafe {
        libc::madvise(start as *mut c_void, end - start, libc::MADV_NOHUGEPAGE);
    }
}

/// Elsewhere mimalloc's option is all there is.
#[cfg(not(target_os = "linux"))]
fn unmark_heap_mapping() {}

/// The start and end of the mapping in the `/proc/self/maps` listing
/// `maps` that contains `addr`.
#[cfg(any(target_os = "linux", test))]
fn mapping_of(maps: &[u8], addr: usize) -> Option<(usize, usize)> {
    maps.split(|&c| c == b'\n').find_map(|line| {
        let range = line.split(|&c| c == b' ').next()?;
        let dash = range.iter().position(|&c| c == b'-')?;
        let hex = |s: &[u8]| usize::from_str_radix(std::str::from_utf8(s).ok()?, 16).ok();
        let (start, end) = (hex(&range[..dash])?, hex(&range[dash + 1..])?);
        (start <= addr && addr < end).then_some((start, end))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" {
        fn mi_version() -> c_int;
    }

    #[test]
    fn the_option_number_matches_the_linked_mimalloc() {
        // SAFETY: mi_version has no preconditions.
        let v = unsafe { mi_version() };
        // `MI_MALLOC_VERSION` of the mimalloc.h `MI_OPTION_ALLOW_THP` was read from.
        assert_eq!(
            v, 30302,
            "re-read mi_option_allow_thp from this mimalloc's mimalloc.h"
        );
    }

    #[test]
    fn env_parses_like_zend_atol() {
        assert!(!huge_pages_from_env(None));
        assert!(!huge_pages_from_env(Some(b"")));
        assert!(!huge_pages_from_env(Some(b"0")));
        assert!(!huge_pages_from_env(Some(b"no")));
        assert!(huge_pages_from_env(Some(b"1")));
        assert!(huge_pages_from_env(Some(b" 2x")));
        assert!(huge_pages_from_env(Some(b"-1")));
    }

    #[test]
    fn the_mapping_holding_an_address_is_found() {
        let maps = b"5a0000000000-5a0000001000 r--p 00000000 08:01 1234 /usr/bin/php\n\
7f0000000000-7f0000400000 rw-p 00000000 00:00 0\n";
        assert_eq!(
            mapping_of(maps, 0x7f00_0000_1000),
            Some((0x7f00_0000_0000, 0x7f00_0040_0000))
        );
        assert_eq!(mapping_of(maps, 0x7f00_0040_0000), None);
        assert_eq!(mapping_of(b"", 1), None);
    }
}
