//! ASCII, ISO-8859-*, Windows-125x, CP866, CP850, KOI8-*, ArmSCII-8.
//!
//! Ref: `libmbfl/filters/mbfilter_singlebyte.c`.

use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_SBCS};
use crate::tables::singlebyte as t;

#[inline]
fn coalesce(a: u16) -> u32 {
    if a == 0 {
        BAD_INPUT
    } else {
        u32::from(a)
    }
}

/// Decode with a table covering bytes `min..=0xFF`.
#[inline]
fn tbl_to_wchar(input: &mut &[u8], buf: &mut [u32], min: u8, tbl: &[u16]) -> usize {
    let n = input.len().min(buf.len());
    for (o, &c) in buf.iter_mut().zip(&input[..n]) {
        *o = if c < min {
            u32::from(c)
        } else {
            coalesce(tbl[usize::from(c - min)])
        };
    }
    *input = &input[n..];
    n
}

/// Encode with a table covering bytes `min..=0xFF` (first match wins).
#[inline]
#[allow(clippy::cast_possible_truncation)]
fn tbl_from_wchar(
    input: &[u32],
    buf: &mut ConvertBuf,
    min: u8,
    tbl: &[u16],
    this: crate::convert::FromWcharFn,
) {
    for &w in input {
        if w < u32::from(min) {
            buf.push(w as u8);
        } else if let Some(i) = tbl.iter().position(|&x| u32::from(x) == w) {
            buf.push(i as u8 + min);
        } else {
            illegal_output(w, this, buf);
        }
    }
}

macro_rules! def_sb_tbl {
    ($enc:ident, $dec:ident, $encf:ident, $no:ident, $name:literal, $mime:literal,
     [$($alias:literal),*], $min:literal, $tbl:path) => {
        fn $dec(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
            tbl_to_wchar(input, buf, $min, &$tbl)
        }
        fn $encf(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
            tbl_from_wchar(input, buf, $min, &$tbl, $encf);
        }
        def_sb!($enc, $dec, $encf, $no, $name, $mime, [$($alias),*]);
    };
}

macro_rules! def_sb {
    ($enc:ident, $dec:ident, $encf:ident, $no:ident, $name:literal, $mime:literal,
     [$($alias:literal),*]) => {
        #[doc = concat!("`", $name, "`.")]
        pub static $enc: Encoding = Encoding {
            no: EncodingNo::$no,
            name: $name,
            mime_name: Some($mime),
            aliases: &[$($alias),*],
            mblen_table: None,
            flag: ENCTYPE_SBCS,
            input_filter: None,
            output_filter: None,
            to_wchar: $dec,
            from_wchar: $encf,
            check: None,
            cut: None,
        };
    };
}

def_sb!(
    ENC_ASCII,
    ascii_to_wchar,
    wchar_to_ascii,
    Ascii,
    "ASCII",
    "US-ASCII",
    [
        "ANSI_X3.4-1968",
        "iso-ir-6",
        "ANSI_X3.4-1986",
        "ISO_646.irv:1991",
        "US-ASCII",
        "ISO646-US",
        "us",
        "IBM367",
        "IBM-367",
        "cp367",
        "csASCII"
    ]
);

fn ascii_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let n = input.len().min(buf.len());
    for (o, &c) in buf.iter_mut().zip(&input[..n]) {
        *o = if c < 0x80 { u32::from(c) } else { BAD_INPUT };
    }
    *input = &input[n..];
    n
}

#[allow(clippy::cast_possible_truncation)]
fn wchar_to_ascii(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w < 0x80 {
            buf.push(w as u8);
        } else {
            illegal_output(w, wchar_to_ascii, buf);
        }
    }
}

def_sb!(
    ENC_8859_1,
    latin1_to_wchar,
    wchar_to_latin1,
    Iso8859_1,
    "ISO-8859-1",
    "ISO-8859-1",
    ["ISO8859-1", "latin1"]
);

fn latin1_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let n = input.len().min(buf.len());
    for (o, &c) in buf.iter_mut().zip(&input[..n]) {
        *o = u32::from(c);
    }
    *input = &input[n..];
    n
}

#[allow(clippy::cast_possible_truncation)]
fn wchar_to_latin1(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w < 0x100 {
            buf.push(w as u8);
        } else {
            illegal_output(w, wchar_to_latin1, buf);
        }
    }
}

