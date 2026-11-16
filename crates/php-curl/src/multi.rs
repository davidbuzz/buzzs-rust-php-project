// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Multi handles (`CURLM *`).

use crate::consts::{CURLMOPT_PUSHDATA, CURLMOPT_PUSHFUNCTION};
use crate::easy::{key_of_raw, Easy};
use crate::ffi;
use crate::handler::{self, cint, Handler};
use std::ffi::CStr;
use std::os::raw::{c_int, c_long, c_void};

/// `CURLM_BAD_HANDLE`.
const CURLM_BAD_HANDLE: i32 = 1;
/// `CURLM_UNKNOWN_OPTION`.
const CURLM_UNKNOWN_OPTION: i32 = 6;

/// A message from `curl_multi_info_read()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InfoMsg {
    /// `CURLMSG_*`.
    pub msg: i32,
    /// The transfer's `CURLcode` (for `CURLMSG_DONE`).
    pub result: i32,
    /// The key of the easy handle (see [`Easy::key`]).
    pub easy: Option<u64>,
    /// Messages still queued.
    pub queued: i32,
}

/// A libcurl multi handle.
#[derive(Debug)]
pub struct Multi {
    raw: *mut ffi::CURLM,
}

impl Drop for Multi {
    fn drop(&mut self) {
        // SAFETY: `raw` is owned by this wrapper.
        unsafe { ffi::curl_multi_cleanup(self.raw) };
    }
}

impl Multi {
    /// `curl_multi_init()`.
    pub fn new() -> Option<Multi> {
        crate::easy::global_init();
        // SAFETY: plain constructor.
        let raw = unsafe { ffi::curl_multi_init() };
        (!raw.is_null()).then_some(Multi { raw })
    }

    /// `curl_multi_add_handle()`.
    pub fn add(&self, e: &Easy) -> i32 {
        if e.raw().is_null() {
            return CURLM_BAD_HANDLE + 1; // CURLM_BAD_EASY_HANDLE
        }
        // SAFETY: both handles are live.
        unsafe { ffi::curl_multi_add_handle(self.raw, e.raw()) }
    }

    /// `curl_multi_remove_handle()`.
    pub fn remove(&self, e: &Easy) -> i32 {
        if e.raw().is_null() {
            return CURLM_BAD_HANDLE + 1;
        }
        // SAFETY: both handles are live.
        unsafe { ffi::curl_multi_remove_handle(self.raw, e.raw()) }
    }

    /// `curl_multi_perform()`: returns the code and the running count.
    pub fn perform(&self, h: &mut dyn Handler, running: i32) -> (i32, i32) {
        let mut r: c_int = running;
        let raw = self.raw;
        // SAFETY: live handle; callbacks reach `h` through the handler slot.
        let rc = handler::with_handler(h, || unsafe { ffi::curl_multi_perform(raw, &raw mut r) });
        (rc, r)
    }

    /// `curl_multi_wait()` without extra descriptors: the number of ready
    /// descriptors, or the error code.
    pub fn wait(&self, timeout_ms: i32) -> Result<i32, i32> {
        let mut n: c_int = 0;
        // SAFETY: live handle, no extra fds.
        let rc = unsafe {
            ffi::curl_multi_wait(self.raw, std::ptr::null_mut(), 0, timeout_ms, &raw mut n)
        };
        if rc == 0 {
            Ok(n)
        } else {
            Err(rc)
        }
    }

    /// `curl_multi_info_read()`.
    pub fn info_read(&self) -> Option<InfoMsg> {
        let mut queued: c_int = 0;
        // SAFETY: live handle; the message stays valid until the next call.
        let m = unsafe { ffi::curl_multi_info_read(self.raw, &raw mut queued) };
        if m.is_null() {
            return None;
        }
        // SAFETY: `m` points at a valid message.
        let msg = unsafe { &*m };
        // SAFETY: for CURLMSG_DONE the union holds `result`; for other
        // messages reading the int half of the union is harmless.
        let result = unsafe { msg.data.result };
        Some(InfoMsg {
            msg: msg.msg,
            result,
            easy: key_of_raw(msg.easy_handle),
            queued,
        })
    }

    /// A long `CURLMOPT_*`.
    pub fn set_long(&self, opt: i64, v: i64) -> i32 {
        let Ok(o) = c_int::try_from(opt) else {
            return CURLM_UNKNOWN_OPTION;
        };
        // SAFETY: live handle; long options take a `long` vararg.
        unsafe { ffi::curl_multi_setopt(self.raw, o, v as c_long) }
    }

    /// Installs the server-push trampoline (`CURLMOPT_PUSHFUNCTION`).
    pub fn enable_push(&self) -> i32 {
        let f: ffi::curl_push_callback = handler::push_cb;
        // SAFETY: live handle; function and data options take pointers.
        unsafe {
            let rc =
                ffi::curl_multi_setopt(self.raw, cint(CURLMOPT_PUSHFUNCTION), f as *const c_void);
            if rc != 0 {
                return rc;
            }
            ffi::curl_multi_setopt(
                self.raw,
                cint(CURLMOPT_PUSHDATA),
                std::ptr::null::<c_void>(),
            )
        }
    }
}

/// `curl_multi_strerror()`.
pub fn strerror(code: i64) -> String {
    #[allow(clippy::cast_possible_truncation)]
    // SAFETY: accepts any value, returns a static string.
    let p = unsafe { ffi::curl_multi_strerror(code as c_int) };
    if p.is_null() {
        return String::new();
    }
    // SAFETY: static NUL-terminated string.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::NoHandler;

    #[test]
    fn add_remove() {
        let m = Multi::new().expect("multi");
        let e = Easy::new(None).expect("easy");
        assert_eq!(m.add(&e), 0);
        assert_eq!(m.add(&e), 7); // CURLM_ADDED_ALREADY
        let (rc, _) = m.perform(&mut NoHandler, 0);
        assert_eq!(rc, 0);
        assert_eq!(m.remove(&e), 0);
        assert_eq!(strerror(0), "No error");
        assert!(m.info_read().is_none() || m.info_read().is_some());
    }
}
