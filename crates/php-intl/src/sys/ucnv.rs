//! `ucnv_*` — charset converters, including the callback trampolines that
//! route ICU's to/from-Unicode error callbacks to Rust.

use std::ffi::{c_char, c_void};

use crate::error::UErrorCode;
use crate::ffi::{self, len32, UConverter, UConverterFromUnicodeArgs, UConverterToUnicodeArgs};

/// Owned `UConverter*`, plus the callback context ICU calls into (dropped
/// only after `ucnv_close`, which itself fires `REASON_CLOSE` callbacks).
pub struct Converter(*mut UConverter, Option<Box<CallbackCell>>);

impl std::fmt::Debug for Converter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Converter")
            .field(&self.0)
            .finish_non_exhaustive()
    }
}

// SAFETY: the handle may move between threads; it is never shared.
unsafe impl Send for Converter {}

/// The target buffer of a to-Unicode callback.
#[derive(Debug)]
pub struct U16Sink<'a> {
    args: &'a mut UConverterToUnicodeArgs,
}

impl U16Sink<'_> {
    /// Free space in UTF-16 code units.
    pub fn available(&self) -> i64 {
        // SAFETY: target and targetLimit point into the same ICU buffer.
        i64::try_from(unsafe { self.args.targetLimit.offset_from(self.args.target) }).unwrap_or(0)
    }

    /// Appends one code unit (the caller checked `available()`).
    pub fn push(&mut self, u: u16) {
        if self.available() < 1 {
            return;
        }
        // SAFETY: there is room for one more code unit before targetLimit.
        unsafe {
            *self.args.target = u;
            self.args.target = self.args.target.add(1);
        }
    }
}

/// The target buffer of a from-Unicode callback.
#[derive(Debug)]
pub struct ByteSink<'a> {
    args: &'a mut UConverterFromUnicodeArgs,
}

impl ByteSink<'_> {
    /// Free space in bytes.
    pub fn available(&self) -> i64 {
        // SAFETY: target and targetLimit point into the same ICU buffer.
        i64::try_from(unsafe { self.args.targetLimit.offset_from(self.args.target) }).unwrap_or(0)
    }

    /// Appends bytes (the caller checked `available()`).
    pub fn push(&mut self, b: &[u8]) {
        if self.available() < i64::try_from(b.len()).unwrap_or(i64::MAX) {
            return;
        }
        // SAFETY: there is room for `b.len()` bytes before targetLimit.
        unsafe {
            std::ptr::copy_nonoverlapping(b.as_ptr(), self.args.target.cast::<u8>(), b.len());
            self.args.target = self.args.target.add(b.len());
        }
    }
}

/// Receives ICU's conversion callbacks.
#[allow(clippy::wrong_self_convention)]
pub trait Callbacks {
    /// To-Unicode callback: `source` is the remaining input, `units` the
    /// offending bytes; `error` is the in/out status.
    fn to_u(
        &mut self,
        reason: i32,
        source: &[u8],
        units: &[u8],
        error: &mut i32,
        sink: &mut U16Sink<'_>,
    );
    /// From-Unicode callback: `units` are the offending code units.
    fn from_u(
        &mut self,
        reason: i32,
        units: &[u16],
        code_point: i32,
        error: &mut i32,
        sink: &mut ByteSink<'_>,
    );
}

/// The callback context: a stable pointer to a boxed trait object.
pub type CallbackCell = Box<dyn Callbacks>;

unsafe extern "C" fn to_u_trampoline(
    context: *const c_void,
    args: *mut UConverterToUnicodeArgs,
    code_units: *const c_char,
    length: i32,
    reason: i32,
    err: *mut UErrorCode,
) {
    // SAFETY: context is the `*mut CallbackCell` registered by set_callbacks,
    // valid until the converter is closed; ICU passes valid args/err.
    let (cell, args, err) = unsafe {
        (
            &mut *(context.cast_mut().cast::<CallbackCell>()),
            &mut *args,
            &mut *err,
        )
    };
    let source = if args.source.is_null() {
        &[][..]
    } else {
        // SAFETY: source..sourceLimit is the remaining input owned by ICU.
        unsafe {
            std::slice::from_raw_parts(
                args.source.cast::<u8>(),
                usize::try_from(args.sourceLimit.offset_from(args.source)).unwrap_or(0),
            )
        }
    };
    let units = if code_units.is_null() {
        &[][..]
    } else {
        // SAFETY: ICU passes `length` bytes at code_units.
        unsafe {
            std::slice::from_raw_parts(
                code_units.cast::<u8>(),
                usize::try_from(length).unwrap_or(0),
            )
        }
    };
    let mut e = err.0;
    let mut sink = U16Sink { args };
    cell.to_u(reason, source, units, &mut e, &mut sink);
    err.0 = e;
}

