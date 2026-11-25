// Derived from php-src ext/mbstring/mbstring.c. Similarity metric to original: 10%
//   Copyright © The PHP Group and Contributors.
//   Author: Tsukada Takuya <tsukada@fminn.nagano.nagano.jp>
//           Rui Hirokawa <hirokawa@php.net>
//           Hironori Sato <satoh@jpnnet.com>
//           Shigeru Kanemoto <sgk@happysize.co.jp>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! `mb_encode_mimeheader` / `mb_decode_mimeheader` (RFC 2047).
//!
//! Ref: `ext/mbstring/mbstring.c` (`mb_mime_header_encode`,
//! `mb_mime_header_decode`).

use crate::convert::{ConvertBuf, ErrorMode, MIN_WCHAR_BUFSIZE};
use crate::encoding::{self, Encoding, EncodingNo};
use crate::error::{MbError, MbResult};
use crate::state::MbState;
use crate::tables::qencode::MIME_CHAR_NEEDS_QENCODE;

const BASE64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const WCHAR_BUFSIZE: usize = 90;

fn needs_qencode(c: u8) -> bool {
    c > 0x7F || c == b'=' || MIME_CHAR_NEEDS_QENCODE[usize::from(c)] != 0
}

fn transfer_encoded_size(tmp: &[u8], base64: bool) -> usize {
    if base64 {
        tmp.len().div_ceil(3) * 4
    } else {
        tmp.iter()
            .map(|&c| if needs_qencode(c) { 3 } else { 1 })
            .sum()
    }
}

