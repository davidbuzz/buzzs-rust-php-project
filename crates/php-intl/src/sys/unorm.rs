// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `unorm2_*` and `uidna_*`.

use crate::error::UErrorCode;
use crate::ffi::{self, len32, UIDNAInfo, UNormalizer2, UIDNA};

/// A borrowed singleton `UNormalizer2*`.
#[derive(Debug, Clone, Copy)]
pub struct Normalizer(*const UNormalizer2);

// SAFETY: the singletons are immutable and thread-safe per ICU docs.
unsafe impl Send for Normalizer {}

/// Which normalizer singleton.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// NFC
    Nfc,
    /// NFD
    Nfd,
    /// NFKC
    Nfkc,
    /// NFKD
    Nfkd,
    /// `NFKC_Casefold`
    NfkcCf,
}

impl Normalizer {
    /// `unorm2_get*Instance()`.
    pub fn get(form: Form) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: the getters return static singletons.
        let p = unsafe {
            match form {
                Form::Nfc => ffi::unorm2_getNFCInstance(&mut st),
                Form::Nfd => ffi::unorm2_getNFDInstance(&mut st),
                Form::Nfkc => ffi::unorm2_getNFKCInstance(&mut st),
                Form::Nfkd => ffi::unorm2_getNFKDInstance(&mut st),
                Form::NfkcCf => ffi::unorm2_getNFKCCasefoldInstance(&mut st),
            }
        };
        st.check()?;
        if p.is_null() {
            return Err(UErrorCode::MEMORY_ALLOCATION_ERROR);
        }
        Ok(Self(p))
    }

    /// `unorm2_normalize()`.
    pub fn normalize(self, s: &[u16]) -> Result<Vec<u16>, UErrorCode> {
        // SAFETY: buffer contract handled by fill_u16; s is a live slice.
        ffi::fill_u16(|b, c, st| unsafe {
            ffi::unorm2_normalize(self.0, s.as_ptr(), len32(s), b, c, st)
        })
    }

    /// `unorm2_isNormalized()`.
    pub fn is_normalized(self, s: &[u16]) -> Result<bool, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: s is a live slice.
        let r = unsafe { ffi::unorm2_isNormalized(self.0, s.as_ptr(), len32(s), &mut st) };
        st.check().map(|()| r != 0)
    }

    /// `unorm2_getRawDecomposition()` (`None` when there is none).
    pub fn raw_decomposition(self, c: i32) -> Option<Vec<u16>> {
        let mut buf = [0u16; 32];
        let mut st = UErrorCode::ZERO;
        // SAFETY: buf has 32 UChars.
        let n =
            unsafe { ffi::unorm2_getRawDecomposition(self.0, c, buf.as_mut_ptr(), 32, &mut st) };
        if n < 0 {
            return None;
        }
        Some(buf[..usize::try_from(n).unwrap_or(0).min(32)].to_vec())
    }
}

/// An owned `UIDNA*` (UTS #46).
#[derive(Debug)]
pub struct Idna(*mut UIDNA);

impl Idna {
    /// `uidna_openUTS46(options)`.
    pub fn open(options: u32) -> Result<Self, UErrorCode> {
        let mut st = UErrorCode::ZERO;
        // SAFETY: no preconditions.
        let p = unsafe { ffi::uidna_openUTS46(options, &mut st) };
        if st.is_failure() || p.is_null() {
            if !p.is_null() {
                // SAFETY: closed once.
                unsafe { ffi::uidna_close(p) };
            }
            return Err(if st.is_failure() {
                st
            } else {
                UErrorCode::MEMORY_ALLOCATION_ERROR
            });
        }
        Ok(Self(p))
    }

    /// `uidna_nameTo{ASCII,Unicode}UTF8` into a buffer of `cap` bytes →
    /// `(length, bytes, info, status)`.
    pub fn convert(
        &self,
        to_ascii: bool,
        name: &[u8],
        cap: usize,
    ) -> (i32, Vec<u8>, UIDNAInfo, UErrorCode) {
        let mut buf = vec![0u8; cap];
        let mut info = UIDNAInfo::default();
        let mut st = UErrorCode::ZERO;
        // SAFETY: buf has `cap` bytes; name is a live slice.
        let n = unsafe {
            if to_ascii {
                ffi::uidna_nameToASCII_UTF8(
                    self.0,
                    name.as_ptr().cast(),
                    len32(name),
                    buf.as_mut_ptr().cast(),
                    len32(&buf),
                    &mut info,
                    &mut st,
                )
            } else {
                ffi::uidna_nameToUnicodeUTF8(
                    self.0,
                    name.as_ptr().cast(),
                    len32(name),
                    buf.as_mut_ptr().cast(),
                    len32(&buf),
                    &mut info,
                    &mut st,
                )
            }
        };
        let m = usize::try_from(n).unwrap_or(0).min(cap);
        buf.truncate(m);
        (n, buf, info, st)
    }
}

impl Drop for Idna {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once.
        unsafe { ffi::uidna_close(self.0) }
    }
}
