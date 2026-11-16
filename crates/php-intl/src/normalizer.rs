// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `Normalizer` / `normalizer_*` (php-src `ext/intl/normalizer`).

use crate::error::{Error, Result, UErrorCode};
use crate::sys::unorm::{Form, Normalizer};
use crate::ustr;

/// `Normalizer::FORM_*` values.
pub mod form {
    /// `FORM_D` / `NFD`
    pub const D: i64 = 0x4;
    /// `FORM_KD` / `NFKD`
    pub const KD: i64 = 0x8;
    /// `FORM_C` / `NFC`
    pub const C: i64 = 0x10;
    /// `FORM_KC` / `NFKC`
    pub const KC: i64 = 0x20;
    /// `FORM_KC_CF` / `NFKC_CF`
    pub const KC_CF: i64 = 0x30;
}

fn to_form(f: i64) -> Option<Form> {
    Some(match f {
        form::D => Form::Nfd,
        form::KD => Form::Nfkd,
        form::C => Form::Nfc,
        form::KC => Form::Nfkc,
        form::KC_CF => Form::NfkcCf,
        _ => return None,
    })
}

/// `Normalizer::normalize($string, $form)`.
pub fn normalize(s: &[u8], f: i64) -> Result<String> {
    let form =
        to_form(f).ok_or_else(|| Error::arg_value(2, "must be a valid normalization form"))?;
    let u =
        ustr::to_u16(s).map_err(|c| Error::global(c, "Error converting input string to UTF-16"))?;
    let n = Normalizer::get(form).map_err(|c| Error::global(c, "Error normalizing string"))?;
    let out = n
        .normalize(&u)
        .map_err(|c| Error::global(c, "Error normalizing string"))?;
    ustr::from_u16(&out).map_err(|c| Error::global(c, "error converting normalized text UTF-8"))
}

/// `Normalizer::isNormalized($string, $form)`.
pub fn is_normalized(s: &[u8], f: i64) -> Result<bool> {
    let form =
        to_form(f).ok_or_else(|| Error::arg_value(2, "must be a valid normalization form"))?;
    let u = ustr::to_u16(s).map_err(|c| Error::global(c, "Error converting string to UTF-16."))?;
    let n = Normalizer::get(form).map_err(|c| {
        Error::global(
            c,
            "Error testing if string is the given normalization form.",
        )
    })?;
    n.is_normalized(&u).map_err(|c| {
        Error::global(
            c,
            "Error testing if string is the given normalization form.",
        )
    })
}

/// `Normalizer::getRawDecomposition($string, $form)` (`Ok(None)` →
/// `null`).
pub fn raw_decomposition(s: &[u8], f: i64) -> Result<Option<String>> {
    let (cp, n) = ustr::u8_next(s);
    if n != s.len() {
        return Err(Error::global(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "Input string must be exactly one UTF-8 encoded code point long.",
        ));
    }
    if !(0..=0x0010_ffff).contains(&cp) {
        return Err(Error::global(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "Code point out of range",
        ));
    }
    let Some(form) = to_form(f) else {
        return Ok(Some(String::new()));
    };
    let Ok(norm) = Normalizer::get(form) else {
        return Ok(Some(String::new()));
    };
    Ok(norm.raw_decomposition(cp).map(|d| ustr::from_u16_lossy(&d)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm() {
        assert_eq!(normalize("e\u{301}".as_bytes(), form::C).unwrap(), "é");
        assert_eq!(normalize("é".as_bytes(), form::D).unwrap(), "e\u{301}");
        assert_eq!(normalize("ﬁ".as_bytes(), form::KC).unwrap(), "fi");
        assert!(is_normalized("é".as_bytes(), form::C).unwrap());
        assert!(!is_normalized("é".as_bytes(), form::D).unwrap());
        assert_eq!(
            raw_decomposition("é".as_bytes(), form::C).unwrap().unwrap(),
            "e\u{301}"
        );
        assert_eq!(raw_decomposition(b"a", form::C).unwrap(), None);
        assert!(raw_decomposition(b"ab", form::C).is_err());
        assert!(normalize(b"x", 99).is_err());
    }
}
