// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `mb_*` string functions operating in code-point space.
//!
//! Ref: `ext/mbstring/mbstring.c`.

use crate::convert::{fast_convert, ConvertBuf, ErrorMode, BAD_INPUT};
use crate::encoding::{self, Encoding, EncodingNo};
use crate::error::{MbError, MbResult};
use crate::state::MbState;
use crate::unicode::{self, CaseMode};

/// `MBFL_SUBSTR_UNTIL_END`.
pub const SUBSTR_UNTIL_END: usize = usize::MAX;

/// `STR_PAD_LEFT`.
pub const STR_PAD_LEFT: i64 = 0;
/// `STR_PAD_RIGHT`.
pub const STR_PAD_RIGHT: i64 = 1;
/// `STR_PAD_BOTH`.
pub const STR_PAD_BOTH: i64 = 2;

/// Whether `enc` is UTF-8 or one of its mobile variants
/// (`php_mb_is_no_encoding_utf8`).
pub fn is_utf8_family(enc: &Encoding) -> bool {
    (EncodingNo::Utf8..=EncodingNo::Utf8Sb).contains(&enc.no)
}

/// `php_mb_is_unsupported_no_encoding` (for `mb_ord`/`mb_chr`).
pub fn is_unsupported_for_ord(enc: &Encoding) -> bool {
    use EncodingNo as N;
    let n = enc.no;
    n <= N::Qprint
        || (N::Utf7..=N::Utf7imap).contains(&n)
        || (N::Jis..=N::Iso2022JpMs).contains(&n)
        || (N::Cp50220..=N::Cp50222).contains(&n)
}

/// `mb_fast_strlen_utf8`: number of non-continuation bytes.
pub fn fast_strlen_utf8(s: &[u8]) -> usize {
    s.iter().filter(|&&b| !(0x80..0xC0).contains(&b)).count()
}

/// `mb_get_strlen`.
pub fn get_strlen(s: &[u8], enc: &'static Encoding) -> usize {
    if let Some(w) = enc.fixed_width() {
        return s.len() / w;
    }
    if is_utf8_family(enc) && std::str::from_utf8(s).is_ok() {
        return fast_strlen_utf8(s);
    }
    let mut buf = [0u32; 128];
    let mut state = 0;
    let mut rest = s;
    let mut len = 0;
    while !rest.is_empty() {
        len += (enc.to_wchar)(&mut rest, &mut buf, &mut state);
    }
    len
}

/// `mb_get_strwidth`.
pub fn get_strwidth(s: &[u8], enc: &'static Encoding) -> usize {
    let mut buf = [0u32; 128];
    let mut state = 0;
    let mut rest = s;
    let mut width = 0;
    while !rest.is_empty() {
        let n = (enc.to_wchar)(&mut rest, &mut buf, &mut state);
        width += buf[..n]
            .iter()
            .map(|&w| unicode::character_width(w))
            .sum::<usize>();
    }
    width
}

/// `mb_get_substr_slow`.
pub fn get_substr_slow(
    input: &[u8],
    mut from: usize,
    mut len: usize,
    enc: &'static Encoding,
    substchar: u32,
    mode: ErrorMode,
) -> Vec<u8> {
    let mut wchar_buf = [0u32; 128];
    let mut state = 0;
    let cap = if from > input.len() {
        0
    } else {
        len.min(input.len() - from)
    };
    let mut buf = ConvertBuf::new(cap, substchar, mode);
    let mut rest = input;
    while !rest.is_empty() && len > 0 {
        let out_len = (enc.to_wchar)(&mut rest, &mut wchar_buf, &mut state);
        if from >= out_len {
            from -= out_len;
        } else {
            let needed = (out_len - from).min(len);
            (enc.from_wchar)(
                &wchar_buf[from..from + needed],
                &mut buf,
                rest.is_empty() || out_len >= len,
            );
            from = 0;
            len -= needed;
        }
    }
    buf.into_bytes()
}

/// `mb_get_substr`.
pub fn get_substr(
    input: &[u8],
    from: usize,
    len: usize,
    enc: &'static Encoding,
    substchar: u32,
    mode: ErrorMode,
) -> Vec<u8> {
    if len == 0 || (from >= input.len() && enc.no != EncodingNo::SjisMac) {
        return Vec::new();
    }
    if let Some(w) = enc.fixed_width() {
        let from = from.saturating_mul(w);
        let len = len.saturating_mul(w);
        if from >= input.len() {
            return Vec::new();
        }
        let rest = &input[from..];
        return rest[..len.min(rest.len())].to_vec();
    }
    // Valid UTF-8 is sliced on codepoint boundaries without being decoded
    // (`mb_get_substr`'s `ZSTR_IS_VALID_UTF8` case): decoding and encoding
    // it again gives the same bytes. Anything else is decoded, its errors
    // substituted.
    if enc.is_utf8() && std::str::from_utf8(input).is_ok() {
        return utf8_slice(input, from, len).to_vec();
    }
    get_substr_slow(input, from, len, enc, substchar, mode)
}

