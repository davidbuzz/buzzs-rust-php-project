// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The encoding registry (`mbfl_encoding`, `mbfl_encoding.c`).
//!
//! Ref: `libmbfl/mbfl/mbfl_encoding.c`, `libmbfl/mbfl/mbfl_encoding.h`,
//! `libmbfl/mbfl/mbfl_consts.h`.

use crate::codecs;
use crate::convert::{CheckFn, CutFn, FromWcharFn, ToWcharFn};
use crate::legacy::Vtbl;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Single-byte encoding (`MBFL_ENCTYPE_SBCS`).
pub const ENCTYPE_SBCS: u32 = 0x0000_0001;
/// Two bytes per character (`MBFL_ENCTYPE_WCS2`).
pub const ENCTYPE_WCS2: u32 = 0x0000_0002;
/// Four bytes per character (`MBFL_ENCTYPE_WCS4`).
pub const ENCTYPE_WCS4: u32 = 0x0000_0004;
/// Bytes 0x21-0x7E may be part of a multi-byte character
/// (`MBFL_ENCTYPE_GL_UNSAFE`).
pub const ENCTYPE_GL_UNSAFE: u32 = 0x0000_4000;

/// Encoding identifiers, in libmbfl's `enum mbfl_no_encoding` order (the
/// order is significant: e.g. everything up to [`EncodingNo::Qprint`] is a
/// "non-encoding" such as a transfer encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(missing_docs)]
pub enum EncodingNo {
    Pass,
    Wchar,
    Base64,
    Uuencode,
    HtmlEnt,
    Qprint,
    SevenBit,
    EightBit,
    CharsetMin,
    Ucs4,
    Ucs4be,
    Ucs4le,
    Ucs2,
    Ucs2be,
    Ucs2le,
    Utf32,
    Utf32be,
    Utf32le,
    Utf16,
    Utf16be,
    Utf16le,
    Utf8,
    Utf8Docomo,
    Utf8KddiA,
    Utf8KddiB,
    Utf8Sb,
    Utf7,
    Utf7imap,
    Ascii,
    EucJp,
    EucJp2004,
    Sjis,
    EucJpWin,
    SjisDocomo,
    SjisKddi,
    SjisSb,
    SjisMac,
    Sjis2004,
    Cp932,
    SjisWin,
    Cp51932,
    Jis,
    Iso2022Jp,
    Iso2022Jp2004,
    Iso2022JpKddi,
    Iso2022JpMs,
    Gb18030,
    Gb18030_2022,
    Cp1252,
    Cp1254,
    Iso8859_1,
    Iso8859_2,
    Iso8859_3,
    Iso8859_4,
    Iso8859_5,
    Iso8859_6,
    Iso8859_7,
    Iso8859_8,
    Iso8859_9,
    Iso8859_10,
    Iso8859_13,
    Iso8859_14,
    Iso8859_15,
    EucCn,
    Cp936,
    EucTw,
    Big5,
    Cp950,
    EucKr,
    Iso2022Kr,
    Uhc,
    Hz,
    Cp1251,
    Cp866,
    Koi8r,
    Koi8u,
    Iso8859_16,
    Armscii8,
    Cp850,
    Cp50220,
    Cp50221,
    Cp50222,
}

/// A text encoding known to mbstring (`mbfl_encoding`).
#[derive(Debug)]
pub struct Encoding {
    /// Identifier.
    pub no: EncodingNo,
    /// Canonical name (as listed by `mb_list_encodings`).
    pub name: &'static str,
    /// MIME charset name; `None` for C `NULL`.
    pub mime_name: Option<&'static str>,
    /// Aliases (as listed by `mb_encoding_aliases`).
    pub aliases: &'static [&'static str],
    /// Byte length of a character keyed by its first byte, if the encoding
    /// is stateless and has such a table.
    pub mblen_table: Option<&'static [u8; 256]>,
    /// `MBFL_ENCTYPE_*` flags.
    pub flag: u32,
    /// Legacy streaming decoder (bytes to code points), used by `mb_strcut`.
    pub input_filter: Option<&'static Vtbl>,
    /// Legacy streaming encoder (code points to bytes), used by `mb_strcut`.
    pub output_filter: Option<&'static Vtbl>,
    /// Decoder.
    pub to_wchar: ToWcharFn,
    /// Encoder.
    pub from_wchar: FromWcharFn,
    /// Optional fast validity check.
    pub check: Option<CheckFn>,
    /// Optional `mb_strcut` implementation.
    pub cut: Option<CutFn>,
}

impl PartialEq for Encoding {
    fn eq(&self, other: &Self) -> bool {
        self.no == other.no
    }
}

impl Eq for Encoding {}

impl Encoding {
    /// Whether this is UTF-8 proper.
    pub fn is_utf8(&self) -> bool {
        self.no == EncodingNo::Utf8
    }

