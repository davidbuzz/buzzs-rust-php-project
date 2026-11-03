//! The `encoding` option (`xmlFindCharEncodingHandler`): conversion of PHP
//! strings between a legacy character set and UTF-8.

/// A supported character set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    /// UTF-8 (identity).
    Utf8,
    /// ISO-8859-1.
    Latin1,
    /// ISO-8859-15.
    Latin9,
    /// Windows-1252.
    Cp1252,
    /// US-ASCII.
    Ascii,
}

const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0xFFFD, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0xFFFD, 0x017D, 0xFFFD, 0xFFFD, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0xFFFD, 0x017E, 0x0178,
];

const LATIN9_DIFF: [(u8, u32); 8] = [
    (0xA4, 0x20AC),
    (0xA6, 0x0160),
    (0xA8, 0x0161),
    (0xB4, 0x017D),
    (0xB8, 0x017E),
    (0xBC, 0x0152),
    (0xBD, 0x0153),
    (0xBE, 0x0178),
];

impl Charset {
    /// `xmlFindCharEncodingHandler(name)`.
    #[must_use]
    pub fn find(name: &[u8]) -> Option<Self> {
        let upper = String::from_utf8_lossy(name).to_ascii_uppercase();
        Some(match upper.as_str() {
            "UTF-8" | "UTF8" => Charset::Utf8,
            "ISO-8859-1" | "ISO-LATIN-1" | "ISO LATIN 1" | "ISO8859-1" | "ISO_8859-1"
            | "LATIN1" | "L1" | "CP819" | "IBM819" => Charset::Latin1,
            "ISO-8859-15" | "ISO8859-15" | "ISO_8859-15" | "LATIN-9" | "LATIN9" => Charset::Latin9,
            "WINDOWS-1252" | "CP1252" | "MS-ANSI" => Charset::Cp1252,
            "ASCII" | "US-ASCII" | "ANSI_X3.4-1968" | "646" => Charset::Ascii,
            _ => return None,
        })
    }

    fn decode_byte(self, b: u8) -> Option<u32> {
        match self {
            Charset::Utf8 => None,
            Charset::Latin1 => Some(u32::from(b)),
            Charset::Latin9 => Some(
                LATIN9_DIFF
                    .iter()
                    .find(|(c, _)| *c == b)
                    .map_or(u32::from(b), |&(_, u)| u),
            ),
            Charset::Cp1252 => Some(if (0x80..0xA0).contains(&b) {
                u32::from(CP1252_HIGH[usize::from(b - 0x80)])
            } else {
                u32::from(b)
            }),
            Charset::Ascii => (b < 0x80).then_some(u32::from(b)),
        }
    }

    fn encode_char(self, c: u32) -> Option<u8> {
        if c < 0x80 {
            return u8::try_from(c).ok();
        }
        match self {
            Charset::Utf8 | Charset::Ascii => None,
            Charset::Latin1 => u8::try_from(c).ok(),
            Charset::Latin9 => {
                if let Some(&(b, _)) = LATIN9_DIFF.iter().find(|(_, u)| *u == c) {
                    return Some(b);
                }
                u8::try_from(c)
                    .ok()
                    .filter(|b| !LATIN9_DIFF.iter().any(|(x, _)| x == b))
            }
            Charset::Cp1252 => {
                if let Some(p) = CP1252_HIGH
                    .iter()
                    .position(|&u| u32::from(u) == c && u != 0xFFFD)
                {
                    return u8::try_from(p + 0x80).ok();
                }
                u8::try_from(c).ok().filter(|&b| b >= 0xA0)
            }
        }
    }

    /// `xmlCharEncInFunc`: charset → UTF-8; `None` on a conversion error.
    #[must_use]
    pub fn to_utf8(self, s: &[u8]) -> Option<Vec<u8>> {
        if self == Charset::Utf8 {
            return Some(s.to_vec());
        }
        let mut out = Vec::with_capacity(s.len());
        for &b in s {
            let c = char::from_u32(self.decode_byte(b)?)?;
            let mut tmp = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
        }
        Some(out)
    }

    /// `xmlCharEncOutFunc`: UTF-8 → charset. Unrepresentable characters
    /// become `&#N;` references like libxml2 does.
    #[must_use]
    pub fn from_utf8(self, s: &[u8]) -> Option<Vec<u8>> {
        if self == Charset::Utf8 {
            return Some(s.to_vec());
        }
        let text = std::str::from_utf8(s).ok()?;
        let mut out = Vec::with_capacity(s.len());
        for c in text.chars() {
            match self.encode_char(c as u32) {
                Some(b) => out.push(b),
                None => out.extend_from_slice(format!("&#{};", c as u32).as_bytes()),
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin1_roundtrip() {
        let cs = Charset::find(b"iso-8859-1").unwrap();
        assert_eq!(cs.to_utf8(b"caf\xe9").unwrap(), "café".as_bytes());
        assert_eq!(cs.from_utf8("café€".as_bytes()).unwrap(), b"caf\xe9&#8364;");
        assert!(Charset::find(b"non-sense").is_none());
        assert_eq!(Charset::Cp1252.to_utf8(b"\x80").unwrap(), "€".as_bytes());
        assert_eq!(Charset::Latin9.from_utf8("€".as_bytes()).unwrap(), b"\xa4");
    }
}
