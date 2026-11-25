// Derived from php-src ext/standard/password.c. Similarity metric to original: 7%
//   Copyright © The PHP Group and Contributors.
//   Authors: Anthony Ferrara <ircmaxell@php.net>
//            Charles R. Portwood II <charlesportwoodii@erianna.com>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The password hashing API (`ext/standard/password.c`): `password_hash`,
//! `password_verify`, `password_needs_rehash`, `password_get_info`,
//! `password_algos`, for the `2y` (bcrypt), `argon2i` and `argon2id`
//! algorithms.

use php_runtime::{ErrorLevel, RtResult, RuntimeContext, RuntimeError, Value, ZArray, ZStr};

use super::argon2::{self, Variant};
use super::php_crypt;

/// `PHP_PASSWORD_BCRYPT_COST` (php-src master).
pub const BCRYPT_DEFAULT_COST: i64 = 12;
/// `PHP_PASSWORD_ARGON2_MEMORY_COST`.
pub const ARGON2_DEFAULT_MEMORY_COST: i64 = 64 << 10;
/// `PHP_PASSWORD_ARGON2_TIME_COST`.
pub const ARGON2_DEFAULT_TIME_COST: i64 = 4;
/// `PHP_PASSWORD_ARGON2_THREADS`.
pub const ARGON2_DEFAULT_THREADS: i64 = 1;

/// Integer constants registered by `password.c`.
pub const INT_CONSTANTS: &[(&str, i64)] = &[
    ("PASSWORD_BCRYPT_DEFAULT_COST", BCRYPT_DEFAULT_COST),
    (
        "PASSWORD_ARGON2_DEFAULT_MEMORY_COST",
        ARGON2_DEFAULT_MEMORY_COST,
    ),
    (
        "PASSWORD_ARGON2_DEFAULT_TIME_COST",
        ARGON2_DEFAULT_TIME_COST,
    ),
    ("PASSWORD_ARGON2_DEFAULT_THREADS", ARGON2_DEFAULT_THREADS),
];

/// String constants registered by `password.c`.
pub const STR_CONSTANTS: &[(&str, &str)] = &[
    ("PASSWORD_DEFAULT", "2y"),
    ("PASSWORD_BCRYPT", "2y"),
    ("PASSWORD_ARGON2I", "argon2i"),
    ("PASSWORD_ARGON2ID", "argon2id"),
    ("PASSWORD_ARGON2_PROVIDER", "standard"),
];

/// A registered password hashing algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algo {
    /// `2y`.
    Bcrypt,
    /// `argon2i`.
    Argon2i,
    /// `argon2id`.
    Argon2id,
}

impl Algo {
    /// The registry identifier (`password_algos()` entry).
    #[must_use]
    pub fn ident(self) -> &'static str {
        match self {
            Algo::Bcrypt => "2y",
            Algo::Argon2i => "argon2i",
            Algo::Argon2id => "argon2id",
        }
    }

    /// The `algoName` of `password_get_info()`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Algo::Bcrypt => "bcrypt",
            Algo::Argon2i => "argon2i",
            Algo::Argon2id => "argon2id",
        }
    }

    fn variant(self) -> Variant {
        match self {
            Algo::Argon2id => Variant::Id,
            _ => Variant::I,
        }
    }

    /// `php_password_algo_find()`.
    #[must_use]
    pub fn find(ident: &[u8]) -> Option<Algo> {
        match ident {
            b"2y" => Some(Algo::Bcrypt),
            b"argon2i" => Some(Algo::Argon2i),
            b"argon2id" => Some(Algo::Argon2id),
            _ => None,
        }
    }

    /// `algo->valid` (only bcrypt has one).
    fn valid(self, hash: &[u8]) -> bool {
        match self {
            Algo::Bcrypt => bcrypt_get_cost(hash).is_some(),
            _ => true,
        }
    }
}

