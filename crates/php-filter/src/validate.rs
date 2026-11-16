// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The validation filters of `logical_filters.c`: int, boolean, float,
//! regexp, domain, url, ip and mac (email lives in [`crate::email`]).
//!
//! Every validator returns `Ok(Some(value))` on success, `Ok(None)` when
//! validation failed (`RETURN_VALIDATION_FAILED`; the caller turns that into
//! `false`, `null` or a `Filter\FilterFailedException`) and `Err` when an
//! exception was thrown (option `ValueError`s).

use crate::constants::{
    FILTER_FLAG_ALLOW_HEX, FILTER_FLAG_ALLOW_OCTAL, FILTER_FLAG_ALLOW_THOUSAND,
    FILTER_FLAG_GLOBAL_RANGE, FILTER_FLAG_HOSTNAME, FILTER_FLAG_IPV4, FILTER_FLAG_IPV6,
    FILTER_FLAG_NO_PRIV_RANGE, FILTER_FLAG_NO_RES_RANGE, FILTER_FLAG_PATH_REQUIRED,
    FILTER_FLAG_QUERY_REQUIRED,
};
use crate::host::{FilterHost, UriComponents, UriParseResult};
use crate::sanitize::URL_ALLOWED;
use php_runtime::numeric::{is_numeric_string_ex, Number};
use php_runtime::{RtResult, RuntimeError, Value, ZArray, ZStr};

/// The byte at `i`, or NUL past the end (C strings are NUL-terminated and
/// several validators peek one byte beyond their range).
#[inline]
pub(crate) fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

#[inline]
fn is_trim_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | 0x0b | b'\n')
}

/// `PHP_FILTER_TRIM_DEFAULT_EX` without the early return: the trimmed
/// range `(start, end)` (note: no `\f` and no NUL).
pub(crate) fn trim_range(s: &[u8]) -> (usize, usize) {
    let mut start = 0;
    while start < s.len() && is_trim_ws(s[start]) {
        start += 1;
    }
    let mut end = s.len();
    while end > start && is_trim_ws(s[end - 1]) {
        end -= 1;
    }
    (start, end)
}

fn option<'a>(opts: Option<&'a ZArray>, name: &str) -> Option<&'a Value> {
    opts.and_then(|o| o.get_str(name.as_bytes()))
}

/// `FETCH_LONG_OPTION`.
pub(crate) fn long_option<H: FilterHost + ?Sized>(
    host: &mut H,
    opts: Option<&ZArray>,
    name: &str,
) -> RtResult<Option<i64>> {
    option(opts, name).map(|v| v.to_long(host)).transpose()
}

/// `FETCH_DOUBLE_OPTION`.
pub(crate) fn double_option<H: FilterHost + ?Sized>(
    host: &mut H,
    opts: Option<&ZArray>,
    name: &str,
) -> RtResult<Option<f64>> {
    option(opts, name).map(|v| v.to_double(host)).transpose()
}

