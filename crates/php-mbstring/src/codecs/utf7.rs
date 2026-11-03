//! `UTF-7` (RFC 2152).
//!
//! Ref: `libmbfl/filters/mbfilter_utf7.c`, `libmbfl/filters/utf7_helper.h`.

use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};
use crate::legacy::{self, Filter, Sink, Vtbl, BAD_INPUT_I32};

/// `UTF-7`.
pub static ENC_UTF7: Encoding = Encoding {
    no: EncodingNo::Utf7,
    name: "UTF-7",
    mime_name: Some("UTF-7"),
    aliases: &["utf7"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: Some(&VTBL_UTF7_WCHAR),
    output_filter: Some(&VTBL_WCHAR_UTF7),
    to_wchar: utf7_to_wchar,
    from_wchar: wchar_to_utf7,
    check: Some(check_utf7),
    cut: None,
};

/// `vtbl_utf7_wchar`.
pub static VTBL_UTF7_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_utf7_wchar,
    flush: filt_utf7_wchar_flush,
};

/// `vtbl_wchar_utf7`.
pub static VTBL_WCHAR_UTF7: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_wchar_utf7,
    flush: filt_wchar_utf7_flush,
};

/// `mbfl_base64_table` (standard Base64 alphabet).
pub(crate) const BASE64_TABLE: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

// Ways in which a Base64-encoded section can end (`utf7_helper.h`).
pub(crate) const DASH: u8 = 0xFC;
pub(crate) const DIRECT: u8 = 0xFD;
pub(crate) const ASCII: u8 = 0xFE;
pub(crate) const ILLEGAL: u8 = 0xFF;

/// `is_base64_end_valid` (`utf7_helper.h`).
pub(crate) fn is_base64_end_valid(n: u8, gap: bool, is_surrogate: bool) -> bool {
    !(gap || is_surrogate || n == ASCII || n == ILLEGAL)
}

/// `has_surrogate` (`utf7_helper.h`).
pub(crate) fn has_surrogate(cp: u16, is_surrogate: bool) -> bool {
    !is_surrogate && (0xD800..=0xDBFF).contains(&cp)
}

/// `is_base64_end`.
pub(crate) fn is_base64_end(c: u8) -> bool {
    c >= DASH
}

/// Value of a Base64 digit (`decode_base64_char` of the legacy filters;
/// `-1` for anything else). `alphabet_63` is `/` (UTF-7) or `,`
/// (UTF7-IMAP).
pub(crate) fn base64_digit(c: u8, alphabet_63: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        _ if c == alphabet_63 => Some(63),
        _ => None,
    }
}

fn is_optional_direct(c: u8) -> bool {
    // Characters that may be either Base64-encoded or directly encoded.
    b"!\"#$%&*;<=>@[]^_`{|}".contains(&c)
}

fn can_end_base64(c: u32) -> bool {
    matches!(
        c,
        0x20 | 0x09 | 0x0D | 0x0A | 0x27 | 0x28 | 0x29 | 0x2C | 0x2E | 0x3A | 0x3F
    )
}

/// `decode_base64` (new-style decoder).
fn decode_base64(c: u8) -> u8 {
    if let Some(v) = base64_digit(c, b'/') {
        v
    } else if c == b'-' {
        DASH
    } else if can_end_base64(u32::from(c)) || is_optional_direct(c) || c == 0 {
        DIRECT
    } else if c <= 0x7F {
        ASCII
    } else {
        ILLEGAL
    }
}

/// Mutable state of the new-style UTF-7 / UTF7-IMAP decoders.
pub(crate) struct Utf7Out<'a> {
    pub(crate) buf: &'a mut [u32],
    pub(crate) out: usize,
    pub(crate) surrogate1: u16,
}

