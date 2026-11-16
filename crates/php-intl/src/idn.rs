// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `idn_to_ascii()` / `idn_to_utf8()` (php-src `ext/intl/idn`).

use crate::error::{Error, Result};
use crate::sys::unorm::Idna;

/// `INTL_IDNA_VARIANT_UTS46`.
pub const VARIANT_UTS46: i64 = 1;

/// The `$idna_info` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdnaInfo {
    /// `result`
    pub result: Vec<u8>,
    /// `isTransitionalDifferent`
    pub is_transitional_different: bool,
    /// `errors` (bitmask of `IDNA_ERROR_*`)
    pub errors: u32,
}

/// The outcome of a conversion: the value (`None` → `false`) and, when the
/// conversion itself succeeded, the `$idna_info` array to assign.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdnOutcome {
    /// The converted domain, or `None` (`false`) when `errors != 0`.
    pub value: Option<Vec<u8>>,
    /// `$idna_info` (set only when the conversion reached ICU's result).
    pub info: Option<IdnaInfo>,
}

fn convert(domain: &[u8], options: i64, variant: i64, to_ascii: bool) -> Result<IdnOutcome> {
    if domain.is_empty() {
        return Err(Error::arg_value(1, "must not be empty"));
    }
    if domain.len() > (i32::MAX as usize) - 1 {
        return Err(Error::arg_value(
            1,
            format!("must be less than {} bytes", i32::MAX),
        ));
    }
    if variant != VARIANT_UTS46 {
        return Err(Error::arg_value(3, "must be INTL_IDNA_VARIANT_UTS46"));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let idna = Idna::open(options as u32)
        .map_err(|c| Error::global(c, "failed to open UIDNA instance"))?;
    let cap = if to_ascii { 255 } else { 252 * 4 };
    let (len, buf, info, st) = idna.convert(to_ascii, domain, cap);
    if usize::try_from(len).is_ok_and(|l| l >= cap) {
        // php-src returns false without recording an error here.
        return Err(Error::code(crate::UErrorCode::ZERO));
    }
    if st.is_failure() {
        return Err(Error::global(st, "failed to convert name"));
    }
    let info = IdnaInfo {
        result: buf.clone(),
        is_transitional_different: info.isTransitionalDifferent != 0,
        errors: info.errors,
    };
    let value = (info.errors == 0).then_some(buf);
    Ok(IdnOutcome {
        value,
        info: Some(info),
    })
}

/// `idn_to_ascii($domain, $flags, $variant, &$idna_info)`.
pub fn to_ascii(domain: &[u8], options: i64, variant: i64) -> Result<IdnOutcome> {
    convert(domain, options, variant, true)
}

/// `idn_to_utf8($domain, $flags, $variant, &$idna_info)`.
pub fn to_utf8(domain: &[u8], options: i64, variant: i64) -> Result<IdnOutcome> {
    convert(domain, options, variant, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idn() {
        let r = to_ascii("bücher.de".as_bytes(), 0, 1).unwrap();
        assert_eq!(r.value.unwrap(), b"xn--bcher-kva.de");
        let r = to_utf8(b"xn--bcher-kva.de", 0, 1).unwrap();
        assert_eq!(r.value.unwrap(), "bücher.de".as_bytes());
        let r = to_ascii(b"a..b", 0, 1).unwrap();
        assert!(r.value.is_none());
        assert_ne!(r.info.unwrap().errors, 0);
        assert!(to_ascii(b"", 0, 1).is_err());
    }
}