/// The `string|int|null $algo` argument.
#[derive(Clone, Copy, Debug)]
pub enum AlgoArg<'a> {
    /// `null`: the default algorithm.
    Null,
    /// A registry identifier.
    Str(&'a [u8]),
    /// A legacy integer (0 = default, 1 = bcrypt, 2 = argon2i, 3 = argon2id).
    Int(i64),
}

/// `password_algos()`.
#[must_use]
pub fn password_algos() -> ZArray {
    ZArray::from_values(
        [Algo::Bcrypt, Algo::Argon2i, Algo::Argon2id]
            .iter()
            .map(|a| Value::from(a.ident())),
    )
}

/// `php_password_algo_find_zval()`.
#[must_use]
pub fn find_algo(arg: AlgoArg<'_>) -> Option<Algo> {
    match arg {
        AlgoArg::Null | AlgoArg::Int(0 | 1) => Some(Algo::Bcrypt),
        AlgoArg::Str(s) => Algo::find(s),
        AlgoArg::Int(2) => Some(Algo::Argon2i),
        AlgoArg::Int(3) => Some(Algo::Argon2id),
        AlgoArg::Int(_) => None,
    }
}

/// `php_password_algo_extract_ident()`.
fn extract_ident(hash: &[u8]) -> Option<&[u8]> {
    if hash.len() < 3 {
        return None;
    }
    let rest = &hash[1..];
    // strchr() stops at a NUL.
    let rest = rest.split(|&c| c == 0).next().unwrap_or_default();
    let end = rest.iter().position(|&c| c == b'$')?;
    Some(&rest[..end])
}

/// `php_password_algo_identify_ex(hash, NULL)`.
#[must_use]
pub fn identify(hash: &[u8]) -> Option<Algo> {
    let algo = Algo::find(extract_ident(hash)?)?;
    algo.valid(hash).then_some(algo)
}

fn bcrypt_get_cost(h: &[u8]) -> Option<i64> {
    if h.len() != 60
        || h[0] != b'$'
        || h[1] != b'2'
        || h[2] != b'y'
        || h[3] != b'$'
        || !h[4].is_ascii_digit()
        || !h[5].is_ascii_digit()
        || h[6] != b'$'
    {
        return None;
    }
    let cost = i64::from(h[4] - b'0') * 10 + i64::from(h[5] - b'0');
    (4..=31).contains(&cost).then_some(cost)
}

/// `extract_argon2_parameters()` (an `sscanf` that stops at the first
/// mismatch, leaving later values at their defaults).
fn extract_argon2_parameters(hash: &[u8]) -> Option<[Option<i64>; 4]> {
    if hash.len() < "$argon2id$".len() + 1 {
        return None;
    }
    let p = if let Some(p) = hash.strip_prefix(b"$argon2i$") {
        p
    } else {
        hash.strip_prefix(b"$argon2id$")?
    };
    let mut out = [None; 4];
    let mut p = p;
    for (i, lit) in [&b"v="[..], b"$m=", b",t=", b",p="].iter().enumerate() {
        let Some(rest) = scanf_literal(p, lit) else {
            break;
        };
        let Some((v, n)) = scanf_long(rest) else {
            break;
        };
        out[i] = Some(v);
        p = &rest[n..];
    }
    Some(out)
}

/// Matches a `sscanf` literal (whitespace in the input is significant
/// here because the literals contain none).
fn scanf_literal<'a>(s: &'a [u8], lit: &[u8]) -> Option<&'a [u8]> {
    s.strip_prefix(lit)
}

/// `sscanf("%ld")`: skips whitespace, optional sign, digits (saturating).
fn scanf_long(s: &[u8]) -> Option<(i64, usize)> {
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut acc: i64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        let d = i64::from(s[i] - b'0');
        acc = if neg {
            acc.saturating_mul(10).saturating_sub(d)
        } else {
            acc.saturating_mul(10).saturating_add(d)
        };
        i += 1;
    }
    (i > start).then_some((acc, i))
}