/// `FETCH_STRING_OPTION` / `FETCH_STR_OPTION`: only string values count.
pub(crate) fn string_option(opts: Option<&ZArray>, name: &str) -> Option<ZStr> {
    match option(opts, name)?.deref() {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// int
// ---------------------------------------------------------------------------

/// `php_filter_parse_int`. `Some(None)` is the "+0"/"-0" special case, which
/// succeeds without storing a value (the caller's 0 is kept).
fn parse_int(s: &[u8]) -> Option<i64> {
    let mut i = 0;
    let negative = match at(s, 0) {
        b'-' => {
            i = 1;
            true
        }
        b'+' => {
            i = 1;
            false
        }
        _ => false,
    };
    if at(s, i) == b'0' && i + 1 == s.len() {
        return Some(0);
    }
    let first = at(s, i);
    if i >= s.len() || !(b'1'..=b'9').contains(&first) {
        return None;
    }
    let mut value = i64::from(first - b'0');
    if negative {
        value = -value;
    }
    i += 1;
    // MAX_LENGTH_OF_LONG - 1 == 19 on 64-bit platforms.
    if s.len() - i > 19 {
        return None;
    }
    for &c in &s[i..] {
        if !c.is_ascii_digit() {
            return None;
        }
        let digit = i64::from(c - b'0');
        if !negative && value <= (i64::MAX - digit) / 10 {
            value = value * 10 + digit;
        } else if negative && value >= (i64::MIN + digit) / 10 {
            value = value * 10 - digit;
        } else {
            return None;
        }
    }
    Some(value)
}

/// `php_filter_parse_octal` (wrapping into a signed long like the C cast).
#[allow(clippy::cast_possible_wrap)]
fn parse_octal(s: &[u8]) -> Option<i64> {
    let mut v: u64 = 0;
    for &c in s {
        if !(b'0'..=b'7').contains(&c) {
            return None;
        }
        v = v.checked_mul(8)?.checked_add(u64::from(c - b'0'))?;
    }
    Some(v as i64)
}

/// `php_filter_parse_hex` (wrapping into a signed long like the C cast).
#[allow(clippy::cast_possible_wrap)]
pub(crate) fn parse_hex(s: &[u8]) -> Option<i64> {
    let mut v: u64 = 0;
    for &c in s {
        let n = char::from(c).to_digit(16)?;
        v = v.checked_mul(16)?.checked_add(u64::from(n))?;
    }
    Some(v as i64)
}

/// `php_filter_int`: `FILTER_VALIDATE_INT`.
pub fn filter_int<H: FilterHost + ?Sized>(
    host: &mut H,
    s: &[u8],
    flags: i64,
    opts: Option<&ZArray>,
) -> RtResult<Option<Value>> {
    let min = long_option(host, opts, "min_range")?;
    let max = long_option(host, opts, "max_range")?;
    if s.is_empty() {
        return Ok(None);
    }
    let (mut p, end) = trim_range(s);
    if p >= end {
        return Ok(None);
    }
    let value = if s[p] == b'0' {
        p += 1;
        if flags & FILTER_FLAG_ALLOW_HEX != 0 && p < end && matches!(s[p], b'x' | b'X') {
            p += 1;
            if p == end {
                return Ok(None);
            }
            parse_hex(&s[p..end])
        } else if flags & FILTER_FLAG_ALLOW_OCTAL != 0 {
            if p < end && matches!(s[p], b'o' | b'O') {
                p += 1;
                if p == end {
                    return Ok(None);
                }
            }
            parse_octal(&s[p..end])
        } else if p != end {
            None
        } else {
            Some(0)
        }
    } else {
        parse_int(&s[p..end])
    };
    Ok(match value {
        Some(v) if min.is_none_or(|m| v >= m) && max.is_none_or(|m| v <= m) => Some(Value::Int(v)),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// boolean
// ---------------------------------------------------------------------------

/// `php_filter_boolean`: `FILTER_VALIDATE_BOOL`. `"1"`, `"true"`, `"on"`,
/// `"yes"` are true; `"0"`, `"false"`, `"off"`, `"no"` and `""` are false
/// (ASCII case-insensitive, surrounding whitespace ignored).
#[must_use]
pub fn filter_boolean(s: &[u8]) -> Option<Value> {
    let (start, end) = trim_range(s);
    let t = &s[start..end];
    let ret = if t.is_empty() || t == b"0" {
        Some(false)
    } else if t == b"1"
        || ["on", "yes", "true"]
            .iter()
            .any(|w| t.eq_ignore_ascii_case(w.as_bytes()))
    {
        Some(true)
    } else if ["no", "off", "false"]
        .iter()
        .any(|w| t.eq_ignore_ascii_case(w.as_bytes()))
    {
        Some(false)
    } else {
        None
    };
    ret.map(Value::Bool)
}

// ---------------------------------------------------------------------------
// float
// ---------------------------------------------------------------------------

/// `strchr(set, c)` on a C string: the terminating NUL always matches.
fn c_strchr(set: &[u8], c: u8) -> bool {
    c == 0 || set.iter().take_while(|&&b| b != 0).any(|&b| b == c)
}

/// `php_filter_float`: `FILTER_VALIDATE_FLOAT`.
#[allow(clippy::cast_precision_loss)]
pub fn filter_float<H: FilterHost + ?Sized>(
    host: &mut H,
    s: &[u8],
    flags: i64,
    opts: Option<&ZArray>,
    fname: &str,
) -> RtResult<Option<Value>> {
    let (mut p, end) = trim_range(s);
    if p >= end {
        return Ok(None);
    }
    let mut dec_sep = b'.';
    if let Some(d) = string_option(opts, "decimal") {
        if d.len() != 1 {
            return Err(RuntimeError::ValueError(format!(
                "{fname}(): \"decimal\" option must be one character long"
            )));
        }
        dec_sep = d.as_bytes()[0];
    }
    let tsd_sep: Vec<u8> = match string_option(opts, "thousand") {
        Some(t) if t.is_empty() => {
            return Err(RuntimeError::ValueError(format!(
                "{fname}(): \"thousand\" option must not be empty"
            )))
        }
        Some(t) => t.as_bytes().to_vec(),
        None => b"',.".to_vec(),
    };
    let min = double_option(host, opts, "min_range")?;
    let max = double_option(host, opts, "max_range")?;

    let mut num = Vec::with_capacity(end - p);
    if p < end && matches!(s[p], b'+' | b'-') {
        num.push(s[p]);
        p += 1;
    }
    let mut first = true;
    loop {
        let mut n = 0;
        while p < end && s[p].is_ascii_digit() {
            n += 1;
            num.push(s[p]);
            p += 1;
        }
        let c = at(s, p);
        if p == end || c == dec_sep || c == b'e' || c == b'E' {
            if !first && n != 3 {
                return Ok(None);
            }
            if at(s, p) == dec_sep {
                num.push(b'.');
                p += 1;
                while p < end && s[p].is_ascii_digit() {
                    num.push(s[p]);
                    p += 1;
                }
            }
            if matches!(at(s, p), b'e' | b'E') {
                num.push(at(s, p));
                p += 1;
                if p < end && matches!(s[p], b'+' | b'-') {
                    num.push(s[p]);
                    p += 1;
                }
                while p < end && s[p].is_ascii_digit() {
                    num.push(s[p]);
                    p += 1;
                }
            }
            break;
        }
        if flags & FILTER_FLAG_ALLOW_THOUSAND != 0 && c_strchr(&tsd_sep, c) {
            if if first { !(1..=3).contains(&n) } else { n != 3 } {
                return Ok(None);
            }
            first = false;
            p += 1;
        } else {
            return Ok(None);
        }
    }
    if p != end {
        return Ok(None);
    }
    let in_range = |d: f64| min.is_none_or(|m| d >= m) && max.is_none_or(|m| d <= m);
    Ok(match is_numeric_string_ex(&num, false).map(|n| n.value) {
        Some(Number::Int(l)) => {
            let d = l as f64;
            in_range(d).then_some(Value::Float(d))
        }
        Some(Number::Float(d)) => {
            let underflow =
                d == 0.0 && num.len() > 1 && num.iter().any(|c| (b'1'..=b'9').contains(c));
            (!underflow && d.is_finite() && in_range(d)).then_some(Value::Float(d))
        }
        None => None,
    })
}

// ---------------------------------------------------------------------------
// regexp
// ---------------------------------------------------------------------------

/// `php_filter_validate_regexp`: `FILTER_VALIDATE_REGEXP`.
pub fn validate_regexp<H: FilterHost + ?Sized>(
    host: &mut H,
    s: &ZStr,
    opts: Option<&ZArray>,
    fname: &str,
) -> RtResult<Option<Value>> {
    let Some(regexp) = string_option(opts, "regexp") else {
        return Err(RuntimeError::ValueError(format!(
            "{fname}(): \"regexp\" option is missing"
        )));
    };
    Ok(match host.regex_match(&regexp, s.as_bytes())? {
        Some(true) => Some(Value::Str(s.clone())),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// domain
// ---------------------------------------------------------------------------

/// `php_filter_validate_domain_ex`: RFC 1034/1123 length and label rules
/// (with `hostname`, labels must be alphanumeric with inner hyphens).
#[must_use]
pub fn validate_domain_ex(d: &[u8], hostname: bool) -> bool {
    let mut l = d.len();
    let mut e = l;
    // Ignore a trailing dot.
    if l > 0 && d[l - 1] == b'.' {
        e = l - 1;
        l -= 1;
    }
    // The total length cannot exceed 253 characters (final dot excluded).
    if l > 253 {
        return false;
    }
    // The first character must be alphanumeric.
    if at(d, 0) == b'.' || (hostname && !at(d, 0).is_ascii_alphanumeric()) {
        return false;
    }
    let mut label_len: u32 = 1;
    for s in 0..e {
        if d[s] == b'.' {
            // The first and last character of a label must be alphanumeric.
            if at(d, s + 1) == b'.'
                || (hostname
                    && (!d[s - 1].is_ascii_alphanumeric() || !at(d, s + 1).is_ascii_alphanumeric()))
            {
                return false;
            }
            label_len = 1;
        } else {
            if label_len > 63
                || (hostname
                    && (d[s] != b'-' || at(d, s + 1) == 0)
                    && !d[s].is_ascii_alphanumeric())
            {
                return false;
            }
            label_len += 1;
        }
    }
    true
}

/// `php_filter_validate_domain`: `FILTER_VALIDATE_DOMAIN`.
#[must_use]
pub fn validate_domain(s: &ZStr, flags: i64) -> Option<Value> {
    validate_domain_ex(s.as_bytes(), flags & FILTER_FLAG_HOSTNAME != 0)
        .then(|| Value::Str(s.clone()))
}

// ---------------------------------------------------------------------------
// url
// ---------------------------------------------------------------------------

/// `is_userinfo_valid`.
fn is_userinfo_valid(s: &[u8]) -> bool {
    const VALID: &[u8] = b"-._~!$&'()*+,;=:";
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c.is_ascii_alphanumeric() || c == 0 || VALID.contains(&c) {
            i += 1;
        } else if c == b'%' && at(s, i + 1).is_ascii_digit() && at(s, i + 2).is_ascii_hexdigit() {
            i += 3;
        } else {
            return false;
        }
    }
    true
}

/// `php_filter_is_valid_ipv6_hostname`: `[` IPv6 `]`.
fn is_valid_ipv6_hostname(h: &[u8]) -> bool {
    h.len() >= 2
        && h[0] == b'['
        && h[h.len() - 1] == b']'
        && validate_ipv6(&h[1..h.len() - 1], None)
}

/// The name of ext/uri's `parse_url()`-based parser (the default of the
/// `uri_parser_class` option).
pub const PARSE_URL_PARSER: &[u8] = b"parse_url";

/// `php_filter_validate_url`: `FILTER_VALIDATE_URL`.
pub fn validate_url<H: FilterHost + ?Sized>(
    host: &mut H,
    s: &ZStr,
    flags: i64,
    opts: Option<&ZArray>,
    fname: &str,
) -> RtResult<Option<Value>> {
    let bytes = s.as_bytes();
    // php_filter_url() must not remove anything.
    if bytes.iter().any(|&c| !URL_ALLOWED[usize::from(c)]) {
        return Ok(None);
    }
    let parser = string_option(opts, "uri_parser_class");
    let is_parse_url = parser
        .as_ref()
        .is_none_or(|p| p.as_bytes() == PARSE_URL_PARSER);
    let uri: UriComponents = if is_parse_url {
        match crate::url_parse::parse_url(bytes) {
            Some(u) => u.into(),
            None => return Ok(None),
        }
    } else {
        let name = parser.unwrap_or_default();
        match host.uri_parse(name.as_bytes(), bytes)? {
            UriParseResult::UnknownParser => {
                return Err(RuntimeError::ValueError(format!(
                    "{fname}(): \"uri_parser_class\" option has invalid value"
                )))
            }
            UriParseResult::Invalid => return Ok(None),
            UriParseResult::Parsed(u) => u,
        }
    };

    if let Some(scheme) = &uri.scheme {
        if scheme.eq_ignore_ascii_case(b"http") || scheme.eq_ignore_ascii_case(b"https") {
            let Some(h) = &uri.host else {
                return Ok(None);
            };
            if is_parse_url && !is_valid_ipv6_hostname(h) && !validate_domain_ex(h, true) {
                return Ok(None);
            }
        }
    }
    let Some(scheme) = &uri.scheme else {
        return Ok(None);
    };
    if (uri.host.is_none()
        && scheme.as_slice() != b"mailto"
        && scheme.as_slice() != b"news"
        && scheme.as_slice() != b"file")
        || (flags & FILTER_FLAG_PATH_REQUIRED != 0 && uri.path.is_none())
        || (flags & FILTER_FLAG_QUERY_REQUIRED != 0 && uri.query.is_none())
    {
        return Ok(None);
    }
    if is_parse_url
        && (uri.user.as_ref().is_some_and(|u| !is_userinfo_valid(u))
            || uri.password.as_ref().is_some_and(|p| !is_userinfo_valid(p)))
    {
        return Ok(None);
    }
    Ok(Some(Value::Str(s.clone())))
}

// ---------------------------------------------------------------------------
// ip
// ---------------------------------------------------------------------------

/// `_php_filter_validate_ipv4`: dotted quad without leading zeros.
#[must_use]
pub fn validate_ipv4(s: &[u8]) -> Option<[u32; 4]> {
    let mut ip = [0u32; 4];
    let mut n = 0;
    let mut i = 0;
    while i < s.len() {
        if !s[i].is_ascii_digit() {
            return None;
        }
        let leading_zero = s[i] == b'0';
        let mut digits = 1;
        let mut num = u32::from(s[i] - b'0');
        i += 1;
        while i < s.len() && s[i].is_ascii_digit() {
            num = num * 10 + u32::from(s[i] - b'0');
            digits += 1;
            if num > 255 || digits > 3 {
                return None;
            }
            i += 1;
        }
        // A leading 0 would introduce octal numbers, which are unsupported.
        if leading_zero && (num != 0 || digits > 1) {
            return None;
        }
        ip[n] = num;
        n += 1;
        if n == 4 {
            return (i == s.len()).then_some(ip);
        }
        if i >= s.len() || s[i] != b'.' {
            return None;
        }
        i += 1;
    }
    None
}

/// `_php_filter_validate_ipv6`. When `out` is given, it receives the eight
/// 16-bit groups (an embedded IPv4 address always yields `::ffff:a.b.c.d`,
/// like php-src).
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::too_many_lines
)]
pub fn validate_ipv6(s: &[u8], mut out: Option<&mut [i64; 8]>) -> bool {
    if !s.contains(&b':') {
        return false;
    }
    let mut compressed_pos: i64 = -1;
    let mut blocks: i64 = 0;
    let mut len = s.len();
    let mut ip4 = None;
    if let Some(dot) = s.iter().position(|&c| c == b'.') {
        let mut start = dot;
        while start > 0 && s[start - 1] != b':' {
            start -= 1;
        }
        let Some(v4) = validate_ipv4(&s[start..]) else {
            return false;
        };
        ip4 = Some(v4);
        len = start;
        if len < 2 {
            return false;
        }
        if s[start - 2] != b':' {
            // Don't include the ':' before the IPv4 part unless it's "::".
            len -= 1;
        }
        blocks = 2;
    }
    let set = |out: &mut Option<&mut [i64; 8]>, idx: i64, v: i64| {
        if let Some(o) = out.as_deref_mut() {
            if (0..8).contains(&idx) {
                o[idx as usize] = v;
            }
        }
    };
    let mut i = 0;
    'outer: {
        while i < len {
            if s[i] == b':' {
                i += 1;
                if i >= len {
                    // Cannot end in ':' without a previous ':'.
                    return false;
                }
                if s[i] == b':' {
                    if compressed_pos >= 0 {
                        return false;
                    }
                    set(&mut out, blocks, -1);
                    compressed_pos = blocks;
                    blocks += 1;
                    i += 1;
                    if i == len {
                        if blocks > 8 {
                            return false;
                        }
                        break 'outer;
                    }
                } else if i - 1 == 0 {
                    // A leading ':' must be followed by another ':'.
                    return false;
                }
            }
            let mut num: u32 = 0;
            let mut n = 0;
            while i < len {
                let Some(d) = char::from(s[i]).to_digit(16) else {
                    break;
                };
                num = num.wrapping_mul(16).wrapping_add(d);
                n += 1;
                i += 1;
            }
            set(&mut out, blocks, i64::from(num));
            if !(1..=4).contains(&n) {
                return false;
            }
            blocks += 1;
            if blocks > 8 {
                return false;
            }
        }
    }
    if let Some(o) = out {
        if let Some(v4) = ip4 {
            *o = [
                0,
                0,
                0,
                0,
                0,
                0xffff,
                i64::from(256 * v4[0] + v4[1]),
                i64::from(256 * v4[2] + v4[3]),
            ];
        } else if compressed_pos >= 0 && blocks <= 8 {
            let offset = 8 - blocks;
            let mut k = 7;
            while k > compressed_pos + offset {
                o[k as usize] = o[(k - offset) as usize];
                k -= 1;
            }
            let mut k = compressed_pos + offset;
            while k >= compressed_pos {
                o[k as usize] = 0;
                k -= 1;
            }
        }
    }
    (compressed_pos >= 0 && blocks <= 8) || blocks == 8
}

/// Range classification of RFC 6890 (`*_get_status_flags`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RangeFlags {
    global: bool,
    reserved: bool,
    private: bool,
}

/// `ipv4_get_status_flags`; `None` for an address in no special block.
#[allow(clippy::if_same_then_else)]
fn ipv4_status(ip: [u32; 4]) -> Option<RangeFlags> {
    let mut f = RangeFlags::default();
    let [a, b, c, _d] = ip;
    if a == 0 {
        f.reserved = true; // RFC 0791 - This network
    } else if a == 10 {
        f.private = true; // RFC 1918
    } else if a == 100 && (64..=127).contains(&b) {
        // RFC 6598 - Shared Address Space
    } else if a == 127 {
        f.reserved = true; // RFC 1122 - Loopback
    } else if a == 169 && b == 254 {
        f.reserved = true; // RFC 3927 - Link Local
    } else if a == 172 && (16..=31).contains(&b) {
        f.private = true; // RFC 1918
    } else if a == 192 && b == 0 && c == 0 {
        // RFC 6890 - IETF Protocol Assignments
    } else if a == 192 && b == 0 && c == 2 {
        // RFC 5737 - Documentation
    } else if a == 192 && b == 88 && c == 99 {
        f.global = true; // RFC 3068 - 6to4 Relay Anycast
    } else if a == 192 && b == 168 {
        f.private = true; // RFC 1918
    } else if a == 198 && (18..=19).contains(&b) {
        // RFC 2544 - Benchmarking
    } else if a == 198 && b == 51 && c == 100 {
        // RFC 5737 - Documentation
    } else if a == 203 && b == 0 && c == 113 {
        // RFC 5737 - Documentation
    } else if a >= 240 {
        f.reserved = true; // RFC 1122 - Reserved (incl. 255.255.255.255)
    } else {
        return None;
    }
    Some(f)
}

/// `ipv6_get_status_flags`; `None` for an address in no special block.
#[allow(clippy::if_same_then_else)]
fn ipv6_status(ip: &[i64; 8]) -> Option<RangeFlags> {
    let mut f = RangeFlags::default();
    if ip[..7].iter().all(|&g| g == 0) && (ip[7] == 0 || ip[7] == 1) {
        f.reserved = true; // RFC 4291 - Unspecified / Loopback
    } else if ip[0] == 0x0064 && ip[1] == 0xff9b {
        f.global = true; // RFC 6052 - IPv4-IPv6 Translation
    } else if ip[..5].iter().all(|&g| g == 0) && ip[5] == 0xffff {
        f.reserved = true; // RFC 4291 - IPv4-mapped Address
    } else if ip[0] == 0x0100 && ip[1] == 0 && ip[2] == 0 && ip[3] == 0 {
        // RFC 6666 - Discard-Only Address Block
    } else if ip[0] == 0x2001 && ip[1] <= 0x01ff {
        // RFC 4380 TEREDO, RFC 2928 IETF Protocol Assignments, RFC 5180
        // Benchmarking, RFC 4843 ORCHID
    } else if ip[0] == 0x2001 && ip[1] == 0x0db8 {
        // RFC 3849 - Documentation
    } else if ip[0] == 0x2002 {
        // RFC 3056 - 6to4
    } else if (0xfc00..=0xfdff).contains(&ip[0]) {
        f.private = true; // RFC 4193 - Unique-Local
    } else if (0xfe80..=0xfebf).contains(&ip[0]) {
        f.reserved = true; // RFC 4291 - Link-Scoped Unicast
    } else {
        return None;
    }
    Some(f)
}

/// `php_filter_validate_ip`: `FILTER_VALIDATE_IP`.
#[must_use]
pub fn validate_ip(s: &ZStr, flags: i64) -> Option<Value> {
    let b = s.as_bytes();
    let v6 = if b.contains(&b':') {
        true
    } else if b.contains(&b'.') {
        false
    } else {
        return None;
    };
    let want4 = flags & FILTER_FLAG_IPV4 != 0;
    let want6 = flags & FILTER_FLAG_IPV6 != 0;
    if !(want4 && want6) && ((want4 && v6) || (want6 && !v6)) {
        return None;
    }
    let status = if v6 {
        let mut ip = [0i64; 8];
        if !validate_ipv6(b, Some(&mut ip)) {
            return None;
        }
        ipv6_status(&ip)
    } else {
        ipv4_status(validate_ipv4(b)?)
    };
    let ok = match status {
        None => true,
        Some(f) => {
            !((flags & FILTER_FLAG_GLOBAL_RANGE != 0 && !f.global)
                || (flags & FILTER_FLAG_NO_PRIV_RANGE != 0 && f.private)
                || (flags & FILTER_FLAG_NO_RES_RANGE != 0 && f.reserved))
        }
    };
    ok.then(|| Value::Str(s.clone()))
}

// ---------------------------------------------------------------------------
// mac
// ---------------------------------------------------------------------------

/// `php_filter_validate_mac`: `FILTER_VALIDATE_MAC`.
pub fn validate_mac(s: &ZStr, opts: Option<&ZArray>, fname: &str) -> RtResult<Option<Value>> {
    let input = s.as_bytes();
    let exp_sep = string_option(opts, "separator");
    if exp_sep.as_ref().is_some_and(|e| e.len() != 1) {
        return Err(RuntimeError::ValueError(format!(
            "{fname}(): \"separator\" option must be one character long"
        )));
    }
    let (tokens, length, separator) = match input.len() {
        // EUI-64 format: four hex digits separated by dots.
        14 => (3, 4, b'.'),
        // IEEE 802 format: six pairs separated by hyphens or colons.
        17 if input[2] == b'-' => (6, 2, b'-'),
        17 if input[2] == b':' => (6, 2, b':'),
        _ => return Ok(None),
    };
    if exp_sep.is_some_and(|e| e.as_bytes()[0] != separator) {
        return Ok(None);
    }
    for i in 0..tokens {
        let offset = i * (length + 1);
        if i < tokens - 1 && input[offset + length] != separator {
            return Ok(None);
        }
        if parse_hex(&input[offset..offset + length]).is_none() {
            return Ok(None);
        }
    }
    Ok(Some(Value::Str(s.clone())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints() {
        assert_eq!(parse_int(b"123"), Some(123));
        assert_eq!(parse_int(b"-0"), Some(0));
        assert_eq!(parse_int(b"+0"), Some(0));
        assert_eq!(parse_int(b"00"), None);
        assert_eq!(parse_int(b"9223372036854775807"), Some(i64::MAX));
        assert_eq!(parse_int(b"9223372036854775808"), None);
        assert_eq!(parse_int(b"-9223372036854775808"), Some(i64::MIN));
        assert_eq!(parse_int(b"-"), None);
        assert_eq!(parse_hex(b"ffffffffffffffff"), Some(-1));
        assert_eq!(parse_hex(b"1ffffffffffffffff"), None);
        assert_eq!(parse_octal(b"777"), Some(511));
    }

    #[test]
    fn ipv6() {
        let mut ip = [0; 8];
        assert!(validate_ipv6(b"::1", Some(&mut ip)));
        assert_eq!(ip, [0, 0, 0, 0, 0, 0, 0, 1]);
        assert!(validate_ipv6(b"fe80::1:2", Some(&mut ip)));
        assert_eq!(ip, [0xfe80, 0, 0, 0, 0, 0, 1, 2]);
        assert!(validate_ipv6(b"::ffff:1.2.3.4", Some(&mut ip)));
        assert_eq!(ip, [0, 0, 0, 0, 0, 0xffff, 0x0102, 0x0304]);
        assert!(!validate_ipv6(b":1::2", None));
        assert!(!validate_ipv6(b"1::2::3", None));
        assert!(!validate_ipv6(b"1:2:3:4:5:6:7:8:9", None));
        assert!(validate_ipv6(b"1:2:3:4:5:6:7::", None));
        assert!(!validate_ipv6(b"1:", None));
        assert!(validate_ipv4(b"0.0.0.0").is_some());
        assert!(validate_ipv4(b"01.0.0.0").is_none());
        assert!(validate_ipv4(b"256.0.0.0").is_none());
        assert!(validate_ipv4(b"1.2.3").is_none());
        assert!(validate_ipv4(b"1.2.3.4.").is_none());
    }

    #[test]
    fn domains() {
        assert!(validate_domain_ex(b"example.com", true));
        assert!(validate_domain_ex(b"example.com.", true));
        assert!(!validate_domain_ex(b"-example.com", true));
        assert!(!validate_domain_ex(b"a..b", false));
        assert!(validate_domain_ex(b"_a.b", false));
        assert!(!validate_domain_ex(b"_a.b", true));
        assert!(validate_domain_ex(b"", false));
        assert!(!validate_domain_ex(b"", true));
        assert!(!validate_domain_ex(b".", false));
        assert!(validate_domain_ex(&[b'a'; 63], true));
        assert!(!validate_domain_ex(&[b'a'; 64], true));
    }

    #[test]
    fn bools() {
        assert!(matches!(filter_boolean(b" YES "), Some(Value::Bool(true))));
        assert!(matches!(filter_boolean(b""), Some(Value::Bool(false))));
        assert!(matches!(filter_boolean(b"Off"), Some(Value::Bool(false))));
        assert!(filter_boolean(b"2").is_none());
        assert!(filter_boolean(b"\x0ctrue").is_none());
    }
}
