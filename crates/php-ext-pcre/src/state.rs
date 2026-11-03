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
    /// The last large subject a `/u` match proved to be valid UTF-8.
    ///
    /// Stand-in for PHP's `IS_STR_VALID_UTF8` string flag (which `ZStr`
    /// does not have yet, see `PROPOSALS.md`). Holding a handle keeps the
    /// instance alive, so a pointer match can never hit a different (freed
    /// and reallocated) string, and `ZStr` is never mutated while shared.
    static VALID_UTF8: RefCell<Option<ZStr>> = const { RefCell::new(None) };
}

/// Subjects shorter than this are not remembered: validating them is cheap,
/// and not holding on to them keeps refcounts and `memory_get_usage()`
/// exactly as in PHP for ordinary strings.
const VALID_UTF8_MIN_LEN: usize = 4096;

/// Whether `s` is known to be valid UTF-8 (see [`remember_valid_utf8`]).
pub(crate) fn known_valid_utf8(s: &ZStr) -> bool {
    VALID_UTF8.with(|c| c.borrow().as_ref().is_some_and(|k| ZStr::ptr_eq(k, s)))
}

/// Records that a `/u` match validated `s` (`MatchOutcome::subject_valid_utf8`).
pub(crate) fn remember_valid_utf8(s: &ZStr) {
    if s.len() >= VALID_UTF8_MIN_LEN && !s.is_interned() {
        VALID_UTF8.with(|c| *c.borrow_mut() = Some(s.clone()));
    }
}

/// The match configuration for `subject`, carrying the UTF-8 validity hint.
pub(crate) fn config_for(subject: &ZStr) -> MatchConfig {
    with(|p| p.config()).with_known_valid_utf8(known_valid_utf8(subject))
}

/// Marks the cached `pcre.*` settings stale (INI on-modify).
pub(crate) fn invalidate_config() {
    CONFIG_STALE.with(|c| c.set(true));
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

/// `RINIT`/`RSHUTDOWN`: drops the cache and resets the error state.
pub(crate) fn reset() {
    with(|p| *p = Pcre::new());
    VALID_UTF8.with(|c| c.borrow_mut().take());
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

/// `pcre_get_compiled_regex_cache()`: compiles `pattern` (or fetches it
/// from the cache), emitting PHP's warnings. `Ok(None)` means the pattern
/// is invalid; the warning was emitted and `preg_last_error()` set.
pub(crate) fn compile(vm: &mut Vm, pattern: &[u8]) -> VmResult<Option<Regex>> {
    sync_config(vm);
    let locale = ctype_locale();
    let (result, warnings) = with(|p| {
        let r = p.compile_with_locale(pattern, locale.as_deref());
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

    #[test]
    fn valid_utf8_cache_is_by_identity() {
        reset();
        let big = ZStr::from(vec![b'a'; VALID_UTF8_MIN_LEN]);
        let same_bytes = ZStr::from(vec![b'a'; VALID_UTF8_MIN_LEN]);
        assert!(!known_valid_utf8(&big));
        remember_valid_utf8(&big);
        assert!(known_valid_utf8(&big));
        assert!(known_valid_utf8(&big.clone()));
        assert!(!known_valid_utf8(&same_bytes));
        let small = ZStr::from("abc");
        remember_valid_utf8(&small);
        assert!(!known_valid_utf8(&small));
        assert!(known_valid_utf8(&big));
        reset();
        assert!(!known_valid_utf8(&big));
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
