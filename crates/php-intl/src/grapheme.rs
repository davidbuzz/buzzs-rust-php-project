//! `grapheme_*` (php-src `ext/intl/grapheme`).  Offsets and lengths are
//! counted in grapheme clusters except where php-src's ASCII fast paths
//! return byte offsets (identical for ASCII text).

use std::sync::Arc;

use crate::error::{Error, Result, UErrorCode};
use crate::sys::ubrk::{self, BreakIterator};
use crate::sys::ucol::Collator;
use crate::sys::usearch::{self, StringSearch};
use crate::sys::utext::Utf8Text;
use crate::ustr;

/// `GRAPHEME_EXTR_*`.
pub mod extract_type {
    /// `GRAPHEME_EXTR_COUNT`
    pub const COUNT: i64 = 0;
    /// `GRAPHEME_EXTR_MAXBYTES`
    pub const MAXBYTES: i64 = 1;
    /// `GRAPHEME_EXTR_MAXCHARS`
    pub const MAXCHARS: i64 = 2;
}

fn global(c: UErrorCode, m: &'static str) -> Error {
    Error::global(c, m)
}

/// `grapheme_ascii_check()`: the length when the string is ASCII without
/// `\r\n` sequences, else `None`.
fn ascii_check(s: &[u8]) -> Option<usize> {
    for i in 0..s.len() {
        if s[i] > 0x7f || (s.get(i + 1) == Some(&b'\n') && s[i] == b'\r') {
            return None;
        }
    }
    Some(s.len())
}

fn char_iter() -> std::result::Result<BreakIterator, UErrorCode> {
    BreakIterator::open_default(0)
}

/// `grapheme_count_graphemes()` over a prefix of `u`.
fn count_graphemes(bi: &mut BreakIterator, u: &[u16]) -> i32 {
    if bi.set_text_u16(Arc::from(u)).is_err() {
        return 0;
    }
    let mut n = 0;
    while bi.next_boundary() != ubrk::DONE {
        n += 1;
    }
    n
}

/// `grapheme_get_haystack_offset()`.
fn haystack_offset(bi: &mut BreakIterator, offset: i32) -> i32 {
    if offset == 0 {
        return 0;
    }
    let mut offset = offset;
    let (incr, back) = if offset < 0 {
        bi.last();
        (1, true)
    } else {
        (-1, false)
    };
    let mut pos = 0;
    while pos != ubrk::DONE && offset != 0 {
        pos = if back {
            bi.previous()
        } else {
            bi.next_boundary()
        };
        if pos != ubrk::DONE {
            offset += incr;
        }
    }
    if offset != 0 {
        return -1;
    }
    pos
}

fn outside_string(offset: i64, len: usize) -> bool {
    let len = i64::try_from(len).unwrap_or(i64::MAX);
    offset <= i64::from(i32::MIN)
        || offset > i64::from(i32::MAX)
        || if offset < 0 {
            -offset > len
        } else {
            offset > len
        }
}

fn contained_error() -> Error {
    Error::arg_value(3, "must be contained in argument #1 ($haystack)")
}

