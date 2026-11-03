//! Host parsing (domain, IPv4, IPv6, opaque) and host serialization, after
//! lexbor's `lxb_url_host_parse` and friends.

use super::encode::{percent_encode_into, MAP_C0, URL_MAP};
use super::idna;
use super::parser::{Fail, Log, SharedBuf};
use super::Host;
use crate::UrlValidationErrorType as E;

/// `LXB_URL_HOST_OPT_*`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HostOpt {
    pub not_special: bool,
    pub decode: bool,
    pub idna: bool,
}

/// Forbidden host code points (`lxb_url_map_forbidden_host_cp`).
fn is_forbidden_host(c: u8) -> bool {
    matches!(
        c,
        0x00 | 0x09
            | 0x0A
            | 0x0D
            | b' '
            | b'#'
            | b'/'
            | b':'
            | b'<'
            | b'>'
            | b'?'
            | b'@'
            | b'['
            | b'\\'
            | b']'
            | b'^'
            | b'|'
    )
}

/// Forbidden domain code points (`lxb_url_map_forbidden_domain_cp`).
fn is_forbidden_domain(c: u8) -> bool {
    c < 0x20 || c == b'%' || c == 0x7F || is_forbidden_host(c)
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `lxb_url_host_parse`: parses `data[begin..end]`.
pub(crate) fn parse_host(
    log: &mut Log,
    data: &SharedBuf,
    begin: usize,
    end: usize,
    mut opt: HostOpt,
) -> Result<Host, Fail> {
    if begin < end && data[begin] == b'[' {
        if data[end - 1] != b']' {
            log.push(data, end - 1, E::Ipv6Unclosed);
            return Err(Fail);
        }
        return parse_ipv6(log, data, begin + 1, end - 1).map(Host::Ipv6);
    }
    if opt.not_special {
        return parse_opaque_host(log, data, begin, end);
    }
    let input = &data[begin..end];
    let mut domain: Vec<u8> = if opt.decode {
        let mut out = Vec::with_capacity(input.len());
        let mut i = 0;
        while i < input.len() {
            let c = input[i];
            i += 1;
            if c == b'%' {
                if let (Some(h), Some(l)) = (
                    input.get(i).copied().and_then(hex),
                    input.get(i + 1).copied().and_then(hex),
                ) {
                    let v = (h << 4) | l;
                    i += 2;
                    if v >= 0x80 {
                        opt.idna = true;
                    }
                    out.push(v);
                    continue;
                }
            }
            out.push(c);
        }
        out
    } else if opt.idna {
        input.to_vec()
    } else {
        input.to_ascii_lowercase()
    };
    if opt.idna {
        domain = idna::to_ascii(&domain).map_err(|_| Fail)?;
    }
    if domain.is_empty() {
        return Err(Fail);
    }
    if let Some(i) = domain
        .iter()
        .position(|&c| c < 128 && is_forbidden_domain(c))
    {
        log.push(&SharedBuf::new(&domain), i, E::DomainInvalidCodePoint);
        return Err(Fail);
    }
    if is_ipv4(&domain) {
        return parse_ipv4(log, &SharedBuf::new(&domain)).map(Host::Ipv4);
    }
    Ok(Host::Domain(domain))
}

fn parse_opaque_host(
    log: &mut Log,
    data: &SharedBuf,
    begin: usize,
    end: usize,
) -> Result<Host, Fail> {
    if begin == end {
        return Ok(Host::Empty);
    }
    if let Some(i) = data[begin..end]
        .iter()
        .position(|&c| c < 128 && is_forbidden_host(c))
    {
        log.push(data, begin + i, E::HostInvalidCodePoint);
        return Err(Fail);
    }
    let mut out = Vec::new();
    percent_encode_into(&data[begin..end], &mut out, &URL_MAP, MAP_C0, false);
    Ok(Host::Opaque(out))
}

/// Result of `lxb_url_ipv4_number_parse`.
enum Number {
    Decimal(u64),
    /// Hex or octal (a "non-decimal" warning).
    NonDecimal(u64),
    Invalid,
}

fn parse_ipv4_number(s: &[u8]) -> Number {
    if s.is_empty() {
        return Number::Invalid;
    }
    let mut data = s;
    let mut radix = 10u64;
    let mut non_decimal = false;
    if data.len() > 1 && data[0] == b'0' {
        if data[1] == b'x' || data[1] == b'X' {
            data = &data[2..];
            radix = 16;
        } else {
            data = &data[1..];
            radix = 8;
        }
        if data.is_empty() {
            return Number::NonDecimal(0);
        }
        non_decimal = true;
    }
    let mut n: u64 = 0;
    for &c in data {
        let d = match (radix, c) {
            (10, b'0'..=b'9') | (8, b'0'..=b'7') => c - b'0',
            (16, _) => match hex(c) {
                Some(v) => v,
                None => return Number::Invalid,
            },
            _ => return Number::Invalid,
        };
        n = u64::from(d) + n * radix;
        if n > u64::from(u32::MAX) {
            break;
        }
    }
    if non_decimal {
        Number::NonDecimal(n)
    } else {
        Number::Decimal(n)
    }
}

/// `lxb_url_is_ipv4`: whether the last part of the domain is numeric.
fn is_ipv4(data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }
    let mut end = data.len();
    let mut p = end;
    let mut all_digits = true;
    let mut first = true;
    loop {
        p -= 1;
        let c = data[p];
        if hex(c).is_none() {
            if c == b'.' {
                if p == end - 1 {
                    if first {
                        end = p;
                        first = false;
                        if p == 0 {
                            break;
                        }
                        continue;
                    }
                    return false;
                }
                p += 1;
                break;
            } else if c != b'x' && c != b'X' {
                return false;
            }
        }
        if !c.is_ascii_digit() {
            all_digits = false;
        }
        if p == 0 {
            break;
        }
    }
    if p == end {
        return false;
    }
    if all_digits {
        return true;
    }
    !matches!(parse_ipv4_number(&data[p..end]), Number::Invalid)
}

