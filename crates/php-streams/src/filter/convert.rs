// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `convert.*` filters: `convert.base64-encode`, `convert.base64-decode`,
//! `convert.quoted-printable-encode` and `convert.quoted-printable-decode`
//! (`ext/standard/filters.c`).
//!
//! The converters are faithful ports of the `php_conv` state machines,
//! including their bounded-output protocol (`PHP_CONV_ERR_TOO_BIG` makes the
//! driver grow the output buffer and call again), so their observable
//! behaviour — output bytes, bucket boundaries, warnings — matches PHP.

use super::{
    parse_write_seek_mode, Brigade, Bucket, FilterEnv, FilterFactory, FilterFlags, FilterStatus,
    Seekable, StreamFilter,
};
use crate::diag;
use crate::value::Value;

/// `php_conv_err_t` (only the values the bundled converters produce).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConvErr {
    Success,
    Unknown,
    TooBig,
    InvalidSeq,
    UnexpectedEos,
}

const B64_ENC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `b64_tbl_enc[(unsigned char)x]`: the C table repeats the alphabet four
/// times, so only the low six bits matter.
fn enc(x: u8) -> u8 {
    B64_ENC[usize::from(x & 0x3f)]
}

/// `b64_tbl_dec`: 0–63 for alphabet bytes, 128 for `=`, 64 otherwise.
fn dec(c: u8) -> u32 {
    match c {
        b'A'..=b'Z' => u32::from(c - b'A'),
        b'a'..=b'z' => u32::from(c - b'a') + 26,
        b'0'..=b'9' => u32::from(c - b'0') + 52,
        b'+' => 62,
        b'/' => 63,
        b'=' => 128,
        _ => 64,
    }
}

/// `bmask(a)`.
fn bmask(a: u32) -> u32 {
    if a >= 16 {
        0xffff
    } else {
        0xffff >> (16 - a)
    }
}

/// Appends `bytes` to the output and charges them to `ocnt`.
fn put(out: &mut Vec<u8>, ocnt: &mut usize, bytes: &[u8]) {
    out.extend_from_slice(bytes);
    *ocnt -= bytes.len();
}

/// `php_conv_base64_encode`.
#[derive(Debug, Clone)]
struct Base64Encode {
    lbchars: Option<Vec<u8>>,
    erem_len: usize,
    line_ccnt: u32,
    line_len: u32,
    erem: [u8; 3],
}

impl Base64Encode {
    fn new(line_len: u32, lbchars: Option<Vec<u8>>) -> Self {
        Base64Encode {
            lbchars,
            erem_len: 0,
            line_ccnt: line_len,
            line_len,
            erem: [0; 3],
        }
    }

    fn reset(&mut self) {
        self.erem_len = 0;
        self.line_ccnt = self.line_len;
    }

    /// Emits the line break if the line is full; `false` means "too big".
    fn line_break(&self, line_ccnt: &mut u32, out: &mut Vec<u8>, ocnt: &mut usize) -> bool {
        if let Some(lb) = &self.lbchars {
            if *line_ccnt < 4 {
                if *ocnt < lb.len() {
                    return false;
                }
                put(out, ocnt, lb);
                *line_ccnt = self.line_len;
            }
        }
        true
    }

    fn flush(&mut self, out: &mut Vec<u8>, ocnt: &mut usize) -> ConvErr {
        let mut line_ccnt = self.line_ccnt;
        let err = match self.erem_len {
            0 => ConvErr::Success,
            1 | 2 => {
                if !self.line_break(&mut line_ccnt, out, ocnt) {
                    return ConvErr::TooBig;
                }
                if *ocnt < 4 {
                    ConvErr::TooBig
                } else {
                    let e0 = self.erem[0];
                    let quad = if self.erem_len == 1 {
                        [enc(e0 >> 2), enc(e0 << 4), b'=', b'=']
                    } else {
                        let e1 = self.erem[1];
                        [enc(e0 >> 2), enc((e0 << 4) | (e1 >> 4)), enc(e1 << 2), b'=']
                    };
                    put(out, ocnt, &quad);
                    self.erem_len = 0;
                    line_ccnt = line_ccnt.wrapping_sub(4);
                    ConvErr::Success
                }
            }
            _ => ConvErr::Unknown,
        };
        self.line_ccnt = line_ccnt;
        err
    }

