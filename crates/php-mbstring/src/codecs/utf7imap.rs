//! `UTF7-IMAP` (modified UTF-7 for IMAP mailbox names, RFC 3501).
//!
//! Ref: `libmbfl/filters/mbfilter_utf7imap.c`.

use crate::codecs::utf7::{
    base64_digit, cp1, cp2, cp3, encode_base64_cp, has_surrogate, is_base64_end,
    is_base64_end_valid, legacy_base64_step, legacy_flush_fragment, put_b64, restore_state,
    save_state, Utf7Out, DASH, ILLEGAL,
};
use crate::convert::{illegal_output, ConvertBuf, BAD_INPUT};
use crate::encoding::{Encoding, EncodingNo};
use crate::legacy::{self, Filter, Sink, Vtbl, BAD_INPUT_I32};

/// `UTF7-IMAP`.
pub static ENC_UTF7IMAP: Encoding = Encoding {
    no: EncodingNo::Utf7imap,
    name: "UTF7-IMAP",
    mime_name: None,
    aliases: &["mUTF-7"],
    mblen_table: None,
    flag: 0,
    input_filter: Some(&VTBL_UTF7IMAP_WCHAR),
    output_filter: Some(&VTBL_WCHAR_UTF7IMAP),
    to_wchar: utf7imap_to_wchar,
    from_wchar: wchar_to_utf7imap,
    check: Some(check_utf7imap),
    cut: None,
};

/// `vtbl_utf7imap_wchar`.
pub static VTBL_UTF7IMAP_WCHAR: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_utf7imap_wchar,
    flush: filt_utf7imap_wchar_flush,
};

/// `vtbl_wchar_utf7imap`.
pub static VTBL_WCHAR_UTF7IMAP: Vtbl = Vtbl {
    ctor: legacy::common_ctor,
    function: filt_wchar_utf7imap,
    flush: filt_wchar_utf7imap_flush,
};

/// Modified Base64 alphabet of UTF7-IMAP (`,` instead of `/`).
const IMAP_BASE64_TABLE: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

/// `decode_base64` (new-style decoder).
fn decode_base64(c: u8) -> u8 {
    if let Some(v) = base64_digit(c, b',') {
        v
    } else if c == b'-' {
        DASH
    } else {
        ILLEGAL
    }
}

