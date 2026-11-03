//! Ulrich Drepper's SHA-256/SHA-512 crypt (`$5$`, `$6$`), ported from
//! `ext/standard/crypt_sha256.c` / `crypt_sha512.c`, and the FreeBSD MD5
//! crypt (`$1$`) of `ext/standard/php_crypt_r.c`.

// Cryptographic code ported from C: single-letter state variables, fixed-width
// truncating casts and indexed loops mirror the reference implementation.
#![allow(
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::needless_range_loop,
    clippy::explicit_counter_loop,
    clippy::trivially_copy_pass_by_ref
)]

use php_hash::{HashContext, HashOptions};

/// The crypt(3) base-64 alphabet (`b64t`).
const B64T: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

const ROUNDS_DEFAULT: u64 = 5000;
const ROUNDS_MIN: u64 = 1000;
const ROUNDS_MAX: u64 = 999_999_999;
const SALT_LEN_MAX: usize = 16;

/// A minimal incremental digest over `php-hash`.
struct Hasher(HashContext);

impl Hasher {
    fn new(algo: &[u8]) -> Self {
        // The algorithm names used here are always registered.
        let ctx = HashContext::new(algo, 0, b"", &HashOptions::default());
        Hasher(ctx.unwrap_or_else(|_| unreachable!("built-in hash algorithm")))
    }

    fn update(&mut self, data: &[u8]) {
        // Updating a non-finalized context cannot fail.
        let _ = self.0.update(data);
    }

    fn finish(mut self) -> Vec<u8> {
        self.0.finalize().unwrap_or_default()
    }
}

/// `strtoul(s, &end, 10)` (C semantics: leading whitespace, optional sign,
/// saturation at `ULONG_MAX`). Returns the value and the index of `end`.
pub(crate) fn c_strtoul(s: &[u8]) -> (u64, usize) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut acc: u64 = 0;
    let mut overflow = false;
    while i < s.len() && s[i].is_ascii_digit() {
        match acc
            .checked_mul(10)
            .and_then(|a| a.checked_add(u64::from(s[i] - b'0')))
        {
            Some(v) => acc = v,
            None => overflow = true,
        }
        i += 1;
    }
    if i == start {
        return (0, 0);
    }
    if overflow {
        return (u64::MAX, i);
    }
    (if neg { acc.wrapping_neg() } else { acc }, i)
}

#[derive(Clone, Copy)]
enum Variant {
    Sha256,
    Sha512,
}

impl Variant {
    fn algo(self) -> &'static [u8] {
        match self {
            Variant::Sha256 => b"sha256",
            Variant::Sha512 => b"sha512",
        }
    }
    fn len(self) -> usize {
        match self {
            Variant::Sha256 => 32,
            Variant::Sha512 => 64,
        }
    }
    fn prefix(self) -> &'static [u8] {
        match self {
            Variant::Sha256 => b"$5$",
            Variant::Sha512 => b"$6$",
        }
    }
}

/// Repeats `block` cyclically to `len` bytes (the P and S byte sequences).
fn spread(block: &[u8], len: usize) -> Vec<u8> {
    block.iter().copied().cycle().take(len).collect()
}

fn b64_from_24bit(out: &mut Vec<u8>, b2: u8, b1: u8, b0: u8, n: usize) {
    let mut w = (u32::from(b2) << 16) | (u32::from(b1) << 8) | u32::from(b0);
    for _ in 0..n {
        out.push(B64T[(w & 0x3f) as usize]);
        w >>= 6;
    }
}