/// `password_get_info($hash)`.
#[must_use]
pub fn password_get_info(hash: &[u8]) -> ZArray {
    let mut info = ZArray::new();
    let mut options = ZArray::new();
    let t = info.make_mut();
    let algo = extract_ident(hash).and_then(|ident| Algo::find(ident).map(|a| (a, ident)));
    match algo {
        Some((algo, ident)) if algo.valid(hash) => {
            t.insert("algo", Value::from(ident));
            t.insert("algoName", Value::from(algo.name()));
            let o = options.make_mut();
            match algo {
                Algo::Bcrypt => {
                    if let Some(cost) = bcrypt_get_cost(hash) {
                        o.insert("cost", Value::Int(cost));
                    }
                }
                Algo::Argon2i | Algo::Argon2id => {
                    let p = extract_argon2_parameters(hash).unwrap_or([None; 4]);
                    o.insert(
                        "memory_cost",
                        Value::Int(p[1].unwrap_or(ARGON2_DEFAULT_MEMORY_COST)),
                    );
                    o.insert(
                        "time_cost",
                        Value::Int(p[2].unwrap_or(ARGON2_DEFAULT_TIME_COST)),
                    );
                    o.insert(
                        "threads",
                        Value::Int(p[3].unwrap_or(ARGON2_DEFAULT_THREADS)),
                    );
                }
            }
        }
        _ => {
            t.insert("algo", Value::Null);
            t.insert("algoName", Value::from("unknown"));
        }
    }
    info.make_mut().insert("options", Value::Array(options));
    info
}

fn option_long(
    options: Option<&ZArray>,
    key: &str,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<Option<i64>> {
    match options.and_then(|o| o.get_str(key.as_bytes())) {
        Some(v) => Ok(Some(v.to_long(ctx)?)),
        None => Ok(None),
    }
}

/// `password_needs_rehash($hash, $algo, $options)`.
pub fn password_needs_rehash(
    hash: &[u8],
    algo: AlgoArg<'_>,
    options: Option<&ZArray>,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<bool> {
    let Some(new_algo) = find_algo(algo) else {
        return Ok(false);
    };
    if identify(hash) != Some(new_algo) {
        return Ok(true);
    }
    match new_algo {
        Algo::Bcrypt => {
            let new_cost = option_long(options, "cost", ctx)?.unwrap_or(BCRYPT_DEFAULT_COST);
            Ok(bcrypt_get_cost(hash) != Some(new_cost))
        }
        Algo::Argon2i | Algo::Argon2id => {
            let m = option_long(options, "memory_cost", ctx)?.unwrap_or(ARGON2_DEFAULT_MEMORY_COST);
            let t = option_long(options, "time_cost", ctx)?.unwrap_or(ARGON2_DEFAULT_TIME_COST);
            let th = option_long(options, "threads", ctx)?.unwrap_or(ARGON2_DEFAULT_THREADS);
            let p = extract_argon2_parameters(hash).unwrap_or([None; 4]);
            Ok(t != p[2].unwrap_or(0) || m != p[1].unwrap_or(0) || th != p[3].unwrap_or(0))
        }
    }
}

/// `password_verify($password, $hash)`.
#[must_use]
pub fn password_verify(password: &[u8], hash: &[u8]) -> bool {
    match identify(hash) {
        // Unknown hashes go through crypt() like bcrypt (the default algo).
        Some(Algo::Bcrypt) | None => {
            let Some(ret) = php_crypt(password, hash) else {
                return false;
            };
            if hash.len() < 13 {
                return false;
            }
            constant_time_eq(&ret, hash)
        }
        Some(a) => argon2::verify(a.variant(), hash, password).is_ok(),
    }
}

/// `php_safe_bcmp()`: constant-time equality of two byte strings.
#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `php_password_make_salt()`: `length` characters of `./A-Za-z0-9` from
/// `random` (which fills a buffer with CSPRNG bytes).
pub fn make_salt(length: usize, random: &mut dyn FnMut(&mut [u8]) -> bool) -> RtResult<Vec<u8>> {
    let mut buf = vec![0u8; length * 3 / 4 + 1];
    if !random(&mut buf) {
        return Err(RuntimeError::ValueError("Unable to generate salt".into()));
    }
    let enc = base64_encode(&buf);
    if enc.len() < length {
        return Err(RuntimeError::ValueError("Generated salt too short".into()));
    }
    let mut out = Vec::with_capacity(length);
    for &c in &enc[..length] {
        match c {
            b'+' => out.push(b'.'),
            b'=' => return Err(RuntimeError::ValueError("Generated salt too short".into())),
            _ => out.push(c),
        }
    }
    Ok(out)
}

fn base64_encode(data: &[u8]) -> Vec<u8> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63]);
        out.push(T[(n >> 12) as usize & 63]);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63]
        } else {
            b'='
        });
    }
    out
}