/// `mb_utf7imap_to_wchar`. Needs a buffer of at least 5 code points.
#[allow(clippy::too_many_lines, clippy::many_single_char_names)]
pub fn utf7imap_to_wchar(input: &mut &[u8], buf: &mut [u32], state: &mut u32) -> usize {
    debug_assert!(buf.len() >= 5);
    let s = *input;
    let e = s.len();
    // Always leave one free slot in case the string ends in Base64 mode and
    // an error marker must be emitted.
    let limit = buf.len().saturating_sub(1);
    let mut p = 0usize;
    let mut base64 = *state & 1 != 0;
    #[allow(clippy::cast_possible_truncation)]
    let mut o = Utf7Out {
        buf,
        out: 0,
        surrogate1: (*state >> 1) as u16,
    };

    // `handle_base64_end`
    let base64_end = |n: u8, o: &mut Utf7Out, base64: &mut bool, abrupt: bool| {
        if abrupt || n == ILLEGAL || o.surrogate1 != 0 {
            o.emit(BAD_INPUT);
            o.surrogate1 = 0;
        }
        *base64 = false;
    };

    while p < e && o.out < limit {
        if base64 {
            if limit - o.out < 4 {
                break;
            }
            let n1 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n1) {
                base64_end(n1, &mut o, &mut base64, false);
                continue;
            } else if p == e {
                base64_end(n1, &mut o, &mut base64, true);
                continue;
            }
            let n2 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n2) || p == e {
                base64_end(n2, &mut o, &mut base64, true);
                continue;
            }
            let n3 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n3) {
                base64_end(n3, &mut o, &mut base64, true);
                continue;
            }
            o.utf16_cp(cp1(n1, n2, n3), true);
            if p == e {
                if (n3 & 0x3) != 0 || o.surrogate1 != 0 {
                    o.emit(BAD_INPUT);
                }
                break;
            }

            let n4 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n4) {
                base64_end(n4, &mut o, &mut base64, n3 & 0x3 != 0);
                continue;
            } else if p == e {
                base64_end(n4, &mut o, &mut base64, true);
                continue;
            }
            let n5 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n5) || p == e {
                base64_end(n5, &mut o, &mut base64, true);
                continue;
            }
            let n6 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n6) {
                base64_end(n6, &mut o, &mut base64, true);
                continue;
            }
            o.utf16_cp(cp2(n3, n4, n5, n6), true);
            if p == e {
                if (n6 & 0xF) != 0 || o.surrogate1 != 0 {
                    o.emit(BAD_INPUT);
                }
                break;
            }

            let n7 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n7) {
                base64_end(n7, &mut o, &mut base64, n6 & 0xF != 0);
                continue;
            } else if p == e {
                base64_end(n7, &mut o, &mut base64, true);
                continue;
            }
            let n8 = decode_base64(s[p]);
            p += 1;
            if is_base64_end(n8) {
                base64_end(n8, &mut o, &mut base64, true);
                continue;
            }
            o.utf16_cp(cp3(n6, n7, n8), true);
        } else {
            let c = s[p];
            p += 1;
            if c == b'&' {
                if p < e && s[p] == b'-' {
                    o.emit(u32::from(b'&'));
                    p += 1;
                } else {
                    base64 = true;
                }
            } else if (0x20..=0x7E).contains(&c) {
                o.emit(u32::from(c));
            } else {
                o.emit(BAD_INPUT);
            }
        }
    }

    if p == e && base64 {
        // UTF7-IMAP strings may not end in Base64 mode; one slot of the
        // output buffer was reserved for this.
        o.emit(BAD_INPUT);
    }

    *state = (u32::from(o.surrogate1) << 1) | u32::from(base64);
    *input = &s[p..];
    o.out
}

/// `mb_wchar_to_utf7imap`.
#[allow(clippy::cast_possible_truncation)]
pub fn wchar_to_utf7imap(input: &[u32], buf: &mut ConvertBuf, end: bool) {
    let (mut base64, mut nbits, mut cache) = restore_state(buf.state);
    let mut i = 0;
    while i < input.len() {
        let w = input[i];
        if base64 {
            if (0x20..=0x7E).contains(&w) {
                // End of Base64 section: drain cached bits, close it; the
                // code point is then handled by the ASCII branch.
                base64 = false;
                if nbits != 0 {
                    buf.push(
                        IMAP_BASE64_TABLE[((u32::from(cache) << (6 - nbits)) & 0x3F) as usize],
                    );
                }
                nbits = 0;
                cache = 0;
                buf.push(b'-');
                continue;
            } else if w >= 0x11_0000 {
                buf.state = save_state(base64, nbits, cache);
                illegal_output(w, wchar_to_utf7imap, buf);
                (base64, nbits, cache) = restore_state(buf.state);
            } else {
                cache = encode_base64_cp(w, &mut nbits, cache, IMAP_BASE64_TABLE, buf);
            }
        } else if w == u32::from(b'&') {
            buf.extend(b"&-");
        } else if (0x20..=0x7E).contains(&w) {
            buf.push(w as u8);
        } else if w >= 0x11_0000 {
            buf.state = 0;
            illegal_output(w, wchar_to_utf7imap, buf);
            (base64, nbits, cache) = restore_state(buf.state);
        } else {
            buf.push(b'&');
            base64 = true;
            continue;
        }
        i += 1;
    }

    if end {
        if nbits != 0 {
            buf.push(IMAP_BASE64_TABLE[((u32::from(cache) << (6 - nbits)) & 0x3F) as usize]);
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
        // An unexpected 2nd half of a surrogate pair, or printable ASCII
        // (except '&') which should have been encoded directly.
        !((0xDC00..=0xDFFF).contains(&cp) || ((0x20..=0x7E).contains(&cp) && cp != u16::from(b'&')))
    }
}

/// `mb_check_utf7imap`.
#[allow(clippy::too_many_lines)]
pub fn check_utf7imap(input: &[u8]) -> bool {
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
                return false;
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
                return false;
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
            if c == b'&' {
                if p == e {
                    return false;
                }
                let n = decode_base64(input[p]);
                if n == DASH {
                    p += 1;
                } else if n == ILLEGAL {
                    return false;
                } else {
                    base64 = true;
                }
            } else if !(0x20..=0x7E).contains(&c) {
                return false;
            }
        }
    }
    !base64
}

