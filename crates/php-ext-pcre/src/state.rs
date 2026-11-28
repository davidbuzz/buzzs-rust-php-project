// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Per-request `ext/pcre` state (`PCRE_G`): the compiled-pattern cache, the
//! `pcre.*` INI settings and `preg_last_error()`.
//!
//! The VM is single-threaded, so the state lives in a thread-local
//! [`Pcre`]. It is only ever borrowed for the duration of a single
//! `php-pcre` call — never across a call back into the VM — so user
//! callbacks may freely re-enter the `preg_*` functions.

use std::cell::{Cell, RefCell};

use php_pcre::{MatchConfig, Pcre, PregError, Regex};
use php_runtime::ZStr;
use php_vm::ext::VmResult;
use php_vm::Vm;

thread_local! {
    static STATE: RefCell<Pcre> = RefCell::new(Pcre::new());
    /// Whether the `pcre.*` INI values must be re-read (set by the
    /// on-modify handlers and at request boundaries).
    static CONFIG_STALE: Cell<bool> = const { Cell::new(true) };
}

/// Whether `s` is known to be valid UTF-8: the string's
/// `IS_STR_VALID_UTF8` flag (`is_known_valid_utf8()`; the start-offset
/// check is php-pcre's).
pub(crate) fn known_valid_utf8(s: &ZStr) -> bool {
    s.is_valid_utf8()
}

/// Records that a `/u` match validated the whole subject
/// (`MatchOutcome::subject_valid_utf8`: no error, offset 0), as
/// `php_pcre_match_impl` adds `IS_STR_VALID_UTF8` to it; interned subjects
/// are left alone (`!ZSTR_IS_INTERNED`), which `mark_valid_utf8` does.
pub(crate) fn remember_valid_utf8(s: &ZStr) {
    s.mark_valid_utf8();
}

/// The match configuration for `subject`, carrying the UTF-8 validity hint.
pub(crate) fn config_for(subject: &ZStr) -> MatchConfig {
    with(|p| p.config()).with_known_valid_utf8(known_valid_utf8(subject))
}

/// Marks the cached `pcre.*` settings stale (INI on-modify, request
/// start), and forgets the patterns [`leaf_cached`] saw compiled (a new
/// setting compiles them again).
pub(crate) fn invalidate_config() {
    CONFIG_STALE.with(|c| c.set(true));
    let _ = LEAF_SEEN.try_with(|m| {
        if let Ok(mut m) = m.try_borrow_mut() {
            let old = std::mem::take(&mut m.0);
            php_memlimit::persistent(|| drop(old));
            m.1 = 0;
        }
    });
}