    /// `mbfl_encoding_preferred_mime_name`.
    pub fn preferred_mime_name(&self) -> Option<&'static str> {
        self.mime_name.filter(|m| !m.is_empty())
    }

    /// Fixed character width in bytes, if any
    /// (`flag & (SBCS | WCS2 | WCS4)`).
    pub fn fixed_width(&self) -> Option<usize> {
        match self.flag & (ENCTYPE_SBCS | ENCTYPE_WCS2 | ENCTYPE_WCS4) {
            0 => None,
            n => Some(n as usize),
        }
    }
}

/// All supported encodings in `mbfl_encoding_ptr_list` order (the order of
/// `mb_list_encodings()`).
pub static ENCODINGS: [&Encoding; 79] = [
    &codecs::base64::ENC_BASE64,
    &codecs::uuencode::ENC_UUENCODE,
    &codecs::htmlent::ENC_HTML_ENT,
    &codecs::qprint::ENC_QPRINT,
    &codecs::sevenbit::ENC_7BIT,
    &codecs::eightbit::ENC_8BIT,
    &codecs::ucs4::ENC_UCS4,
    &codecs::ucs4::ENC_UCS4BE,
    &codecs::ucs4::ENC_UCS4LE,
    &codecs::ucs2::ENC_UCS2,
    &codecs::ucs2::ENC_UCS2BE,
    &codecs::ucs2::ENC_UCS2LE,
    &codecs::utf32::ENC_UTF32,
    &codecs::utf32::ENC_UTF32BE,
    &codecs::utf32::ENC_UTF32LE,
    &codecs::utf16::ENC_UTF16,
    &codecs::utf16::ENC_UTF16BE,
    &codecs::utf16::ENC_UTF16LE,
    &codecs::utf8::ENC_UTF8,
    &codecs::utf7::ENC_UTF7,
    &codecs::utf7imap::ENC_UTF7IMAP,
    &codecs::singlebyte::ENC_ASCII,
    &codecs::cjk::ENC_EUC_JP,
    &codecs::cjk::ENC_SJIS,
    &codecs::cjk::ENC_EUCJP_WIN,
    &codecs::cjk::ENC_EUCJP2004,
    &codecs::cjk::ENC_SJIS_DOCOMO,
    &codecs::cjk::ENC_SJIS_KDDI,
    &codecs::cjk::ENC_SJIS_SB,
    &codecs::cjk::ENC_SJIS_MAC,
    &codecs::cjk::ENC_SJIS2004,
    &codecs::cjk::ENC_UTF8_DOCOMO,
    &codecs::cjk::ENC_UTF8_KDDI_A,
    &codecs::cjk::ENC_UTF8_KDDI_B,
    &codecs::cjk::ENC_UTF8_SB,
    &codecs::cjk::ENC_CP932,
    &codecs::cjk::ENC_SJISWIN,
    &codecs::cjk::ENC_CP51932,
    &codecs::cjk::ENC_JIS,
    &codecs::cjk::ENC_2022JP,
    &codecs::cjk::ENC_2022JPMS,
    &codecs::cjk::ENC_GB18030,
    &codecs::cjk::ENC_GB18030_2022,
    &codecs::singlebyte::ENC_CP1252,
    &codecs::singlebyte::ENC_CP1254,
    &codecs::singlebyte::ENC_8859_1,
    &codecs::singlebyte::ENC_8859_2,
    &codecs::singlebyte::ENC_8859_3,
    &codecs::singlebyte::ENC_8859_4,
    &codecs::singlebyte::ENC_8859_5,
    &codecs::singlebyte::ENC_8859_6,
    &codecs::singlebyte::ENC_8859_7,
    &codecs::singlebyte::ENC_8859_8,
    &codecs::singlebyte::ENC_8859_9,
    &codecs::singlebyte::ENC_8859_10,
    &codecs::singlebyte::ENC_8859_13,
    &codecs::singlebyte::ENC_8859_14,
    &codecs::singlebyte::ENC_8859_15,
    &codecs::singlebyte::ENC_8859_16,
    &codecs::cjk::ENC_EUC_CN,
    &codecs::cjk::ENC_CP936,
    &codecs::cjk::ENC_HZ,
    &codecs::cjk::ENC_EUC_TW,
    &codecs::cjk::ENC_BIG5,
    &codecs::cjk::ENC_CP950,
    &codecs::cjk::ENC_EUC_KR,
    &codecs::cjk::ENC_UHC,
    &codecs::cjk::ENC_2022KR,
    &codecs::singlebyte::ENC_CP1251,
    &codecs::singlebyte::ENC_CP866,
    &codecs::singlebyte::ENC_KOI8R,
    &codecs::singlebyte::ENC_KOI8U,
    &codecs::singlebyte::ENC_ARMSCII8,
    &codecs::singlebyte::ENC_CP850,
    &codecs::cjk::ENC_2022JP_2004,
    &codecs::cjk::ENC_2022JP_KDDI,
    &codecs::cjk::ENC_CP50220,
    &codecs::cjk::ENC_CP50221,
    &codecs::cjk::ENC_CP50222,
];

/// Longest name, MIME name or alias (bytes); longer inputs cannot match.
const MAX_NAME_LEN: usize = 32;