fn system_random(buf: &mut [u8]) -> bool {
    php_random::csprng::fill(buf).is_ok()
}

/// `password_hash($password, $algo, $options)` using the system CSPRNG.
pub fn password_hash(
    password: &[u8],
    algo: AlgoArg<'_>,
    options: Option<&ZArray>,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<ZStr> {
    password_hash_with_rng(password, algo, options, ctx, &mut system_random)
}

/// [`password_hash`] with an explicit randomness source (for tests).
pub fn password_hash_with_rng(
    password: &[u8],
    algo: AlgoArg<'_>,
    options: Option<&ZArray>,
    ctx: &mut (impl RuntimeContext + ?Sized),
    random: &mut dyn FnMut(&mut [u8]) -> bool,
) -> RtResult<ZStr> {
    let Some(algo) = find_algo(algo) else {
        return Err(RuntimeError::ValueError(
            "password_hash(): Argument #2 ($algo) must be a valid password hashing algorithm"
                .into(),
        ));
    };
    let has_salt = options.is_some_and(|o| o.get_str(b"salt").is_some());
    match algo {
        Algo::Bcrypt => {
            if password.contains(&0) {
                return Err(RuntimeError::ValueError(
                    "Bcrypt password must not contain null character".into(),
                ));
            }
            let cost = option_long(options, "cost", ctx)?.unwrap_or(BCRYPT_DEFAULT_COST);
            if !(4..=31).contains(&cost) {
                return Err(RuntimeError::ValueError(format!(
                    "Invalid bcrypt cost parameter specified: {cost}"
                )));
            }
            if has_salt {
                salt_warning(ctx)?;
            }
            let salt = make_salt(22, random)?;
            let mut setting = format!("$2y${cost:02}$").into_bytes();
            setting.extend_from_slice(&salt);
            match php_crypt(password, &setting) {
                Some(r) if r.len() >= 13 => Ok(ZStr::new(r)),
                _ => Err(RuntimeError::Error(
                    "Password hashing failed for unknown reason".into(),
                )),
            }
        }
        Algo::Argon2i | Algo::Argon2id => {
            #[allow(clippy::cast_sign_loss)]
            let as_size = |v: i64| v as u64;
            let memory_cost = as_size(
                option_long(options, "memory_cost", ctx)?.unwrap_or(ARGON2_DEFAULT_MEMORY_COST),
            );
            if !(argon2::MIN_MEMORY..=argon2::MAX_MEMORY).contains(&memory_cost) {
                return Err(RuntimeError::ValueError(
                    "Memory cost is outside of allowed memory range".into(),
                ));
            }
            let time_cost = as_size(
                option_long(options, "time_cost", ctx)?.unwrap_or(ARGON2_DEFAULT_TIME_COST),
            );
            if !(argon2::MIN_TIME..=argon2::MAX_TIME).contains(&time_cost) {
                return Err(RuntimeError::ValueError(
                    "Time cost is outside of allowed time range".into(),
                ));
            }
            let threads =
                as_size(option_long(options, "threads", ctx)?.unwrap_or(ARGON2_DEFAULT_THREADS));
            if threads > argon2::MAX_LANES || threads == 0 {
                return Err(RuntimeError::ValueError("Invalid number of threads".into()));
            }
            if has_salt {
                salt_warning(ctx)?;
            }
            let salt = make_salt(16, random)?;
            #[allow(clippy::cast_possible_truncation)]
            let params = argon2::Params {
                t_cost: time_cost as u32,
                m_cost: memory_cost as u32,
                lanes: threads as u32,
                version: argon2::VERSION_13,
            };
            argon2::hash_encoded(algo.variant(), &params, password, &salt, 32)
                .map(ZStr::from)
                .map_err(|e| RuntimeError::ValueError(e.message().into()))
        }
    }
}

fn salt_warning(ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<()> {
    ctx.diagnostic(
        ErrorLevel::Warning,
        "password_hash(): The \"salt\" option has been ignored, since providing a custom salt is no longer supported",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use php_runtime::CollectingContext;

    #[test]
    fn info_and_rehash() {
        let h = b"$2y$10$MTIzNDU2Nzg5MDEyMzQ1Nej0NmcAWSLR.oP7XOR9HD/vjUuOj100y";
        let info = password_get_info(h);
        assert_eq!(
            info.get_str(b"algoName")
                .unwrap()
                .as_str()
                .unwrap()
                .as_bytes(),
            b"bcrypt"
        );
        let mut ctx = CollectingContext::new();
        assert!(password_needs_rehash(h, AlgoArg::Null, None, &mut ctx).unwrap());
        let opts = ZArray::from_pairs([("cost", Value::Int(10))]);
        assert!(!password_needs_rehash(h, AlgoArg::Int(1), Some(&opts), &mut ctx).unwrap());
        let info = password_get_info(b"$argon2i$v=19$m=1024,t=2,p=4$c2FsdA$aGFzaA");
        let o = info
            .get_str(b"options")
            .unwrap()
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(
            o.get_str(b"memory_cost").and_then(Value::as_int),
            Some(1024)
        );
        assert_eq!(o.get_str(b"threads").and_then(Value::as_int), Some(4));
    }

    #[test]
    fn verify_bcrypt() {
        assert!(password_verify(
            b"rasmuslerdorf",
            b"$2y$07$usesomesillystringfore2uDLvp1Ii2e./U9C8sBjqp8I90dH6hi"
        ));
        assert!(!password_verify(
            b"rasmuslerdorf",
            b"$2y$07$usesomesillystringfore2uDLvp1Ii2e./U9C8sBjqp8I90dH6hj"
        ));
        assert!(password_verify(b"rasmuslerdorf", b"rl.3StKT.4T8M"));
    }

    #[test]
    fn hash_errors() {
        let mut ctx = CollectingContext::new();
        let opts = ZArray::from_pairs([("cost", Value::Int(3))]);
        let e = password_hash(b"x", AlgoArg::Null, Some(&opts), &mut ctx).unwrap_err();
        assert_eq!(
            e.message(),
            Some("Invalid bcrypt cost parameter specified: 3")
        );
        let e = password_hash(b"x", AlgoArg::Int(9), None, &mut ctx).unwrap_err();
        assert!(e
            .message()
            .unwrap()
            .contains("must be a valid password hashing algorithm"));
        let opts = ZArray::from_pairs([("cost", Value::Int(4)), ("salt", Value::from("x"))]);
        let h = password_hash(b"pw", AlgoArg::Null, Some(&opts), &mut ctx).unwrap();
        assert!(password_verify(b"pw", h.as_bytes()));
        assert_eq!(ctx.diagnostics.len(), 1);
    }
}