fn transfer_encode(tmp: &mut ConvertBuf, out: &mut Vec<u8>, base64: bool) {
    let data = &tmp.out;
    if base64 {
        let mut chunks = data.chunks_exact(3);
        for c in &mut chunks {
            let bits = (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
            for sh in [18, 12, 6, 0] {
                out.push(BASE64_TABLE[((bits >> sh) & 0x3F) as usize]);
            }
        }
        match *chunks.remainder() {
            [a] => {
                let bits = u32::from(a);
                out.extend([
                    BASE64_TABLE[((bits >> 2) & 0x3F) as usize],
                    BASE64_TABLE[((bits & 0x3) << 4) as usize],
                    b'=',
                    b'=',
                ]);
            }
            [a, b] => {
                let bits = (u32::from(a) << 8) | u32::from(b);
                out.extend([
                    BASE64_TABLE[((bits >> 10) & 0x3F) as usize],
                    BASE64_TABLE[((bits >> 4) & 0x3F) as usize],
                    BASE64_TABLE[((bits & 0xF) << 2) as usize],
                    b'=',
                ]);
            }
            _ => {}
        }
    } else {
        for &c in data {
            if needs_qencode(c) {
                out.extend([
                    b'=',
                    b"0123456789ABCDEF"[usize::from(c >> 4)],
                    b"0123456789ABCDEF"[usize::from(c & 0xF)],
                ]);
            } else {
                out.push(c);
            }
        }
    }
    tmp.reset(0);
}

/// `mb_mime_header_encode`.
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub fn mime_header_encode(
    input: &[u8],
    incode: &'static Encoding,
    outcode: &'static Encoding,
    base64: bool,
    linefeed: &[u8],
    indent: i64,
) -> Vec<u8> {
    if input.is_empty() {
        return Vec::new();
    }
    let mut indent = if (0..74).contains(&indent) {
        indent as usize
    } else {
        0
    };
    let mut linefeed = &linefeed[..linefeed.len().min(8)];
    if let Some(z) = linefeed.iter().position(|&b| b == 0) {
        linefeed = &linefeed[..z];
    }
    let mime_name = outcode.mime_name.unwrap_or("");

    let mut state = 0u32;
    let mut wb = [0u32; WCHAR_BUFSIZE];

    // Pass-through check.
    {
        let mut rest = input;
        let mut leading = true;
        let mut passthrough = true;
        'scan: while !rest.is_empty() {
            let n = (incode.to_wchar)(&mut rest, &mut wb, &mut state);
            for &w in &wb[..n] {
                if leading {
                    if w == u32::from(b' ') {
                        continue;
                    }
                    leading = false;
                }
                if !(0x21..=0x7E).contains(&w)
                    || w == u32::from(b'=')
                    || w == u32::from(b'?')
                    || w == u32::from(b'_')
                {
                    passthrough = false;
                    break 'scan;
                }
            }
        }
        if passthrough {
            return input.to_vec();
        }
    }

    let mut rest = input;
    let mut buf: Vec<u8> = Vec::with_capacity(input.len());
    let mut offset = 0usize;
    let mut line_start = 0usize;
    let mut p;
    let mut e;

    // ASCII phase; `Some(word_start)` when switching to MIME encoding.
    let mime_from: Option<usize>;
    'ascii: loop {
        if rest.is_empty() {
            // Unreachable in practice: the passthrough check found a
            // character needing encoding, so the loop below exits first.
            return buf;
        }
        let n = (incode.to_wchar)(&mut rest, &mut wb[offset..], &mut state);
        p = 0usize;
        e = offset + n;
        let mut word_start = 0usize;
        while p < e && wb[p] == u32::from(b' ') && (p - word_start) <= 74 {
            p += 1;
        }
        let mut feed = false;
        while p < e {
            let w = wb[p];
            p += 1;
            if !(0x20..=0x7E).contains(&w)
                || w == u32::from(b'?')
                || w == u32::from(b'=')
                || w == u32::from(b'_')
                || (w == u32::from(b' ') && (p - word_start) > 74)
            {
                feed = true;
                break;
            } else if w == u32::from(b' ') {
                if buf.len() - line_start + (p - word_start) + indent > 75 {
                    buf.extend_from_slice(linefeed);
                    buf.push(b' ');
                    indent = 0;
                    line_start = buf.len();
                } else if !buf.is_empty() {
                    buf.push(b' ');
                }
                while word_start < p - 1 {
                    buf.push((wb[word_start] & 0xFF) as u8);
                    word_start += 1;
                }
                word_start += 1;
                while p < e && wb[p] == u32::from(b' ') {
                    p += 1;
                }
            }
        }
        if !feed {
            if rest.is_empty() {
                if word_start < e && !buf.is_empty() {
                    if buf.len() - line_start + (p - word_start) + indent > 74 {
                        buf.extend_from_slice(linefeed);
                        buf.push(b' ');
                    } else {
                        buf.push(b' ');
                    }
                }
                while word_start < e {
                    buf.push((wb[word_start] & 0xFF) as u8);
                    word_start += 1;
                }
                return buf;
            } else if word_start < MIN_WCHAR_BUFSIZE {
                feed = true;
            } else {
                offset = e - word_start;
                wb.copy_within(word_start..e, 0);
                continue 'ascii;
            }
        }
        // feed_and_mime_encode:
        debug_assert!(feed);
        if buf.len() - line_start + indent + mime_name.len() > 55 {
            buf.extend_from_slice(linefeed);
            buf.push(b' ');
            indent = 0;
            line_start = buf.len();
        } else if !buf.is_empty() {
            buf.push(b' ');
        }
        mime_from = Some(word_start);
        break;
    }

    // mime_encoding_needed:
    let mut p = mime_from.unwrap_or(0);
    let mut tmp = ConvertBuf::new(rest.len(), u32::from(b'?'), ErrorMode::Char);
    offset = e - p;
    let mut need_refill = WCHAR_BUFSIZE - offset >= MIN_WCHAR_BUFSIZE;
    if need_refill {
        wb.copy_within(p..e, 0);
    }
    loop {
        if need_refill {
            let n = (incode.to_wchar)(&mut rest, &mut wb[offset..], &mut state);
            p = 0;
            e = offset + n;
        }
        need_refill = false;
        // start_new_line:
        buf.extend_from_slice(b"=?");
        buf.extend_from_slice(mime_name.as_bytes());
        buf.extend_from_slice(if base64 { b"?B?" } else { b"?Q?" });
        let mut n: usize = 12;
        let space_available = 73usize
            .wrapping_sub(indent)
            .wrapping_sub(buf.len() - line_start);
        loop {
            let tmppos = tmp.len();
            let tmpstate = tmp.state;
            n = n.min(e - p);
            (outcode.from_wchar)(&wb[p..p + n], &mut tmp, false);
            let tmppos2 = tmp.len();
            let tmpstate2 = tmp.state;
            (outcode.from_wchar)(&[], &mut tmp, true);
            if transfer_encoded_size(&tmp.out, base64) <= space_available || (n == 1 && tmppos == 0)
            {
                p += n;
                if p == e {
                    transfer_encode(&mut tmp, &mut buf, base64);
                    buf.extend_from_slice(b"?=");
                    return buf;
                }
                tmp.reset(tmppos2);
                tmp.state = tmpstate2;
            } else {
                tmp.reset(tmppos);
                tmp.state = tmpstate;
                if n == 1 {
                    (outcode.from_wchar)(&[], &mut tmp, true);
                    transfer_encode(&mut tmp, &mut buf, base64);
                    tmp.state = 0;
                    buf.extend_from_slice(b"?=");
                    indent = 0;
                    if !rest.is_empty() || p < e {
                        buf.extend_from_slice(linefeed);
                        buf.push(b' ');
                        line_start = buf.len();
                        offset = e - p;
                        if !rest.is_empty() && WCHAR_BUFSIZE - offset >= MIN_WCHAR_BUFSIZE {
                            wb.copy_within(p..e, 0);
                            need_refill = true;
                        }
                        break;
                    }
                    return buf;
                }
                n = (n >> 1).max(1);
            }
        }
    }
}

