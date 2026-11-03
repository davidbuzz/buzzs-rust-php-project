//! Per-request `ext/pcre` state (`PCRE_G`): the compiled-pattern cache, the
//! `pcre.*` INI settings and `preg_last_error()`.
//!
//! The VM is single-threaded, so the state lives in a thread-local
//! [`Pcre`]. It is only ever borrowed for the duration of a single
//! `php-pcre` call — never across a call back into the VM — so user
//! callbacks may freely re-enter the `preg_*` functions.

use std::cell::RefCell;

use php_pcre::{MatchConfig, Pcre, PregError, Regex};
use php_vm::ext::VmResult;
use php_vm::Vm;

thread_local! {
    static STATE: RefCell<Pcre> = RefCell::new(Pcre::new());
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

/// `RSHUTDOWN`: drops the cache and resets the error state.
pub(crate) fn reset() {
    with(|p| *p = Pcre::new());
}

/// Re-reads the `pcre.*` INI settings (PHP applies them through on-modify
/// handlers; reading them before each operation is equivalent) and
/// returns the match configuration to use.
pub(crate) fn sync_config(vm: &Vm) -> MatchConfig {
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
    fn last_error_round_trip() {
        reset();
        assert_eq!(last_error(), PregError::NoError);
        set_last_error(PregError::BadUtf8);
        assert_eq!(last_error(), PregError::BadUtf8);
        reset();
        assert_eq!(last_error(), PregError::NoError);
    }
}