/// The `len` codepoints of valid UTF-8 `s` from codepoint `from` on (as
/// many as there are).
fn utf8_slice(s: &[u8], from: usize, len: usize) -> &[u8] {
    // The byte offset of each codepoint: the bytes that are no
    // continuation byte (`10xxxxxx`).
    let mut starts = s
        .iter()
        .enumerate()
        .filter(|(_, &b)| b & 0xC0 != 0x80)
        .map(|(i, _)| i);
    let Some(start) = starts.nth(from) else {
        return &[];
    };
    let stop = match len.checked_sub(1) {
        Some(more) => starts.nth(more).unwrap_or(s.len()),
        None => start,
    };
    &s[start..stop]
}

/// `memnstr`: first occurrence of `needle` in `hay`.
fn memnstr(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `zend_memnrstr`: last occurrence of `needle` in `hay`.
fn memnrstr(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(hay.len());
    }
    if needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// `offset_to_pointer_utf8`.
fn offset_to_pointer_utf8(s: &[u8], start: usize, offset: i64) -> Option<usize> {
    if offset < 0 {
        let mut pos = s.len();
        let mut offset = offset;
        while offset < 0 {
            if pos <= start {
                return None;
            }
            pos -= 1;
            let c = s[pos];
            if c < 0x80 || (c & 0xC0) != 0x80 {
                offset += 1;
            }
        }
        Some(pos)
    } else {
        let mut pos = start;
        let mut offset = offset;
        while offset > 0 {
            offset -= 1;
            if pos >= s.len() {
                return None;
            }
            pos += usize::from(crate::codecs::utf8::MBLEN_TABLE_UTF8[usize::from(s[pos])]);
        }
        Some(pos.min(s.len()))
    }
}

/// Error from [`find_strpos`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrposError {
    /// `MBFL_ERROR_NOT_FOUND`.
    NotFound,
    /// `MBFL_ERROR_OFFSET`.
    Offset,
}

/// `mb_find_strpos`.
pub fn find_strpos(
    haystack: &[u8],
    needle: &[u8],
    enc: &'static Encoding,
    offset: i64,
    reverse: bool,
) -> Result<usize, StrposError> {
    let (h, n);
    let (h, n): (&[u8], &[u8]) = if is_utf8_family(enc) {
        (haystack, needle)
    } else {
        h = fast_convert(haystack, enc, encoding::utf8(), 0, ErrorMode::BadUtf8).0;
        n = fast_convert(needle, enc, encoding::utf8(), 0, ErrorMode::BadUtf8).0;
        (&h, &n)
    };
    find_strpos_utf8(h, n, offset, reverse)
}

fn find_strpos_utf8(h: &[u8], n: &[u8], offset: i64, reverse: bool) -> Result<usize, StrposError> {
    let Some(mut op) = offset_to_pointer_utf8(h, 0, offset) else {
        return Err(StrposError::Offset);
    };
    if h.len() < n.len() {
        return Err(StrposError::NotFound);
    }
    let found = if !reverse {
        memnstr(&h[op..], n).map(|p| p + op)
    } else if offset >= 0 {
        memnrstr(&h[op..], n).map(|p| p + op)
    } else {
        let needle_len = fast_strlen_utf8(n);
        op = offset_to_pointer_utf8(h, op, i64::try_from(needle_len).unwrap_or(i64::MAX))
            .unwrap_or(h.len());
        memnrstr(&h[..op], n)
    };
    found
        .map(|p| fast_strlen_utf8(&h[..p]))
        .ok_or(StrposError::NotFound)
}

/// `php_mb_stripos`.
pub fn stripos(
    reverse: bool,
    haystack: &[u8],
    needle: &[u8],
    offset: i64,
    enc: &'static Encoding,
) -> Result<usize, StrposError> {
    let u8 = encoding::utf8();
    let h = unicode::convert_case(
        CaseMode::FoldSimple,
        haystack,
        enc,
        u8,
        ErrorMode::BadUtf8,
        0,
    );
    let n = unicode::convert_case(CaseMode::FoldSimple, needle, enc, u8, ErrorMode::BadUtf8, 0);
    find_strpos_utf8(&h, &n, offset, reverse)
}

/// Which `mb_strstr` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrstrVariant {
    /// `mb_strstr`.
    Strstr,
    /// `mb_strrchr`.
    Strrchr,
    /// `mb_stristr`.
    Stristr,
    /// `mb_strrichr`.
    Strrichr,
}

fn to_i64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn offset_error() -> MbError {
    MbError::arg_value(3, "offset", "must be contained in argument #1 ($haystack)")
}

fn usize_from_i64(v: i64) -> usize {
    usize::try_from(v).unwrap_or(0)
}