fn decode_base64(c: u8) -> Option<u32> {
    Some(u32::from(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    }))
}

fn qprint_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'F' => Some(c - b'A' + 10),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

/// `mime_header_decode_encoded_word`: `s[p..]` starts with `=?`; returns the
/// position after the word, or `None` if it is not a valid encoded word.
#[allow(clippy::cast_possible_truncation, clippy::many_single_char_names)]
fn decode_encoded_word(
    s: &[u8],
    p: usize,
    outcode: &'static Encoding,
    out: &mut ConvertBuf,
    state: &mut u32,
) -> Option<usize> {
    let mut e = s.len();
    if e - p < 6 {
        return None;
    }
    let charset = p + 2;
    let charset_end = charset + s[charset..e].iter().position(|&b| b == b'?')?;
    let encoding = charset_end + 1;
    let mut q = encoding + 1;
    if q >= e || s[q] != b'?' {
        return None;
    }
    q += 1;
    let incode = encoding::by_cname(&s[charset..charset_end])?;
    if let Some(m) = s[q..e].windows(2).position(|w| w == b"?=") {
        e = q + m;
    } else if q < e && s[e - 1] == b'?' {
        e -= 1;
    }
    let mut bytes = Vec::with_capacity(e.saturating_sub(q));
    match s[encoding] {
        b'Q' | b'q' => {
            while q < e {
                let c = s[q];
                q += 1;
                if c == b'_' {
                    bytes.push(b' ');
                    continue;
                } else if c == b'=' && e - q >= 2 {
                    let (c2, c3) = (s[q], s[q + 1]);
                    q += 2;
                    if let (Some(h), Some(l)) = (qprint_val(c2), qprint_val(c3)) {
                        bytes.push((h << 4) | l);
                        continue;
                    } else if c2 == b'\r' {
                        if c3 != b'\n' {
                            q -= 1;
                        }
                        continue;
                    } else if c2 == b'\n' {
                        q -= 1;
                        continue;
                    }
                }
                bytes.push(c);
            }
        }
        b'B' | b'b' => {
            let (mut bits, mut cache) = (0u32, 0u32);
            while q < e {
                let c = s[q];
                q += 1;
                if matches!(c, b'\r' | b'\n' | b' ' | b'\t' | b'=') {
                    continue;
                }
                let Some(d) = decode_base64(c) else {
                    bytes.push(b'?');
                    continue;
                };
                bits += 6;
                cache = (cache << 6) | (d & 0x3F);
                if bits == 24 {
                    bytes.extend([(cache >> 16) as u8, (cache >> 8) as u8, cache as u8]);
                    bits = 0;
                    cache = 0;
                }
            }
            if bits == 18 {
                bytes.extend([(cache >> 10) as u8, (cache >> 2) as u8]);
            } else if bits == 12 {
                bytes.push((cache >> 4) as u8);
            }
        }
        _ => return None,
    }
    let mut wb = [0u32; 128];
    let mut rest = &bytes[..];
    while !rest.is_empty() {
        let n = (incode.to_wchar)(&mut rest, &mut wb, state);
        (outcode.from_wchar)(&wb[..n], out, false);
    }
    Some(e + 2)
}

fn is_ws(c: u8) -> bool {
    matches!(c, b'\n' | b'\r' | b'\t' | b' ')
}