impl Utf7Out<'_> {
    pub(crate) fn emit(&mut self, w: u32) {
        self.buf[self.out] = w;
        self.out += 1;
    }

    /// `handle_utf16_cp`; `imap` additionally rejects Base64-encoded
    /// printable ASCII other than `&`.
    pub(crate) fn utf16_cp(&mut self, cp: u16, imap: bool) {
        loop {
            if self.surrogate1 != 0 {
                if (0xDC00..=0xDFFF).contains(&cp) {
                    let w = (u32::from(self.surrogate1 & 0x3FF) << 10)
                        + u32::from(cp & 0x3FF)
                        + 0x1_0000;
                    self.emit(w);
                    self.surrogate1 = 0;
                } else {
                    self.emit(BAD_INPUT);
                    self.surrogate1 = 0;
                    continue;
                }
            } else if (0xD800..=0xDBFF).contains(&cp) {
                self.surrogate1 = cp;
            } else if (0xDC00..=0xDFFF).contains(&cp)
                || (imap && (0x20..=0x7E).contains(&cp) && cp != u16::from(b'&'))
            {
                self.emit(BAD_INPUT);
            } else {
                self.emit(u32::from(cp));
            }
            return;
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
pub(crate) fn cp1(n1: u8, n2: u8, n3: u8) -> u16 {
    ((u32::from(n1) << 10) | (u32::from(n2) << 4) | (u32::from(n3 & 0x3C) >> 2)) as u16
}

#[allow(clippy::cast_possible_truncation)]
pub(crate) fn cp2(n3: u8, n4: u8, n5: u8, n6: u8) -> u16 {
    ((u32::from(n3) << 14)
        | (u32::from(n4) << 8)
        | (u32::from(n5) << 2)
        | (u32::from(n6 & 0x30) >> 4)) as u16
}

#[allow(clippy::cast_possible_truncation)]
pub(crate) fn cp3(n6: u8, n7: u8, n8: u8) -> u16 {
    ((u32::from(n6) << 12) | (u32::from(n7) << 6) | u32::from(n8)) as u16
}

/// `mb_utf7_to_wchar`. Needs a buffer of at least 5 code points.
#[allow(clippy::too_many_lines, clippy::many_single_char_names)]
pub fn utf7_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    debug_assert!(buf.len() >= 5);
    let s = *input;
    let e = s.len();
    let limit = buf.len();
    let mut p = 0usize;
    let mut base64 = *state & 1 != 0;
    #[allow(clippy::cast_possible_truncation)]
    let mut o = Utf7Out {
        buf,
        out: 0,
        surrogate1: (*state >> 1) as u16,
    };

    // `handle_base64_end`
    let base64_end = |n: u8, p: &mut usize, o: &mut Utf7Out, base64: &mut bool, abrupt: bool| {
        if abrupt || o.surrogate1 != 0 {
            o.emit(BAD_INPUT);
            o.surrogate1 = 0;
        }
        if n == ILLEGAL {
            o.emit(BAD_INPUT);
        } else if n == DIRECT || n == ASCII {
            *p -= 1; // Unconsume byte
        }
        *base64 = false;
    };

    while p < e && o.out < limit {
        if base64 {
            if limit - o.out < 5 {
                break;
            }
            let n1 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n1) {
                base64_end(n1, &mut p, &mut o, &mut base64, false);
                continue;
            } else if p == e {
                base64_end(n1, &mut p, &mut o, &mut base64, true);
                continue;
            }
            let n2 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n2) || p == e {
                base64_end(n2, &mut p, &mut o, &mut base64, true);
                continue;
            }
            let n3 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n3) {
                base64_end(n3, &mut p, &mut o, &mut base64, true);
                continue;
            }
            o.utf16_cp(cp1(n1, n2, n3), false);
            if p == e {
                // Error if the padding bits are not zero or a surrogate pair
                // is incomplete when the Base64 section ends.
                if (n3 & 0x3) != 0 || o.surrogate1 != 0 {
                    o.emit(BAD_INPUT);
                    o.surrogate1 = 0;
                }
                break;
            }

            let n4 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n4) {
                base64_end(n4, &mut p, &mut o, &mut base64, n3 & 0x3 != 0);
                continue;
            } else if p == e {
                base64_end(n4, &mut p, &mut o, &mut base64, true);
                continue;
            }
            let n5 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n5) || p == e {
                base64_end(n5, &mut p, &mut o, &mut base64, true);
                continue;
            }
            let n6 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n6) {
                base64_end(n6, &mut p, &mut o, &mut base64, true);
                continue;
            }
            o.utf16_cp(cp2(n3, n4, n5, n6), false);
            if p == e {
                if (n6 & 0xF) != 0 || o.surrogate1 != 0 {
                    o.emit(BAD_INPUT);
                    o.surrogate1 = 0;
                }
                break;
            }

            let n7 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n7) {
                base64_end(n7, &mut p, &mut o, &mut base64, n6 & 0xF != 0);
                continue;
            } else if p == e {
                base64_end(n7, &mut p, &mut o, &mut base64, true);
                continue;
            }
            let n8 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n8) {
                base64_end(n8, &mut p, &mut o, &mut base64, true);
                continue;
            }
            o.utf16_cp(cp3(n6, n7, n8), false);
        } else {
            // ASCII text section
            let c = s[p];
            p += 1;
            if c == b'+' {
                if p < e {
                    if s[p] == b'-' {
                        o.emit(u32::from(b'+'));
                        p += 1;
                    } else {
                        base64 = true;
                    }
                }
                // A '+' at the very end of the input is ignored.
            } else if c <= 0x7F {
                o.emit(u32::from(c));
            } else {
                o.emit(BAD_INPUT);
            }
        }
    }

    if p == e && o.surrogate1 != 0 {
        o.emit(BAD_INPUT);
    }

    *state = (u32::from(o.surrogate1) << 1) | u32::from(base64);
    *input = &s[p..];
    o.out
}

