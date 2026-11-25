// Derived from php-src ext/standard/dns.c. Similarity metric to original: 10%
//   Copyright © The PHP Group and Contributors.
//   Authors: The typical suspects
//            Pollita <pollita@php.net>
//            Marcus Boerger <helly@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! DNS functions (`ext/standard/dns.c`): `dns_check_record`/`checkdnsrr`,
//! `dns_get_record`, `dns_get_mx`/`getmxrr`.
//!
//! Queries go through a [`Resolver`] (the system resolver's `res_search`
//! by default); answers are parsed here from the DNS wire format with the
//! exact record-array shapes of `php_parserr()`, including its quirks
//! (partial records on truncated data, the AAAA `::` compression, `TXT`
//! chunk truncation).

use php_runtime::{RtResult, RuntimeContext, RuntimeError, Value, ZArray, ZStr};

use crate::sys;

/// DNS record type numbers.
pub mod rtype {
    /// A.
    pub const A: i32 = 1;
    /// NS.
    pub const NS: i32 = 2;
    /// CNAME.
    pub const CNAME: i32 = 5;
    /// SOA.
    pub const SOA: i32 = 6;
    /// PTR.
    pub const PTR: i32 = 12;
    /// HINFO.
    pub const HINFO: i32 = 13;
    /// MX.
    pub const MX: i32 = 15;
    /// TXT.
    pub const TXT: i32 = 16;
    /// AAAA.
    pub const AAAA: i32 = 28;
    /// SRV.
    pub const SRV: i32 = 33;
    /// NAPTR.
    pub const NAPTR: i32 = 35;
    /// A6.
    pub const A6: i32 = 38;
    /// ANY.
    pub const ANY: i32 = 255;
    /// CAA.
    pub const CAA: i32 = 257;
}

/// The `DNS_*` constants.
pub const CONSTANTS: &[(&str, i64)] = &[
    ("DNS_A", 0x0000_0001),
    ("DNS_NS", 0x0000_0002),
    ("DNS_CNAME", 0x0000_0010),
    ("DNS_SOA", 0x0000_0020),
    ("DNS_PTR", 0x0000_0800),
    ("DNS_HINFO", 0x0000_1000),
    ("DNS_CAA", 0x0000_2000),
    ("DNS_MX", 0x0000_4000),
    ("DNS_TXT", 0x0000_8000),
    ("DNS_A6", 0x0100_0000),
    ("DNS_SRV", 0x0200_0000),
    ("DNS_NAPTR", 0x0400_0000),
    ("DNS_AAAA", 0x0800_0000),
    ("DNS_ANY", 0x1000_0000),
    ("DNS_ALL", DNS_ALL),
];

const DNS_ANY: i64 = 0x1000_0000;
const DNS_ALL: i64 = 0x0000_0001
    | 0x0000_0002
    | 0x0000_0010
    | 0x0000_0020
    | 0x0000_0800
    | 0x0000_1000
    | 0x0000_2000
    | 0x0000_4000
    | 0x0000_8000
    | 0x0100_0000
    | 0x0200_0000
    | 0x0400_0000
    | 0x0800_0000;
const NUM_TYPES: i32 = 13;

/// A failed query (`h_errno`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsError {
    /// `HOST_NOT_FOUND`.
    HostNotFound,
    /// `TRY_AGAIN`.
    TryAgain,
    /// `NO_RECOVERY`.
    NoRecovery,
    /// `NO_DATA`.
    NoData,
    /// Anything else (including resolver initialisation failures).
    Other,
}

/// The DNS query transport.
pub trait Resolver {
    /// `res_search(name, C_IN, type)`: the raw answer message.
    fn search(&mut self, name: &[u8], rtype: i32) -> Result<Vec<u8>, DnsError>;
}

/// The system resolver (`/etc/resolv.conf`).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn search(&mut self, name: &[u8], rtype: i32) -> Result<Vec<u8>, DnsError> {
        sys::res_query(name, rtype).map_err(|h| match h {
            1 => DnsError::HostNotFound,
            2 => DnsError::TryAgain,
            3 => DnsError::NoRecovery,
            4 => DnsError::NoData,
            _ => DnsError::Other,
        })
    }
}

