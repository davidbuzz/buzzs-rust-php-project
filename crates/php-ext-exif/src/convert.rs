//! The `zend_multibyte_*` conversions exif performs, backed by the
//! `php-mbstring` libmbfl port (PHP routes them through mbstring when it is
//! loaded, which phprust always does).

use php_exif::EncodingConverter;
use php_mbstring::convert::fast_convert;
use php_mbstring::encoding::by_cname;
use php_mbstring::ErrorMode;

/// [`EncodingConverter`] over `php-mbstring`: `php_mb_zend_encoding_converter`
/// with mbstring's default substitution (`'?'`).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MbConverter;

impl EncodingConverter for MbConverter {
    fn is_known(&self, name: &str) -> bool {
        by_cname(name.as_bytes()).is_some()
    }

    fn convert(&self, input: &[u8], to: &str, from: &str) -> Option<Vec<u8>> {
        let to = by_cname(to.as_bytes())?;
        let from = by_cname(from.as_bytes())?;
        Some(fast_convert(input, from, to, u32::from(b'?'), ErrorMode::Char).0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_ucs2() {
        let c = MbConverter;
        assert!(c.is_known("UCS-2LE"));
        assert!(!c.is_known("nope"));
        assert_eq!(
            c.convert(b"a\0b\0", "UTF-8", "UCS-2LE").as_deref(),
            Some(&b"ab"[..])
        );
        assert_eq!(c.convert(b"x", "nope", "UTF-8"), None);
    }
}