/// `grapheme_strpos_utf16()` → `(grapheme position, code point position)`,
/// `None` when not found.
fn strpos_utf16(
    haystack: &[u8],
    needle: &[u8],
    offset: i32,
    ignore_case: bool,
    last: bool,
    locale: &str,
) -> Result<Option<(i32, i32)>> {
    if haystack.is_empty() && !needle.is_empty() {
        return Ok(None);
    }
    let uh: Arc<[u16]> = Arc::from(
        ustr::to_u16(haystack).map_err(|c| global(c, "Error converting input string to UTF-16"))?,
    );
    let un: Arc<[u16]> = Arc::from(
        ustr::to_u16(needle).map_err(|c| global(c, "Error converting needle string to UTF-16"))?,
    );
    let mut bi = char_iter().map_err(|c| global(c, "Failed to get iterator"))?;
    bi.set_text_u16(uh.clone())
        .map_err(|c| global(c, "Failed to set up iterator"))?;
    if un.is_empty() {
        let offset_pos = haystack_offset(&mut bi, offset);
        if offset_pos == -1 {
            return Err(contained_error());
        }
        let n = if last && offset >= 0 {
            count_graphemes(&mut bi, &uh)
        } else {
            count_graphemes(&mut bi, &uh[..usize::try_from(offset_pos).unwrap_or(0)])
        };
        return Ok(Some((n, -1)));
    }
    let mut offset_pos = 0;
    if offset != 0 {
        offset_pos = haystack_offset(&mut bi, offset);
        if offset_pos == -1 {
            return Err(contained_error());
        }
    }
    let char_pos = {
        let mut src = StringSearch::open(un, uh.clone(), locale, &mut bi)
            .map_err(|c| global(c, "Error creating search object"))?;
        if ignore_case {
            src.set_secondary_strength()
                .map_err(|c| global(c, "Error setting collation strength"))?;
        }
        if offset != 0 {
            src.set_offset(if last { 0 } else { offset_pos })
                .map_err(|c| global(c, "Invalid search offset"))?;
        }
        let lookup = |c| global(c, "Error looking up string");
        let mut char_pos;
        if last {
            if offset >= 0 {
                char_pos = src.last_match().map_err(lookup)?;
                if char_pos < offset_pos {
                    char_pos = usearch::DONE;
                }
            } else {
                let mut prev = usearch::DONE;
                loop {
                    char_pos = src.next_match().map_err(lookup)?;
                    if char_pos == usearch::DONE || char_pos > offset_pos {
                        char_pos = prev;
                        break;
                    }
                    prev = char_pos;
                    let next = src.following(char_pos);
                    if next == ubrk::DONE {
                        break;
                    }
                    src.set_offset(next)
                        .map_err(|c| global(c, "Invalid search offset"))?;
                }
            }
        } else {
            char_pos = src.next_match().map_err(lookup)?;
        }
        if char_pos != usearch::DONE && src.is_boundary(char_pos) {
            Some(char_pos)
        } else {
            None
        }
    };
    let Some(char_pos) = char_pos else {
        return Ok(None);
    };
    let cp = usize::try_from(char_pos).unwrap_or(0);
    let ret = count_graphemes(&mut bi, &uh[..cp]);
    let uchar_pos = i32::try_from(String::from_utf16_lossy(&uh[..cp]).chars().count()).unwrap_or(0);
    Ok(Some((ret, uchar_pos)))
}

fn memmem(h: &[u8], n: &[u8], from: usize) -> Option<usize> {
    if from > h.len() {
        return None;
    }
    if n.is_empty() {
        return Some(from);
    }
    h[from..]
        .windows(n.len())
        .position(|w| w == n)
        .map(|p| p + from)
}

/// `grapheme_strlen($string)` (`Ok(None)` never; conversion failures are
/// intl errors returning `null`).
pub fn strlen(s: &[u8]) -> Result<i64> {
    if ascii_check(s).is_some() {
        return Ok(i64::try_from(s.len()).unwrap_or(i64::MAX));
    }
    let u = ustr::to_u16(s).map_err(|c| global(c, "Error converting input string to UTF-16"))?;
    let mut bi = char_iter().map_err(|c| global(c, "Error converting input string to UTF-16"))?;
    Ok(i64::from(count_graphemes(&mut bi, &u)))
}

fn check_offset(offset: i64, len: usize) -> Result<i32> {
    if outside_string(offset, len) {
        return Err(contained_error());
    }
    Ok(i32::try_from(offset).unwrap_or(0))
}

/// `grapheme_strpos()` (`Ok(None)` → `false`).
pub fn strpos(h: &[u8], n: &[u8], offset: i64, locale: &str) -> Result<Option<i64>> {
    let off = check_offset(offset, h.len())?;
    if off >= 0 && ascii_check(h).is_some() {
        let noff = usize::try_from(off).unwrap_or(0);
        if let Some(f) = memmem(h, n, noff) {
            return Ok(Some(i64::try_from(f).unwrap_or(0)));
        }
        if ascii_check(n).is_some() {
            return Ok(None);
        }
    }
    Ok(strpos_utf16(h, n, off, false, false, locale)?.map(|(p, _)| i64::from(p)))
}

/// `grapheme_stripos()`.
pub fn stripos(h: &[u8], n: &[u8], offset: i64, locale: &str) -> Result<Option<i64>> {
    let off = check_offset(offset, h.len())?;
    if ascii_check(h).is_some() {
        let noff = if off >= 0 {
            off
        } else {
            i32::try_from(h.len()).unwrap_or(0) + off
        };
        let hl = h.to_ascii_lowercase();
        let nl = n.to_ascii_lowercase();
        if let Some(f) = memmem(&hl, &nl, usize::try_from(noff).unwrap_or(0)) {
            return Ok(Some(i64::try_from(f).unwrap_or(0)));
        }
        if ascii_check(n).is_some() {
            return Ok(None);
        }
    }
    Ok(strpos_utf16(h, n, off, true, false, locale)?.map(|(p, _)| i64::from(p)))
}