    fn convert(
        &mut self,
        input: Option<&mut &[u8]>,
        out: &mut Vec<u8>,
        ocnt: &mut usize,
    ) -> ConvErr {
        let Some(input) = input else {
            return self.flush(out, ocnt);
        };
        let s: &[u8] = input;
        let mut ps = 0usize;
        let mut line_ccnt = self.line_ccnt;
        let mut err = ConvErr::Success;

        // consume the remainder first
        match self.erem_len {
            1 if s.len() >= 2 => {
                if !self.line_break(&mut line_ccnt, out, ocnt) {
                    return ConvErr::TooBig;
                }
                if *ocnt < 4 {
                    err = ConvErr::TooBig;
                } else {
                    let e0 = self.erem[0];
                    put(
                        out,
                        ocnt,
                        &[
                            enc(e0 >> 2),
                            enc((e0 << 4) | (s[0] >> 4)),
                            enc((s[0] << 2) | (s[1] >> 6)),
                            enc(s[1]),
                        ],
                    );
                    ps = 2;
                    self.erem_len = 0;
                    line_ccnt = line_ccnt.wrapping_sub(4);
                }
            }
            2 if !s.is_empty() => {
                if !self.line_break(&mut line_ccnt, out, ocnt) {
                    return ConvErr::TooBig;
                }
                if *ocnt < 4 {
                    err = ConvErr::TooBig;
                } else {
                    let (e0, e1) = (self.erem[0], self.erem[1]);
                    put(
                        out,
                        ocnt,
                        &[
                            enc(e0 >> 2),
                            enc((e0 << 4) | (e1 >> 4)),
                            enc((e1 << 2) | (s[0] >> 6)),
                            enc(s[0]),
                        ],
                    );
                    ps = 1;
                    self.erem_len = 0;
                    line_ccnt = line_ccnt.wrapping_sub(4);
                }
            }
            _ => {}
        }

        if err == ConvErr::Success {
            while s.len() - ps >= 3 {
                if !self.line_break(&mut line_ccnt, out, ocnt) {
                    err = ConvErr::TooBig;
                    break;
                }
                if *ocnt < 4 {
                    err = ConvErr::TooBig;
                    break;
                }
                let (a, b, c) = (s[ps], s[ps + 1], s[ps + 2]);
                put(
                    out,
                    ocnt,
                    &[
                        enc(a >> 2),
                        enc((a << 4) | (b >> 4)),
                        enc((b << 2) | (c >> 6)),
                        enc(c),
                    ],
                );
                ps += 3;
                line_ccnt = line_ccnt.wrapping_sub(4);
            }
            if err == ConvErr::Success {
                while ps < s.len() {
                    self.erem[self.erem_len] = s[ps];
                    self.erem_len += 1;
                    ps += 1;
                }
            }
        }

        *input = &s[ps..];
        self.line_ccnt = line_ccnt;
        err
    }
}

/// `php_conv_base64_decode`.
#[derive(Debug, Clone, Default)]
struct Base64Decode {
    urem: u32,
    urem_nbits: u32,
    ustat: u32,
    eos: bool,
}

impl Base64Decode {
    fn reset(&mut self) {
        *self = Base64Decode::default();
    }