// ---------------------------------------------------------------------------
// Legacy streaming filters (used by `mb_strcut`).

/// `mbfl_filt_conv_utf7imap_wchar`.
pub fn filt_utf7imap_wchar(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status != 0 {
        // Modified Base64
        let digit = u8::try_from(c).ok().and_then(|b| base64_digit(b, b','));
        let Some(n) = digit else {
            if c == i32::from(b'-') {
                if filter.status == 1 {
                    // "&-" -> "&"
                    filter.cache = 0;
                    filter.status = 0;
                    out.put(i32::from(b'&'));
                } else if filter.cache != 0 {
                    // Base64 section ended abruptly, with a partially encoded
                    // character or on the 1st half of a surrogate pair.
                    filter.cache = 0;
                    filter.status = 0;
                    out.put(BAD_INPUT_I32);
                } else {
                    filter.cache = 0;
                    filter.status = 0;
                }
            } else {
                filter.cache = 0;
                filter.status = 0;
                out.put(BAD_INPUT_I32);
            }
            return;
        };
        legacy_base64_step(i32::from(n), true, filter, out);
    } else if c == i32::from(b'&') {
        filter.status += 1;
    } else if (0x20..=0x7E).contains(&c) {
        out.put(c);
    } else {
        out.put(BAD_INPUT_I32);
    }
}

fn filt_utf7imap_wchar_flush(filter: &mut Filter, out: &mut dyn Sink) {
    if filter.status != 0 {
        // A UTF7-IMAP string may not end inside a Base64 section.
        out.put(BAD_INPUT_I32);
        filter.status = 0;
    }
    out.flush();
}

/// `mbfl_filt_conv_wchar_utf7imap`.
pub fn filt_wchar_utf7imap(c: i32, filter: &mut Filter, out: &mut dyn Sink) {
    let n;
    if c == i32::from(b'&') {
        n = 1;
    } else if (0x20..=0x7E).contains(&c) || c == 0 {
        n = 2;
    } else if (0..0x1_0000).contains(&c) {
        n = 0;
    } else if (0x1_0000..0x20_0000).contains(&c) {
        filter.feed(((c >> 10) - 0x40) | 0xD800, out);
        filter.feed((c & 0x3FF) | 0xDC00, out);
        return;
    } else {
        legacy::illegal_output(c, filter, out);
        return;
    }

    let t = IMAP_BASE64_TABLE;
    match filter.status {
        0 => {
            if n != 0 {
                out.put(c);
                if n == 1 {
                    out.put(i32::from(b'-'));
                }
            } else {
                out.put(i32::from(b'&'));
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
                out.put(i32::from(b'-'));
                out.put(c);
                if n == 1 {
                    out.put(i32::from(b'-'));
                }
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
                out.put(i32::from(b'-'));
                out.put(c);
                if n == 1 {
                    out.put(i32::from(b'-'));
                }
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
                out.put(i32::from(b'-'));
                out.put(c);
                if n == 1 {
                    out.put(i32::from(b'-'));
                }
                filter.status = 0;
            } else {
                filter.status = 1;
                filter.cache = c;
            }
        }
    }
}

/// `mbfl_filt_conv_wchar_utf7imap_flush`. (Like libmbfl, this does not
/// flush downstream.)
fn filt_wchar_utf7imap_flush(filter: &mut Filter, out: &mut dyn Sink) {
    legacy_flush_fragment(IMAP_BASE64_TABLE, filter, out);
}