impl MbState {
    fn substr(&self, s: &[u8], from: usize, len: usize, enc: &'static Encoding) -> Vec<u8> {
        get_substr(
            s,
            from,
            len,
            enc,
            self.illegal_substchar(),
            self.illegal_mode(),
        )
    }

    /// `mb_strlen($string, $encoding)`.
    pub fn mb_strlen(&mut self, s: &[u8], encoding: Option<&[u8]>) -> MbResult<i64> {
        let enc = self.get_encoding(encoding, 2, "encoding")?;
        Ok(to_i64(get_strlen(s, enc)))
    }

    fn strpos_common(
        &mut self,
        haystack: &[u8],
        needle: &[u8],
        offset: i64,
        encoding: Option<&[u8]>,
        f: impl FnOnce(&'static Encoding) -> Result<usize, StrposError>,
    ) -> MbResult<Option<i64>> {
        let _ = (haystack, needle, offset);
        let enc = self.get_encoding(encoding, 4, "encoding")?;
        match f(enc) {
            Ok(n) => Ok(Some(to_i64(n))),
            Err(StrposError::NotFound) => Ok(None),
            Err(StrposError::Offset) => Err(offset_error()),
        }
    }

    /// `mb_strpos`; `Ok(None)` is `false`.
    pub fn mb_strpos(
        &mut self,
        haystack: &[u8],
        needle: &[u8],
        offset: i64,
        encoding: Option<&[u8]>,
    ) -> MbResult<Option<i64>> {
        self.strpos_common(haystack, needle, offset, encoding, |e| {
            find_strpos(haystack, needle, e, offset, false)
        })
    }

    /// `mb_strrpos`.
    pub fn mb_strrpos(
        &mut self,
        haystack: &[u8],
        needle: &[u8],
        offset: i64,
        encoding: Option<&[u8]>,
    ) -> MbResult<Option<i64>> {
        self.strpos_common(haystack, needle, offset, encoding, |e| {
            find_strpos(haystack, needle, e, offset, true)
        })
    }

    /// `mb_stripos`.
    pub fn mb_stripos(
        &mut self,
        haystack: &[u8],
        needle: &[u8],
        offset: i64,
        encoding: Option<&[u8]>,
    ) -> MbResult<Option<i64>> {
        self.strpos_common(haystack, needle, offset, encoding, |e| {
            stripos(false, haystack, needle, offset, e)
        })
    }

    /// `mb_strripos`.
    pub fn mb_strripos(
        &mut self,
        haystack: &[u8],
        needle: &[u8],
        offset: i64,
        encoding: Option<&[u8]>,
    ) -> MbResult<Option<i64>> {
        self.strpos_common(haystack, needle, offset, encoding, |e| {
            stripos(true, haystack, needle, offset, e)
        })
    }

    /// `mb_strstr` / `mb_strrchr` / `mb_stristr` / `mb_strrichr`;
    /// `Ok(None)` is `false`.
    pub fn mb_strstr_variant(
        &mut self,
        variant: StrstrVariant,
        haystack: &[u8],
        needle: &[u8],
        before_needle: bool,
        encoding: Option<&[u8]>,
    ) -> MbResult<Option<Vec<u8>>> {
        let enc = self.get_encoding(encoding, 4, "encoding")?;
        let reverse = matches!(variant, StrstrVariant::Strrchr | StrstrVariant::Strrichr);
        let n = if matches!(variant, StrstrVariant::Stristr | StrstrVariant::Strrichr) {
            stripos(reverse, haystack, needle, 0, enc)
        } else {
            find_strpos(haystack, needle, enc, 0, reverse)
        };
        Ok(n.ok().map(|n| {
            if before_needle {
                self.substr(haystack, 0, n, enc)
            } else {
                self.substr(haystack, n, SUBSTR_UNTIL_END, enc)
            }
        }))
    }

    /// `mb_substr_count`.
    pub fn mb_substr_count(
        &mut self,
        haystack: &[u8],
        needle: &[u8],
        encoding: Option<&[u8]>,
    ) -> MbResult<i64> {
        if needle.is_empty() {
            return Err(MbError::must_not_be_empty(2, "needle"));
        }
        let enc = self.get_encoding(encoding, 3, "encoding")?;
        let u8 = encoding::utf8();
        let h = fast_convert(haystack, enc, u8, 0, ErrorMode::BadUtf8).0;
        let n = fast_convert(needle, enc, u8, 0, ErrorMode::BadUtf8).0;
        if !is_utf8_family(enc) && n.is_empty() {
            return Err(MbError::must_not_be_empty(2, "needle"));
        }
        if h.len() < n.len() {
            return Ok(0);
        }
        let mut count = 0;
        let mut p = 0;
        while let Some(i) = memnstr(&h[p..], &n) {
            p += i + n.len();
            count += 1;
            if n.is_empty() {
                // zend_memnstr with an empty needle never advances; PHP
                // cannot reach this (needle is non-empty), guard anyway.
                break;
            }
        }
        Ok(count)
    }

    /// `mb_substr($string, $start, $length, $encoding)`.
    pub fn mb_substr(
        &mut self,
        s: &[u8],
        from: i64,
        len: Option<i64>,
        encoding: Option<&[u8]>,
    ) -> MbResult<Vec<u8>> {
        // (The message only on the error: formatted for every call, it was
        // a fifth of a short `mb_substr()`.)
        let range = || format!("must be between {} and {}", i64::MIN + 1, i64::MAX);
        if from == i64::MIN {
            return Err(MbError::arg_value(2, "start", range()));
        }
        if len == Some(i64::MIN) {
            return Err(MbError::arg_value(3, "length", range()));
        }
        let enc = self.get_encoding(encoding, 4, "encoding")?;
        let mblen = if from < 0 || len.is_some_and(|l| l < 0) {
            get_strlen(s, enc)
        } else {
            0
        };
        let real_from = if from >= 0 {
            usize_from_i64(from)
        } else if from.unsigned_abs() < mblen as u64 {
            mblen - usize::try_from(from.unsigned_abs()).unwrap_or(0)
        } else {
            0
        };
        let real_len = match len {
            None => SUBSTR_UNTIL_END,
            Some(l) if l >= 0 => usize_from_i64(l),
            Some(l) => {
                if real_from < mblen && u128::from(l.unsigned_abs()) < (mblen - real_from) as u128 {
                    (mblen - real_from) - usize::try_from(l.unsigned_abs()).unwrap_or(0)
                } else {
                    0
                }
            }
        };
        Ok(self.substr(s, real_from, real_len, enc))
    }

    /// `mb_str_split`.
    pub fn mb_str_split(
        &mut self,
        s: &[u8],
        split_len: i64,
        encoding: Option<&[u8]>,
    ) -> MbResult<Vec<Vec<u8>>> {
        if split_len <= 0 {
            return Err(MbError::arg_value(2, "length", "must be greater than 0"));
        } else if split_len > i64::from(u32::MAX / 4) {
            return Err(MbError::arg_value(2, "length", "is too large"));
        }
        let mut split_len = usize_from_i64(split_len);
        if split_len > s.len() {
            split_len = s.len();
        }
        let enc = self.get_encoding(encoding, 3, "encoding")?;
        let mut out = Vec::new();
        if s.is_empty() {
            return Ok(out);
        }
        if let Some(w) = enc.fixed_width() {
            let chunk = w * split_len;
            out.extend(s.chunks(chunk).map(<[u8]>::to_vec));
        } else if let Some(tab) = enc.mblen_table {
            let mut p = 0;
            while p < s.len() {
                let start = p;
                let mut count = 0;
                while count < split_len && p < s.len() {
                    p += usize::from(tab[usize::from(s[p])]);
                    count += 1;
                }
                p = p.min(s.len());
                out.push(s[start..p].to_vec());
            }
        } else {
            let sub = self.illegal_substchar();
            let mode = self.illegal_mode();
            let mut wchar_buf = [0u32; 128];
            let mut state = 0;
            let mut rest = s;
            let mut char_count = 0usize;
            let mut buf = ConvertBuf::new(0, sub, mode);
            while !rest.is_empty() {
                let out_len = (enc.to_wchar)(&mut rest, &mut wchar_buf, &mut state);
                let mut i = 0;
                if char_count > 0 {
                    if out_len >= split_len - char_count {
                        (enc.from_wchar)(&wchar_buf[..split_len - char_count], &mut buf, true);
                        i += split_len - char_count;
                        char_count = 0;
                        out.push(std::mem::take(&mut buf.out));
                    } else {
                        (enc.from_wchar)(&wchar_buf[..out_len], &mut buf, rest.is_empty());
                        char_count += out_len;
                        continue;
                    }
                }
                while i < out_len {
                    buf = ConvertBuf::new(split_len, sub, mode);
                    if out_len - i >= split_len {
                        (enc.from_wchar)(&wchar_buf[i..i + split_len], &mut buf, true);
                        i += split_len;
                        out.push(std::mem::take(&mut buf.out));
                    } else {
                        (enc.from_wchar)(&wchar_buf[i..out_len], &mut buf, rest.is_empty());
                        char_count = out_len - i;
                        break;
                    }
                }
            }
            if char_count > 0 {
                out.push(buf.out);
            }
        }
        Ok(out)
    }

    /// `mb_strwidth`.
    pub fn mb_strwidth(&mut self, s: &[u8], encoding: Option<&[u8]>) -> MbResult<i64> {
        let enc = self.get_encoding(encoding, 2, "encoding")?;
        Ok(to_i64(get_strwidth(s, enc)))
    }

    /// `mb_strimwidth`.
    pub fn mb_strimwidth(
        &mut self,
        s: &[u8],
        from: i64,
        width: i64,
        trim_marker: &[u8],
        encoding: Option<&[u8]>,
    ) -> MbResult<Vec<u8>> {
        let enc = self.get_encoding(encoding, 5, "encoding")?;
        let mut from = from;
        if from != 0 {
            let str_len = to_i64(get_strlen(s, enc));
            if from < 0 {
                from += str_len;
            }
            if from < 0 || from > str_len {
                return Err(MbError::arg_value(2, "start", "is out of range"));
            }
        }
        let mut width = width;
        if width < 0 {
            self.deprecated("passing a negative integer to argument #3 ($width) is deprecated");
            width += to_i64(get_strwidth(s, enc));
            if from > 0 {
                let trimmed = self.substr(s, 0, usize_from_i64(from), enc);
                width -= to_i64(get_strwidth(&trimmed, enc));
            }
            if width < 0 {
                return Err(MbError::arg_value(3, "width", "is out of range"));
            }
        }
        Ok(self.trim_string(
            s,
            trim_marker,
            enc,
            usize_from_i64(from),
            usize_from_i64(width),
        ))
    }

    /// `mb_trim_string`.
    fn trim_string(
        &self,
        input: &[u8],
        marker: &[u8],
        enc: &'static Encoding,
        from: usize,
        width: usize,
    ) -> Vec<u8> {
        let mut wchar_buf = [0u32; 128];
        let mut rest = input;
        let mut state = 0;
        let mut remaining_width = width;
        let mut to_skip = from;
        let mut first_call = true;
        let mut input_err = false;
        let mut width = width;

        // Phase 1: does the string fit?
        let mut overflow: Option<(bool, usize)> = None;
        'outer: while !rest.is_empty() {
            let out_len = (enc.to_wchar)(&mut rest, &mut wchar_buf, &mut state);
            if out_len <= to_skip {
                to_skip -= out_len;
            } else {
                for &w in &wchar_buf[to_skip..out_len] {
                    let cw = unicode::character_width(w);
                    input_err |= w == BAD_INPUT;
                    if remaining_width < cw {
                        overflow = Some((first_call, out_len));
                        break 'outer;
                    }
                    remaining_width -= cw;
                }
                to_skip = 0;
            }
            first_call = false;
        }

        let Some((reuse, first_out_len)) = overflow else {
            if !input_err {
                if from == 0 {
                    return input.to_vec();
                }
                return self.substr(input, from, SUBSTR_UNTIL_END, enc);
            }
            return get_substr_slow(
                input,
                from,
                SUBSTR_UNTIL_END,
                enc,
                self.illegal_substchar(),
                self.illegal_mode(),
            );
        };

        let marker_width = get_strwidth(marker, enc);
        if width <= marker_width {
            return marker.to_vec();
        }
        width -= marker_width;
        let mut buf = ConvertBuf::new(width, self.illegal_substchar(), self.illegal_mode());
        let mut from = from;
        let mut out_len;
        if reuse {
            out_len = first_out_len;
        } else {
            rest = input;
            state = 0;
            out_len = (enc.to_wchar)(&mut rest, &mut wchar_buf, &mut state);
        }
        loop {
            if out_len <= from {
                from -= out_len;
            } else {
                let mut done = false;
                for i in from..out_len {
                    let cw = unicode::character_width(wchar_buf[i]);
                    if width < cw {
                        (enc.from_wchar)(&wchar_buf[from..i], &mut buf, true);
                        done = true;
                        break;
                    }
                    width -= cw;
                }
                if done {
                    break;
                }
                (enc.from_wchar)(&wchar_buf[from..out_len], &mut buf, false);
                from = 0;
            }
            if rest.is_empty() {
                // Unreachable in PHP (asserted); finish defensively.
                (enc.from_wchar)(&[], &mut buf, true);
                break;
            }
            out_len = (enc.to_wchar)(&mut rest, &mut wchar_buf, &mut state);
        }
        buf.out.extend_from_slice(marker);
        buf.out
    }