    fn convert(
        &mut self,
        input: Option<&mut &[u8]>,
        out: &mut Vec<u8>,
        ocnt: &mut usize,
    ) -> ConvErr {
        const NBITSOF_PACK: u32 = 8;
        let Some(input) = input else {
            if self.eos || self.urem_nbits == 0 {
                return ConvErr::Success;
            }
            return ConvErr::UnexpectedEos;
        };
        let s: &[u8] = input;
        let mut ps = 0usize;
        let mut err = ConvErr::Success;
        let mut urem = self.urem;
        let mut urem_nbits = self.urem_nbits;
        let mut ustat = self.ustat;
        let mut pack: u32 = 0;
        let mut pack_bcnt: u32 = NBITSOF_PACK;

        loop {
            if pack_bcnt >= urem_nbits {
                pack_bcnt -= urem_nbits;
                pack |= urem.wrapping_shl(pack_bcnt);
                urem_nbits = 0;
            } else {
                urem_nbits -= pack_bcnt;
                pack |= urem >> urem_nbits;
                urem &= bmask(urem_nbits);
                pack_bcnt = 0;
            }
            if pack_bcnt > 0 {
                if ps >= s.len() {
                    break;
                }
                let i = dec(s[ps]);
                ps += 1;
                ustat |= i & 0x80;

                if i & 0xc0 == 0 {
                    if ustat != 0 {
                        err = ConvErr::InvalidSeq;
                        break;
                    }
                    if 6 <= pack_bcnt {
                        pack_bcnt -= 6;
                        pack |= i << pack_bcnt;
                        urem = 0;
                    } else {
                        urem_nbits = 6 - pack_bcnt;
                        pack |= i >> urem_nbits;
                        urem = i & bmask(urem_nbits);
                        pack_bcnt = 0;
                    }
                } else if ustat != 0 {
                    if pack_bcnt == 8 || pack_bcnt == 2 {
                        err = ConvErr::InvalidSeq;
                        break;
                    }
                    self.eos = true;
                }
            }
            if (pack_bcnt | ustat) == 0 {
                if *ocnt < 1 {
                    err = ConvErr::TooBig;
                    break;
                }
                #[allow(clippy::cast_possible_truncation)]
                put(out, ocnt, &[pack as u8]);
                pack = 0;
                pack_bcnt = NBITSOF_PACK;
            }
        }

        if urem_nbits >= pack_bcnt {
            urem |= pack.wrapping_shl(urem_nbits - pack_bcnt);
        } else {
            urem |= pack >> (pack_bcnt - urem_nbits);
        }
        urem_nbits += NBITSOF_PACK - pack_bcnt;

        self.urem = urem;
        self.urem_nbits = urem_nbits;
        self.ustat = ustat;
        *input = &s[ps..];
        err
    }
}

/// `PHP_CONV_QPRINT_OPT_BINARY`.
const QP_OPT_BINARY: u32 = 1;
/// `PHP_CONV_QPRINT_OPT_FORCE_ENCODE_FIRST`.
const QP_OPT_FORCE_ENCODE_FIRST: u32 = 2;

const QP_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

/// `php_conv_qprint_encode`.
#[derive(Debug, Clone)]
struct QprintEncode {
    lbchars: Option<Vec<u8>>,
    opts: u32,
    line_ccnt: u32,
    line_len: u32,
    lb_ptr: usize,
    lb_cnt: usize,
}

impl QprintEncode {
    fn new(line_len: u32, lbchars: Option<Vec<u8>>, opts: u32) -> Self {
        QprintEncode {
            lbchars,
            opts,
            line_ccnt: line_len,
            line_len,
            lb_ptr: 0,
            lb_cnt: 0,
        }
    }

    fn reset(&mut self) {
        self.line_ccnt = self.line_len;
        self.lb_ptr = 0;
        self.lb_cnt = 0;
    }

    /// Emits a soft line break (`=` + line break chars). `false` means
    /// "too big".
    fn soft_break(&self, line_ccnt: &mut u32, out: &mut Vec<u8>, ocnt: &mut usize) -> bool {
        let Some(lb) = &self.lbchars else {
            return true;
        };
        if *ocnt < lb.len() + 1 {
            return false;
        }
        put(out, ocnt, b"=");
        put(out, ocnt, lb);
        *line_ccnt = self.line_len;
        true
    }