fn should_direct_encode(c: u32) -> bool {
    matches!(c, 0x41..=0x5A | 0x61..=0x7A | 0x30..=0x39 | 0 | 0x2F | 0x2D) || can_end_base64(c)
}

/// Encoder state packing shared by UTF-7 and UTF7-IMAP
/// (`SAVE_CONVERSION_STATE` / `RESTORE_CONVERSION_STATE`).
pub(crate) fn save_state(base64: bool, nbits: u32, cache: u8) -> u32 {
    (u32::from(cache) << 4) | (nbits << 1) | u32::from(base64)
}

#[allow(clippy::cast_possible_truncation)]
pub(crate) fn restore_state(state: u32) -> (bool, u32, u8) {
    (state & 1 != 0, (state >> 1) & 0x7, (state >> 4) as u8)
}

/// Base64-encode `w` (a code point below `0x110000`), preceded by `nbits`
/// cached bits; returns the new cache byte.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn encode_base64_cp(
    w: u32,
    nbits: &mut u32,
    cache: u8,
    table: &[u8; 64],
    buf: &mut ConvertBuf,
) -> u8 {
    let bits: u64 = if w >= 0x1_0000 {
        // Must use a surrogate pair.
        let w = w - 0x1_0000;
        *nbits += 32;
        (u64::from(cache) << 32)
            | 0xD800_DC00
            | u64::from((w & 0xF_FC00) << 6)
            | u64::from(w & 0x3FF)
    } else {
        *nbits += 16;
        u64::from((u32::from(cache) << 16) | w)
    };
    while *nbits >= 6 {
        buf.push(table[((bits >> (*nbits - 6)) & 0x3F) as usize]);
        *nbits -= 6;
    }
    bits as u8
}

/// `mb_wchar_to_utf7`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_utf7(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    let (mut base64, mut nbits, mut cache) = restore_state(buf.state);
    let mut i = 0;
    while i < input.len() {
        let w = input[i];
        if base64 {
            if should_direct_encode(w) {
                // End of Base64 section: drain cached bits, close it; the
                // code point is then handled by the ASCII branch.
                base64 = false;
                if nbits != 0 {
                    buf.push(BASE64_TABLE[((u32::from(cache) << (6 - nbits)) & 0x3F) as usize]);
                }
                nbits = 0;
                cache = 0;
                if !can_end_base64(w) {
                    buf.push(b'-');
                }
                continue;
            } else if w >= 0x11_0000 {
                buf.state = save_state(base64, nbits, cache);
                illegal_output(w, wchar_to_utf7, buf);
                (base64, nbits, cache) = restore_state(buf.state);
            } else {
                cache = encode_base64_cp(w, &mut nbits, cache, BASE64_TABLE, buf);
            }
        } else if should_direct_encode(w) {
            buf.push(w as u8);
        } else if w >= 0x11_0000 {
            buf.state = 0;
            illegal_output(w, wchar_to_utf7, buf);
            (base64, nbits, cache) = restore_state(buf.state);
        } else {
            buf.push(b'+');
            base64 = true;
            continue;
        }
        i += 1;
    }

    if end {
        if nbits != 0 {
            buf.push(BASE64_TABLE[((u32::from(cache) << (6 - nbits)) & 0x3F) as usize]);
        }
        if base64 {
            buf.push(b'-');
        }
    } else {
        buf.state = save_state(base64, nbits, cache);
    }
}