    /// `mb_convert_case($string, $mode, $encoding)`.
    pub fn mb_convert_case(
        &mut self,
        s: &[u8],
        mode: i64,
        encoding: Option<&[u8]>,
    ) -> MbResult<Vec<u8>> {
        let enc = self.get_encoding(encoding, 3, "encoding")?;
        let Some(mode) = CaseMode::from_i64(mode) else {
            return Err(MbError::arg_value(
                2,
                "mode",
                "must be one of the MB_CASE_* constants",
            ));
        };
        Ok(self.convert_case(mode, s, enc))
    }

    fn convert_case(&self, mode: CaseMode, s: &[u8], enc: &'static Encoding) -> Vec<u8> {
        unicode::convert_case(
            mode,
            s,
            enc,
            enc,
            self.illegal_mode(),
            self.illegal_substchar(),
        )
    }

    /// `mb_strtoupper`.
    pub fn mb_strtoupper(&mut self, s: &[u8], encoding: Option<&[u8]>) -> MbResult<Vec<u8>> {
        let enc = self.get_encoding(encoding, 2, "encoding")?;
        Ok(self.convert_case(CaseMode::Upper, s, enc))
    }

    /// `mb_strtolower`.
    pub fn mb_strtolower(&mut self, s: &[u8], encoding: Option<&[u8]>) -> MbResult<Vec<u8>> {
        let enc = self.get_encoding(encoding, 2, "encoding")?;
        Ok(self.convert_case(CaseMode::Lower, s, enc))
    }