/// `lxb_url_ipv4_parse` on a domain string.
fn parse_ipv4(log: &mut Log, data: &SharedBuf) -> Result<u32, Fail> {
    /// Multipliers for the last part, indexed by `5 - parts`.
    const ST: [u64; 6] = [0, 256, 65_536, 16_777_216, 4_294_967_296, 0];
    if data.is_empty() {
        return Err(Fail);
    }
    let mut parts = [0u64; 5];
    let mut i = 0usize;
    let mut begin = 0usize;
    let mut append = |log: &mut Log, begin: usize, end: usize, i: usize| -> Result<(), Fail> {
        let v = match parse_ipv4_number(&data[begin..end]) {
            Number::Invalid => {
                log.push(data, begin, E::Ipv4NonNumericPart);
                return Err(Fail);
            }
            Number::NonDecimal(v) => {
                log.push(data, begin, E::Ipv4NonDecimalPart);
                v
            }
            Number::Decimal(v) => v,
        };
        parts[i] = v;
        if v > 255 {
            log.push(data, begin, E::Ipv4OutOfRangePart);
        }
        Ok(())
    };
    for p in 0..data.len() {
        if data[p] == b'.' {
            if i == 4 {
                log.push(data, begin, E::Ipv4TooManyParts);
                return Err(Fail);
            }
            i += 1;
            append(log, begin, p, i)?;
            begin = p + 1;
        }
    }
    if begin < data.len() {
        if i == 4 {
            log.push(data, begin, E::Ipv4TooManyParts);
            return Err(Fail);
        }
        i += 1;
        append(log, begin, data.len(), i)?;
    } else if data[data.len() - 1] == b'.' {
        log.push(data, begin, E::Ipv4EmptyPart);
    }
    if parts[i] >= ST[5 - i] {
        return Err(Fail);
    }
    let mut ip = parts[i];
    for (j, part) in parts.iter().enumerate().take(i).skip(1) {
        if *part > 255 {
            return Err(Fail);
        }
        ip += part * ST[4 - j];
    }
    u32::try_from(ip).map_err(|_| Fail)
}

/// `lxb_url_ipv6_parse` on `data[begin..end]` (without brackets).
pub(crate) fn parse_ipv6(
    log: &mut Log,
    data: &SharedBuf,
    begin: usize,
    end: usize,
) -> Result<[u16; 8], Fail> {
    let mut ipv6 = [0u16; 8];
    let mut piece = 0usize;
    let mut compress: Option<usize> = None;
    let mut p = begin;
    let fail = |log: &mut Log, p: usize, t: E| -> Result<[u16; 8], Fail> {
        log.push(data, p, t);
        Err(Fail)
    };
    if p < end {
        if data[p] == b':' {
            if p + 1 >= end || data[p + 1] != b':' {
                let at = if p + 1 >= end { p } else { p + 1 };
                return fail(log, at, E::Ipv6InvalidCompression);
            }
            p += 2;
            piece += 1;
            compress = Some(piece);
        }
        while p < end {
            if piece == 8 {
                return fail(log, p, E::Ipv6TooManyPieces);
            }
            if data[p] == b':' {
                if compress.is_some() {
                    return fail(log, p, E::Ipv6MultipleCompression);
                }
                p += 1;
                piece += 1;
                compress = Some(piece);
                continue;
            }
            let mut num: u16 = 0;
            let mut len = 0;
            while len < 4 && p < end {
                let Some(v) = hex(data[p]) else {
                    break;
                };
                num = (num << 4) | u16::from(v);
                p += 1;
                len += 1;
            }
            if p >= end {
                ipv6[piece] = num;
                piece += 1;
                break;
            }
            if data[p] == b'.' {
                if len == 0 {
                    return fail(log, p, E::Ipv4InIpv6InvalidCodePoint);
                }
                p -= len;
                if piece > 6 {
                    return fail(log, p, E::Ipv4InIpv6TooManyPieces);
                }
                p = parse_ipv4_in_ipv6(log, data, p, end, &mut ipv6, &mut piece)?;
                break;
            }
            if data[p] == b':' {
                p += 1;
                if p >= end {
                    return fail(log, p, E::Ipv6InvalidCodePoint);
                }
            } else {
                return fail(log, p, E::Ipv6InvalidCodePoint);
            }
            ipv6[piece] = num;
            piece += 1;
        }
    }
    if let Some(c) = compress {
        let mut num = piece - c;
        let mut i = 7;
        while i != 0 && num > 0 {
            ipv6.swap(c + num - 1, i);
            i -= 1;
            num -= 1;
        }
    } else if piece != 8 {
        return fail(log, p, E::Ipv6TooFewPieces);
    }
    Ok(ipv6)
}