const HFIXEDSZ: usize = 12;
const QFIXEDSZ: usize = 4;
/// `sizeof(name) - 2` in `php_parserr` (`MAXHOSTNAMELEN` 1024).
const NAME_MAX: usize = 1022;

fn get16(m: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([
        m.get(at).copied().unwrap_or(0),
        m.get(at + 1).copied().unwrap_or(0),
    ])
}

fn get32(m: &[u8], at: usize) -> u32 {
    (u32::from(get16(m, at)) << 16) | u32::from(get16(m, at + 2))
}

/// `ns_name_ntop()` special characters.
fn special(c: u8) -> bool {
    matches!(c, b'"' | b'.' | b';' | b'\\' | b'(' | b')' | b'@' | b'$')
}

/// `dn_expand(msg, eom, comp_dn, ...)`: the presentation form of the
/// (possibly compressed) name at `at`, and the bytes it occupies there.
fn dn_expand(msg: &[u8], end: usize, at: usize, max: usize) -> Option<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut pos = at;
    let mut consumed: Option<usize> = None;
    let mut checked = 0usize;
    loop {
        if pos >= end {
            return None;
        }
        let len = msg[pos];
        match len & 0xc0 {
            0 => {
                pos += 1;
                if len == 0 {
                    break;
                }
                let l = usize::from(len);
                if pos + l > end {
                    return None;
                }
                checked += l + 1;
                if checked >= end {
                    return None;
                }
                if !out.is_empty() {
                    out.push(b'.');
                }
                for &c in &msg[pos..pos + l] {
                    if special(c) {
                        out.push(b'\\');
                        out.push(c);
                    } else if c > 0x20 && c < 0x7f {
                        out.push(c);
                    } else {
                        out.extend_from_slice(format!("\\{c:03}").as_bytes());
                    }
                }
                pos += l;
            }
            0xc0 => {
                if pos + 1 >= end {
                    return None;
                }
                if consumed.is_none() {
                    consumed = Some(pos + 2 - at);
                }
                let target = (usize::from(len & 0x3f) << 8) | usize::from(msg[pos + 1]);
                if target >= end {
                    return None;
                }
                checked += 2;
                if checked >= end {
                    // Pointer loop.
                    return None;
                }
                pos = target;
            }
            _ => return None,
        }
    }
    if out.len() + 1 > max {
        return None;
    }
    Some((out, consumed.unwrap_or_else(|| pos - at)))
}

/// `dn_skipname()`.
fn dn_skipname(msg: &[u8], end: usize, at: usize) -> Option<usize> {
    let mut pos = at;
    loop {
        if pos >= end {
            return None;
        }
        let len = msg[pos];
        match len & 0xc0 {
            0 => {
                pos += 1;
                if len == 0 {
                    return Some(pos - at);
                }
                pos += usize::from(len);
            }
            0xc0 => {
                return if pos + 2 > end {
                    None
                } else {
                    Some(pos + 2 - at)
                }
            }
            _ => return None,
        }
    }
}

fn hex16(out: &mut Vec<u8>, s: u16) {
    out.extend_from_slice(format!("{s:x}").as_bytes());
}

/// The AAAA/A6 group-printing state machine of `php_parserr()`.
struct V6Printer {
    buf: Vec<u8>,
    have_break: bool,
    in_break: bool,
}