    fn ulcfirst(&mut self, s: &[u8], encoding: Option<&[u8]>, mode: CaseMode) -> MbResult<Vec<u8>> {
        let enc = self.get_encoding(encoding, 2, "encoding")?;
        let first = self.substr(s, 0, 1, enc);
        let head = self.convert_case(mode, &first, enc);
        if first == head {
            return Ok(s.to_vec());
        }
        let mut out = head;
        out.extend(self.substr(s, 1, SUBSTR_UNTIL_END, enc));
        Ok(out)
    }

    /// `mb_ucfirst` (PHP 8.4).
    pub fn mb_ucfirst(&mut self, s: &[u8], encoding: Option<&[u8]>) -> MbResult<Vec<u8>> {
        self.ulcfirst(s, encoding, CaseMode::Title)
    }

    /// `mb_lcfirst` (PHP 8.4).
    pub fn mb_lcfirst(&mut self, s: &[u8], encoding: Option<&[u8]>) -> MbResult<Vec<u8>> {
        self.ulcfirst(s, encoding, CaseMode::Lower)
    }

    /// `mb_trim` / `mb_ltrim` / `mb_rtrim` (PHP 8.4): `left`/`right` select
    /// the sides; `characters` is `None` for the default whitespace set.
    pub fn mb_trim_sides(
        &mut self,
        s: &[u8],
        characters: Option<&[u8]>,
        encoding: Option<&[u8]>,
        left: bool,
        right: bool,
    ) -> MbResult<Vec<u8>> {
        const DEFAULT: [u32; 27] = [
            0x20, 0x0C, 0x0A, 0x0D, 0x09, 0x0B, 0x00, 0xA0, 0x1680, 0x2000, 0x2001, 0x2002, 0x2003,
            0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200A, 0x2028, 0x2029, 0x202F, 0x205F,
            0x3000, 0x85, 0x180E,
        ];
        let enc = self.get_encoding(encoding, 3, "encoding")?;
        let set: Vec<u32> = match characters {
            None => DEFAULT.to_vec(),
            Some(what) => {
                let mut wchar_buf = [0u32; 128];
                let mut state = 0;
                let mut rest = what;
                let mut set = Vec::new();
                let mut first = true;
                while !rest.is_empty() {
                    let n = (enc.to_wchar)(&mut rest, &mut wchar_buf, &mut state);
                    if n <= 4 && first {
                        set = wchar_buf[..n].to_vec();
                        break;
                    }
                    first = false;
                    set.extend_from_slice(&wchar_buf[..n]);
                }
                if first && set.is_empty() && what.is_empty() {
                    return Ok(s.to_vec());
                }
                set
            }
        };
        Ok(self.trim_each_wchar(s, &set, left, right, enc))
    }

