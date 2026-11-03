//! `ulistfmt_*`.

use crate::error::UErrorCode;
use crate::ffi::{self, UChar, UListFormatter};

/// An owned `UListFormatter*`.
#[derive(Debug)]
pub struct ListFormatter(*mut UListFormatter);

impl Drop for ListFormatter {
    fn drop(&mut self) {
        // SAFETY: owned pointer from ulistfmt_openForType.
        unsafe { ffi::ulistfmt_close(self.0) }
    }
}

impl ListFormatter {
    /// `ulistfmt_openForType(locale, type, width)`.
    pub fn open(locale: &str, kind: i32, width: i32) -> Result<Self, UErrorCode> {
        let loc = ffi::cstring(locale);
        let mut st = UErrorCode::ZERO;
        // SAFETY: loc is NUL-terminated; st is a valid out-pointer.
        let p = unsafe { ffi::ulistfmt_openForType(loc.as_ptr(), kind, width, &mut st) };
        if p.is_null() {
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        let out = Self(p);
        st.check()?;
        Ok(out)
    }

    /// `ulistfmt_format()`.
    pub fn format(&self, items: &[Vec<UChar>]) -> Result<Vec<UChar>, UErrorCode> {
        let ptrs: Vec<*const UChar> = items.iter().map(|s| s.as_ptr()).collect();
        let lens: Vec<i32> = items.iter().map(|s| ffi::len32(s)).collect();
        // SAFETY: ptrs/lens describe `items`, which outlives the call; the
        // buffer contract is handled by fill_u16.
        ffi::fill_u16(|b, c, st| unsafe {
            ffi::ulistfmt_format(
                self.0,
                ptrs.as_ptr(),
                lens.as_ptr(),
                ffi::len32(&ptrs),
                b,
                c,
                st,
            )
        })
    }
}