    #[allow(clippy::too_many_lines)]
    fn convert(
        &mut self,
        input: Option<&mut &[u8]>,
        out: &mut Vec<u8>,
        ocnt: &mut usize,
    ) -> ConvErr {
        let mut line_ccnt = self.line_ccnt;
        let opts = self.opts;
        let mut lb_ptr = self.lb_ptr;
        let mut lb_cnt = self.lb_cnt;

        let Some(input) = input else {
            return ConvErr::Success;
        };
        let s: &[u8] = input;
        let mut ps = 0usize;
        let mut icnt = s.len();
        let mut trail_ws: u32 = 0;
        let mut err = ConvErr::Success;
        let binary = opts & QP_OPT_BINARY != 0;
        let lb: Option<&[u8]> = self.lbchars.as_deref();
        // C reads lbchars[i] of a NUL-terminated copy; out-of-range reads see 0.
        let lb_at = |i: usize| lb.and_then(|l| l.get(i).copied()).unwrap_or(0);

        macro_rules! consume {
            () => {
                if lb_ptr < lb_cnt {
                    lb_ptr += 1;
                } else {
                    lb_cnt = 0;
                    lb_ptr = 0;
                    icnt -= 1;
                    ps += 1;
                }
            };
        }

        loop {
            if !binary {
                if let Some(lbc) = lb.filter(|l| !l.is_empty()) {
                    // look ahead for the line break chars
                    if icnt > 0 && s[ps] == lbc[lb_cnt] {
                        lb_cnt += 1;
                        if lb_cnt >= lbc.len() {
                            if *ocnt < lb_cnt {
                                lb_cnt -= 1;
                                err = ConvErr::TooBig;
                                break;
                            }
                            put(out, ocnt, &lbc[..lb_cnt]);
                            line_ccnt = self.line_len;
                            lb_ptr = 0;
                            lb_cnt = 0;
                        }
                        ps += 1;
                        icnt -= 1;
                        continue;
                    }
                }
            }

            if lb_ptr >= lb_cnt && icnt == 0 {
                break;
            }

            let c = if lb_ptr < lb_cnt {
                lb_at(lb_ptr)
            } else {
                s[ps]
            };

            if !binary && trail_ws == 0 && (c == b'\t' || c == b' ') {
                if line_ccnt < 2 && lb.is_some() {
                    if !self.soft_break(&mut line_ccnt, out, ocnt) {
                        err = ConvErr::TooBig;
                        break;
                    }
                } else {
                    if *ocnt < 1 {
                        err = ConvErr::TooBig;
                        break;
                    }
                    // Check to see if this is EOL whitespace.
                    if lb.is_some() {
                        let mut lb_cnt2 = 0usize;
                        let mut ps2 = ps;
                        trail_ws = 1;
                        let mut j = icnt.wrapping_sub(1);
                        while j > 0 && ps2 < s.len() {
                            let ch = s[ps2];
                            if ch == lb_at(lb_cnt2) {
                                lb_cnt2 += 1;
                                if lb_cnt2 >= lb.map_or(0, <[u8]>::len) {
                                    break;
                                }
                            } else if lb_cnt2 != 0 || (ch != b'\t' && ch != b' ') {
                                trail_ws = 0;
                                break;
                            } else {
                                trail_ws += 1;
                            }
                            j -= 1;
                            ps2 += 1;
                        }
                    }
                    if trail_ws == 0 {
                        put(out, ocnt, &[c]);
                        line_ccnt = line_ccnt.wrapping_sub(1);
                        consume!();
                    }
                }
            } else if (opts & QP_OPT_FORCE_ENCODE_FIRST == 0 || line_ccnt < self.line_len)
                && ((33..=60).contains(&c) || (62..=126).contains(&c))
            {
                if line_ccnt < 2 && lb.is_some() && !self.soft_break(&mut line_ccnt, out, ocnt) {
                    err = ConvErr::TooBig;
                    break;
                }
                if *ocnt < 1 {
                    err = ConvErr::TooBig;
                    break;
                }
                put(out, ocnt, &[c]);
                line_ccnt = line_ccnt.wrapping_sub(1);
                consume!();
            } else {
                if line_ccnt < 4 && lb.is_some() && !self.soft_break(&mut line_ccnt, out, ocnt) {
                    err = ConvErr::TooBig;
                    break;
                }
                if *ocnt < 3 {
                    err = ConvErr::TooBig;
                    break;
                }
                put(
                    out,
                    ocnt,
                    &[
                        b'=',
                        QP_DIGITS[usize::from(c >> 4)],
                        QP_DIGITS[usize::from(c & 0x0f)],
                    ],
                );
                line_ccnt = line_ccnt.wrapping_sub(3);
                trail_ws = trail_ws.saturating_sub(1);
                consume!();
            }
        }

        *input = &s[ps..];
        self.line_ccnt = line_ccnt;
        self.lb_ptr = lb_ptr;
        self.lb_cnt = lb_cnt;
        err
    }
}