    fn trim_each_wchar(
        &self,
        s: &[u8],
        set: &[u32],
        left: bool,
        right: bool,
        enc: &'static Encoding,
    ) -> Vec<u8> {
        let mut wchar_buf = [0u32; 128];
        let mut state = 0;
        let mut rest = s;
        let (mut l, mut r, mut total) = (0usize, 0usize, 0usize);
        let mut ltrim = left;
        while !rest.is_empty() {
            let n = (enc.to_wchar)(&mut rest, &mut wchar_buf, &mut state);
            total += n;
            for &w in &wchar_buf[..n] {
                if set.contains(&w) {
                    if ltrim {
                        l += 1;
                    }
                    if right {
                        r += 1;
                    }
                } else {
                    ltrim = false;
                    if right {
                        r = 0;
                    }
                }
            }
        }
        if l == 0 && r == 0 {
            return s.to_vec();
        }
        self.substr(s, l, total.saturating_sub(r + l), enc)
    }

    /// `mb_trim`.
    pub fn mb_trim(
        &mut self,
        s: &[u8],
        chars: Option<&[u8]>,
        encoding: Option<&[u8]>,
    ) -> MbResult<Vec<u8>> {
        self.mb_trim_sides(s, chars, encoding, true, true)
    }

    /// `mb_ltrim`.
    pub fn mb_ltrim(
        &mut self,
        s: &[u8],
        chars: Option<&[u8]>,
        encoding: Option<&[u8]>,
    ) -> MbResult<Vec<u8>> {
        self.mb_trim_sides(s, chars, encoding, true, false)
    }

