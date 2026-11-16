// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `mb_encode_numericentity` / `mb_decode_numericentity`.
//!
//! Ref: `ext/mbstring/mbstring.c` (`html_numeric_entity_encode`,
//! `html_numeric_entity_decode`).

use crate::convert::ConvertBuf;
use crate::encoding::Encoding;
use crate::error::{MbError, MbResult};
use crate::state::MbState;

/// `make_conversion_map`: `values` are the convmap array's elements as the
/// VM converted them with `zval_try_get_long` (`None` = not convertible).
pub fn make_conversion_map(values: &[Option<i64>]) -> MbResult<Vec<u32>> {
    if values.len() % 4 != 0 {
        return Err(MbError::arg_value(
            2,
            "map",
            "must have a multiple of 4 elements",
        ));
    }
    values
        .iter()
        .map(|v| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            v.map(|v| v as u32).ok_or_else(|| {
                MbError::arg_value(2, "map", "must only be composed of values of type int")
            })
        })
        .collect()
}

fn entity_convert(w: u32, convmap: &[u32]) -> Option<u32> {
    convmap
        .chunks_exact(4)
        .find_map(|m| (w >= m[0] && w <= m[1]).then(|| w.wrapping_add(m[2]) & m[3]))
}

fn entity_deconvert(number: u32, convmap: &[u32]) -> Option<u32> {
    convmap.chunks_exact(4).find_map(|m| {
        let cp = number.wrapping_sub(m[2]);
        (cp >= m[0] && cp <= m[1]).then_some(cp)
    })
}

/// `html_numeric_entity_encode`.
pub fn encode(
    input: &[u8],
    enc: &'static Encoding,
    convmap: &[u32],
    hex: bool,
    substchar: u32,
    mode: crate::ErrorMode,
) -> Vec<u8> {
    let mut wchar_buf = [0u32; 32];
    let mut converted: Vec<u32> = Vec::with_capacity(32 * 13);
    let mut state = 0;
    let mut rest = input;
    let mut buf = ConvertBuf::new(input.len(), substchar, mode);
    while !rest.is_empty() {
        let n = (enc.to_wchar)(&mut rest, &mut wchar_buf, &mut state);
        converted.clear();
        for &w in &wchar_buf[..n] {
            if let Some(mut w) = entity_convert(w, convmap) {
                converted.extend([u32::from(b'&'), u32::from(b'#')]);
                if hex {
                    converted.push(u32::from(b'x'));
                }
                if w == 0 {
                    converted.push(u32::from(b'0'));
                } else {
                    let mut digits = Vec::with_capacity(10);
                    let base = if hex { 16 } else { 10 };
                    while w > 0 {
                        digits.push(u32::from(b"0123456789ABCDEF"[(w % base) as usize]));
                        w /= base;
                    }
                    converted.extend(digits.iter().rev());
                }
                converted.push(u32::from(b';'));
            } else {
                converted.push(w);
            }
        }
        (enc.from_wchar)(&converted, &mut buf, rest.is_empty());
    }
    buf.into_bytes()
}

const DEC_ENTITY_MINLEN: usize = 3;
const HEX_ENTITY_MINLEN: usize = 4;
const DEC_ENTITY_MAXLEN: usize = 12;
const HEX_ENTITY_MAXLEN: usize = 11;

const AMP: u32 = b'&' as u32;

fn is_hex_digit(w: u32) -> bool {
    (u32::from(b'0')..=u32::from(b'9')).contains(&w)
        || (u32::from(b'A')..=u32::from(b'F')).contains(&w)
        || (u32::from(b'a')..=u32::from(b'f')).contains(&w)
}

fn is_dec_digit(w: u32) -> bool {
    (u32::from(b'0')..=u32::from(b'9')).contains(&w)
}