fn is_utf16_cp_valid(cp: u16, is_surrogate: bool) -> bool {
    if is_surrogate {
        (0xDC00..=0xDFFF).contains(&cp)
    } else {
        // 2nd part of a surrogate pair came unexpectedly.
        !(0xDC00..=0xDFFF).contains(&cp)
    }
}

fn can_encode_directly(c: u8) -> bool {
    should_direct_encode(u32::from(c)) || is_optional_direct(c) || c == 0
}

/// `mb_check_utf7`.
#[allow(clippy::too_many_lines)]
pub fn check_utf7(input: &[u8]) -> bool {
    let e = input.len();
    let mut p = 0usize;
    let mut base64 = false;
    let mut is_surrogate = false;
    let next = |p: &mut usize| {
        let n = decode_base64(input[*p]);
        *p += 1;
        n
    };

    while p < e {
        if base64 {
            let n1 = next(&mut p);
            if is_base64_end(n1) {
                if !is_base64_end_valid(n1, false, is_surrogate) {
                    return false;
                }
                base64 = false;
                continue;
            } else if p == e {
                return false;
            }
            let n2 = next(&mut p);
            if is_base64_end(n2) || p == e {
                return false;
            }
            let n3 = next(&mut p);
            if is_base64_end(n3) {
                return false;
            }
            let c1 = cp1(n1, n2, n3);
            if !is_utf16_cp_valid(c1, is_surrogate) {
                return false;
            }
            is_surrogate = has_surrogate(c1, is_surrogate);
            if p == e {
                return !((n3 & 0x3) != 0 || is_surrogate);
            }

            let n4 = next(&mut p);
            if is_base64_end(n4) {
                if !is_base64_end_valid(n4, n3 & 0x3 != 0, is_surrogate) {
                    return false;
                }
                base64 = false;
                continue;
            } else if p == e {
                return false;
            }
            let n5 = next(&mut p);
            if is_base64_end(n5) || p == e {
                return false;
            }
            let n6 = next(&mut p);
            if is_base64_end(n6) {
                return false;
            }
            let c2 = cp2(n3, n4, n5, n6);
            if !is_utf16_cp_valid(c2, is_surrogate) {
                return false;
            }
            is_surrogate = has_surrogate(c2, is_surrogate);
            if p == e {
                return !((n6 & 0xF) != 0 || is_surrogate);
            }

            let n7 = next(&mut p);
            if is_base64_end(n7) {
                if !is_base64_end_valid(n7, n6 & 0xF != 0, is_surrogate) {
                    return false;
                }
                base64 = false;
                continue;
            } else if p == e {
                return false;
            }
            let n8 = next(&mut p);
            if is_base64_end(n8) {
                return false;
            }
            let c3 = cp3(n6, n7, n8);
            if !is_utf16_cp_valid(c3, is_surrogate) {
                return false;
            }
            is_surrogate = has_surrogate(c3, is_surrogate);
        } else {
            let c = input[p];
            p += 1;
            if c == b'+' {
                if p == e {
                    return !is_surrogate;
                }
                let n = decode_base64(input[p]);
                match n.cmp(&DASH) {
                    std::cmp::Ordering::Equal => p += 1,
                    // '+' followed by something other than Base64 or '-'.
                    std::cmp::Ordering::Greater => return false,
                    std::cmp::Ordering::Less => base64 = true,
                }
            } else if !can_encode_directly(c) {
                return false;
            }
        }
    }
    !is_surrogate
}

// ---------------------------------------------------------------------------
// Legacy streaming filters (used by `mb_strcut`).