def_sb_tbl!(
    ENC_8859_2,
    l2_to_wchar,
    wchar_to_l2,
    Iso8859_2,
    "ISO-8859-2",
    "ISO-8859-2",
    ["ISO8859-2", "latin2"],
    0xA0,
    t::ISO8859_2_UCS_TABLE
);
def_sb_tbl!(
    ENC_8859_3,
    l3_to_wchar,
    wchar_to_l3,
    Iso8859_3,
    "ISO-8859-3",
    "ISO-8859-3",
    ["ISO8859-3", "latin3"],
    0xA0,
    t::ISO8859_3_UCS_TABLE
);
def_sb_tbl!(
    ENC_8859_4,
    l4_to_wchar,
    wchar_to_l4,
    Iso8859_4,
    "ISO-8859-4",
    "ISO-8859-4",
    ["ISO8859-4", "latin4"],
    0xA0,
    t::ISO8859_4_UCS_TABLE
);
def_sb_tbl!(
    ENC_8859_5,
    l5_to_wchar,
    wchar_to_l5,
    Iso8859_5,
    "ISO-8859-5",
    "ISO-8859-5",
    ["ISO8859-5", "cyrillic"],
    0xA0,
    t::ISO8859_5_UCS_TABLE
);
def_sb_tbl!(
    ENC_8859_6,
    l6_to_wchar,
    wchar_to_l6,
    Iso8859_6,
    "ISO-8859-6",
    "ISO-8859-6",
    ["ISO8859-6", "arabic"],
    0xA0,
    t::ISO8859_6_UCS_TABLE
);
def_sb_tbl!(
    ENC_8859_7,
    l7_to_wchar,
    wchar_to_l7,
    Iso8859_7,
    "ISO-8859-7",
    "ISO-8859-7",
    ["ISO8859-7", "greek"],
    0xA0,
    t::ISO8859_7_UCS_TABLE
);
def_sb_tbl!(
    ENC_8859_8,
    l8_to_wchar,
    wchar_to_l8,
    Iso8859_8,
    "ISO-8859-8",
    "ISO-8859-8",
    ["ISO8859-8", "hebrew"],
    0xA0,
    t::ISO8859_8_UCS_TABLE
);
def_sb_tbl!(
    ENC_8859_9,
    l9_to_wchar,
    wchar_to_l9,
    Iso8859_9,
    "ISO-8859-9",
    "ISO-8859-9",
    ["ISO8859-9", "latin5"],
    0xA0,
    t::ISO8859_9_UCS_TABLE
);
def_sb_tbl!(
    ENC_8859_10,
    l10_to_wchar,
    wchar_to_l10,
    Iso8859_10,
    "ISO-8859-10",
    "ISO-8859-10",
    ["ISO8859-10", "latin6"],
    0xA0,
    t::ISO8859_10_UCS_TABLE
);
def_sb_tbl!(
    ENC_8859_13,
    l13_to_wchar,
    wchar_to_l13,
    Iso8859_13,
    "ISO-8859-13",
    "ISO-8859-13",
    ["ISO8859-13"],
    0xA0,
    t::ISO8859_13_UCS_TABLE
);
def_sb_tbl!(
    ENC_8859_14,
    l14_to_wchar,
    wchar_to_l14,
    Iso8859_14,
    "ISO-8859-14",
    "ISO-8859-14",
    ["ISO8859-14", "latin8"],
    0xA0,
    t::ISO8859_14_UCS_TABLE
);
def_sb_tbl!(
    ENC_8859_15,
    l15_to_wchar,
    wchar_to_l15,
    Iso8859_15,
    "ISO-8859-15",
    "ISO-8859-15",
    ["ISO8859-15"],
    0xA0,
    t::ISO8859_15_UCS_TABLE
);
def_sb_tbl!(
    ENC_8859_16,
    l16_to_wchar,
    wchar_to_l16,
    Iso8859_16,
    "ISO-8859-16",
    "ISO-8859-16",
    ["ISO8859-16"],
    0xA0,
    t::ISO8859_16_UCS_TABLE
);
def_sb_tbl!(
    ENC_CP1251,
    cp1251_to_wchar,
    wchar_to_cp1251,
    Cp1251,
    "Windows-1251",
    "Windows-1251",
    ["CP1251", "CP-1251", "WINDOWS-1251"],
    0x80,
    t::CP1251_UCS_TABLE
);
def_sb_tbl!(
    ENC_CP1254,
    cp1254_to_wchar,
    wchar_to_cp1254,
    Cp1254,
    "Windows-1254",
    "Windows-1254",
    ["CP1254", "CP-1254", "WINDOWS-1254"],
    0x80,
    t::CP1254_UCS_TABLE
);
def_sb_tbl!(
    ENC_CP866,
    cp866_to_wchar,
    wchar_to_cp866,
    Cp866,
    "CP866",
    "CP866",
    ["CP-866", "IBM866", "IBM-866"],
    0x80,
    t::CP866_UCS_TABLE
);
def_sb_tbl!(
    ENC_CP850,
    cp850_to_wchar,
    wchar_to_cp850,
    Cp850,
    "CP850",
    "CP850",
    ["CP-850", "IBM850", "IBM-850"],
    0x80,
    t::CP850_UCS_TABLE
);
def_sb_tbl!(
    ENC_KOI8R,
    koi8r_to_wchar,
    wchar_to_koi8r,
    Koi8r,
    "KOI8-R",
    "KOI8-R",
    ["KOI8R"],
    0x80,
    t::KOI8R_UCS_TABLE
);
def_sb_tbl!(
    ENC_KOI8U,
    koi8u_to_wchar,
    wchar_to_koi8u,
    Koi8u,
    "KOI8-U",
    "KOI8-U",
    ["KOI8U"],
    0x80,
    t::KOI8U_UCS_TABLE
);