    /// `mb_rtrim`.
    pub fn mb_rtrim(
        &mut self,
        s: &[u8],
        chars: Option<&[u8]>,
        encoding: Option<&[u8]>,
    ) -> MbResult<Vec<u8>> {
        self.mb_trim_sides(s, chars, encoding, false, true)
    }

    /// `mb_str_pad` (PHP 8.3).
    pub fn mb_str_pad(
        &mut self,
        input: &[u8],
        length: i64,
        pad: &[u8],
        pad_type: i64,
        encoding: Option<&[u8]>,
    ) -> MbResult<Vec<u8>> {
        let enc = self.get_encoding(encoding, 5, "encoding")?;
        let input_length = get_strlen(input, enc);
        if length < 0 || usize_from_i64(length) <= input_length {
            return Ok(input.to_vec());
        }
        if pad.is_empty() {
            return Err(MbError::must_not_be_empty(3, "pad_string"));
        }
        if !(STR_PAD_LEFT..=STR_PAD_BOTH).contains(&pad_type) {
            return Err(MbError::arg_value(
                4,
                "pad_type",
                "must be STR_PAD_LEFT, STR_PAD_RIGHT, or STR_PAD_BOTH",
            ));
        }
        let pad_length = get_strlen(pad, enc);
        if pad_length == 0 {
            return Err(MbError::must_not_be_empty(3, "pad_string"));
        }
        let num = usize_from_i64(length) - input_length;
        let (left, right) = match pad_type {
            STR_PAD_RIGHT => (0, num),
            STR_PAD_LEFT => (num, 0),
            _ => (num / 2, num - num / 2),
        };
        let fill = |out: &mut Vec<u8>, copies: usize| {
            for _ in 0..copies {
                out.extend_from_slice(pad);
            }
        };
        let left_rem = self.substr(pad, 0, left % pad_length, enc);
        let right_rem = self.substr(pad, 0, right % pad_length, enc);
        let total = (left / pad_length)
            .checked_mul(pad.len())
            .and_then(|a| (right / pad_length).checked_mul(pad.len()).map(|b| (a, b)))
            .and_then(|(a, b)| a.checked_add(b))
            .and_then(|t| t.checked_add(left_rem.len() + right_rem.len() + input.len()));
        let Some(total) = total.filter(|&t| t < isize::MAX as usize / 2) else {
            return Err(MbError::Error("String size overflow".into()));
        };
        let mut out = Vec::with_capacity(total);
        fill(&mut out, left / pad_length);
        out.extend_from_slice(&left_rem);
        out.extend_from_slice(input);
        fill(&mut out, right / pad_length);
        out.extend_from_slice(&right_rem);
        Ok(out)
    }

    /// `mb_ord`; `Ok(None)` is `false`.
    pub fn mb_ord(&mut self, s: &[u8], encoding: Option<&[u8]>) -> MbResult<Option<i64>> {
        if s.is_empty() {
            return Err(MbError::must_not_be_empty(1, "string"));
        }
        let enc = self.get_encoding(encoding, 2, "encoding")?;
        if is_unsupported_for_ord(enc) {
            return Err(MbError::Value(format!(
                "mb_ord() does not support the \"{}\" encoding",
                enc.name
            )));
        }
        let mut buf = [0u32; crate::convert::MIN_WCHAR_BUFSIZE];
        let mut state = 0;
        let mut rest = s;
        let n = (enc.to_wchar)(&mut rest, &mut buf, &mut state);
        if n == 0 || buf[0] == BAD_INPUT {
            return Ok(None);
        }
        Ok(Some(i64::from(buf[0])))
    }