/// Shared step of the legacy UTF-7 / UTF7-IMAP decoders once a 16-bit unit
/// `s` is complete; `n` holds the leftover bits for `filter.cache`. Returns
/// nothing; `imap` selects the UTF7-IMAP variant.
fn legacy_utf16_unit(s: i32, n: i32, imap: bool, filter: &mut Filter, out: &mut dyn Sink) {
    if (0xD800..0xDC00).contains(&s) {
        // 1st part of surrogate pair
        if !imap && filter.cache & 0x0FFF_0000 != 0 {
            // We were waiting for the 2nd part of a surrogate pair.
            out.put(BAD_INPUT_I32);
        }
        filter.cache = (((s & 0x3FF) << 16) + 0x40_0000) | n;
    } else if (0xDC00..0xE000).contains(&s) {
        // 2nd part of surrogate pair
        if filter.cache & 0x0FFF_0000 != 0 {
            let s = (s & 0x3FF) | ((filter.cache & 0x0FFF_0000) >> 6);
            filter.cache = n;
            out.put(s);
        } else {
            out.put(BAD_INPUT_I32);
            if !imap {
                filter.cache = n;
            }
        }
    } else if imap {
        filter.cache = n;
        // Characters which can be expressed as literal ASCII should not be
        // Base64-encoded.
        if !(0x20..=0x7E).contains(&s) || s == i32::from(b'&') {
            out.put(s);
        } else {
            out.put(BAD_INPUT_I32);
        }
    } else {
        if filter.cache & 0x0FFF_0000 != 0 {
            out.put(BAD_INPUT_I32);
        }
        filter.cache = n;
        out.put(s);
    }
}

/// Shared Modified Base64 state machine (states 1-9) of the legacy UTF-7 and
/// UTF7-IMAP decoders, fed one 6-bit value `n`.
pub(crate) fn legacy_base64_step(n: i32, imap: bool, filter: &mut Filter, out: &mut dyn Sink) {
    match filter.status {
        1 | 2 => {
            filter.cache |= n << 10;
            filter.status = 3;
        }
        3 => {
            filter.cache |= n << 4;
            filter.status = 4;
        }
        4 => {
            let s = ((n >> 2) & 0xF) | (filter.cache & 0xFFFF);
            filter.status = 5;
            legacy_utf16_unit(s, (n & 0x3) << 14, imap, filter, out);
        }
        5 => {
            filter.cache |= n << 8;
            filter.status = 6;
        }
        6 => {
            filter.cache |= n << 2;
            filter.status = 7;
        }
        7 => {
            let s = ((n >> 4) & 0x3) | (filter.cache & 0xFFFF);
            filter.status = 8;
            legacy_utf16_unit(s, (n & 0xF) << 12, imap, filter, out);
        }
        8 => {
            filter.cache |= n << 6;
            filter.status = 9;
        }
        _ => {
            let s = n | (filter.cache & 0xFFFF);
            filter.status = 2;
            legacy_utf16_unit(s, 0, imap, filter, out);
        }
    }
}

/// `mbfl_filt_conv_utf7_wchar`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn filt_utf7_wchar(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status != 0 {
        // Modified Base64
        let Some(n) = base64_digit(c as u8, b'/') else {
            if filter.cache != 0 {
                // Either a surrogate pair's 2nd half never came, or the last
                // Base64 data was not padded with zeroes.
                out.put(BAD_INPUT_I32);
            }
            if c == i32::from(b'-') {
                if filter.status == 1 {
                    // "+-" -> "+"
                    out.put(i32::from(b'+'));
                }
            } else if (0..0x80).contains(&c) {
                out.put(c);
            } else {
                out.put(BAD_INPUT_I32);
            }
            filter.cache = 0;
            filter.status = 0;
            return;
        };
        legacy_base64_step(i32::from(n), false, filter, out);
    } else if c == i32::from(b'+') {
        filter.status = 1;
    } else if (0..0x80).contains(&c) {
        out.put(c);
    } else {
        out.put(BAD_INPUT_I32);
    }
}

fn filt_utf7_wchar_flush(filter: &mut Filter, out: &mut dyn Sink) {
    if filter.cache != 0 {
        filter.cache = 0;
        out.put(BAD_INPUT_I32);
    }
    out.flush();
}

/// Emit the Base64 digit for `v & 0x3F`.
pub(crate) fn put_b64(table: &[u8; 64], v: i32, out: &mut dyn Sink) {
    #[allow(clippy::cast_sign_loss)]
    out.put(i32::from(table[(v & 0x3F) as usize]));
}