/// Runs `f` with the request's [`Pcre`] state.
pub(crate) fn with<R>(f: impl FnOnce(&mut Pcre) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// Stores the value `preg_last_error()` returns.
pub(crate) fn set_last_error(e: PregError) {
    with(|p| p.set_last_error(e));
}

/// The current `preg_last_error()` value.
pub(crate) fn last_error() -> PregError {
    with(|p| p.last_error())
}

/// `RSHUTDOWN`: resets the request's state; the compiled patterns stay
/// for the next request, as php-src's `PCRE_G(pcre_cache)` does.
pub(crate) fn reset() {
    with(Pcre::end_request);
    invalidate_config();
}

/// Re-reads the `pcre.*` INI settings when they changed and returns the
/// match configuration to use.
pub(crate) fn sync_config(vm: &Vm) -> MatchConfig {
    if !CONFIG_STALE.with(|c| c.replace(false)) {
        return with(|p| p.config());
    }
    let backtrack = vm.ini_long("pcre.backtrack_limit");
    let recursion = vm.ini_long("pcre.recursion_limit");
    let jit = vm.ini_bool("pcre.jit");
    with(|p| {
        p.set_backtrack_limit(backtrack);
        p.set_recursion_limit(recursion);
        p.set_jit(jit);
        p.config()
    })
}

/// The `LC_CTYPE` locale when it is not the C locale (`BG(ctype_string)`).
///
/// `setlocale()` is implemented over the C library, so the process locale
/// is the source of truth. PHP itself switches `LC_CTYPE` to `C.UTF-8` at
/// startup without recording it, and the character tables of `C.UTF-8`
/// equal those of `C` for single bytes; all three spellings of the C
/// locale therefore mean "no locale-specific tables".
pub(crate) fn ctype_locale() -> Option<Vec<u8>> {
    // SAFETY: `setlocale` with a NULL locale only queries the current
    // setting. The returned pointer is either NULL or a valid C string that
    // stays valid until the next `setlocale` call; we copy it immediately
    // and the VM is single-threaded.
    let ptr = unsafe { libc::setlocale(libc::LC_CTYPE, std::ptr::null()) };
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-NULL result of `setlocale` is a NUL-terminated string.
    let name = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_bytes();
    if is_c_locale(name) {
        None
    } else {
        Some(name.to_vec())
    }
}

/// Whether `name` names the C locale.
fn is_c_locale(name: &[u8]) -> bool {
    matches!(
        name,
        b"C" | b"POSIX" | b"C.UTF-8" | b"C.utf8" | b"C.UTF8" | b"C.utf-8"
    )
}

/// Patterns [`leaf_cached`] found compiled, as bytes (the most recent
/// [`LEAF_MEMO`] of them, oldest replaced first): a second look at one is
/// a comparison, without the cache's hash and lookup. Kept outside the
/// counted heap, as the cache itself is (`pemalloc`).
const LEAF_MEMO: usize = 8;

thread_local! {
    static LEAF_SEEN: RefCell<(Vec<Box<[u8]>>, usize)> = const { RefCell::new((Vec::new(), 0)) };
}

/// Whether `pattern` is compiled and in the cache (a pattern that compiles
/// compiles under any ctype locale, which changes only the character
/// tables), with no stale `pcre.*` values, so that a `preg_match()` or
/// `preg_replace()` of it reports nothing: the VM's
/// [`php_vm::ext::PregLeafFn`], which lets the tape call those two without
/// a frame (their handler then compiles as always, under the locale). A
/// first use or a changed setting is left to the framed call, which
/// compiles and warns as before.
pub(crate) fn leaf_cached(pattern: &ZStr) -> bool {
    if CONFIG_STALE.with(Cell::get) {
        return false;
    }
    let bytes = pattern.as_bytes();
    let seen = LEAF_SEEN
        .try_with(|m| {
            m.try_borrow()
                .is_ok_and(|m| m.0.iter().any(|p| **p == *bytes))
        })
        .unwrap_or(false);
    if seen {
        return true;
    }
    let cached = STATE
        .try_with(|s| {
            s.try_borrow()
                .is_ok_and(|p| p.cached(bytes, pattern.hash_value()).is_some())
        })
        .unwrap_or(false);
    if cached {
        let _ = LEAF_SEEN.try_with(|m| {
            if let Ok(mut m) = m.try_borrow_mut() {
                let (list, next) = &mut *m;
                let copy: Box<[u8]> = php_memlimit::persistent(|| Box::from(bytes));
                if list.len() < LEAF_MEMO {
                    php_memlimit::persistent(|| list.push(copy));
                } else {
                    let i = *next % LEAF_MEMO;
                    let old = std::mem::replace(&mut list[i], copy);
                    php_memlimit::persistent(|| drop(old));
                    *next = i + 1;
                }
            }
        });
    }
    cached
}

/// `pcre_get_compiled_regex_cache()`: compiles `pattern` (or fetches it
/// from the cache), emitting PHP's warnings. `Ok(None)` means the pattern
/// is invalid; the warning was emitted and `preg_last_error()` set.
///
/// A hit (the common case) is one lookup by the hash the string already
/// carries (`zend_hash_find(&PCRE_G(pcre_cache), regex)`): the pattern is
/// not hashed again and nothing is allocated.
pub(crate) fn compile(vm: &mut Vm, pattern: &ZStr) -> VmResult<Option<Regex>> {
    if CONFIG_STALE.with(Cell::get) {
        sync_config(vm);
    }
    let locale = ctype_locale();
    if locale.is_none() {
        let bytes = pattern.as_bytes();
        let hash = pattern.hash_value();
        if let Some(re) = with(|p| p.cached(bytes, hash)) {
            return Ok(Some(re));
        }
    }
    compile_slow(vm, pattern.as_bytes(), locale)
}

/// [`compile`] past a cache hit: compiles (under the locale's key when
/// one is set), emitting the warnings.
#[inline(never)]
fn compile_slow(vm: &mut Vm, pattern: &[u8], locale: Option<Vec<u8>>) -> VmResult<Option<Regex>> {
    // The cache outlives the request (`pemalloc` in php-src): what a
    // compilation keeps is not part of `memory_get_usage()`.
    let (result, warnings) = with(|p| {
        let r = php_memlimit::persistent(|| p.compile_with_locale(pattern, locale.as_deref()));
        (r, p.take_warnings())
    });
    for w in warnings {
        vm.php_warning(&String::from_utf8_lossy(&w))?;
    }
    match result {
        Ok(re) => Ok(Some(re)),
        Err(e) => {
            vm.php_warning(&String::from_utf8_lossy(&e.message()))?;
            // A user error handler may have run preg_* in between.
            set_last_error(e.last_error());
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_locale_spellings() {
        assert!(is_c_locale(b"C"));
        assert!(is_c_locale(b"C.UTF-8"));
        assert!(is_c_locale(b"POSIX"));
        assert!(!is_c_locale(b"de_DE.ISO-8859-1"));
    }

    /// The hint is the subject's own `IS_STR_VALID_UTF8` flag: any size,
    /// any number of subjects, shared by every handle, kept across
    /// requests' pcre state, never on interned strings.
    #[test]
    fn valid_utf8_hint_is_the_string_flag() {
        reset();
        let big = ZStr::from(vec![b'a'; 64 * 1024]);
        let small = ZStr::from("abc");
        let same_bytes = ZStr::from("abc");
        assert!(!known_valid_utf8(&big));
        remember_valid_utf8(&big);
        remember_valid_utf8(&small);
        assert!(known_valid_utf8(&big) && known_valid_utf8(&small));
        assert!(known_valid_utf8(&big.clone()));
        assert!(
            !known_valid_utf8(&same_bytes),
            "a flag, not a content cache"
        );
        assert_eq!(big.refcount(), 1, "no handle is kept");
        let interned = ZStr::intern(b"interned subject");
        remember_valid_utf8(&interned);
        assert!(!known_valid_utf8(&interned));
        reset();
        assert!(known_valid_utf8(&small));
        // A flagged string loses the flag when it changes in place.
        let mut grown = small;
        grown.append(b"\xff");
        assert!(!known_valid_utf8(&grown));
    }

    /// The cache is looked up by the string's own hash, so the two hashes
    /// must be one function.
    #[test]
    fn string_hash_is_the_cache_hash() {
        for p in [
            &b""[..],
            b"/",
            b"/a+/",
            b"/abcdefgh/",
            b"/(\\d+)-(\\d+)/u",
            b"\xff\x00",
        ] {
            assert_eq!(
                ZStr::from(p).hash_value(),
                php_pcre::pattern_hash(p),
                "{p:?}"
            );
        }
        assert_eq!(
            ZStr::intern(b"/interned/i").hash_value(),
            php_pcre::pattern_hash(b"/interned/i")
        );
    }

    #[test]
    fn last_error_round_trip() {
        reset();
        assert_eq!(last_error(), PregError::NoError);
        set_last_error(PregError::BadUtf8);
        assert_eq!(last_error(), PregError::BadUtf8);
        reset();
        assert_eq!(last_error(), PregError::NoError);
    }
}