/// `grapheme_strrpos_ascii()`.
fn strrpos_ascii(h: &[u8], n: &[u8], offset: i32) -> Option<i64> {
    let hl = i64::try_from(h.len()).unwrap_or(i64::MAX);
    let nl = i64::try_from(n.len()).unwrap_or(i64::MAX);
    let (p, e) = if offset >= 0 {
        (i64::from(offset), hl - nl)
    } else if nl > -i64::from(offset) {
        (0, hl - nl)
    } else {
        (0, hl + i64::from(offset))
    };
    let mut e = e;
    while e >= p {
        let eu = usize::try_from(e).ok()?;
        if h.get(eu..eu + n.len()) == Some(n) {
            return Some(e - p + if offset > 0 { i64::from(offset) } else { 0 });
        }
        e -= 1;
    }
    None
}

/// `grapheme_strrpos()`.
pub fn strrpos(h: &[u8], n: &[u8], offset: i64, locale: &str) -> Result<Option<i64>> {
    let off = check_offset(offset, h.len())?;
    if ascii_check(h).is_some() {
        if let Some(p) = strrpos_ascii(h, n, off) {
            return Ok(Some(p));
        }
        if ascii_check(n).is_some() {
            return Ok(None);
        }
    }
    Ok(strpos_utf16(h, n, off, false, true, locale)?.map(|(p, _)| i64::from(p)))
}

/// `grapheme_strripos()`.
pub fn strripos(h: &[u8], n: &[u8], offset: i64, locale: &str) -> Result<Option<i64>> {
    let off = check_offset(offset, h.len())?;
    if ascii_check(h).is_some() {
        if let Some(p) = strrpos_ascii(&h.to_ascii_lowercase(), &n.to_ascii_lowercase(), off) {
            return Ok(Some(p));
        }
        if ascii_check(n).is_some() {
            return Ok(None);
        }
    }
    Ok(strpos_utf16(h, n, off, true, true, locale)?.map(|(p, _)| i64::from(p)))
}

/// `grapheme_substr($string, $offset, $length)`.
pub fn substr(s: &[u8], start: i64, length: Option<i64>) -> Result<Vec<u8>> {
    let mut start = i32::try_from(start).map_err(|_| Error::arg_value(2, "is too large"))?;
    let length = length.unwrap_or(i64::try_from(s.len()).unwrap_or(i64::MAX));
    let mut length = i32::try_from(length).map_err(|_| Error::arg_value(3, "is too large"))?;
    if ascii_check(s).is_some() {
        let len = i32::try_from(s.len())
            .map_err(|_| global(UErrorCode::ILLEGAL_ARGUMENT_ERROR, "invalid parameters"))?;
        let mut f = start;
        if f < 0 {
            f = (len + f).max(0);
        } else if f > len {
            f = len;
        }
        let mut l = length;
        if l < 0 {
            l = ((len - f) + l).max(0);
        } else if l > len - f {
            l = len - f;
        }
        let (f, l) = (
            usize::try_from(f).unwrap_or(0),
            usize::try_from(l).unwrap_or(0),
        );
        return Ok(s[f..f + l].to_vec());
    }
    let u: Arc<[u16]> = Arc::from(
        ustr::to_u16(s).map_err(|c| global(c, "Error converting input string to UTF-16"))?,
    );
    let mut bi = char_iter().map_err(Error::code)?;
    let _ = bi.set_text_u16(u.clone());
    let mut start_pos = 0;
    let back = start < 0;
    if back {
        bi.last();
    }
    while start != 0 {
        start_pos = if back {
            bi.previous()
        } else {
            bi.next_boundary()
        };
        if start_pos == ubrk::DONE {
            break;
        }
        start += if back { 1 } else { -1 };
    }
    if start != 0 {
        if start > 0 {
            return Ok(Vec::new());
        }
        start_pos = 0;
        bi.first();
    }
    let sp = usize::try_from(start_pos).unwrap_or(0);
    let conv = |x: &[u16]| -> Result<Vec<u8>> {
        ustr::from_u16(x)
            .map(String::into_bytes)
            .map_err(|c| global(c, "Error converting output string to UTF-8"))
    };
    if i64::from(length) >= i64::try_from(s.len()).unwrap_or(i64::MAX) {
        return conv(&u[sp..]);
    }
    if length == 0 {
        return Ok(Vec::new());
    }
    let back = length < 0;
    if back {
        bi.last();
    }
    let mut end_pos = 0;
    while length != 0 {
        end_pos = if back {
            bi.previous()
        } else {
            bi.next_boundary()
        };
        if end_pos == ubrk::DONE {
            break;
        }
        length += if back { 1 } else { -1 };
    }
    if end_pos == ubrk::DONE {
        if length < 0 {
            return Ok(Vec::new());
        }
        end_pos = i32::try_from(u.len()).unwrap_or(0);
    }
    let ep = usize::try_from(end_pos).unwrap_or(0);
    if sp > ep {
        return Ok(Vec::new());
    }
    conv(&u[sp..ep])
}

