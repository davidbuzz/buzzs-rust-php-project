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
    &[
        "SJIS",
        "CP932",
        "MS932",
        "SHIFT_JIS",
        "SJIS-WIN",
        "WINDOWS-31J",
    ],
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
    ENC_NAME_MAP.iter().position(|names| {
        names
            .iter()
            .any(|n| n.as_bytes().eq_ignore_ascii_case(name))
    })
}

/// `ONIG_OPTION_IGNORECASE`.
pub const OPTION_IGNORECASE: u32 = 1;
/// `ONIG_OPTION_EXTEND`.
pub const OPTION_EXTEND: u32 = 1 << 1;
/// `ONIG_OPTION_MULTILINE` (Ruby: `.` matches newline).
pub const OPTION_MULTILINE: u32 = 1 << 2;
/// `ONIG_OPTION_SINGLELINE` (`$` matches only at the end).
pub const OPTION_SINGLELINE: u32 = 1 << 3;
/// `ONIG_OPTION_FIND_LONGEST`.
pub const OPTION_FIND_LONGEST: u32 = 1 << 4;
/// `ONIG_OPTION_FIND_NOT_EMPTY`.
pub const OPTION_FIND_NOT_EMPTY: u32 = 1 << 5;

/// Oniguruma syntaxes selectable through option letters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// `j`.
    Java,
    /// `u`.
    GnuRegex,
    /// `g`.
    Grep,
    /// `c`.
    Emacs,
    /// `r` (default).
    Ruby,
    /// `z`.
    Perl,
    /// `b`.
    PosixBasic,
    /// `d`.
    PosixExtended,
}

/// `_php_mb_regex_init_options`: parse an option string into option bits
/// and a syntax (`None` options string = defaults untouched).
pub fn parse_options(s: &[u8]) -> Result<(u32, Syntax), crate::MbError> {
    let mut opt = 0;
    let mut syntax = Syntax::Ruby;
    for &c in s {
        match c {
            b'i' => opt |= OPTION_IGNORECASE,
            b'x' => opt |= OPTION_EXTEND,
            b'm' => opt |= OPTION_MULTILINE,
            b's' => opt |= OPTION_SINGLELINE,
            b'p' => opt |= OPTION_MULTILINE | OPTION_SINGLELINE,
            b'l' => opt |= OPTION_FIND_LONGEST,
            b'n' => opt |= OPTION_FIND_NOT_EMPTY,
            b'j' => syntax = Syntax::Java,
            b'u' => syntax = Syntax::GnuRegex,
            b'g' => syntax = Syntax::Grep,
            b'c' => syntax = Syntax::Emacs,
            b'r' => syntax = Syntax::Ruby,
            b'z' => syntax = Syntax::Perl,
            b'b' => syntax = Syntax::PosixBasic,
            b'd' => syntax = Syntax::PosixExtended,
            _ => {
                return Err(crate::MbError::Value(format!(
                    "Option \"{}\" is not supported",
                    char::from(c)
                )))
            }
        }
    }
    Ok((opt, syntax))
}

/// `_php_mb_regex_get_option_string`.
pub fn option_string(opt: u32, syntax: Syntax) -> String {
    let mut s = String::new();
    if opt & OPTION_IGNORECASE != 0 {
        s.push('i');
    }
    if opt & OPTION_EXTEND != 0 {
        s.push('x');
    }
    if opt & (OPTION_MULTILINE | OPTION_SINGLELINE) == OPTION_MULTILINE | OPTION_SINGLELINE {
        s.push('p');
    } else {
        if opt & OPTION_MULTILINE != 0 {
            s.push('m');
        }
        if opt & OPTION_SINGLELINE != 0 {
            s.push('s');
        }
    }
    if opt & OPTION_FIND_LONGEST != 0 {
        s.push('l');
    }
    if opt & OPTION_FIND_NOT_EMPTY != 0 {
        s.push('n');
    }
    s.push(match syntax {
        Syntax::Java => 'j',
        Syntax::GnuRegex => 'u',
        Syntax::Grep => 'g',
        Syntax::Emacs => 'c',
        Syntax::Ruby => 'r',
        Syntax::Perl => 'z',
        Syntax::PosixBasic => 'b',
        Syntax::PosixExtended => 'd',
    });
    s
}

/// Regex state (`MBREX(...)`): encoding and default options.
#[derive(Debug, Clone)]
pub struct RegexState {
    default_mbctype: usize,
    current_mbctype: usize,
    current_encoding: &'static Encoding,
    /// `MBREX(regex_default_options)`.
    pub default_options: u32,
    /// `MBREX(regex_default_syntax)`.
    pub default_syntax: Syntax,
}

impl Default for RegexState {
    fn default() -> Self {
        Self {
            default_mbctype: 1,
            current_mbctype: 1,
            current_encoding: encoding::utf8(),
            default_options: OPTION_MULTILINE | OPTION_SINGLELINE,
            default_syntax: Syntax::Ruby,
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

    /// `mb_regex_set_options($options)`: sets new defaults (when given) and
    /// returns the *previous* option string (or the current one).
    pub fn mb_regex_set_options(&mut self, options: Option<&[u8]>) -> crate::MbResult<String> {
        let (opt, syntax) = match options {
            Some(o) => {
                let (opt, syntax) = parse_options(o)?;
                let prev = (self.regex.default_options, self.regex.default_syntax);
                self.regex.default_options = opt;
                self.regex.default_syntax = syntax;
                prev
            }
            None => (self.regex.default_options, self.regex.default_syntax),
        };
        Ok(option_string(opt, syntax))
    }

    /// The mbstring encoding used for regex subjects.
    pub fn regex_encoding(&self) -> &'static Encoding {
        self.regex.encoding()
    }
}

#[cfg(test)]
mod tests {
    use crate::MbState;

    #[test]
    fn regex_options_round_trip() {
        let mut st = MbState::new();
        assert_eq!(st.mb_regex_set_options(None).unwrap(), "pr");
        assert_eq!(st.mb_regex_set_options(Some(b"ixz")).unwrap(), "pr");
        assert_eq!(st.mb_regex_set_options(None).unwrap(), "ixz");
        assert_eq!(
            st.mb_regex_set_options(Some(b"Q")).unwrap_err().to_string(),
            "Option \"Q\" is not supported"
        );
        assert_eq!(st.mb_regex_encoding_get(), "UTF-8");
        st.mb_regex_encoding_set(b"sjis-win").unwrap();
        assert_eq!(st.mb_regex_encoding_get(), "SJIS");
        assert!(st.mb_regex_encoding_set(b"UTF-7").is_err());
    }
}