/// `php_conv_qprint_decode`.
#[derive(Debug, Clone)]
struct QprintDecode {
    lbchars: Option<Vec<u8>>,
    scan_stat: u32,
    next_char: u32,
    lb_ptr: usize,
    lb_cnt: usize,
}

impl QprintDecode {
    fn new(lbchars: Option<Vec<u8>>) -> Self {
        QprintDecode {
            lbchars,
            scan_stat: 0,
            next_char: 0,
            lb_ptr: 0,
            lb_cnt: 0,
        }
    }

    fn reset(&mut self) {
        self.scan_stat = 0;
        self.next_char = 0;
        self.lb_ptr = 0;
        self.lb_cnt = 0;
    }

    #[allow(clippy::too_many_lines)]
    fn convert(
        &mut self,
        input: Option<&mut &[u8]>,
        out: &mut Vec<u8>,
        ocnt: &mut usize,
    ) -> ConvErr {
        let mut lb_ptr = self.lb_ptr;
        let mut lb_cnt = self.lb_cnt;

        let Some(input) = input else {
            if self.scan_stat != 0 {
                return ConvErr::UnexpectedEos;
            }
            return ConvErr::Success;
        };
        let s: &[u8] = input;
        let mut ps = 0usize;
        let mut err = ConvErr::Success;
        let mut scan_stat = self.scan_stat;
        let mut next_char = self.next_char;
        let has_lb = self.lbchars.is_some();
        let lb: &[u8] = self.lbchars.as_deref().unwrap_or(&[]);

        'outer: loop {
            let mut case_ = scan_stat;
            loop {
                match case_ {
                    0 => {
                        if ps >= s.len() {
                            break 'outer;
                        }
                        if s[ps] == b'=' {
                            scan_stat = 1;
                        } else {
                            if *ocnt < 1 {
                                err = ConvErr::TooBig;
                                break 'outer;
                            }
                            put(out, ocnt, &[s[ps]]);
                        }
                        ps += 1;
                        continue 'outer;
                    }
                    1 => {
                        if ps >= s.len() {
                            break 'outer;
                        }
                        let ch = s[ps];
                        if ch == b' ' || ch == b'\t' {
                            scan_stat = 4;
                            ps += 1;
                            continue 'outer;
                        } else if !has_lb && lb_cnt == 0 && ch == b'\r' {
                            // auto-detect line endings: \r\n (or mac \r)
                            lb_cnt += 1;
                            scan_stat = 5;
                            ps += 1;
                            continue 'outer;
                        } else if !has_lb && lb_cnt == 0 && ch == b'\n' {
                            // auto-detect line endings: unix \n
                            lb_cnt = 0;
                            lb_ptr = 0;
                            scan_stat = 0;
                            ps += 1;
                            continue 'outer;
                        } else if lb_cnt < lb.len() && ch == lb[lb_cnt] {
                            lb_cnt += 1;
                            scan_stat = 5;
                            ps += 1;
                            continue 'outer;
                        }
                        case_ = 2;
                    }
                    2 => {
                        if ps >= s.len() {
                            break 'outer;
                        }
                        let ch = s[ps];
                        if !ch.is_ascii_hexdigit() {
                            err = ConvErr::InvalidSeq;
                            break 'outer;
                        }
                        let v = if ch >= b'a' {
                            ch - 0x57
                        } else if ch >= b'A' {
                            ch - 0x37
                        } else {
                            ch - 0x30
                        };
                        next_char = next_char.wrapping_shl(4) | u32::from(v);
                        scan_stat += 1;
                        ps += 1;
                        if scan_stat != 3 {
                            continue 'outer;
                        }
                        case_ = 3;
                    }
                    3 => {
                        if *ocnt < 1 {
                            err = ConvErr::TooBig;
                            break 'outer;
                        }
                        #[allow(clippy::cast_possible_truncation)]
                        put(out, ocnt, &[next_char as u8]);
                        scan_stat = 0;
                        continue 'outer;
                    }
                    4 => {
                        if ps >= s.len() {
                            break 'outer;
                        }
                        let ch = s[ps];
                        if lb_cnt < lb.len() && ch == lb[lb_cnt] {
                            lb_cnt += 1;
                            scan_stat = 5;
                        } else if ch != b'\t' && ch != b' ' {
                            err = ConvErr::InvalidSeq;
                            break 'outer;
                        }
                        ps += 1;
                        continue 'outer;
                    }
                    5 => {
                        if ps >= s.len() {
                            break 'outer;
                        }
                        let ch = s[ps];
                        if !has_lb && lb_cnt == 1 && ch == b'\n' {
                            // soft line break with network line ending
                            lb_cnt = 0;
                            lb_ptr = 0;
                            scan_stat = 0;
                            ps += 1;
                        } else if (!has_lb && lb_cnt > 0) || lb_cnt >= lb.len() {
                            // mac line ending / complete soft line break
                            lb_cnt = 0;
                            lb_ptr = 0;
                            scan_stat = 0;
                        } else if ch == lb[lb_cnt] {
                            lb_cnt += 1;
                            ps += 1;
                        } else {
                            scan_stat = 6;
                        }
                        continue 'outer;
                    }
                    _ => {
                        // 6: replay the partially matched line break chars
                        if lb_ptr < lb_cnt {
                            if *ocnt < 1 {
                                err = ConvErr::TooBig;
                                break 'outer;
                            }
                            put(out, ocnt, &[lb[lb_ptr]]);
                            lb_ptr += 1;
                        } else {
                            scan_stat = 0;
                            lb_cnt = 0;
                            lb_ptr = 0;
                        }
                        continue 'outer;
                    }
                }
            }
        }

