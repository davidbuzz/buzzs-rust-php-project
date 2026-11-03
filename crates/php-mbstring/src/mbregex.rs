//! `mb_ereg*` support (port of the encoding bookkeeping of
//! `ext/mbstring/php_mbregex.c`).
//!
//! # Status
//!
//! Only the regex *encoding* state (`mb_regex_encoding`) is implemented
//! here. The matching functions (`mb_ereg`, `mb_eregi`, `mb_ereg_replace`,
//! `mb_eregi_replace`, `mb_ereg_replace_callback`, `mb_split`,
//! `mb_ereg_match`, `mb_ereg_search*`, `mb_regex_set_options`) need an
//! Oniguruma-compatible engine (Ruby syntax by default, `ONIG_OPTION_*`
//! flags, per-encoding character semantics). The intended route is:
//!
//! 1. Convert pattern and subject from the regex encoding
//!    ([`MbState::regex_encoding`](crate::MbState::regex_encoding)) to UTF-8
//!    with [`crate::convert::fast_convert`] (keeping a byte-offset map back
//!    to the original encoding for `mb_ereg_search_getpos`/`_regs`);
//! 2. translate the Oniguruma/Ruby syntax to PCRE2 syntax (`(?<name>)`,
//!    `\h` = hex digit, `\A`/`\z`, POSIX brackets, option letters
//!    `i m x s p l n e j u g c r z b d` from `_php_mb_regex_init_options`)
//!    and run it through `php-pcre`;
//! 3. map match offsets back and convert captured substrings back to the
//!    regex encoding.

use crate::encoding::{self, Encoding};

/// Oniguruma encodings PHP knows, with their accepted names; the first name
/// is the one `mb_regex_encoding()` reports (`enc_name_map`).
pub static ENC_NAME_MAP: &[&[&str]] = &[
    &["EUC-JP", "EUCJP", "X-EUC-JP", "UJIS", "EUCJP", "EUCJP-WIN"],
    &["UTF-8", "UTF8"],
    &["UTF-16", "UTF-16BE"],
    &["UTF-16LE"],
    &["UCS-4", "UTF-32", "UTF-32BE"],
    &["UCS-4LE", "UTF-32LE"],
    &["SJIS", "CP932", "MS932", "SHIFT_JIS", "SJIS-WIN", "WINDOWS-31J"],
    &["BIG5", "BIG-5", "BIGFIVE", "CN-BIG5", "BIG-FIVE"],
    &["EUC-CN", "EUCCN", "EUC_CN", "GB-2312", "GB2312"],
    &["EUC-TW", "EUCTW", "EUC_TW"],
    &["EUC-KR", "EUCKR", "EUC_KR"],
    &["KOI8R", "KOI8-R", "KOI-8R"],
    &["ISO-8859-1", "ISO8859-1"],
    &["ISO-8859-2", "ISO8859-2"],
    &["ISO-8859-3", "ISO8859-3"],
    &["ISO-8859-4", "ISO8859-4"],
    &["ISO-8859-5", "ISO8859-5"],
    &["ISO-8859-6", "ISO8859-6"],
    &["ISO-8859-7", "ISO8859-7"],
    &["ISO-8859-8", "ISO8859-8"],
    &["ISO-8859-9", "ISO8859-9"],
    &["ISO-8859-10", "ISO8859-10"],
    &["ISO-8859-11", "ISO8859-11"],
    &["ISO-8859-13", "ISO8859-13"],
    &["ISO-8859-14", "ISO8859-14"],
    &["ISO-8859-15", "ISO8859-15"],
    &["ISO-8859-16", "ISO8859-16"],
    &["ASCII", "US-ASCII", "US_ASCII", "ISO646"],
];

/// `_php_mb_regex_name2mbctype`.
pub fn name2mbctype(name: &[u8]) -> Option<usize> {
    let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
    if name.is_empty() {
        return None;
    }
    ENC_NAME_MAP
        .iter()
        .position(|names| names.iter().any(|n| n.as_bytes().eq_ignore_ascii_case(name)))
}

/// Regex encoding state (`MBREX(default_mbctype)` / `current_mbctype`).
#[derive(Debug, Clone)]
pub struct RegexState {
    default_mbctype: usize,
    current_mbctype: usize,
    current_encoding: &'static Encoding,
}

impl Default for RegexState {
    fn default() -> Self {
        Self {
            default_mbctype: 1,
            current_mbctype: 1,
            current_encoding: encoding::utf8(),
        }
    }
}

impl RegexState {
    /// `php_mb_regex_set_default_mbctype`.
    pub fn set_default_mbctype(&mut self, name: &[u8]) -> bool {
        match name2mbctype(name) {
            Some(i) => {
                self.default_mbctype = i;
                true
            }
            None => false,
        }
    }

    /// `php_mb_regex_set_mbctype`.
    pub fn set_mbctype(&mut self, name: &[u8]) -> bool {
        let Some(i) = name2mbctype(name) else {
            return false;
        };
        let Some(enc) = encoding::by_cname(name) else {
            return false;
        };
        self.current_mbctype = i;
        self.current_encoding = enc;
        true
    }

    /// `php_mb_regex_get_mbctype`.
    pub fn mbctype_name(&self) -> &'static str {
        ENC_NAME_MAP[self.current_mbctype][0]
    }

    /// The mbstring encoding matching the current regex encoding.
    pub fn encoding(&self) -> &'static Encoding {
        self.current_encoding
    }

    /// `PHP_RSHUTDOWN_FUNCTION(mb_regex)`.
    pub fn request_shutdown(&mut self) {
        self.current_mbctype = self.default_mbctype;
        let name = ENC_NAME_MAP[self.default_mbctype][0];
        if let Some(e) = encoding::by_name(name.as_bytes()) {
            self.current_encoding = e;
        }
    }
}

impl crate::MbState {
    /// `mb_regex_encoding()` (getter).
    pub fn mb_regex_encoding_get(&self) -> &'static str {
        self.regex.mbctype_name()
    }

    /// `mb_regex_encoding($encoding)` (setter).
    pub fn mb_regex_encoding_set(&mut self, name: &[u8]) -> crate::MbResult<bool> {
        if self.regex.set_mbctype(name) {
            Ok(true)
        } else {
            let shown = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
            Err(crate::MbError::invalid_encoding(1, "encoding", shown))
        }
    }

    /// The mbstring encoding used for regex subjects.
    pub fn regex_encoding(&self) -> &'static Encoding {
        self.regex.encoding()
    }
}
