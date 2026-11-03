//! Character-set conversion used for `UserComment` (UNICODE / JIS) and the
//! Windows XP tags (`XPTitle`, `XPComment`, ...).
//!
//! PHP performs these conversions through `zend_multibyte_*`, i.e. through
//! mbstring when it is loaded.  The VM can plug its own mbstring
//! implementation in through [`EncodingConverter`]; [`BuiltinConverter`] is a
//! self-contained implementation of the conversions the default `exif.*`
//! settings need (UCS-2/UTF-16/UTF-32 to UTF-8/ISO-8859-1/ISO-8859-15 and
//! back), producing mbstring's output, including its `'?'` substitution of
//! unmappable or malformed input.

/// What exif needs from the multibyte layer.
pub trait EncodingConverter {
    /// `zend_multibyte_fetch_encoding(name) != NULL`: whether `name` names a
    /// known encoding.  Used by the `exif.*` INI handlers and to decide
    /// whether a conversion is attempted at all.
    fn is_known(&self, name: &str) -> bool;

    /// `zend_multibyte_encoding_converter()`: converts `input` from `from` to
    /// `to`.  `None` when either encoding is unknown or the conversion
    /// failed — exif then keeps the raw bytes.
    fn convert(&self, input: &[u8], to: &str, from: &str) -> Option<Vec<u8>>;
}

/// The built-in converter (see the module documentation).
#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltinConverter;

/// Encodings understood by [`BuiltinConverter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Enc {
    Ucs2,
    Ucs2Be,
    Ucs2Le,
    Utf16,
    Utf16Be,
    Utf16Le,
    Ucs4,
    Ucs4Be,
    Ucs4Le,
    Utf8,
    Latin1,
    Latin15,
    Ascii,
    /// Known to mbstring but not convertible here (JIS family, ...).
    Opaque,
}

fn lookup(name: &str) -> Option<Enc> {
    let n = name.trim().to_ascii_uppercase();
    Some(match n.as_str() {
        "UCS-2" | "UCS2" | "ISO-10646-UCS-2" | "UNICODE" => Enc::Ucs2,
        "UCS-2BE" => Enc::Ucs2Be,
        "UCS-2LE" => Enc::Ucs2Le,
        "UTF-16" | "UTF16" => Enc::Utf16,
        "UTF-16BE" => Enc::Utf16Be,
        "UTF-16LE" => Enc::Utf16Le,
        "UCS-4" | "UCS4" | "ISO-10646-UCS-4" | "UTF-32" | "UTF32" => Enc::Ucs4,
        "UCS-4BE" | "UTF-32BE" => Enc::Ucs4Be,
        "UCS-4LE" | "UTF-32LE" => Enc::Ucs4Le,
        "UTF-8" | "UTF8" => Enc::Utf8,
        "ISO-8859-1" | "ISO8859-1" | "LATIN1" => Enc::Latin1,
        "ISO-8859-15" | "ISO8859-15" | "LATIN9" => Enc::Latin15,
        "ASCII" | "US-ASCII" | "ANSI_X3.4-1968" | "ISO646-US" | "IBM367" | "CP367" => Enc::Ascii,
        "JIS" | "ISO-2022-JP" | "SJIS" | "SHIFT_JIS" | "EUC-JP" | "EUCJP" | "CP932"
        | "SJIS-WIN" | "EUCJP-WIN" | "CP51932" | "JIS-MS" | "ISO-2022-JP-MS" | "CP50220"
        | "CP50221" | "CP50222" | "UTF-7" | "8BIT" | "7BIT" | "BASE64" | "HTML-ENTITIES"
        | "QUOTED-PRINTABLE" | "UUENCODE" | "WINDOWS-1252" | "CP1252" | "WINDOWS-1251"
        | "CP1251" | "KOI8-R" | "BIG-5" | "BIG5" | "EUC-KR" | "EUC-CN" | "GB18030" | "CP936"
        | "UHC" | "ISO-8859-2" | "ISO-8859-3" | "ISO-8859-4" | "ISO-8859-5" | "ISO-8859-6"
        | "ISO-8859-7" | "ISO-8859-8" | "ISO-8859-9" | "ISO-8859-10" | "ISO-8859-13"
        | "ISO-8859-14" | "ISO-8859-16" => Enc::Opaque,
        _ => return None,
    })
}

const BAD: u32 = u32::MAX;