/// `U8_FWD_N` over valid UTF-8.
fn u8_fwd_n(s: &[u8], n: i32) -> usize {
    let mut i = 0;
    let mut k = 0;
    while k < n && i < s.len() {
        i += match s[i] {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            _ => 4,
        };
        k += 1;
    }
    i.min(s.len())
}

/// `grapheme_strstr()` / `grapheme_stristr()` (`Ok(None)` → `false`).
pub fn strstr(
    h: &[u8],
    n: &[u8],
    before: bool,
    ignore_case: bool,
    locale: &str,
) -> Result<Option<Vec<u8>>> {
    if !ignore_case && ascii_check(h).is_some() && ascii_check(n).is_some() {
        let Some(f) = memmem(h, n, 0) else {
            return Ok(None);
        };
        return Ok(Some(if before {
            h[..f].to_vec()
        } else {
            h[f..].to_vec()
        }));
    }
    let Some((_, uchar_pos)) = strpos_utf16(h, n, 0, ignore_case, false, locale)? else {
        return Ok(None);
    };
    let p = u8_fwd_n(h, uchar_pos);
    Ok(Some(if before {
        h[..p].to_vec()
    } else {
        h[p..].to_vec()
    }))
}

/// `U8_FWD_1` with ICU's well-formedness checks.
fn u8_fwd_1(text: &[u8], at: usize) -> usize {
    let lead = text[at];
    let mut j = at + 1;
    let need = match lead {
        0xc2..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf4 => 3,
        _ => 0,
    };
    for k in 0..need {
        let Some(&t) = text.get(j) else { break };
        let ok = if k == 0 {
            match lead {
                0xe0 => (0xa0..=0xbf).contains(&t),
                0xed => (0x80..=0x9f).contains(&t),
                0xf0 => (0x90..=0xbf).contains(&t),
                0xf4 => (0x80..=0x8f).contains(&t),
                _ => (0x80..=0xbf).contains(&t),
            }
        } else {
            (0x80..=0xbf).contains(&t)
        };
        if !ok {
            break;
        }
        j += 1;
    }
    j
}

/// The outcome of `grapheme_extract()`: the result and the value to
/// assign to `$next` (when passed), which php sets even on failure.
pub type ExtractOutcome = (Result<Vec<u8>>, Option<i64>);