/// Lowercased name / MIME name / alias -> encoding, built once. Names take
/// precedence over MIME names, which take precedence over aliases (the
/// search order of `mbfl_name2encoding_ex`).
fn name_map() -> &'static HashMap<Vec<u8>, &'static Encoding> {
    static MAP: OnceLock<HashMap<Vec<u8>, &'static Encoding>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        for e in ENCODINGS {
            map.entry(e.name.to_ascii_lowercase().into_bytes())
                .or_insert(e);
        }
        for e in ENCODINGS {
            if let Some(m) = e.mime_name {
                map.entry(m.to_ascii_lowercase().into_bytes()).or_insert(e);
            }
        }
        for e in ENCODINGS {
            for a in e.aliases {
                map.entry(a.to_ascii_lowercase().into_bytes()).or_insert(e);
            }
        }
        map
    })
}

/// `mbfl_name2encoding_ex`: look an encoding up by name, then MIME name,
/// then alias, ASCII case-insensitively. `"pass"` and `"wchar"` are not
/// found (they are not in the public list).
pub fn by_name(name: &[u8]) -> Option<&'static Encoding> {
    if name.len() > MAX_NAME_LEN {
        return None;
    }
    let mut buf = [0u8; MAX_NAME_LEN];
    let key = &mut buf[..name.len()];
    key.copy_from_slice(name);
    key.make_ascii_lowercase();
    name_map().get(&key[..]).copied()
}

/// `mbfl_name2encoding`: like [`by_name`] but the name ends at the first NUL
/// byte (it is treated as a C string).
pub fn by_cname(name: &[u8]) -> Option<&'static Encoding> {
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    by_name(&name[..end])
}

/// `mbfl_no2encoding`. The internal encodings `pass` and `wchar` map to the
/// `pass` pseudo-encoding.
pub fn get(no: EncodingNo) -> &'static Encoding {
    match no {
        EncodingNo::Pass | EncodingNo::Wchar | EncodingNo::CharsetMin => {
            &codecs::eightbit::ENC_PASS
        }
        _ => ENCODINGS
            .iter()
            .find(|e| e.no == no)
            .copied()
            .unwrap_or(&codecs::eightbit::ENC_PASS),
    }
}

/// The `pass` pseudo-encoding (only valid for `mb_http_output`).
pub fn pass() -> &'static Encoding {
    &codecs::eightbit::ENC_PASS
}

/// UTF-8.
pub fn utf8() -> &'static Encoding {
    &codecs::utf8::ENC_UTF8
}

/// `mb_list_encodings()`.
pub fn list() -> impl Iterator<Item = &'static Encoding> {
    ENCODINGS.iter().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_by_name_mime_and_alias() {
        assert_eq!(by_name(b"utf-8").map(|e| e.name), Some("UTF-8"));
        assert_eq!(by_name(b"UTF8").map(|e| e.name), Some("UTF-8"));
        assert_eq!(by_name(b"latin1").map(|e| e.name), Some("ISO-8859-1"));
        assert_eq!(by_name(b"Shift_JIS").map(|e| e.name), Some("SJIS"));
        assert_eq!(by_name(b"ISO-2022-JP").map(|e| e.name), Some("ISO-2022-JP"));
        assert_eq!(by_name(b"pass"), None);
        assert_eq!(by_name(b"wchar"), None);
        assert_eq!(by_cname(b"UTF-8\0junk").map(|e| e.name), Some("UTF-8"));
    }

    #[test]
    fn map_matches_linear_search() {
        let linear = |name: &[u8]| {
            ENCODINGS
                .iter()
                .find(|e| e.name.as_bytes().eq_ignore_ascii_case(name))
                .or_else(|| {
                    ENCODINGS.iter().find(|e| {
                        e.mime_name
                            .is_some_and(|m| m.as_bytes().eq_ignore_ascii_case(name))
                    })
                })
                .or_else(|| {
                    ENCODINGS.iter().find(|e| {
                        e.aliases
                            .iter()
                            .any(|a| a.as_bytes().eq_ignore_ascii_case(name))
                    })
                })
                .map(|e| e.no)
        };
        for e in ENCODINGS {
            let mut names = vec![e.name];
            names.extend(e.mime_name);
            names.extend(e.aliases.iter().copied());
            for n in names {
                for v in [
                    n.to_string(),
                    n.to_ascii_uppercase(),
                    n.to_ascii_lowercase(),
                ] {
                    assert!(v.len() <= MAX_NAME_LEN, "{v}");
                    assert_eq!(
                        by_name(v.as_bytes()).map(|e| e.no),
                        linear(v.as_bytes()),
                        "{v}"
                    );
                }
            }
        }
        assert_eq!(by_name(&[b'x'; 100]), None);
        assert_eq!(by_name(b"UTF-8\0"), None);
    }

    #[test]
    fn list_order_starts_and_ends_like_php() {
        let names: Vec<_> = list().map(|e| e.name).collect();
        assert_eq!(names[0], "BASE64");
        assert_eq!(names[78], "CP50222");
        assert_eq!(names.len(), 79);
    }
}