fn decode(input: &[u8], enc: Enc) -> Option<Vec<u32>> {
    let mut out = Vec::with_capacity(input.len());
    match enc {
        Enc::Ucs2 | Enc::Ucs2Be | Enc::Ucs2Le | Enc::Utf16 | Enc::Utf16Be | Enc::Utf16Le => {
            let mut be = !matches!(enc, Enc::Ucs2Le | Enc::Utf16Le);
            let mut data = input;
            if matches!(enc, Enc::Ucs2 | Enc::Utf16) && data.len() >= 2 {
                if data[..2] == [0xFF, 0xFE] {
                    be = false;
                    data = &data[2..];
                } else if data[..2] == [0xFE, 0xFF] {
                    data = &data[2..];
                }
            }
            let utf16 = matches!(enc, Enc::Utf16 | Enc::Utf16Be | Enc::Utf16Le);
            let mut units = data.chunks_exact(2).map(|c| {
                if be {
                    u32::from(c[0]) << 8 | u32::from(c[1])
                } else {
                    u32::from(c[1]) << 8 | u32::from(c[0])
                }
            });
            let mut pending: Option<u32> = None;
            loop {
                let unit = match pending.take() {
                    Some(u) => u,
                    None => match units.next() {
                        Some(u) => u,
                        None => break,
                    },
                };
                if utf16 && (0xD800..0xDC00).contains(&unit) {
                    match units.next() {
                        Some(lo) if (0xDC00..0xE000).contains(&lo) => {
                            out.push(0x10000 + ((unit - 0xD800) << 10) + (lo - 0xDC00));
                        }
                        Some(other) => {
                            out.push(BAD);
                            pending = Some(other);
                        }
                        None => out.push(BAD),
                    }
                } else if utf16 && (0xDC00..0xE000).contains(&unit) {
                    out.push(BAD);
                } else {
                    out.push(unit);
                }
            }
            if data.len() % 2 == 1 {
                out.push(BAD);
            }
        }
        Enc::Ucs4 | Enc::Ucs4Be | Enc::Ucs4Le => {
            let mut be = enc != Enc::Ucs4Le;
            let mut data = input;
            if enc == Enc::Ucs4 && data.len() >= 4 {
                if data[..4] == [0xFF, 0xFE, 0, 0] {
                    be = false;
                    data = &data[4..];
                } else if data[..4] == [0, 0, 0xFE, 0xFF] {
                    data = &data[4..];
                }
            }
            for c in data.chunks_exact(4) {
                let v = if be {
                    u32::from_be_bytes([c[0], c[1], c[2], c[3]])
                } else {
                    u32::from_le_bytes([c[0], c[1], c[2], c[3]])
                };
                out.push(if v < 0x11_0000 { v } else { BAD });
            }
            if data.len() % 4 != 0 {
                out.push(BAD);
            }
        }
        Enc::Utf8 => decode_utf8(input, &mut out),
        Enc::Latin1 => out.extend(input.iter().map(|&b| u32::from(b))),
        Enc::Latin15 => out.extend(input.iter().map(|&b| latin15_to_unicode(b))),
        Enc::Ascii => out.extend(
            input
                .iter()
                .map(|&b| if b < 0x80 { u32::from(b) } else { BAD }),
        ),
        Enc::Opaque => return None,
    }
    Some(out)
}