        *input = &s[ps..];
        self.scan_stat = scan_stat;
        self.lb_ptr = lb_ptr;
        self.lb_cnt = lb_cnt;
        self.next_char = next_char;
        err
    }
}

/// The converter behind a `convert.*` filter.
#[derive(Debug, Clone)]
enum Conv {
    Base64Encode(Base64Encode),
    Base64Decode(Base64Decode),
    QprintEncode(QprintEncode),
    QprintDecode(QprintDecode),
}

impl Conv {
    fn convert(
        &mut self,
        input: Option<&mut &[u8]>,
        out: &mut Vec<u8>,
        ocnt: &mut usize,
    ) -> ConvErr {
        match self {
            Conv::Base64Encode(c) => c.convert(input, out, ocnt),
            Conv::Base64Decode(c) => c.convert(input, out, ocnt),
            Conv::QprintEncode(c) => c.convert(input, out, ocnt),
            Conv::QprintDecode(c) => c.convert(input, out, ocnt),
        }
    }

    fn reset(&mut self) {
        match self {
            Conv::Base64Encode(c) => c.reset(),
            Conv::Base64Decode(c) => c.reset(),
            Conv::QprintEncode(c) => c.reset(),
            Conv::QprintDecode(c) => c.reset(),
        }
    }
}