/// `grapheme_extract($haystack, $size, $type, $offset, &$next)`.
#[allow(clippy::too_many_lines)]
pub fn extract(s: &[u8], size: i64, ty: i64, start: i64) -> ExtractOutcome {
    let len = i64::try_from(s.len()).unwrap_or(i64::MAX);
    let lstart = if start < 0 { start + len } else { start };
    let next0 = Some(lstart);
    if !(extract_type::COUNT..=extract_type::MAXCHARS).contains(&ty) {
        return (
            Err(Error::arg_value(3, "must be one of GRAPHEME_EXTR_COUNT, GRAPHEME_EXTR_MAXBYTES, or GRAPHEME_EXTR_MAXCHARS")),
            next0,
        );
    }
    if lstart > i64::from(i32::MAX) || lstart < 0 || lstart >= len {
        return (
            Err(global(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "start not contained in string",
            )),
            next0,
        );
    }
    if size < 0 {
        return (
            Err(Error::arg_value(2, "must be greater than or equal to 0")),
            next0,
        );
    }
    if size > i64::from(i32::MAX) {
        return (Err(Error::arg_value(2, "is too large")), next0);
    }
    if size == 0 {
        return (Ok(Vec::new()), next0);
    }
    let mut start = usize::try_from(lstart).unwrap_or(0);
    let is_single_or_lead = |b: u8| b < 0x80 || (0xc2..=0xf4).contains(&b);
    while !is_single_or_lead(s[start]) {
        start += 1;
        if start >= s.len() {
            return (
                Err(global(
                    UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                    "grapheme_extract: invalid input string",
                )),
                next0,
            );
        }
    }
    let pstr = &s[start..];
    let size_u = usize::try_from(size).unwrap_or(usize::MAX);
    let check_len = (size_u.saturating_add(1)).min(pstr.len());
    if ascii_check(&pstr[..check_len]).is_some() {
        let nsize = size_u.min(pstr.len());
        return (
            Ok(pstr[..nsize].to_vec()),
            Some(i64::try_from(start + nsize).unwrap_or(0)),
        );
    }
    let bytes: Arc<[u8]> = Arc::from(pstr);
    let Ok(mut bi) = char_iter() else {
        return (Ok(Vec::new()), next0);
    };
    if bi.set_text(bytes).is_err() {
        return (
            Err(global(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "Error opening UTF-8 text",
            )),
            next0,
        );
    }
    let size32 = i32::try_from(size).unwrap_or(i32::MAX);
    let ret_pos: i32 = match ty {
        extract_type::COUNT => {
            let (mut n, mut ret) = (size32, 0);
            while n > 0 {
                let p = bi.next_boundary();
                if p == ubrk::DONE {
                    break;
                }
                ret = p;
                n -= 1;
            }
            ret
        }
        extract_type::MAXBYTES => {
            let mut ret = 0;
            loop {
                let p = bi.next_boundary();
                if p == ubrk::DONE || p > size32 {
                    break;
                }
                ret = p;
            }
            ret
        }
        _ => {
            let (mut ret, mut count) = (0usize, 0i32);
            loop {
                let p = bi.next_boundary();
                if p == ubrk::DONE {
                    break;
                }
                let pu = usize::try_from(p).unwrap_or(0);
                let mut bp = ret;
                while bp < pu {
                    count += 1;
                    bp = u8_fwd_1(pstr, bp);
                }
                if count > size32 {
                    break;
                }
                ret = bp;
            }
            i32::try_from(ret).unwrap_or(0)
        }
    };
    let rp = usize::try_from(ret_pos).unwrap_or(0);
    (
        Ok(pstr[..rp].to_vec()),
        Some(i64::try_from(start + rp).unwrap_or(0)),
    )
}

/// `grapheme_str_split($string, $length)`.
pub fn str_split(s: &[u8], split_len: i64) -> Result<Vec<Vec<u8>>> {
    let max = i64::from(u32::MAX / 4);
    if split_len <= 0 || split_len > max {
        return Err(Error::arg_value(
            2,
            format!("must be greater than 0 and less than or equal to {max}"),
        ));
    }
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let mut bi = char_iter().map_err(Error::code)?;
    bi.set_text(Arc::from(s))
        .map_err(|(c, _)| global(c, "Error opening UTF-8 text"))?;
    let mut out = Vec::new();
    let (mut pstr, mut end, mut i, mut current, mut pos, mut end_len) =
        (0usize, 0usize, 0i64, 0i32, 0i32, 0i32);
    while pos != ubrk::DONE {
        end_len = pos - current;
        pos = bi.next_boundary();
        if i == split_len - 1 {
            if pos != ubrk::DONE {
                let n = usize::try_from(pos - current).unwrap_or(0);
                out.push(s[pstr..pstr + n].to_vec());
                end = pstr + n;
                i = 0;
                pstr += n;
                current = pos;
            }
        } else {
            i += 1;
        }
    }
    if i != 0 && end_len != 0 {
        let n = usize::try_from(end_len).unwrap_or(0);
        out.push(s[end..end + n].to_vec());
    }
    Ok(out)
}

/// `grapheme_strrev($string)`.
pub fn strrev(s: &[u8]) -> Result<Vec<u8>> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    if Utf8Text::open(Arc::from(s)).is_err() {
        return Err(global(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "Error opening UTF-8 text",
        ));
    }
    let mut bi = char_iter().map_err(Error::code)?;
    bi.set_text(Arc::from(s))
        .map_err(|(c, _)| global(c, "Error opening UTF-8 text"))?;
    let mut out = Vec::with_capacity(s.len());
    let mut pos = bi.last();
    if pos == ubrk::DONE {
        return Ok(out);
    }
    let mut current = s.len();
    while pos != ubrk::DONE {
        pos = bi.previous();
        if pos == ubrk::DONE {
            break;
        }
        let p = usize::try_from(pos).unwrap_or(0);
        out.extend_from_slice(&s[p..current]);
        current = p;
    }
    Ok(out)
}