fn decode_utf8(input: &[u8], out: &mut Vec<u32>) {
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        let (len, init) = match b {
            0x00..=0x7F => {
                out.push(u32::from(b));
                i += 1;
                continue;
            }
            0xC2..=0xDF => (2, u32::from(b & 0x1F)),
            0xE0..=0xEF => (3, u32::from(b & 0x0F)),
            0xF0..=0xF4 => (4, u32::from(b & 0x07)),
            _ => {
                out.push(BAD);
                i += 1;
                continue;
            }
        };
        let mut cp = init;
        let mut ok = true;
        let mut j = 1;
        while j < len {
            match input.get(i + j) {
                Some(&c) if c & 0xC0 == 0x80 => {
                    // Second-byte range restrictions (overlongs/surrogates/too big).
                    if j == 1
                        && ((b == 0xE0 && c < 0xA0)
                            || (b == 0xED && c > 0x9F)
                            || (b == 0xF0 && c < 0x90)
                            || (b == 0xF4 && c > 0x8F))
                    {
                        ok = false;
                        break;
                    }
                    cp = cp << 6 | u32::from(c & 0x3F);
                    j += 1;
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            out.push(cp);
            i += len;
        } else {
            out.push(BAD);
            i += j;
        }
    }
}

fn latin15_to_unicode(b: u8) -> u32 {
    match b {
        0xA4 => 0x20AC,
        0xA6 => 0x160,
        0xA8 => 0x161,
        0xB4 => 0x17D,
        0xB8 => 0x17E,
        0xBC => 0x152,
        0xBD => 0x153,
        0xBE => 0x178,
        _ => u32::from(b),
    }
}

fn unicode_to_latin15(cp: u32) -> Option<u8> {
    Some(match cp {
        0x20AC => 0xA4,
        0x160 => 0xA6,
        0x161 => 0xA8,
        0x17D => 0xB4,
        0x17E => 0xB8,
        0x152 => 0xBC,
        0x153 => 0xBD,
        0x178 => 0xBE,
        0xA4 | 0xA6 | 0xA8 | 0xB4 | 0xB8 | 0xBC | 0xBD | 0xBE => return None,
        _ => return u8::try_from(cp).ok(),
    })
}

// Every narrowing cast below is preceded by a range check or a mask.
#[allow(clippy::cast_possible_truncation)]
fn encode(cps: &[u32], enc: Enc) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(cps.len());
    for &cp in cps {
        let cp = if cp == BAD { u32::from(b'?') } else { cp };
        match enc {
            Enc::Utf8 => {
                if cp < 0x80 {
                    out.push(cp as u8);
                } else if cp < 0x800 {
                    out.push(0xC0 | (cp >> 6) as u8);
                    out.push(0x80 | (cp & 0x3F) as u8);
                } else if cp < 0x1_0000 {
                    out.push(0xE0 | (cp >> 12) as u8);
                    out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
                    out.push(0x80 | (cp & 0x3F) as u8);
                } else if cp < 0x11_0000 {
                    out.push(0xF0 | (cp >> 18) as u8);
                    out.push(0x80 | ((cp >> 12) & 0x3F) as u8);
                    out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
                    out.push(0x80 | (cp & 0x3F) as u8);
                } else {
                    out.push(b'?');
                }
            }
            Enc::Latin1 => out.push(u8::try_from(cp).unwrap_or(b'?')),
            Enc::Latin15 => out.push(unicode_to_latin15(cp).unwrap_or(b'?')),
            Enc::Ascii => out.push(if cp < 0x80 { cp as u8 } else { b'?' }),
            Enc::Ucs2 | Enc::Ucs2Be | Enc::Ucs2Le => {
                let unit = u16::try_from(cp).unwrap_or(u16::from(b'?'));
                if enc == Enc::Ucs2Le {
                    out.extend_from_slice(&unit.to_le_bytes());
                } else {
                    out.extend_from_slice(&unit.to_be_bytes());
                }
            }
            Enc::Utf16 | Enc::Utf16Be | Enc::Utf16Le => {
                let mut units = [0u16; 2];
                let n = if let Some(c) = char::from_u32(cp) {
                    c.encode_utf16(&mut units).len()
                } else {
                    units[0] = u16::from(b'?');
                    1
                };
                for u in &units[..n] {
                    if enc == Enc::Utf16Le {
                        out.extend_from_slice(&u.to_le_bytes());
                    } else {
                        out.extend_from_slice(&u.to_be_bytes());
                    }
                }
            }
            Enc::Ucs4 | Enc::Ucs4Be => out.extend_from_slice(&cp.to_be_bytes()),
            Enc::Ucs4Le => out.extend_from_slice(&cp.to_le_bytes()),
            Enc::Opaque => return None,
        }
    }
    Some(out)
}

impl EncodingConverter for BuiltinConverter {
    fn is_known(&self, name: &str) -> bool {
        lookup(name).is_some()
    }

    fn convert(&self, input: &[u8], to: &str, from: &str) -> Option<Vec<u8>> {
        let to = lookup(to)?;
        let from = lookup(from)?;
        let cps = decode(input, from)?;
        encode(&cps, to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ucs2_to_latin15() {
        let c = BuiltinConverter;
        assert_eq!(
            c.convert(b"\x00A\x20\xAC\x00", "ISO-8859-15", "UCS-2BE"),
            Some(b"A\xA4?".to_vec())
        );
        assert_eq!(
            c.convert(b"\x00A\x4e\x00", "ISO-8859-15", "UCS-2BE"),
            Some(b"A?".to_vec())
        );
        assert_eq!(
            c.convert(b"A\x00B\x00", "ISO-8859-1", "ucs-2le"),
            Some(b"AB".to_vec())
        );
    }

    #[test]
    fn ucs2_surrogates_to_utf8_pass_through() {
        let c = BuiltinConverter;
        assert_eq!(
            c.convert(b"\xd8\x3d\xde\x00", "UTF-8", "UCS-2BE"),
            Some(b"\xed\xa0\xbd\xed\xb8\x80".to_vec())
        );
    }

    #[test]
    fn unknown_and_opaque() {
        let c = BuiltinConverter;
        assert!(!c.is_known(""));
        assert!(!c.is_known("nope"));
        assert!(c.is_known("JIS"));
        assert_eq!(c.convert(b"abc", "", "UCS-2BE"), None);
        assert_eq!(c.convert(b"abc", "UTF-8", "JIS"), None);
    }

    #[test]
    fn utf8_roundtrip() {
        let c = BuiltinConverter;
        let s = "h\u{e9}llo \u{20ac} \u{1F600}".as_bytes();
        let u16 = c.convert(s, "UTF-16LE", "UTF-8").unwrap();
        assert_eq!(c.convert(&u16, "UTF-8", "UTF-16LE").unwrap(), s);
        assert_eq!(c.convert(b"\xff", "UTF-8", "UTF-8").unwrap(), b"?");
    }
}