#[allow(clippy::many_single_char_names)]
fn sha_crypt(variant: Variant, key: &[u8], salt: &[u8]) -> Option<Vec<u8>> {
    let hlen = variant.len();
    let mut salt = salt.strip_prefix(variant.prefix()).unwrap_or(salt);
    let mut rounds = ROUNDS_DEFAULT;
    let mut rounds_custom = false;
    if let Some(num) = salt.strip_prefix(b"rounds=") {
        let (srounds, end) = c_strtoul(num);
        if num.get(end) == Some(&b'$') {
            salt = &num[end + 1..];
            if !(ROUNDS_MIN..=ROUNDS_MAX).contains(&srounds) {
                return None;
            }
            rounds = srounds;
            rounds_custom = true;
        }
    }
    let salt_len = salt
        .iter()
        .position(|&c| c == b'$')
        .unwrap_or(salt.len())
        .min(SALT_LEN_MAX);
    let salt = &salt[..salt_len];
    let algo = variant.algo();

    let mut ctx = Hasher::new(algo);
    ctx.update(key);
    ctx.update(salt);

    let mut alt = Hasher::new(algo);
    alt.update(key);
    alt.update(salt);
    alt.update(key);
    let alt_result = alt.finish();

    let mut cnt = key.len();
    while cnt > hlen {
        ctx.update(&alt_result);
        cnt -= hlen;
    }
    ctx.update(&alt_result[..cnt]);

    let mut cnt = key.len();
    while cnt > 0 {
        if cnt & 1 != 0 {
            ctx.update(&alt_result);
        } else {
            ctx.update(key);
        }
        cnt >>= 1;
    }
    let mut alt_result = ctx.finish();

    let mut alt = Hasher::new(algo);
    for _ in 0..key.len() {
        alt.update(key);
    }
    let p_bytes = spread(&alt.finish(), key.len());

    let mut alt = Hasher::new(algo);
    for _ in 0..16 + usize::from(alt_result[0]) {
        alt.update(salt);
    }
    let s_bytes = spread(&alt.finish(), salt_len);

    for cnt in 0..rounds {
        let mut ctx = Hasher::new(algo);
        if cnt & 1 != 0 {
            ctx.update(&p_bytes);
        } else {
            ctx.update(&alt_result);
        }
        if cnt % 3 != 0 {
            ctx.update(&s_bytes);
        }
        if cnt % 7 != 0 {
            ctx.update(&p_bytes);
        }
        if cnt & 1 != 0 {
            ctx.update(&alt_result);
        } else {
            ctx.update(&p_bytes);
        }
        alt_result = ctx.finish();
    }

    let mut out = variant.prefix().to_vec();
    if rounds_custom {
        out.extend_from_slice(format!("rounds={rounds}$").as_bytes());
    }
    out.extend_from_slice(salt);
    out.push(b'$');
    let a = &alt_result;
    match variant {
        Variant::Sha256 => {
            for (x, y, z) in [
                (0, 10, 20),
                (21, 1, 11),
                (12, 22, 2),
                (3, 13, 23),
                (24, 4, 14),
                (15, 25, 5),
                (6, 16, 26),
                (27, 7, 17),
                (18, 28, 8),
                (9, 19, 29),
            ] {
                b64_from_24bit(&mut out, a[x], a[y], a[z], 4);
            }
            b64_from_24bit(&mut out, 0, a[31], a[30], 3);
        }
        Variant::Sha512 => {
            for k in 0..21 {
                let x = k;
                let y = k + 21;
                let z = k + 42;
                // The triples of crypt_sha512.c: (0,21,42), (22,43,1), (44,2,23), ...
                let (x, y, z) = match k % 3 {
                    0 => (x, y, z),
                    1 => (y, z, x),
                    _ => (z, x, y),
                };
                b64_from_24bit(&mut out, a[x], a[y], a[z], 4);
            }
            b64_from_24bit(&mut out, 0, 0, a[63], 2);
        }
    }
    Some(out)
}

/// `php_sha256_crypt_r()`: `None` when `rounds=` is out of range.
#[must_use]
pub fn sha256_crypt(key: &[u8], salt: &[u8]) -> Option<Vec<u8>> {
    sha_crypt(Variant::Sha256, key, salt)
}

/// `php_sha512_crypt_r()`: `None` when `rounds=` is out of range.
#[must_use]
pub fn sha512_crypt(key: &[u8], salt: &[u8]) -> Option<Vec<u8>> {
    sha_crypt(Variant::Sha512, key, salt)
}