/// `html_numeric_entity_decode`.
#[allow(clippy::too_many_lines)]
pub fn decode(
    input: &[u8],
    enc: &'static Encoding,
    convmap: &[u32],
    substchar: u32,
    mode: crate::ErrorMode,
) -> Vec<u8> {
    let mut wb = [0u32; 128];
    let mut converted: Vec<u32> = Vec::with_capacity(128);
    let mut state = 0;
    let mut rest = input;
    let mut buf = ConvertBuf::new(input.len(), substchar, mode);
    let mut offset = 0usize;

    while !rest.is_empty() {
        let n = (enc.to_wchar)(&mut rest, &mut wb[offset..127], &mut state);
        let out_len = n + offset;
        wb[out_len] = AMP; // sentinel
        converted.clear();
        let in_more = !rest.is_empty();
        let mut p;
        if offset == 0 {
            p = wb.iter().position(|&w| w == AMP).unwrap_or(out_len);
            if p == out_len {
                (enc.from_wchar)(&wb[..out_len], &mut buf, !in_more);
                continue;
            }
            converted.extend_from_slice(&wb[..p]);
        } else {
            p = 0;
        }

        // `found_ampersand` loop
        let mut carry: Option<usize> = None;
        loop {
            let mut p2 = p;
            p2 += 1;
            let mut skip_to_scan = false;
            if wb[p2] == u32::from(b'#') {
                p2 += 1;
                if wb[p2] == u32::from(b'x') {
                    p2 += 1;
                    while is_hex_digit(wb[p2]) {
                        p2 += 1;
                    }
                    let elen = p2 - p;
                    if p2 == out_len && in_more && elen <= HEX_ENTITY_MAXLEN {
                        carry = Some(p);
                        break;
                    } else if !(HEX_ENTITY_MINLEN..=HEX_ENTITY_MAXLEN).contains(&elen) {
                        converted.extend_from_slice(&wb[p..p2]);
                    } else {
                        let mut value: u32 = 0;
                        for &w in &wb[p + 3..p2] {
                            let d = if w <= u32::from(b'9') {
                                w - u32::from(b'0')
                            } else if w >= u32::from(b'a') {
                                10 + (w - u32::from(b'a'))
                            } else {
                                10 + (w - u32::from(b'A'))
                            };
                            value = value.wrapping_mul(16).wrapping_add(d);
                        }
                        if let Some(cp) = entity_deconvert(value, convmap) {
                            converted.push(cp);
                            if wb[p2] == u32::from(b';') {
                                p2 += 1;
                            }
                        } else {
                            converted.extend_from_slice(&wb[p..p2]);
                        }
                    }
                } else {
                    while is_dec_digit(wb[p2]) {
                        p2 += 1;
                    }
                    let elen = p2 - p;
                    if p2 == out_len && in_more && elen <= DEC_ENTITY_MAXLEN {
                        carry = Some(p);
                        break;
                    } else if !(DEC_ENTITY_MINLEN..=DEC_ENTITY_MAXLEN).contains(&elen) {
                        converted.extend_from_slice(&wb[p..p2]);
                    } else {
                        let mut value: u32 = 0;
                        let mut too_big = false;
                        for &w in &wb[p + 2..p2] {
                            if value > 0x1999_9999 {
                                too_big = true;
                                break;
                            }
                            value = value
                                .wrapping_mul(10)
                                .wrapping_add(w.wrapping_sub(u32::from(b'0')));
                        }
                        if too_big {
                            converted.extend_from_slice(&wb[p..p2]);
                            skip_to_scan = true;
                        } else if let Some(cp) = entity_deconvert(value, convmap) {
                            converted.push(cp);
                            if wb[p2] == u32::from(b';') {
                                p2 += 1;
                            }
                        } else {
                            converted.extend_from_slice(&wb[p..p2]);
                        }
                    }
                }
            } else if p2 == out_len && in_more {
                // '&' at the end of the buffer: carry it over.
                carry = Some(usize::MAX);
                break;
            } else {
                converted.push(AMP);
            }
            let _ = skip_to_scan;

            // decimal_entity_too_big: scan to next '&'
            p = p2;
            while wb[p2] != AMP {
                p2 += 1;
            }
            if p2 > p {
                converted.extend_from_slice(&wb[p..p2]);
                p = p2;
            }
            if p < out_len {
                continue;
            }
            break;
        }

        match carry {
            Some(usize::MAX) => {
                wb[0] = AMP;
                offset = 1;
            }
            Some(start) => {
                // p2 == out_len here: move the partial entity to the front.
                let len = out_len - start;
                wb.copy_within(start..out_len, 0);
                offset = len;
            }
            None => offset = 0,
        }
        (enc.from_wchar)(&converted, &mut buf, !in_more);
    }
    buf.into_bytes()
}

impl MbState {
    /// `mb_encode_numericentity($string, $map, $encoding, $hex)`.
    pub fn mb_encode_numericentity(
        &mut self,
        s: &[u8],
        map: &[Option<i64>],
        encoding: Option<&[u8]>,
        hex: bool,
    ) -> MbResult<Vec<u8>> {
        let enc = self.get_encoding(encoding, 3, "encoding")?;
        let convmap = make_conversion_map(map)?;
        Ok(encode(
            s,
            enc,
            &convmap,
            hex,
            self.illegal_substchar(),
            self.illegal_mode(),
        ))
    }

    /// `mb_decode_numericentity($string, $map, $encoding)`.
    pub fn mb_decode_numericentity(
        &mut self,
        s: &[u8],
        map: &[Option<i64>],
        encoding: Option<&[u8]>,
    ) -> MbResult<Vec<u8>> {
        let enc = self.get_encoding(encoding, 3, "encoding")?;
        let convmap = make_conversion_map(map)?;
        Ok(decode(
            s,
            enc,
            &convmap,
            self.illegal_substchar(),
            self.illegal_mode(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::utf8;
    use crate::ErrorMode;

    const MAP: [u32; 4] = [0x0, 0x1_FFFF, 0, 0x1F_FFFF];

    #[test]
    fn encode_basic() {
        let out = encode(
            "aé€".as_bytes(),
            utf8(),
            &[0x80, 0x10_FFFF, 0, 0x1F_FFFF],
            false,
            63,
            ErrorMode::Char,
        );
        assert_eq!(out, b"a&#233;&#8364;");
        let out = encode(
            "é".as_bytes(),
            utf8(),
            &[0x80, 0x10_FFFF, 0, 0x1F_FFFF],
            true,
            63,
            ErrorMode::Char,
        );
        assert_eq!(out, b"&#xE9;");
    }

    #[test]
    fn decode_basic() {
        let d = |s: &[u8]| decode(s, utf8(), &MAP, 63, ErrorMode::Char);
        assert_eq!(d(b"&#233;&#x20AC;x&#;&&#12"), "é€x&#;&\u{c}".as_bytes());
        assert_eq!(d(b"&#99999999999;"), b"&#99999999999;");
    }

    #[test]
    fn decode_across_buffer_boundary() {
        let mut s = vec![b'a'; 124];
        s.extend_from_slice(b"&#x41;&#66;");
        let out = decode(&s, utf8(), &MAP, 63, ErrorMode::Char);
        let mut exp = vec![b'a'; 124];
        exp.extend_from_slice(b"AB");
        assert_eq!(out, exp);
    }
}