/// `lxb_url_ipv4_in_ipv6_parse`: the dotted-decimal tail of an IPv6
/// address, written into `ipv6[*piece..]`. Returns the position after it.
fn parse_ipv4_in_ipv6(
    log: &mut Log,
    data: &SharedBuf,
    mut p: usize,
    end: usize,
    ipv6: &mut [u16; 8],
    piece: &mut usize,
) -> Result<usize, Fail> {
    let mut fail = |p: usize, t: E| -> Result<usize, Fail> {
        log.push(data, p, t);
        Err(Fail)
    };
    let mut seen = 0;
    while p < end {
        let mut value: i32 = -1;
        if seen > 0 {
            if data[p] == b'.' && seen < 4 {
                p += 1;
                if p >= end {
                    break;
                }
            } else {
                return fail(p, E::Ipv4InIpv6InvalidCodePoint);
            }
        }
        loop {
            let c = data[p];
            if !c.is_ascii_digit() {
                if value == -1 {
                    return fail(p, E::Ipv4InIpv6InvalidCodePoint);
                }
                break;
            }
            if value == -1 {
                value = i32::from(c - b'0');
            } else if value == 0 {
                return fail(p, E::Ipv4InIpv6InvalidCodePoint);
            } else {
                value = value * 10 + i32::from(c - b'0');
            }
            if value > 255 {
                return fail(p, E::Ipv4InIpv6OutOfRangePart);
            }
            p += 1;
            if p >= end {
                break;
            }
        }
        let v = u16::try_from(value).unwrap_or(0);
        ipv6[*piece] = ipv6[*piece].wrapping_mul(0x100).wrapping_add(v);
        seen += 1;
        if seen == 2 || seen == 4 {
            *piece += 1;
        }
    }
    if seen != 4 {
        return fail(p, E::Ipv4InIpv6TooFewParts);
    }
    Ok(p)
}

/// Serializes an IPv4 address.
pub fn serialize_ipv4(ip: u32) -> Vec<u8> {
    let o = ip.to_be_bytes();
    format!("{}.{}.{}.{}", o[0], o[1], o[2], o[3]).into_bytes()
}

/// Serializes an IPv6 address with lexbor's compression rules (no brackets).
pub fn serialize_ipv6(ipv6: &[u16; 8]) -> Vec<u8> {
    let mut count = 0usize;
    let mut tmp_count = usize::from(ipv6[0] == 0);
    let mut compress: Option<usize> = None;
    let mut tmp_compress = 0usize;
    for i in 1..8 {
        if ipv6[i] == 0 {
            if ipv6[i - 1] == 0 {
                tmp_count += 1;
            } else {
                tmp_count = 1;
                tmp_compress = i;
            }
        } else if tmp_count > count {
            compress = Some(tmp_compress);
            count = tmp_count;
        }
    }
    if tmp_count > count {
        compress = Some(tmp_compress);
        count = tmp_count;
    }
    if compress == Some(1) && ipv6[0] == 0 {
        compress = Some(0);
    }
    if count < 2 {
        compress = None;
    }
    let mut out = Vec::new();
    let mut ignore = false;
    let mut i = 0;
    while i < 8 {
        if ignore {
            if ipv6[i] == 0 {
                i += 1;
                continue;
            }
            ignore = false;
        }
        if compress == Some(i) {
            out.push(b':');
            if i == 0 {
                out.push(b':');
            }
            i += count;
            ignore = true;
            continue;
        }
        out.extend_from_slice(format!("{:x}", ipv6[i]).as_bytes());
        if i != 7 {
            out.push(b':');
        }
        i += 1;
    }
    out
}

