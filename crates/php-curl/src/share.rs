//! Share handles (`CURLSH *`), including the persistent ones of
//! `curl_share_init_persistent()`.

use crate::consts::CURLSHOPT_SHARE;
use crate::ffi;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::{c_int, c_long};

/// `CURLSHE_BAD_OPTION`.
const CURLSHE_BAD_OPTION: i32 = 1;

thread_local! {
    /// `CURL_G(persistent_curlsh)`: persistent handles by option set,
    /// alive for the whole process.
    static PERSISTENT: RefCell<HashMap<u64, usize>> = RefCell::new(HashMap::new());
}

/// A libcurl share handle.
#[derive(Debug)]
pub struct Share {
    raw: *mut ffi::CURLSH,
    persistent: bool,
}

impl Drop for Share {
    fn drop(&mut self) {
        if !self.persistent {
            // SAFETY: `raw` is owned by this wrapper.
            unsafe { ffi::curl_share_cleanup(self.raw) };
        }
    }
}

impl Share {
    /// `curl_share_init()`.
    pub fn new() -> Option<Share> {
        crate::easy::global_init();
        // SAFETY: plain constructor.
        let raw = unsafe { ffi::curl_share_init() };
        (!raw.is_null()).then_some(Share {
            raw,
            persistent: false,
        })
    }

    /// The persistent handle for the option set `id` (bit set over
    /// `opts`), created with `CURLSHOPT_SHARE` of every value in `opts`.
    /// `Err` carries the failing `CURLSHcode`.
    pub fn persistent(id: u64, opts: &[i64]) -> Result<Share, i32> {
        if let Some(raw) = PERSISTENT.with(|p| p.borrow().get(&id).copied()) {
            return Ok(Share {
                raw: raw as *mut ffi::CURLSH,
                persistent: true,
            });
        }
        let s = Share::new().ok_or(2)?; // CURLSHE_NOMEM-ish
        for &o in opts {
            let rc = s.setopt(CURLSHOPT_SHARE, o);
            if rc != 0 {
                return Err(rc);
            }
        }
        PERSISTENT.with(|p| p.borrow_mut().insert(id, s.raw as usize));
        let raw = s.raw;
        std::mem::forget(s);
        Ok(Share {
            raw,
            persistent: true,
        })
    }

    pub(crate) fn raw(&self) -> *mut ffi::CURLSH {
        self.raw
    }

    /// `curl_share_setopt()` with a long value.
    pub fn setopt(&self, opt: i64, v: i64) -> i32 {
        let Ok(o) = c_int::try_from(opt) else {
            return CURLSHE_BAD_OPTION;
        };
        // SAFETY: live handle; the options used take a `long` vararg.
        unsafe { ffi::curl_share_setopt(self.raw, o, v as c_long) }
    }
}

/// `curl_share_strerror()`.
pub fn strerror(code: i64) -> String {
    #[allow(clippy::cast_possible_truncation)]
    // SAFETY: accepts any value, returns a static string.
    let p = unsafe { ffi::curl_share_strerror(code as c_int) };
    if p.is_null() {
        return String::new();
    }
    // SAFETY: static NUL-terminated string.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::{CURLSHOPT_UNSHARE, CURL_LOCK_DATA_DNS};

    #[test]
    fn share_opts() {
        let s = Share::new().expect("share");
        assert_eq!(s.setopt(CURLSHOPT_SHARE, CURL_LOCK_DATA_DNS), 0);
        assert_eq!(s.setopt(CURLSHOPT_UNSHARE, CURL_LOCK_DATA_DNS), 0);
        assert_eq!(strerror(0), "No error");
        let a = Share::persistent(1, &[CURL_LOCK_DATA_DNS]).expect("p");
        let b = Share::persistent(1, &[CURL_LOCK_DATA_DNS]).expect("p");
        assert_eq!(a.raw(), b.raw());
    }
}