/// `php_md5_crypt_r()`.
#[must_use]
pub fn md5_crypt(pw: &[u8], salt: &[u8]) -> Vec<u8> {
    const MAGIC: &[u8] = b"$1$";
    let sp = salt.strip_prefix(MAGIC).unwrap_or(salt);
    let sl = sp
        .iter()
        .take(8)
        .position(|&c| c == b'$' || c == 0)
        .unwrap_or(sp.len().min(8));
    let sp = &sp[..sl];

    let mut ctx = Hasher::new(b"md5");
    ctx.update(pw);
    ctx.update(MAGIC);
    ctx.update(sp);

    let mut ctx1 = Hasher::new(b"md5");
    ctx1.update(pw);
    ctx1.update(sp);
    ctx1.update(pw);
    let fin = ctx1.finish();
    let mut pl = pw.len();
    while pl > 0 {
        let n = pl.min(16);
        ctx.update(&fin[..n]);
        pl -= n;
    }
    // `final` has been zeroed at this point: a NUL byte is added.
    let mut i = pw.len();
    while i != 0 {
        if i & 1 != 0 {
            ctx.update(&[0]);
        } else {
            ctx.update(&pw[..1]);
        }
        i >>= 1;
    }
    let mut fin = ctx.finish();

    for i in 0..1000 {
        let mut ctx1 = Hasher::new(b"md5");
        if i & 1 != 0 {
            ctx1.update(pw);
        } else {
            ctx1.update(&fin);
        }
        if i % 3 != 0 {
            ctx1.update(sp);
        }
        if i % 7 != 0 {
            ctx1.update(pw);
        }
        if i & 1 != 0 {
            ctx1.update(&fin);
        } else {
            ctx1.update(pw);
        }
        fin = ctx1.finish();
    }

    let mut out = MAGIC.to_vec();
    out.extend_from_slice(sp);
    out.push(b'$');
    for (a, b, c) in [(0, 6, 12), (1, 7, 13), (2, 8, 14), (3, 9, 15), (4, 10, 5)] {
        b64_from_24bit(&mut out, fin[a], fin[b], fin[c], 4);
    }
    b64_from_24bit(&mut out, 0, 0, fin[11], 2);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drepper_vectors_sha256() {
        let cases: &[(&[u8], &[u8], &[u8])] = &[
            (
                b"$5$saltstring",
                b"Hello world!",
                b"$5$saltstring$5B8vYYiY.CVt1RlTTf8KbXBH3hsxY/GNooZaBBGWEc5",
            ),
            (
                b"$5$rounds=10000$saltstringsaltstring",
                b"Hello world!",
                b"$5$rounds=10000$saltstringsaltst$3xv.VbSHBb41AL9AvLeujZkZRBAwqFMz2.opqey6IcA",
            ),
            (
                b"$5$rounds=77777$short",
                b"we have a short salt string but not a short password",
                b"$5$rounds=77777$short$JiO1O3ZpDAxGJeaDIuqCoEFysAe1mZNJRs3pw0KQRd/",
            ),
        ];
        for (salt, key, expect) in cases {
            assert_eq!(sha256_crypt(key, salt).as_deref(), Some(*expect));
        }
        assert_eq!(sha256_crypt(b"x", b"$5$rounds=10$roundstoolow"), None);
    }

    #[test]
    fn drepper_vectors_sha512() {
        assert_eq!(
            sha512_crypt(b"Hello world!", b"$6$saltstring").unwrap(),
            b"$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1".to_vec()
        );
    }

    #[test]
    fn md5_vector() {
        assert_eq!(
            md5_crypt(b"rasmuslerdorf", b"$1$rasmusle$"),
            b"$1$rasmusle$rISCgZzpwk3UhDidwXvin0".to_vec()
        );
    }

    #[test]
    fn strtoul() {
        assert_eq!(c_strtoul(b"5000$"), (5000, 4));
        assert_eq!(c_strtoul(b"$"), (0, 0));
        assert_eq!(c_strtoul(b" -1$"), (u64::MAX, 3));
    }
}