impl V6Printer {
    fn group(&mut self, s: u16) {
        if s != 0 {
            if !self.buf.is_empty() {
                self.in_break = false;
                self.buf.push(b':');
            }
            hex16(&mut self.buf, s);
        } else if !self.have_break {
            self.have_break = true;
            self.in_break = true;
            self.buf.push(b':');
        } else if !self.in_break {
            self.buf.extend_from_slice(b":0");
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.have_break && self.in_break {
            self.buf.push(b':');
        }
        self.buf
    }
}

/// `php_parserr()`: parses one resource record at `cp`. Returns the next
/// position (`None` = stop parsing) and the record array, if any (it may
/// be partially filled when the data is truncated, as in PHP).
#[allow(clippy::too_many_lines)]
fn parse_rr(
    msg: &[u8],
    cp: usize,
    type_to_fetch: i32,
    store: bool,
    raw: bool,
) -> (Option<usize>, Option<ZArray>) {
    let end = msg.len();
    let Some((name, n)) = dn_expand(msg, end, cp, NAME_MAX) else {
        return (None, None);
    };
    let mut cp = cp + n;
    if cp + 10 > end {
        return (None, None);
    }
    let typ = i32::from(get16(msg, cp));
    let ttl = get32(msg, cp + 4);
    let dlen = usize::from(get16(msg, cp + 8));
    cp += 10;
    if cp + dlen > end {
        return (None, None);
    }
    if dlen == 0 {
        return (None, None);
    }
    if (type_to_fetch != rtype::ANY && typ != type_to_fetch) || !store {
        return (Some(cp + dlen), None);
    }
    let mut rec = ZArray::new();
    let t = rec.make_mut();
    t.insert("host", Value::Str(ZStr::new(name)));
    t.insert("class", Value::from("IN"));
    t.insert("ttl", Value::Int(i64::from(ttl)));
    if raw {
        t.insert("type", Value::Int(i64::from(typ)));
        t.insert("data", Value::Str(ZStr::new(&msg[cp..cp + dlen])));
        return (Some(cp + dlen), Some(rec));
    }
    macro_rules! check {
        ($n:expr) => {
            if cp + ($n) > end {
                return (None, Some(rec));
            }
        };
    }
    macro_rules! expand {
        () => {
            match dn_expand(msg, end, cp, NAME_MAX) {
                Some((nm, n)) => {
                    cp += n;
                    Value::Str(ZStr::new(nm))
                }
                None => return (None, Some(rec)),
            }
        };
    }
    let str_at = |from: usize, n: usize| Value::Str(ZStr::new(&msg[from..from + n]));
    match typ {
        rtype::A => {
            check!(4);
            t.insert("type", Value::from("A"));
            let ip = format!(
                "{}.{}.{}.{}",
                msg[cp],
                msg[cp + 1],
                msg[cp + 2],
                msg[cp + 3]
            );
            t.insert("ip", Value::from(ip));
            cp += dlen;
        }
        rtype::MX | rtype::CNAME | rtype::NS | rtype::PTR => {
            if typ == rtype::MX {
                check!(2);
                t.insert("type", Value::from("MX"));
                t.insert("pri", Value::Int(i64::from(get16(msg, cp))));
                cp += 2;
            } else {
                let name = match typ {
                    rtype::CNAME => "CNAME",
                    rtype::NS => "NS",
                    _ => "PTR",
                };
                t.insert("type", Value::from(name));
            }
            let target = expand!();
            rec.make_mut().insert("target", target);
        }
        rtype::HINFO => {
            t.insert("type", Value::from("HINFO"));
            check!(1);
            let n = usize::from(msg[cp]);
            cp += 1;
            check!(n);
            rec.make_mut().insert("cpu", str_at(cp, n));
            cp += n;
            check!(1);
            let n = usize::from(msg[cp]);
            cp += 1;
            check!(n);
            rec.make_mut().insert("os", str_at(cp, n));
            cp += n;
        }
        rtype::CAA => {
            t.insert("type", Value::from("CAA"));
            check!(1);
            rec.make_mut()
                .insert("flags", Value::Int(i64::from(msg[cp])));
            cp += 1;
            check!(1);
            let n = usize::from(msg[cp]);
            cp += 1;
            check!(n);
            rec.make_mut().insert("tag", str_at(cp, n));
            cp += n;
            if dlen < n + 2 {
                return (None, Some(rec));
            }
            let n = dlen - n - 2;
            check!(n);
            rec.make_mut().insert("value", str_at(cp, n));
            cp += n;
        }
        rtype::TXT => {
            t.insert("type", Value::from("TXT"));
            let mut txt = Vec::new();
            let mut entries = ZArray::new();
            let mut l1 = 0usize;
            while l1 < dlen {
                let mut n = usize::from(msg[cp + l1]);
                if l1 + n >= dlen {
                    n = dlen - (l1 + 1);
                }
                if n > 0 {
                    let chunk = &msg[cp + l1 + 1..cp + l1 + 1 + n];
                    txt.extend_from_slice(chunk);
                    let _ = entries.make_mut().push(Value::Str(ZStr::new(chunk)));
                }
                l1 += n + 1;
            }
            cp += dlen;
            t.insert("txt", Value::Str(ZStr::new(txt)));
            t.insert("entries", Value::Array(entries));
        }
        rtype::SOA => {
            t.insert("type", Value::from("SOA"));
            let mname = expand!();
            rec.make_mut().insert("mname", mname);
            let rname = expand!();
            rec.make_mut().insert("rname", rname);
            check!(20);
            for (i, k) in ["serial", "refresh", "retry", "expire", "minimum-ttl"]
                .iter()
                .enumerate()
            {
                rec.make_mut()
                    .insert(*k, Value::Int(i64::from(get32(msg, cp + 4 * i))));
            }
            cp += 20;
        }
        rtype::AAAA => {
            check!(16);
            let mut p = V6Printer {
                buf: Vec::new(),
                have_break: false,
                in_break: false,
            };
            for i in 0..8 {
                p.group(get16(msg, cp + 2 * i));
            }
            cp += 16;
            t.insert("type", Value::from("AAAA"));
            t.insert("ipv6", Value::Str(ZStr::new(p.finish())));
        }
        rtype::A6 => {
            let start = cp;
            t.insert("type", Value::from("A6"));
            check!(1);
            let n = usize::from(msg[cp]);
            cp += 1;
            rec.make_mut()
                .insert("masklen", Value::Int(i64::try_from(n).unwrap_or(0)));
            let mut p = V6Printer {
                buf: Vec::new(),
                have_break: false,
                in_break: false,
            };
            if n > 15 {
                p.have_break = true;
                p.in_break = true;
                p.buf.push(b':');
            }
            if n % 16 > 8 {
                // Partial short: PHP formats it without advancing its
                // output pointer, so the digits are overwritten later.
                let b = msg.get(cp).copied().unwrap_or(0);
                if b != 0 {
                    if !p.buf.is_empty() {
                        p.in_break = false;
                        p.buf.push(b':');
                    }
                } else if !p.have_break {
                    p.have_break = true;
                    p.in_break = true;
                    p.buf.push(b':');
                } else if !p.in_break {
                    p.buf.extend_from_slice(b":0");
                }
                cp += 1;
            }
            let mut i = (n + 8) / 16;
            while i < 8 {
                if cp + 2 > end {
                    return (None, Some(rec));
                }
                p.group(get16(msg, cp));
                cp += 2;
                i += 1;
            }
            rec.make_mut()
                .insert("ipv6", Value::Str(ZStr::new(p.finish())));
            if cp < start + dlen {
                let chain = expand!();
                rec.make_mut().insert("chain", chain);
            }
        }
        rtype::SRV => {
            check!(6);
            t.insert("type", Value::from("SRV"));
            t.insert("pri", Value::Int(i64::from(get16(msg, cp))));
            t.insert("weight", Value::Int(i64::from(get16(msg, cp + 2))));
            t.insert("port", Value::Int(i64::from(get16(msg, cp + 4))));
            cp += 6;
            let target = expand!();
            rec.make_mut().insert("target", target);
        }
        rtype::NAPTR => {
            check!(4);
            t.insert("type", Value::from("NAPTR"));
            t.insert("order", Value::Int(i64::from(get16(msg, cp))));
            t.insert("pref", Value::Int(i64::from(get16(msg, cp + 2))));
            cp += 4;
            for key in ["flags", "services", "regex"] {
                check!(1);
                let n = usize::from(msg[cp]);
                cp += 1;
                check!(n);
                rec.make_mut().insert(key, str_at(cp, n));
                cp += n;
            }
            let replacement = expand!();
            rec.make_mut().insert("replacement", replacement);
        }
        _ => return (Some(cp + dlen), None),
    }
    (Some(cp), Some(rec))
}

fn record_type(name: &[u8]) -> Option<i32> {
    let up = name.to_ascii_uppercase();
    Some(match up.as_slice() {
        b"A" => rtype::A,
        b"NS" => rtype::NS,
        b"MX" => rtype::MX,
        b"PTR" => rtype::PTR,
        b"ANY" => rtype::ANY,
        b"SOA" => rtype::SOA,
        b"CAA" => rtype::CAA,
        b"TXT" => rtype::TXT,
        b"CNAME" => rtype::CNAME,
        b"AAAA" => rtype::AAAA,
        b"SRV" => rtype::SRV,
        b"NAPTR" => rtype::NAPTR,
        b"A6" => rtype::A6,
        _ => return None,
    })
}

fn path_check(func: &str, hostname: &[u8]) -> RtResult<()> {
    if hostname.contains(&0) {
        return Err(RuntimeError::ValueError(format!(
            "{func}(): Argument #1 ($hostname) must not contain any null bytes"
        )));
    }
    Ok(())
}

/// `dns_check_record($hostname, $type)` (alias `checkdnsrr`); `func` is
/// the name used in error messages.
pub fn dns_check_record(
    resolver: &mut dyn Resolver,
    func: &str,
    hostname: &[u8],
    rtype_name: Option<&[u8]>,
) -> RtResult<bool> {
    path_check(func, hostname)?;
    if hostname.is_empty() {
        return Err(RuntimeError::ValueError(format!(
            "{func}(): Argument #1 ($hostname) must not be empty"
        )));
    }
    let typ = match rtype_name {
        None => rtype::MX,
        Some(n) => record_type(n).ok_or_else(|| {
            RuntimeError::ValueError(format!(
                "{func}(): Argument #2 ($type) must be a valid DNS record type"
            ))
        })?,
    };
    Ok(match resolver.search(hostname, typ) {
        Ok(ans) => get16(&ans, 6) != 0,
        Err(_) => false,
    })
}

/// The result of [`dns_get_record`].
#[derive(Debug)]
pub struct DnsRecords {
    /// The return value (`false` on failure).
    pub value: Value,
    /// `$authoritative_name_servers`, if requested.
    pub authns: Option<ZArray>,
    /// `$additional_records`, if requested.
    pub addtl: Option<ZArray>,
}

/// `dns_get_record($hostname, $type, &$authoritative_name_servers,
/// &$additional_records, $raw)`; `want_authns`/`want_addtl` tell whether
/// the by-reference arguments were passed.
#[allow(clippy::too_many_lines, clippy::fn_params_excessive_bools)]
pub fn dns_get_record(
    resolver: &mut dyn Resolver,
    hostname: &[u8],
    type_param: i64,
    want_authns: bool,
    want_addtl: bool,
    raw: bool,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<DnsRecords> {
    path_check("dns_get_record", hostname)?;
    let mut authns = want_authns.then(ZArray::new);
    let mut addtl = want_addtl.then(ZArray::new);
    if raw {
        if !(1..=0xFFFF).contains(&type_param) {
            return Err(RuntimeError::ValueError(
                "dns_get_record(): Argument #2 ($type) must be between 1 and 65535 when argument #5 ($raw) is true".into(),
            ));
        }
    } else if (type_param & !DNS_ALL) != 0 && type_param != DNS_ANY {
        return Err(RuntimeError::ValueError(
            "dns_get_record(): Argument #2 ($type) must be a DNS_* constant".into(),
        ));
    }
    let mut result = ZArray::new();
    let mut ty: i32 = if raw {
        -1
    } else if type_param == DNS_ANY {
        NUM_TYPES + 1
    } else {
        0
    };
    let limit = if want_addtl { NUM_TYPES + 2 } else { NUM_TYPES };
    let mut first_query = true;
    let mut store_results = true;
    let bits: [(i64, i32); 13] = [
        (0x0000_0001, rtype::A),
        (0x0000_0002, rtype::NS),
        (0x0000_0010, rtype::CNAME),
        (0x0000_0020, rtype::SOA),
        (0x0000_0800, rtype::PTR),
        (0x0000_1000, rtype::HINFO),
        (0x0000_4000, rtype::MX),
        (0x0000_8000, rtype::TXT),
        (0x0800_0000, rtype::AAAA),
        (0x0200_0000, rtype::SRV),
        (0x0400_0000, rtype::NAPTR),
        (0x0100_0000, rtype::A6),
        (0x0000_2000, rtype::CAA),
    ];
    while ty < limit || first_query {
        first_query = false;
        let type_to_fetch = match ty {
            -1 => {
                ty = NUM_TYPES - 1;
                i32::try_from(type_param).unwrap_or(0)
            }
            0..=12 => {
                let (bit, t) = bits[usize::try_from(ty).unwrap_or(0)];
                if type_param & bit != 0 {
                    t
                } else {
                    0
                }
            }
            NUM_TYPES => {
                store_results = false;
                ty += 1;
                continue;
            }
            _ => rtype::ANY,
        };
        ty += 1;
        if type_to_fetch == 0 {
            continue;
        }
        let ans = match resolver.search(hostname, type_to_fetch) {
            Ok(a) => a,
            Err(DnsError::NoData | DnsError::HostNotFound) => continue,
            Err(e) => {
                let msg = match e {
                    DnsError::NoRecovery => "An unexpected server failure occurred.",
                    DnsError::TryAgain => "A temporary server error occurred.",
                    _ => "DNS Query failed",
                };
                ctx.diagnostic(
                    php_runtime::ErrorLevel::Warning,
                    &format!("dns_get_record(): {msg}"),
                )?;
                return Ok(DnsRecords {
                    value: Value::Bool(false),
                    authns,
                    addtl,
                });
            }
        };
        let end = ans.len();
        let mut qd = get16(&ans, 4);
        let mut an = get16(&ans, 6);
        let mut ns = get16(&ans, 8);
        let mut ar = get16(&ans, 10);
        let mut cp = Some(HFIXEDSZ);
        while qd > 0 {
            qd -= 1;
            let next = cp.and_then(|c| dn_skipname(&ans, end, c).map(|n| c + n + QFIXEDSZ));
            if next.is_none() {
                ctx.diagnostic(
                    php_runtime::ErrorLevel::Warning,
                    "dns_get_record(): Unable to parse DNS data received",
                )?;
                return Ok(DnsRecords {
                    value: Value::Bool(false),
                    authns,
                    addtl,
                });
            }
            cp = next;
        }
        while an > 0 {
            an -= 1;
            let Some(c) = cp.filter(|&c| c < end) else {
                break;
            };
            let (next, rec) = parse_rr(&ans, c, type_to_fetch, store_results, raw);
            cp = next;
            if let (Some(r), true) = (rec, store_results) {
                let _ = result.make_mut().push(Value::Array(r));
            }
        }
        if want_authns || want_addtl {
            while ns > 0 {
                ns -= 1;
                let Some(c) = cp.filter(|&c| c < end) else {
                    break;
                };
                let (next, rec) = parse_rr(&ans, c, rtype::ANY, want_authns, raw);
                cp = next;
                if let (Some(r), Some(list)) = (rec, authns.as_mut()) {
                    let _ = list.make_mut().push(Value::Array(r));
                }
            }
        }
        if want_addtl {
            while ar > 0 {
                ar -= 1;
                let Some(c) = cp.filter(|&c| c < end) else {
                    break;
                };
                let (next, rec) = parse_rr(&ans, c, rtype::ANY, true, raw);
                cp = next;
                if let (Some(r), Some(list)) = (rec, addtl.as_mut()) {
                    let _ = list.make_mut().push(Value::Array(r));
                }
            }
        }
    }
    Ok(DnsRecords {
        value: Value::Array(result),
        authns,
        addtl,
    })
}

/// `dns_get_mx($hostname, &$hosts, &$weights)` (alias `getmxrr`): returns
/// the result and the `$hosts`/`$weights` arrays.
pub fn dns_get_mx(
    resolver: &mut dyn Resolver,
    func: &str,
    hostname: &[u8],
) -> RtResult<(bool, ZArray, ZArray)> {
    path_check(func, hostname)?;
    let mut hosts = ZArray::new();
    let mut weights = ZArray::new();
    let Ok(ans) = resolver.search(hostname, rtype::MX) else {
        return Ok((false, hosts, weights));
    };
    let end = ans.len();
    let mut cp = HFIXEDSZ;
    for _ in 0..get16(&ans, 4) {
        match dn_skipname(&ans, end, cp) {
            Some(n) => cp += n + QFIXEDSZ,
            None => return Ok((false, ZArray::new(), ZArray::new())),
        }
    }
    let mut count = i32::from(get16(&ans, 6));
    loop {
        count -= 1;
        if count < 0 || cp >= end {
            break;
        }
        let Some(n) = dn_skipname(&ans, end, cp) else {
            return Ok((false, hosts, weights));
        };
        cp += n;
        let typ = i32::from(get16(&ans, cp));
        cp += 2 + 2 + 4;
        let dlen = usize::from(get16(&ans, cp));
        cp += 2;
        if typ != rtype::MX {
            cp += dlen;
            continue;
        }
        let weight = get16(&ans, cp);
        cp += 2;
        let Some((name, n)) = dn_expand(&ans, end, cp, 1023) else {
            return Ok((false, hosts, weights));
        };
        cp += n;
        let _ = hosts.make_mut().push(Value::Str(ZStr::new(name)));
        let _ = weights.make_mut().push(Value::Int(i64::from(weight)));
    }
    let ok = !hosts.is_empty();
    Ok((ok, hosts, weights))
}

#[cfg(test)]
mod tests {
    use super::*;
    use php_runtime::CollectingContext;

    /// Builds a DNS response for `name` with the given answer records
    /// `(type, rdata)`.
    fn response(name: &str, answers: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut m = vec![0x12, 0x34, 0x81, 0x80, 0, 1];
        m.extend_from_slice(&u16::try_from(answers.len()).unwrap().to_be_bytes());
        m.extend_from_slice(&[0, 0, 0, 0]);
        for label in name.split('.') {
            m.push(u8::try_from(label.len()).unwrap());
            m.extend_from_slice(label.as_bytes());
        }
        m.push(0);
        m.extend_from_slice(&[0, 255, 0, 1]);
        for (t, rdata) in answers {
            m.extend_from_slice(&[0xc0, 12]);
            m.extend_from_slice(&t.to_be_bytes());
            m.extend_from_slice(&[0, 1, 0, 0, 0x0e, 0x10]);
            m.extend_from_slice(&u16::try_from(rdata.len()).unwrap().to_be_bytes());
            m.extend_from_slice(rdata);
        }
        m
    }

    struct Canned(Vec<u8>);
    impl Resolver for Canned {
        fn search(&mut self, _: &[u8], _: i32) -> Result<Vec<u8>, DnsError> {
            Ok(self.0.clone())
        }
    }

    fn dump(v: &Value) -> String {
        let mut ctx = CollectingContext::new();
        String::from_utf8(php_runtime::dump::var_export(v, &mut ctx).unwrap()).unwrap()
    }

    #[test]
    fn record_shapes() {
        let mut aaaa = vec![0x20, 0x01, 0x0d, 0xb8];
        aaaa.extend_from_slice(&[0; 10]);
        aaaa.extend_from_slice(&[0, 1]);
        let msg = response(
            "example.com",
            &[
                (1, vec![93, 184, 216, 34]),
                (28, aaaa),
                (15, vec![0, 10, 2, b'm', b'x', 0xc0, 12]),
                (16, vec![3, b'a', b'b', b'c', 2, b'd', b'e']),
                (257, vec![0, 5, b'i', b's', b's', b'u', b'e', b'c', b'a']),
                (33, vec![0, 1, 0, 2, 0, 80, 0]),
            ],
        );
        let mut ctx = CollectingContext::new();
        let r = dns_get_record(
            &mut Canned(msg),
            b"example.com",
            DNS_ANY,
            false,
            false,
            false,
            &mut ctx,
        )
        .unwrap();
        let s = dump(&r.value);
        assert!(s.contains("'ip' => '93.184.216.34'"), "{s}");
        assert!(s.contains("'ipv6' => '2001:db8::1'"), "{s}");
        assert!(s.contains("'target' => 'mx.example.com'"), "{s}");
        assert!(s.contains("'txt' => 'abcde'"), "{s}");
        assert!(
            s.contains("'tag' => 'issue'") && s.contains("'value' => 'ca'"),
            "{s}"
        );
        assert!(
            s.contains("'port' => 80") && s.contains("'target' => ''"),
            "{s}"
        );
        assert!(
            s.contains("'ttl' => 3600") && s.contains("'class' => 'IN'"),
            "{s}"
        );
    }

    #[test]
    fn mx_and_check() {
        let msg = response(
            "example.com",
            &[
                (15, vec![0, 10, 2, b'm', b'x', 0xc0, 12]),
                (1, vec![1, 2, 3, 4]),
            ],
        );
        let (ok, hosts, weights) =
            dns_get_mx(&mut Canned(msg.clone()), "getmxrr", b"example.com").unwrap();
        assert!(ok);
        assert_eq!(hosts.len(), 1);
        assert_eq!(weights.get_int(0).and_then(Value::as_int), Some(10));
        assert!(
            dns_check_record(&mut Canned(msg), "checkdnsrr", b"example.com", Some(b"a")).unwrap()
        );
    }

    #[test]
    fn aaaa_formatting_quirks() {
        let cases: &[([u16; 8], &str)] = &[
            ([0, 0, 0, 0, 0, 0, 0, 1], "::1"),
            ([0; 8], "::"),
            ([1, 0, 0, 2, 0, 0, 0, 3], "1::2:0:0:0:3"),
            ([1, 2, 3, 4, 5, 6, 7, 8], "1:2:3:4:5:6:7:8"),
            ([0xfe80, 0, 0, 0, 0, 0, 0, 0], "fe80::"),
        ];
        for (groups, expect) in cases {
            let mut p = V6Printer {
                buf: Vec::new(),
                have_break: false,
                in_break: false,
            };
            for g in groups {
                p.group(*g);
            }
            assert_eq!(String::from_utf8(p.finish()).unwrap(), *expect);
        }
    }

    #[test]
    fn errors() {
        let msg = response("x", &[]);
        let e =
            dns_check_record(&mut Canned(msg.clone()), "dns_check_record", b"", None).unwrap_err();
        assert_eq!(
            e.message(),
            Some("dns_check_record(): Argument #1 ($hostname) must not be empty")
        );
        let e = dns_check_record(&mut Canned(msg.clone()), "checkdnsrr", b"x", Some(b"BOGUS"))
            .unwrap_err();
        assert_eq!(
            e.message(),
            Some("checkdnsrr(): Argument #2 ($type) must be a valid DNS record type")
        );
        let mut ctx = CollectingContext::new();
        let e = dns_get_record(
            &mut Canned(msg.clone()),
            b"x",
            0x2000_0000,
            false,
            false,
            false,
            &mut ctx,
        )
        .unwrap_err();
        assert_eq!(
            e.message(),
            Some("dns_get_record(): Argument #2 ($type) must be a DNS_* constant")
        );
        let e =
            dns_get_record(&mut Canned(msg), b"x", 0, false, false, true, &mut ctx).unwrap_err();
        assert_eq!(
            e.message(),
            Some("dns_get_record(): Argument #2 ($type) must be between 1 and 65535 when argument #5 ($raw) is true")
        );
    }
}