/// Serializes a host the way `lxb_url_serialize_host` does.
pub fn serialize_host(host: &Host) -> Vec<u8> {
    match host {
        Host::Domain(d) | Host::Opaque(d) => d.clone(),
        Host::Ipv4(ip) => serialize_ipv4(*ip),
        Host::Ipv6(v) => {
            let mut out = vec![b'['];
            out.extend_from_slice(&serialize_ipv6(v));
            out.push(b']');
            out
        }
        Host::None | Host::Empty => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(s: &str, special: bool) -> (Result<Host, Fail>, Vec<String>) {
        let mut log = Log::default();
        let opt = HostOpt {
            not_special: !special,
            decode: s.contains('%'),
            idna: s.bytes().any(|c| c >= 0x80) || s.to_ascii_lowercase().contains("xn--"),
        };
        let r = parse_host(&mut log, &SharedBuf::new(s.as_bytes()), 0, s.len(), opt);
        let names = log
            .errors
            .iter()
            .map(|e| e.error_type.name().to_string())
            .collect();
        (r, names)
    }

    #[test]
    fn ipv4_forms() {
        assert_eq!(host("192.168", true).0, Ok(Host::Ipv4(0xC000_00A8)));
        assert_eq!(host("0x7f.1", true).0, Ok(Host::Ipv4(0x7F00_0001)));
        assert_eq!(host("010.0.0.1", true).0, Ok(Host::Ipv4(0x0800_0001)));
        assert_eq!(host("1.2.3.4.", true).0, Ok(Host::Ipv4(0x0102_0304)));
        let (r, e) = host("1.2.3.4.5", true);
        assert!(r.is_err());
        assert_eq!(e, vec!["Ipv4TooManyParts"]);
        let (r, e) = host("1.256", true);
        assert_eq!(r, Ok(Host::Ipv4(0x0100_0100)));
        assert_eq!(e, vec!["Ipv4OutOfRangePart"]);
        assert!(host("256.1.1.1", true).0.is_err());
        assert_eq!(
            host("example.com", true).0,
            Ok(Host::Domain(b"example.com".to_vec()))
        );
    }

    #[test]
    fn ipv6_forms() {
        let (r, _) = host("[::1]", true);
        assert_eq!(r, Ok(Host::Ipv6([0, 0, 0, 0, 0, 0, 0, 1])));
        let (r, _) = host("[::ffff:1.2.3.4]", true);
        assert_eq!(r, Ok(Host::Ipv6([0, 0, 0, 0, 0, 0xffff, 0x0102, 0x0304])));
        let (r, e) = host("[::1", true);
        assert!(r.is_err());
        assert_eq!(e, vec!["Ipv6Unclosed"]);
        let (r, e) = host("[1:2]", true);
        assert!(r.is_err());
        assert_eq!(e, vec!["Ipv6TooFewPieces"]);
        let (r, e) = host("[::g]", true);
        assert!(r.is_err());
        assert_eq!(e, vec!["Ipv6InvalidCodePoint"]);
    }

    #[test]
    fn domains_and_opaque() {
        assert_eq!(
            host("EXAMPLE.com", true).0,
            Ok(Host::Domain(b"example.com".to_vec()))
        );
        assert_eq!(
            host("bücher.de", true).0,
            Ok(Host::Domain(b"xn--bcher-kva.de".to_vec()))
        );
        let (r, e) = host("a<b", true);
        assert!(r.is_err());
        assert_eq!(e, vec!["DomainInvalidCodePoint"]);
        assert_eq!(host("Ex mple", false).1, vec!["HostInvalidCodePoint"]);
        assert_eq!(host("Exé", false).0, Ok(Host::Opaque(b"Ex%C3%A9".to_vec())));
        assert_eq!(host("", false).0, Ok(Host::Empty));
    }

    #[test]
    fn ipv6_serialization() {
        assert_eq!(serialize_ipv6(&[0, 0, 0, 0, 0, 0, 0, 1]), b"::1");
        assert_eq!(serialize_ipv6(&[1, 0, 0, 0, 0, 0, 0, 0]), b"1::");
        assert_eq!(
            serialize_ipv6(&[0x2001, 0xdb8, 0, 0, 1, 0, 0, 1]),
            b"2001:db8::1:0:0:1"
        );
        assert_eq!(
            serialize_ipv6(&[1, 0, 2, 0, 3, 0, 4, 0]),
            b"1:0:2:0:3:0:4:0"
        );
        assert_eq!(serialize_ipv6(&[0; 8]), b"::");
        assert_eq!(serialize_ipv4(0x7F00_0001), b"127.0.0.1");
    }
}