/// `mbfl_filt_conv_wchar_utf7`.
pub fn filt_wchar_utf7(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    let mut n = 0;
    if (0..0x80).contains(&c) {
        #[allow(clippy::cast_sign_loss)]
        let u = c as u32;
        if matches!(u, 0x41..=0x5A | 0x61..=0x7A | 0x30..=0x39 | 0 | 0x2F | 0x2D) {
            n = 1;
        } else if can_end_base64(u) {
            n = 2;
        }
    } else if (0..0x1_0000).contains(&c) {
        // Base64-encoded
    } else if (0x1_0000..0x11_0000).contains(&c) {
        filter.feed(((c >> 10) - 0x40) | 0xD800, out);
        filter.feed((c & 0x3FF) | 0xDC00, out);
        return;
    } else {
        legacy::illegal_output(c, filter, out);
        return;
    }

    let t = BASE64_TABLE;
    match filter.status {
        0 => {
            if n != 0 {
                out.put(c);
            } else {
                out.put(i32::from(b'+'));
                filter.status = 1;
                filter.cache = c;
            }
        }
        1 => {
            let s = filter.cache;
            put_b64(t, s >> 10, out);
            put_b64(t, s >> 4, out);
            if n != 0 {
                put_b64(t, (s << 2) & 0x3C, out);
                if n == 1 {
                    out.put(i32::from(b'-'));
                }
                out.put(c);
                filter.status = 0;
            } else {
                filter.status = 2;
                filter.cache = ((s & 0xF) << 16) | c;
            }
        }
        2 => {
            let s = filter.cache;
            put_b64(t, s >> 14, out);
            put_b64(t, s >> 8, out);
            put_b64(t, s >> 2, out);
            if n != 0 {
                put_b64(t, (s << 4) & 0x30, out);
                if n == 1 {
                    out.put(i32::from(b'-'));
                }
                out.put(c);
                filter.status = 0;
            } else {
                filter.status = 3;
                filter.cache = ((s & 0x3) << 16) | c;
            }
        }
        _ => {
            let s = filter.cache;
            put_b64(t, s >> 12, out);
            put_b64(t, s >> 6, out);
            put_b64(t, s, out);
            if n != 0 {
                if n == 1 {
                    out.put(i32::from(b'-'));
                }
                out.put(c);
                filter.status = 0;
            } else {
                filter.status = 1;
                filter.cache = c;
            }
        }
    }
}

/// Flush the pending Base64 fragment of the legacy UTF-7 / UTF7-IMAP
/// encoders (shared by both flush functions).
pub(crate) fn legacy_flush_fragment(table: &[u8; 64], filter: &mut Filter, out: &mut dyn Sink) {
    let (status, cache) = (filter.status, filter.cache);
    filter.status = 0;
    filter.cache = 0;
    match status {
        1 => {
            put_b64(table, cache >> 10, out);
            put_b64(table, cache >> 4, out);
            put_b64(table, (cache << 2) & 0x3C, out);
            out.put(i32::from(b'-'));
        }
        2 => {
            put_b64(table, cache >> 14, out);
            put_b64(table, cache >> 8, out);
            put_b64(table, cache >> 2, out);
            put_b64(table, (cache << 4) & 0x30, out);
            out.put(i32::from(b'-'));
        }
        3 => {
            put_b64(table, cache >> 12, out);
            put_b64(table, cache >> 6, out);
            put_b64(table, cache, out);
            out.put(i32::from(b'-'));
        }
        _ => {}
    }
}