/// `grapheme_levenshtein($s1, $s2, $ins, $rep, $del, $locale)`.
pub fn levenshtein(
    s1: &[u8],
    s2: &[u8],
    ins: i64,
    rep: i64,
    del: i64,
    locale: &str,
) -> Result<i64> {
    let max = i64::from(u32::MAX / 4);
    for (arg, v) in [(3, ins), (4, rep), (5, del)] {
        if v <= 0 || v > max {
            return Err(Error::arg_value(
                arg,
                format!("must be greater than 0 and less than or equal to {max}"),
            ));
        }
    }
    let (mut a, mut b) = (s1, s2);
    if a.len() < b.len() && ins == rep && rep == del {
        std::mem::swap(&mut a, &mut b);
    }
    let conv = |x: &[u8]| {
        ustr::to_u16(x).map_err(|c| global(c, "Error converting input string to UTF-16"))
    };
    let ua: Arc<[u16]> = Arc::from(conv(a)?);
    let ub: Arc<[u16]> = Arc::from(conv(b)?);
    let bounds = |u: &Arc<[u16]>| -> Result<Vec<usize>> {
        let mut bi = char_iter().map_err(Error::code)?;
        bi.set_text_u16(u.clone()).map_err(Error::code)?;
        let mut v = vec![0usize];
        loop {
            let p = bi.next_boundary();
            if p == ubrk::DONE {
                break;
            }
            v.push(usize::try_from(p).unwrap_or(0));
        }
        Ok(v)
    };
    let ba = bounds(&ua)?;
    let bb = bounds(&ub)?;
    let (n1, n2) = (ba.len() - 1, bb.len() - 1);
    if n1 == 0 {
        return Ok(i64::try_from(n2).unwrap_or(0) * ins);
    }
    if n2 == 0 {
        return Ok(i64::try_from(n1).unwrap_or(0) * del);
    }
    let (coll, _) = Collator::open(locale).map_err(|c| global(c, "Error on ucol_open"))?;
    let mut p1: Vec<i64> = (0..=n2)
        .map(|i| i64::try_from(i).unwrap_or(0) * ins)
        .collect();
    let mut p2 = vec![0i64; n2 + 1];
    for i1 in 0..n1 {
        p2[0] = p1[0] + del;
        let ga = &ua[ba[i1]..ba[i1 + 1]];
        for i2 in 0..n2 {
            let gb = &ub[bb[i2]..bb[i2 + 1]];
            let mut c0 = if coll.strcoll(ga, gb) == 0 {
                p1[i2]
            } else {
                p1[i2] + rep
            };
            let c1 = p1[i2 + 1] + del;
            if c1 < c0 {
                c0 = c1;
            }
            let c2 = p2[i2] + ins;
            if c2 < c0 {
                c0 = c2;
            }
            p2[i2 + 1] = c0;
        }
        std::mem::swap(&mut p1, &mut p2);
    }
    Ok(p1[n2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert_eq!(strlen("a\u{0301}e".as_bytes()).unwrap(), 2);
        assert_eq!(strlen(b"abc").unwrap(), 3);
        assert_eq!(strpos("añob".as_bytes(), b"b", 0, "").unwrap(), Some(3));
        assert_eq!(
            stripos("AÑOB".as_bytes(), "ñ".as_bytes(), 0, "").unwrap(),
            Some(1)
        );
        assert_eq!(strrpos("aba".as_bytes(), b"a", 0, "").unwrap(), Some(2));
        assert_eq!(
            substr("ñañaña".as_bytes(), 1, Some(2)).unwrap(),
            "aña".as_bytes()[..3].to_vec()
        );
        assert_eq!(
            strstr("añob".as_bytes(), "o".as_bytes(), false, false, "")
                .unwrap()
                .unwrap(),
            b"ob"
        );
        assert_eq!(strrev("añ😀".as_bytes()).unwrap(), "😀ña".as_bytes());
        assert_eq!(
            str_split("añb".as_bytes(), 2).unwrap(),
            vec!["añ".as_bytes().to_vec(), b"b".to_vec()]
        );
        assert_eq!(levenshtein(b"kitten", b"sitting", 1, 1, 1, "").unwrap(), 3);
        let (r, next) = extract("añb".as_bytes(), 2, 0, 0);
        assert_eq!((r.unwrap(), next), ("añ".as_bytes().to_vec(), Some(3)));
    }
}