def_sb!(
    ENC_CP1252,
    cp1252_to_wchar,
    wchar_to_cp1252,
    Cp1252,
    "Windows-1252",
    "Windows-1252",
    ["cp1252"]
);

fn cp1252_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let n = input.len().min(buf.len());
    for (o, &c) in buf.iter_mut().zip(&input[..n]) {
        *o = if (0x80..0xA0).contains(&c) {
            coalesce(t::CP1252_UCS_TABLE[usize::from(c - 0x80)])
        } else {
            u32::from(c)
        };
    }
    *input = &input[n..];
    n
}

#[allow(clippy::cast_possible_truncation)]
fn wchar_to_cp1252(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if w >= 0x100 {
            if let Some(i) = t::CP1252_UCS_TABLE.iter().position(|&x| u32::from(x) == w) {
                buf.push(i as u8 + 0x80);
            } else {
                illegal_output(w, wchar_to_cp1252, buf);
            }
        } else if w <= 0x7F || w >= 0xA0 || matches!(w, 0x81 | 0x8D | 0x8F | 0x90 | 0x9D) {
            buf.push(w as u8);
        } else {
            illegal_output(w, wchar_to_cp1252, buf);
        }
    }
}

def_sb!(
    ENC_ARMSCII8,
    armscii8_to_wchar,
    wchar_to_armscii8,
    Armscii8,
    "ArmSCII-8",
    "ArmSCII-8",
    ["ArmSCII8", "ARMSCII-8", "ARMSCII8"]
);

fn armscii8_to_wchar(input: &mut &[u8], buf: &mut [u32], _state: &mut u32) -> usize {
    let n = input.len().min(buf.len());
    for (o, &c) in buf.iter_mut().zip(&input[..n]) {
        *o = if c < 0xA0 {
            u32::from(c)
        } else {
            coalesce(t::ARMSCII8_UCS_TABLE[usize::from(c - 0xA0)])
        };
    }
    *input = &input[n..];
    n
}

#[allow(clippy::cast_possible_truncation)]
fn wchar_to_armscii8(input: &[u32], buf: &mut ConvertBuf, _end: bool) {
    for &w in input {
        if (0x28..=0x2F).contains(&w) {
            buf.push(t::UCS_ARMSCII8_TABLE[(w - 0x28) as usize]);
        } else if w < 0xA0 {
            buf.push(w as u8);
        } else if let Some(i) = t::ARMSCII8_UCS_TABLE
            .iter()
            .position(|&x| u32::from(x) == w)
        {
            buf.push(0xA0 + i as u8);
        } else {
            illegal_output(w, wchar_to_armscii8, buf);
        }
    }
}