/// `mbfl_filt_conv_wchar_utf7_flush`.
pub fn filt_wchar_utf7_flush(filter: &mut Filter, out: &mut dyn Sink) {
    legacy_flush_fragment(BASE64_TABLE, filter, out);
    out.flush();
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::convert::{decode_all, encode_all, ErrorMode};
    use crate::legacy::ByteSink;

    /// Collects the raw `i32` values a filter emits.
    #[derive(Default)]
    pub(crate) struct IntSink(pub(crate) Vec<i32>);
    impl Sink for IntSink {
        fn put(&mut self, c: i32) {
            self.0.push(c);
        }
    }

    /// Deterministic pseudo-random valid code points (xorshift).
    pub(crate) fn random_cps(seed: u32, n: usize) -> Vec<u32> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let cp = match x % 4 {
                    0 => x % 0x80,
                    1 => b" -+&/,.?!A\t"[(x as usize >> 3) % 11].into(),
                    2 => 0x80 + x % 0xD780,
                    _ => 0x1_0000 + x % 0x10_0000,
                };
                if cp == 0 {
                    1
                } else {
                    cp
                }
            })
            .collect()
    }

    pub(crate) fn legacy_decode(vtbl: &Vtbl, input: &[u8]) -> Vec<u32> {
        let mut f = Filter::new(vtbl);
        let mut sink = IntSink::default();
        for &b in input {
            f.feed(i32::from(b), &mut sink);
        }
        f.flush(&mut sink);
        #[allow(clippy::cast_sign_loss)]
        sink.0.into_iter().map(|c| c as u32).collect()
    }

    #[allow(clippy::cast_possible_wrap)]
    pub(crate) fn legacy_encode(vtbl: &Vtbl, input: &[u32]) -> Vec<u8> {
        let mut f = Filter::new(vtbl);
        let mut sink = ByteSink::default();
        for &w in input {
            f.feed(w as i32, &mut sink);
        }
        f.flush(&mut sink);
        sink.0
    }

    #[test]
    fn decode_phpt_cases() {
        // utf_encodings.phpt / gh10192_utf7.phpt
        let d = |s: &[u8]| decode_all(s, &ENC_UTF7);
        assert_eq!(d(b"+-"), vec![0x2B]);
        assert_eq!(d(b"+AGE-"), vec![0x61]);
        assert_eq!(d(b"+2D3eAA-"), vec![0x1F600]);
        assert_eq!(d(b"+l"), vec![BAD_INPUT]);
        assert_eq!(d(b"+A"), vec![BAD_INPUT]);
        assert_eq!(d(b"+\x80"), vec![BAD_INPUT]);
        assert_eq!(d(b"\xFF"), vec![BAD_INPUT]);
        assert_eq!(d(b"+RR8I"), vec![0x451F, BAD_INPUT]);
        assert_eq!(d(b"a+"), vec![0x61]);
        assert!(check_utf7(b"+AGE-a+-"));
        assert!(!check_utf7(b"+AGEA-"));
        assert!(!check_utf7(b"+2D0-"));
        assert!(check_utf7(b"+"));
    }

    #[test]
    fn encode_cases() {
        let e = |w: &[u32]| encode_all(w, &ENC_UTF7, 0x3F, ErrorMode::Char);
        assert_eq!(e(&[0x25, 0x41]), b"+ACU-A");
        assert_eq!(e(&[0x25, 0x25, 0x41]), b"+ACUAJQ-A");
        assert_eq!(e(&[0x1F600, 0x20]), b"+2D3eAA ");
        assert_eq!(e(&[0x1234_5678]), b"?");
        let e = |w: &[u32]| encode_all(w, &ENC_UTF7, 0x25, ErrorMode::Char);
        assert_eq!(e(&[0x1234_5678]), b"+ACU-");
    }

    #[test]
    fn legacy_filters_agree_with_fast_path_on_valid_text() {
        for seed in 1..60 {
            let cps = random_cps(seed, (seed as usize * 7) % 90);
            let encoded = encode_all(&cps, &ENC_UTF7, 0x3F, ErrorMode::Char);
            assert_eq!(
                legacy_encode(&VTBL_WCHAR_UTF7, &cps),
                encoded,
                "seed {seed}"
            );
            assert_eq!(
                legacy_decode(&VTBL_UTF7_WCHAR, &encoded),
                cps,
                "seed {seed}"
            );
            assert!(check_utf7(&encoded));
        }
    }

    #[test]
    fn legacy_decoder_errors() {
        // Unlike the fast path, the legacy decoder accepts a section ending
        // with zero padding bits even if a whole extra Base64 digit is present.
        assert_eq!(
            legacy_decode(&VTBL_UTF7_WCHAR, b"+AGEA-x"),
            vec![0x61, 0x78]
        );
        assert_eq!(
            decode_all(b"+AGEA-x", &ENC_UTF7),
            vec![0x61, BAD_INPUT, 0x78]
        );
        assert_eq!(
            legacy_decode(&VTBL_UTF7_WCHAR, b"+AGEB-x"),
            vec![0x61, BAD_INPUT, 0x78]
        );
        assert_eq!(legacy_decode(&VTBL_UTF7_WCHAR, b"+2D0-"), vec![BAD_INPUT]);
        assert_eq!(legacy_decode(&VTBL_UTF7_WCHAR, b"+2D0"), vec![BAD_INPUT]);
        assert_eq!(
            legacy_decode(&VTBL_UTF7_WCHAR, b"+-\x80"),
            vec![0x2B, BAD_INPUT]
        );
        assert_eq!(legacy_encode(&VTBL_WCHAR_UTF7, &[0x11_0000, 0x41]), b"?A");
    }
}