/// `mb_mime_header_decode`.
#[allow(clippy::many_single_char_names)]
pub fn mime_header_decode(input: &[u8], outcode: &'static Encoding) -> Vec<u8> {
    let s = input;
    let e = s.len();
    let mut p = 0usize;
    let mut state = 0u32;
    let mut space_pending = false;
    let mut buf = ConvertBuf::new(s.len(), u32::from(b'?'), ErrorMode::Char);
    let ascii = encoding::get(EncodingNo::Ascii);
    while p < e {
        let c = s[p];
        if c == b'=' && s.get(p + 1) == Some(&b'?') && e - p >= 6 {
            if let Some(ie) = s[p + 2..].iter().position(|&b| b == b'?') {
                let incode_end = p + 2 + ie;
                if e - incode_end >= 3 {
                    if let Some(np) = decode_encoded_word(s, p, outcode, &mut buf, &mut state) {
                        p = np;
                        if p < e && is_ws(s[p]) {
                            while p < e && is_ws(s[p]) {
                                p += 1;
                            }
                            space_pending = true;
                        } else {
                            space_pending = false;
                        }
                        continue;
                    }
                }
            }
        }
        if space_pending {
            (outcode.from_wchar)(&[u32::from(b' ')], &mut buf, false);
            space_pending = false;
        }
        if c != b'\n' && c != b'\r' {
            let mut end = p + 1;
            while end < e && !matches!(s[end], b'=' | b'\n' | b'\r') {
                end += 1;
            }
            let mut wb = [0u32; 128];
            let mut rest = &s[p..end];
            while !rest.is_empty() {
                let n = (ascii.to_wchar)(&mut rest, &mut wb, &mut state);
                (outcode.from_wchar)(&wb[..n], &mut buf, false);
            }
            p = end;
        }
        if p < e && (s[p] == b'\n' || s[p] == b'\r') {
            p += 1;
            while p < e && is_ws(s[p]) {
                p += 1;
            }
            if p < e {
                (outcode.from_wchar)(&[u32::from(b' ')], &mut buf, false);
            }
        }
    }
    (outcode.from_wchar)(&[], &mut buf, true);
    buf.into_bytes()
}

impl MbState {
    /// `mb_encode_mimeheader($string, $charset, $transfer_encoding,
    /// $newline, $indent)`.
    pub fn mb_encode_mimeheader(
        &mut self,
        s: &[u8],
        charset: Option<&[u8]>,
        transfer_encoding: Option<&[u8]>,
        newline: &[u8],
        indent: i64,
    ) -> MbResult<Vec<u8>> {
        let mut base64 = true;
        let charset_enc = if let Some(name) = charset {
            let enc = self.get_encoding(Some(name), 2, "charset")?;
            if enc.preferred_mime_name().is_none() || enc.no == EncodingNo::Qprint {
                return Err(MbError::arg_value(
                    2,
                    "charset",
                    format!(
                        "\"{}\" cannot be used for MIME header encoding",
                        String::from_utf8_lossy(name)
                    ),
                ));
            }
            enc
        } else {
            let lang = self.language();
            let t = encoding::get(lang.mail_header_encoding).name.as_bytes()[0];
            if t == b'Q' || t == b'q' {
                base64 = false;
            }
            encoding::get(lang.mail_charset)
        };
        if let Some(t) = transfer_encoding.and_then(|t| t.first()) {
            if *t == b'Q' || *t == b'q' {
                base64 = false;
            }
        }
        Ok(mime_header_encode(
            s,
            self.internal_encoding(),
            charset_enc,
            base64,
            newline,
            indent,
        ))
    }

    /// `mb_decode_mimeheader($string)`.
    pub fn mb_decode_mimeheader(&mut self, s: &[u8]) -> Vec<u8> {
        mime_header_decode(s, self.internal_encoding())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_and_decode() {
        let mut st = MbState::new();
        let enc = st
            .mb_encode_mimeheader(
                "Привет мир".as_bytes(),
                Some(b"UTF-8"),
                Some(b"B"),
                b"\r\n",
                0,
            )
            .unwrap();
        assert_eq!(enc, b"=?UTF-8?B?0J/RgNC40LLQtdGCINC80LjRgA==?=");
        assert_eq!(st.mb_decode_mimeheader(&enc), "Привет мир".as_bytes());
        assert_eq!(
            st.mb_encode_mimeheader(b"plain ascii", None, None, b"\r\n", 0)
                .unwrap(),
            b"plain ascii"
        );
    }
}