/// `php_conv_get_string_prop_ex`.
fn string_prop(options: &Value, name: &str) -> Option<Vec<u8>> {
    options.get(name).map(|v| v.to_bytes().unwrap_or_default())
}

/// `php_conv_get_uint_prop_ex`: negative values become 0, then the value is
/// truncated to `unsigned int`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn uint_prop(options: &Value, name: &str) -> u32 {
    options
        .get(name)
        .map_or(0, |v| v.to_long().max(0) as u64 as u32)
}

/// `php_conv_get_bool_prop_ex`.
fn bool_prop(options: &Value, name: &str) -> bool {
    options.get(name).is_some_and(Value::is_true)
}

/// Line length and line break chars as resolved by `php_conv_open()`.
fn line_options(options: Option<&Value>) -> (u32, Option<Vec<u8>>) {
    let Some(options) = options else {
        return (0, None);
    };
    let lbchars = string_prop(options, "line-break-chars");
    let line_len = uint_prop(options, "line-length");
    if line_len < 4 {
        (0, None)
    } else {
        (line_len, Some(lbchars.unwrap_or_else(|| b"\r\n".to_vec())))
    }
}

/// The conversion a `convert.*` filter performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertMode {
    /// `convert.base64-encode`.
    Base64Encode,
    /// `convert.base64-decode`.
    Base64Decode,
    /// `convert.quoted-printable-encode`.
    QuotedPrintableEncode,
    /// `convert.quoted-printable-decode`.
    QuotedPrintableDecode,
}

impl ConvertMode {
    /// Parses the part after `convert.` (case-insensitively).
    pub fn from_name(s: &str) -> Option<Self> {
        let s = s.to_ascii_lowercase();
        Some(match s.as_str() {
            "base64-encode" => ConvertMode::Base64Encode,
            "base64-decode" => ConvertMode::Base64Decode,
            "quoted-printable-encode" => ConvertMode::QuotedPrintableEncode,
            "quoted-printable-decode" => ConvertMode::QuotedPrintableDecode,
            _ => return None,
        })
    }
}

/// `php_conv_open()`.
fn conv_open(mode: ConvertMode, options: Option<&Value>) -> Conv {
    match mode {
        ConvertMode::Base64Encode => {
            let (line_len, lbchars) = line_options(options);
            Conv::Base64Encode(Base64Encode::new(line_len, lbchars))
        }
        ConvertMode::Base64Decode => Conv::Base64Decode(Base64Decode::default()),
        ConvertMode::QuotedPrintableEncode => {
            let (line_len, lbchars) = line_options(options);
            let mut opts = 0;
            if let Some(o) = options {
                if bool_prop(o, "binary") {
                    opts |= QP_OPT_BINARY;
                }
                if bool_prop(o, "force-encode-first") {
                    opts |= QP_OPT_FORCE_ENCODE_FIRST;
                }
            }
            Conv::QprintEncode(QprintEncode::new(line_len, lbchars, opts))
        }
        ConvertMode::QuotedPrintableDecode => {
            let lbchars = options.and_then(|o| string_prop(o, "line-break-chars"));
            Conv::QprintDecode(QprintDecode::new(lbchars))
        }
    }
}

/// A `convert.*` filter instance (`php_convert_filter`).
#[derive(Debug, Clone)]
pub struct ConvertFilter {
    conv: Conv,
    filtername: String,
    write_seekable: Seekable,
}

impl ConvertFilter {
    /// Creates a converter filter. `filtername` is used in warnings;
    /// `options` is the (array) filter parameter.
    pub fn new(mode: ConvertMode, filtername: &str, options: Option<&Value>) -> Self {
        ConvertFilter {
            conv: conv_open(mode, options),
            filtername: filtername.to_string(),
            write_seekable: Seekable::Always,
        }
    }