unsafe extern "C" fn from_u_trampoline(
    context: *const c_void,
    args: *mut UConverterFromUnicodeArgs,
    code_units: *const u16,
    length: i32,
    code_point: i32,
    reason: i32,
    err: *mut UErrorCode,
) {
    // SAFETY: as in to_u_trampoline.
    let (cell, args, err) = unsafe {
        (
            &mut *(context.cast_mut().cast::<CallbackCell>()),
            &mut *args,
            &mut *err,
        )
    };
    let units = if code_units.is_null() {
        &[][..]
    } else {
        // SAFETY: ICU passes `length` UChars at code_units.
        unsafe { std::slice::from_raw_parts(code_units, usize::try_from(length).unwrap_or(0)) }
    };
    let mut e = err.0;
    let mut sink = ByteSink { args };
    cell.from_u(reason, units, code_point, &mut e, &mut sink);
    err.0 = e;
}

impl Converter {
    /// `ucnv_open(name)` → the converter and the (warning) status.
    pub fn open(name: &str) -> Result<(Self, UErrorCode), UErrorCode> {
        let n = ffi::cstring(name);
        let mut st = UErrorCode::ZERO;
        // SAFETY: n is NUL-terminated.
        let p = unsafe { ffi::ucnv_open(n.as_ptr(), &mut st) };
        if st.is_failure() || p.is_null() {
            if !p.is_null() {
                // SAFETY: closed once.
                unsafe { ffi::ucnv_close(p) };
            }
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        Ok((Self(p, None), st))
    }

    /// Registers the trampolines, routing callbacks to `cell` (owned by
    /// the converter until after it is closed).
    pub fn set_callbacks(&mut self, cell: CallbackCell) -> Result<(), UErrorCode> {
        let mut boxed = Box::new(cell);
        let cell: *mut CallbackCell = &mut *boxed;
        self.1 = Some(boxed);
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live; the context is boxed in self.1 (stable
        // address) and outlives every ICU call on this converter.
        unsafe {
            ffi::ucnv_setToUCallBack(
                self.0,
                Some(to_u_trampoline),
                cell.cast_const().cast(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut st,
            );
        }
        st.check()?;
        // SAFETY: as above.
        unsafe {
            ffi::ucnv_setFromUCallBack(
                self.0,
                Some(from_u_trampoline),
                cell.cast_const().cast(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut st,
            );
        }
        st.check()
    }

    /// `ucnv_getName()`.
    pub fn name(&self) -> Result<String, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: self.0 is live; the name is static.
        let p = unsafe { ffi::ucnv_getName(self.0, &mut st) };
        st.check()?;
        // SAFETY: p is NUL-terminated or null.
        Ok(unsafe { ffi::cstr_to_string(p) }.unwrap_or_default())
    }

    /// `ucnv_getType()`.
    pub fn conv_type(&self) -> i32 {
        // SAFETY: self.0 is live.
        unsafe { ffi::ucnv_getType(self.0) }
    }

    /// `ucnv_setSubstChars()`.
    pub fn set_subst_chars(&mut self, chars: &[u8]) -> Result<(), UErrorCode> {
        let mut st = UErrorCode::ZERO;
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        // SAFETY: chars is a live slice; the length is passed as int8 like php-src.
        unsafe {
            ffi::ucnv_setSubstChars(self.0, chars.as_ptr().cast(), chars.len() as i8, &mut st);
        }
        st.check()
    }

    /// `ucnv_getSubstChars()`.
    pub fn subst_chars(&self) -> Result<Vec<u8>, UErrorCode> {
        let mut buf = [0u8; 127];
        let mut len: i8 = 127;
        let mut st = UErrorCode::ZERO;
        // SAFETY: buf has 127 bytes as advertised.
        unsafe { ffi::ucnv_getSubstChars(self.0, buf.as_mut_ptr().cast(), &mut len, &mut st) };
        st.check()?;
        Ok(buf[..usize::try_from(len).unwrap_or(0).min(127)].to_vec())
    }

    /// `ucnv_toUChars()` with php-src's preflight-then-convert sequence.
    pub fn to_uchars(&mut self, src: &[u8]) -> Result<Vec<u16>, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: preflight with a null destination.
        let n = unsafe {
            ffi::ucnv_toUChars(
                self.0,
                std::ptr::null_mut(),
                0,
                src.as_ptr().cast(),
                len32(src),
                &mut st,
            )
        };
        if st.is_failure() && st != UErrorCode::BUFFER_OVERFLOW_ERROR {
            return Err(st);
        }
        let cap = usize::try_from(n).unwrap_or(0) + 1;
        let mut buf = vec![0u16; cap + 1];
        let mut st = UErrorCode::ZERO;
        // SAFETY: buf has `cap` + 1 UChars.
        let m = unsafe {
            ffi::ucnv_toUChars(
                self.0,
                buf.as_mut_ptr(),
                len32(&buf[..cap]),
                src.as_ptr().cast(),
                len32(src),
                &mut st,
            )
        };
        st.check()?;
        buf.truncate(usize::try_from(m).unwrap_or(0));
        Ok(buf)
    }

    /// `ucnv_fromUChars()` with php-src's preflight-then-convert sequence.
    pub fn from_uchars(&mut self, src: &[u16]) -> Result<Vec<u8>, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: preflight with a null destination.
        let n = unsafe {
            ffi::ucnv_fromUChars(
                self.0,
                std::ptr::null_mut(),
                0,
                src.as_ptr(),
                len32(src),
                &mut st,
            )
        };
        if st.is_failure() && st != UErrorCode::BUFFER_OVERFLOW_ERROR {
            return Err(st);
        }
        let len = usize::try_from(n).unwrap_or(0);
        let mut buf = vec![0u8; len + 1];
        let mut st = UErrorCode::ZERO;
        // SAFETY: buf has len + 1 bytes.
        let m = unsafe {
            ffi::ucnv_fromUChars(
                self.0,
                buf.as_mut_ptr().cast(),
                len32(&buf),
                src.as_ptr(),
                len32(src),
                &mut st,
            )
        };
        st.check()?;
        buf.truncate(usize::try_from(m).unwrap_or(0));
        Ok(buf)
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once (callbacks may fire; their
        // context outlives the converter by contract of set_callbacks).
        unsafe { ffi::ucnv_close(self.0) }
    }
}

/// `ucnv_countAvailable()`/`ucnv_getAvailableName()`.
pub fn available() -> Vec<String> {
    // SAFETY: no preconditions.
    let n = unsafe { ffi::ucnv_countAvailable() };
    // SAFETY: i is in range; names are static.
    (0..n)
        .filter_map(|i| unsafe { ffi::cstr_to_string(ffi::ucnv_getAvailableName(i)) })
        .collect()
}

/// `ucnv_countAliases()`/`ucnv_getAlias()`.
pub fn aliases(name: &str) -> Result<Vec<String>, UErrorCode> {
    let n = ffi::cstring(name);
    let mut st = UErrorCode::ZERO;
    // SAFETY: n is NUL-terminated.
    let count = unsafe { ffi::ucnv_countAliases(n.as_ptr(), &mut st) };
    st.check()?;
    let mut out = Vec::new();
    for i in 0..count {
        let mut st = UErrorCode::ZERO;
        // SAFETY: as above; aliases are static.
        let p = unsafe { ffi::ucnv_getAlias(n.as_ptr(), i, &mut st) };
        st.check()?;
        // SAFETY: p is NUL-terminated or null.
        out.push(unsafe { ffi::cstr_to_string(p) }.unwrap_or_default());
    }
    Ok(out)
}

/// `ucnv_countStandards()`/`ucnv_getStandard()`.
pub fn standards() -> Result<Vec<String>, UErrorCode> {
    // SAFETY: no preconditions.
    let count = unsafe { ffi::ucnv_countStandards() };
    let mut out = Vec::new();
    for i in 0..count {
        let mut st = UErrorCode::ZERO;
        // SAFETY: i in range; names are static.
        let p = unsafe { ffi::ucnv_getStandard(i, &mut st) };
        st.check()?;
        // SAFETY: p is NUL-terminated or null.
        out.push(unsafe { ffi::cstr_to_string(p) }.unwrap_or_default());
    }
    Ok(out)
}