    /// `mb_chr`; `Ok(None)` is `false`.
    pub fn mb_chr(&mut self, cp: i64, encoding: Option<&[u8]>) -> MbResult<Option<Vec<u8>>> {
        let enc = self.get_encoding(encoding, 2, "encoding")?;
        if is_unsupported_for_ord(enc) {
            return Err(MbError::Value(format!(
                "mb_chr() does not support the \"{}\" encoding",
                enc.name
            )));
        }
        if !(0..=0x10_FFFF).contains(&cp) {
            return Ok(None);
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let cp = cp as u32;
        if is_utf8_family(enc) {
            if (0xD800..0xE000).contains(&cp) {
                return Ok(None);
            }
            let mut out = Vec::new();
            crate::codecs::utf8::push_utf8(&mut out, cp);
            return Ok(Some(out));
        }
        let ucs4 = encoding::get(EncodingNo::Ucs4be);
        let (out, errors) = fast_convert(
            &cp.to_be_bytes(),
            ucs4,
            enc,
            self.illegal_substchar(),
            self.illegal_mode(),
        );
        if errors != 0 {
            return Ok(None);
        }
        Ok(Some(out))
    }

    /// `mb_scrub`.
    pub fn mb_scrub(&mut self, s: &[u8], encoding: Option<&[u8]>) -> MbResult<Vec<u8>> {
        let enc = self.get_encoding(encoding, 2, "encoding")?;
        Ok(self.convert_ex(s, enc, enc))
    }

    /// `php_mb_convert_encoding_ex`: convert with the current substitution
    /// settings, counting errors into `illegalchars`.
    pub fn convert_ex(
        &mut self,
        s: &[u8],
        to: &'static Encoding,
        from: &'static Encoding,
    ) -> Vec<u8> {
        let (out, errors) =
            fast_convert(s, from, to, self.illegal_substchar(), self.illegal_mode());
        self.illegalchars += u64::from(errors);
        out
    }

    /// `mb_strcut`.
    pub fn mb_strcut(
        &mut self,
        s: &[u8],
        from: i64,
        len: Option<i64>,
        encoding: Option<&[u8]>,
    ) -> MbResult<Vec<u8>> {
        let enc = self.get_encoding(encoding, 4, "encoding")?;
        let slen = to_i64(s.len());
        let mut len = len.unwrap_or(slen);
        let mut from = from;
        if from < 0 {
            from = (slen + from).max(0);
        }
        if len < 0 {
            len = ((slen - from) + len).max(0);
        }
        if from > slen || len == 0 {
            return Ok(Vec::new());
        }
        let from = usize_from_i64(from);
        let len = usize_from_i64(len);
        Ok(crate::strcut::strcut(s, from, len, enc))
    }

    /// `mb_check_encoding($value)` for a string value (arrays are walked by
    /// the VM, calling this for each key and string value).
    pub fn mb_check_encoding(&mut self, s: &[u8], encoding: Option<&[u8]>) -> MbResult<bool> {
        let enc = self.get_encoding(encoding, 2, "encoding")?;
        Ok(check_str_encoding(s, enc))
    }

    /// `mb_check_encoding()` with no value: deprecated, reports whether any
    /// conversion errors happened so far.
    pub fn mb_check_encoding_noarg(&mut self, encoding: Option<&[u8]>) -> MbResult<bool> {
        self.get_encoding(encoding, 2, "encoding")?;
        self.deprecated("Calling mb_check_encoding() without argument is deprecated");
        Ok(self.illegalchars == 0)
    }

    /// `mb_convert_kana`.
    pub fn mb_convert_kana(
        &mut self,
        s: &[u8],
        mode: Option<&[u8]>,
        encoding: Option<&[u8]>,
    ) -> MbResult<Vec<u8>> {
        let opt = crate::codecs::cjk::kana::kana_mode_from_option(mode)?;
        let enc = self.get_encoding(encoding, 3, "encoding")?;
        Ok(crate::codecs::cjk::kana::jp_kana_convert(
            s,
            enc,
            opt,
            self.illegal_substchar(),
            self.illegal_mode(),
        ))
    }
}

/// `php_mb_check_encoding` / `mb_check_str_encoding`.
pub fn check_str_encoding(s: &[u8], enc: &'static Encoding) -> bool {
    if enc.is_utf8() {
        return std::str::from_utf8(s).is_ok();
    }
    if let Some(check) = enc.check {
        return check(s);
    }
    let mut buf = [0u32; 128];
    let mut state = 0;
    let mut rest = s;
    let n = (enc.to_wchar)(&mut rest, &mut buf[..8], &mut state);
    if buf[..n].contains(&BAD_INPUT) {
        return false;
    }
    while !rest.is_empty() {
        let n = (enc.to_wchar)(&mut rest, &mut buf, &mut state);
        if buf[..n].contains(&BAD_INPUT) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod substr_tests {
    use super::{get_substr, get_substr_slow};
    use crate::encoding::utf8;

    /// Valid UTF-8 sliced on codepoint boundaries is what decoding it gives
    /// (`mb_get_substr`'s valid-UTF-8 case), for every start and length
    /// over strings of one- to four-byte codepoints.
    #[test]
    fn valid_utf8_slices_match_the_decoding_path() {
        let mode = crate::state::MbState::default().illegal_mode();
        for s in [
            "",
            "a",
            "hello",
            "héllo wörld",
            "日本語テキスト",
            "a\u{1F600}b\u{1F601}c",
        ] {
            let b = s.as_bytes();
            for from in 0..=b.len() + 1 {
                for len in [0, 1, 2, 3, 7, usize::MAX] {
                    assert_eq!(
                        get_substr(b, from, len, utf8(), 0x3F, mode),
                        get_substr_slow(b, from, len, utf8(), 0x3F, mode),
                        "{s:?} {from} {len}"
                    );
                }
            }
        }
    }
}