    fn warn(&self, what: &str) {
        diag::warning(format!("Stream filter ({}): {what}", self.filtername));
    }

    /// `strfilter_convert_append_bucket()`: converts one input bucket (or
    /// flushes the converter when `input` is `None`) and appends the output
    /// as a single bucket. Returns `false` on failure (after warning).
    fn append_bucket(
        &mut self,
        input: Option<&[u8]>,
        out: &mut Brigade,
        consumed: &mut usize,
    ) -> bool {
        let buf_len = input.map_or(0, <[u8]>::len);
        let initial = if input.is_some() { buf_len } else { 64 };
        let mut icnt = if input.is_some() { buf_len } else { 1 };
        let mut out_buf_size = initial;
        let mut ocnt = initial;
        let mut out_buf = Vec::with_capacity(initial);

        while icnt > 0 {
            let err = match input {
                None => self.conv.convert(None, &mut out_buf, &mut ocnt),
                Some(data) => {
                    let mut rest = &data[buf_len - icnt..];
                    let e = self.conv.convert(Some(&mut rest), &mut out_buf, &mut ocnt);
                    icnt = rest.len();
                    e
                }
            };
            match err {
                ConvErr::InvalidSeq => {
                    self.warn("invalid byte sequence");
                    return false;
                }
                ConvErr::Unknown => {
                    self.warn("unknown error");
                    return false;
                }
                ConvErr::TooBig => {
                    // grow the output buffer and go again
                    ocnt += out_buf_size;
                    out_buf_size <<= 1;
                }
                ConvErr::Success | ConvErr::UnexpectedEos => {
                    if input.is_none() {
                        icnt = 0;
                    }
                }
            }
        }

        if !out_buf.is_empty() {
            out.push_back(Bucket::new(out_buf));
        }
        *consumed += buf_len - icnt;
        true
    }
}

impl StreamFilter for ConvertFilter {
    fn label(&self) -> &'static str {
        "convert.*"
    }

    fn filter(
        &mut self,
        _env: &mut FilterEnv,
        input: &mut Brigade,
        output: &mut Brigade,
        consumed: Option<&mut usize>,
        flags: FilterFlags,
    ) -> FilterStatus {
        let mut n = 0usize;
        while let Some(bucket) = input.pop_front() {
            if !self.append_bucket(Some(&bucket.data), output, &mut n) {
                return FilterStatus::ErrFatal;
            }
        }
        if flags.is_closing() && !self.append_bucket(None, output, &mut n) {
            return FilterStatus::ErrFatal;
        }
        if let Some(c) = consumed {
            *c = n;
        }
        FilterStatus::PassOn
    }

    fn seek(&mut self, _env: &mut FilterEnv, _offset: i64, _whence: i32) -> bool {
        self.conv.reset();
        true
    }

    fn read_seekable(&self) -> Seekable {
        Seekable::Start
    }

    fn write_seekable(&self) -> Seekable {
        self.write_seekable
    }
}

/// The `convert.*` factory (`strfilter_convert_create`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ConvertFilterFactory;

impl FilterFactory for ConvertFilterFactory {
    fn create(
        &self,
        name: &str,
        params: Option<&Value>,
        _persistent: bool,
    ) -> Option<Box<dyn StreamFilter>> {
        if params.is_some_and(|p| !matches!(p, Value::Array(_))) {
            diag::warning(format!("Stream filter ({name}): invalid filter parameter"));
            return None;
        }
        let dot = name.find('.')?;
        let write_seekable = parse_write_seek_mode(params)?;
        let mode = ConvertMode::from_name(&name[dot + 1..])?;
        let mut f = ConvertFilter::new(mode, name, params);
        f.write_seekable = write_seekable;
        Some(Box::new(f))
    }
}

#[cfg(test)]
mod tests;
