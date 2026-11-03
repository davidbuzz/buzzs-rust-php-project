//! Unsigned magnitude arithmetic on little-endian `u64` limb slices.
//!
//! All functions accept non-normalized input unless documented otherwise and
//! return normalized vectors (no high zero limbs; zero is the empty vector).
//!
//! Limb arithmetic deliberately splits `u128`/`i128` products into their low
//! and high 64-bit halves, so truncating casts are the intended semantics.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names
)]

use std::cmp::Ordering;

/// Operand size (in limbs) from which multiplication switches to Karatsuba.
const KARATSUBA_THRESHOLD: usize = 32;

/// Removes high zero limbs.
pub fn normalize(v: &mut Vec<u64>) {
    while v.last() == Some(&0) {
        v.pop();
    }
}

/// Returns the slice without high zero limbs.
pub fn trim(a: &[u64]) -> &[u64] {
    let mut n = a.len();
    while n > 0 && a[n - 1] == 0 {
        n -= 1;
    }
    &a[..n]
}

/// Compares two magnitudes.
pub fn cmp(a: &[u64], b: &[u64]) -> Ordering {
    let (a, b) = (trim(a), trim(b));
    a.len()
        .cmp(&b.len())
        .then_with(|| a.iter().rev().cmp(b.iter().rev()))
}

/// Number of significant bits.
pub fn bit_len(a: &[u64]) -> u64 {
    let a = trim(a);
    match a.last() {
        None => 0,
        Some(top) => (a.len() as u64 - 1) * 64 + u64::from(64 - top.leading_zeros()),
    }
}

/// `a + b`.
pub fn add(a: &[u64], b: &[u64]) -> Vec<u64> {
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(long.len() + 1);
    let mut carry = false;
    for (i, &x) in long.iter().enumerate() {
        let y = short.get(i).copied().unwrap_or(0);
        let (s1, c1) = x.overflowing_add(y);
        let (s2, c2) = s1.overflowing_add(u64::from(carry));
        out.push(s2);
        carry = c1 || c2;
    }
    if carry {
        out.push(1);
    }
    normalize(&mut out);
    out
}

/// In-place `acc += b << (64 * offset)`; `acc` grows as needed.
pub fn add_assign_at(acc: &mut Vec<u64>, b: &[u64], offset: usize) {
    if acc.len() < offset + b.len() {
        acc.resize(offset + b.len(), 0);
    }
    let mut carry = false;
    let mut i = offset;
    for &y in b {
        let (s1, c1) = acc[i].overflowing_add(y);
        let (s2, c2) = s1.overflowing_add(u64::from(carry));
        acc[i] = s2;
        carry = c1 || c2;
        i += 1;
    }
    while carry {
        if i == acc.len() {
            acc.push(1);
            break;
        }
        let (s, c) = acc[i].overflowing_add(1);
        acc[i] = s;
        carry = c;
        i += 1;
    }
}

/// `a - b`, requires `a >= b`.
pub fn sub(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = a.to_vec();
    sub_assign(&mut out, b);
    out
}

/// In-place `a -= b`, requires `a >= b`.
pub fn sub_assign(a: &mut Vec<u64>, b: &[u64]) {
    let mut borrow = false;
    let mut i = 0;
    while i < a.len() && (i < b.len() || borrow) {
        let y = b.get(i).copied().unwrap_or(0);
        let (d1, b1) = a[i].overflowing_sub(y);
        let (d2, b2) = d1.overflowing_sub(u64::from(borrow));
        a[i] = d2;
        borrow = b1 || b2;
        i += 1;
    }
    debug_assert!(!borrow && trim(b).len() <= a.len(), "magnitude underflow");
    normalize(a);
}

/// `a * m + c` for a single limb.
pub fn mul_limb_add(a: &[u64], m: u64, c: u64) -> Vec<u64> {
    let mut out = Vec::with_capacity(a.len() + 1);
    let mut carry = u128::from(c);
    for &x in a {
        let t = u128::from(x) * u128::from(m) + carry;
        out.push(t as u64);
        carry = t >> 64;
    }
    out.push(carry as u64);
    normalize(&mut out);
    out
}

fn schoolbook(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = vec![0u64; a.len() + b.len()];
    for (i, &x) in a.iter().enumerate() {
        if x == 0 {
            continue;
        }
        let mut carry = 0u128;
        for (j, &y) in b.iter().enumerate() {
            let t = u128::from(x) * u128::from(y) + u128::from(out[i + j]) + carry;
            out[i + j] = t as u64;
            carry = t >> 64;
        }
        let mut k = i + b.len();
        while carry != 0 {
            let t = u128::from(out[k]) + carry;
            out[k] = t as u64;
            carry = t >> 64;
            k += 1;
        }
    }
    normalize(&mut out);
    out
}

fn karatsuba(a: &[u64], b: &[u64]) -> Vec<u64> {
    // Precondition: a.len() >= b.len() >= KARATSUBA_THRESHOLD.
    let n = b.len();
    if a.len() >= 2 * n {
        // Unbalanced: multiply in chunks of the shorter operand.
        let mut acc = Vec::with_capacity(a.len() + n);
        for (k, chunk) in a.chunks(n).enumerate() {
            let p = mul(chunk, b);
            add_assign_at(&mut acc, &p, k * n);
        }
        normalize(&mut acc);
        return acc;
    }
    let m = a.len().div_ceil(2);
    let (a0, a1) = (trim(&a[..m]), &a[m..]);
    if n <= m {
        let lo = mul(a0, b);
        let hi = mul(a1, b);
        let mut acc = lo;
        add_assign_at(&mut acc, &hi, m);
        normalize(&mut acc);
        return acc;
    }
    let (b0, b1) = (trim(&b[..m]), &b[m..]);
    let z0 = mul(a0, b0);
    let z2 = mul(a1, b1);
    let sa = add(a0, a1);
    let sb = add(b0, b1);
    let mut z1 = mul(&sa, &sb);
    sub_assign(&mut z1, &z0);
    sub_assign(&mut z1, &z2);
    let mut acc = z0;
    add_assign_at(&mut acc, &z1, m);
    add_assign_at(&mut acc, &z2, 2 * m);
    normalize(&mut acc);
    acc
}

/// `a * b`.
pub fn mul(a: &[u64], b: &[u64]) -> Vec<u64> {
    let (a, b) = (trim(a), trim(b));
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    if b.len() == 1 {
        return mul_limb_add(a, b[0], 0);
    }
    if b.len() < KARATSUBA_THRESHOLD {
        schoolbook(a, b)
    } else {
        karatsuba(a, b)
    }
}

/// Divides by a single non-zero limb, returning `(quotient, remainder)`.
pub fn divrem_limb(a: &[u64], d: u64) -> (Vec<u64>, u64) {
    assert!(d != 0, "division by zero limb");
    let mut q = vec![0u64; a.len()];
    let mut r = 0u128;
    let dd = u128::from(d);
    for i in (0..a.len()).rev() {
        let cur = (r << 64) | u128::from(a[i]);
        q[i] = (cur / dd) as u64;
        r = cur % dd;
    }
    normalize(&mut q);
    (q, r as u64)
}

/// Remainder of a division by a single non-zero limb.
pub fn rem_limb(a: &[u64], d: u64) -> u64 {
    let dd = u128::from(d);
    let mut r = 0u128;
    for &x in a.iter().rev() {
        r = ((r << 64) | u128::from(x)) % dd;
    }
    r as u64
}

/// `a << bits`.
pub fn shl(a: &[u64], bits: u64) -> Vec<u64> {
    let a = trim(a);
    if a.is_empty() {
        return Vec::new();
    }
    let limbs = usize::try_from(bits / 64).expect("shift too large");
    let s = (bits % 64) as u32;
    let mut out = vec![0u64; limbs];
    out.reserve(a.len() + 1);
    if s == 0 {
        out.extend_from_slice(a);
    } else {
        let mut carry = 0u64;
        for &x in a {
            out.push((x << s) | carry);
            carry = x >> (64 - s);
        }
        out.push(carry);
    }
    normalize(&mut out);
    out
}

/// `a >> bits`.
pub fn shr(a: &[u64], bits: u64) -> Vec<u64> {
    let a = trim(a);
    let Ok(limbs) = usize::try_from(bits / 64) else {
        return Vec::new();
    };
    if limbs >= a.len() {
        return Vec::new();
    }
    let s = (bits % 64) as u32;
    let src = &a[limbs..];
    let mut out = Vec::with_capacity(src.len());
    if s == 0 {
        out.extend_from_slice(src);
    } else {
        for i in 0..src.len() {
            let hi = src.get(i + 1).copied().unwrap_or(0);
            out.push((src[i] >> s) | (hi << (64 - s)));
        }
    }
    normalize(&mut out);
    out
}

/// Whether any of the low `bits` bits of `a` is set.
pub fn low_bits_nonzero(a: &[u64], bits: u64) -> bool {
    let full = usize::try_from(bits / 64).unwrap_or(usize::MAX);
    let s = bits % 64;
    for (i, &x) in a.iter().enumerate() {
        if i < full {
            if x != 0 {
                return true;
            }
        } else {
            return i == full && s != 0 && x & ((1u64 << s) - 1) != 0;
        }
    }
    false
}

/// Knuth algorithm D. Returns `(quotient, remainder)`; `b` must be non-zero.
pub fn divrem(a: &[u64], b: &[u64]) -> (Vec<u64>, Vec<u64>) {
    let (a, b) = (trim(a), trim(b));
    assert!(!b.is_empty(), "division by zero");
    if cmp(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    if b.len() == 1 {
        let (q, r) = divrem_limb(a, b[0]);
        let mut r = vec![r];
        normalize(&mut r);
        return (q, r);
    }
    // Normalize so that the top limb of the divisor has its high bit set.
    let s = u64::from(b[b.len() - 1].leading_zeros());
    let v = shl(b, s);
    let mut u = shl(a, s);
    if u.len() == a.len() {
        u.push(0);
    }
    if u.len() < a.len() + 1 {
        u.resize(a.len() + 1, 0);
    }
    let n = v.len();
    let m = u.len() - n - 1;
    let mut q = vec![0u64; m + 1];
    let vtop = u128::from(v[n - 1]);
    let vnext = u128::from(v[n - 2]);
    let base = 1u128 << 64;
    for j in (0..=m).rev() {
        let num = (u128::from(u[j + n]) << 64) | u128::from(u[j + n - 1]);
        let mut qhat = num / vtop;
        let mut rhat = num % vtop;
        while qhat >= base || qhat * vnext > ((rhat << 64) | u128::from(u[j + n - 2])) {
            qhat -= 1;
            rhat += vtop;
            if rhat >= base {
                break;
            }
        }
        // Multiply and subtract.
        let mut borrow: i128 = 0;
        let mut carry: u128 = 0;
        for i in 0..n {
            let p = qhat * u128::from(v[i]) + carry;
            carry = p >> 64;
            let t = i128::from(u[i + j]) - i128::from(p as u64) + borrow;
            u[i + j] = t as u64;
            borrow = t >> 64;
        }
        let t = i128::from(u[j + n]) - carry as i128 + borrow;
        u[j + n] = t as u64;
        if t < 0 {
            // Add back.
            qhat -= 1;
            let mut c = 0u128;
            for i in 0..n {
                let s2 = u128::from(u[i + j]) + u128::from(v[i]) + c;
                u[i + j] = s2 as u64;
                c = s2 >> 64;
            }
            u[j + n] = u[j + n].wrapping_add(c as u64);
        }
        q[j] = qhat as u64;
    }
    normalize(&mut q);
    u.truncate(n);
    let r = shr(&u, s);
    (q, r)
}

/// Integer square root `floor(sqrt(a))` (Newton iteration).
pub fn isqrt(a: &[u64]) -> Vec<u64> {
    let a = trim(a);
    if a.is_empty() {
        return Vec::new();
    }
    let bits = bit_len(a);
    // Initial guess 2^ceil(bits/2) >= sqrt(a).
    let mut x = shl(&[1], bits.div_ceil(2));
    loop {
        // y = (x + a / x) / 2
        let (q, _) = divrem(a, &x);
        let y = shr(&add(&x, &q), 1);
        if cmp(&y, &x) != Ordering::Less {
            return x;
        }
        x = y;
    }
}

/// `floor(a^(1/n))` for `n >= 1`.
pub fn iroot(a: &[u64], n: u64) -> Vec<u64> {
    let a = trim(a);
    if a.is_empty() || n == 1 {
        return a.to_vec();
    }
    if n == 2 {
        return isqrt(a);
    }
    let bits = bit_len(a);
    if n >= bits {
        // 1 <= a < 2^bits <= 2^n, so the root is 1.
        return vec![1];
    }
    // Initial guess 2^ceil(bits/n) >= root.
    let mut x = shl(&[1], bits.div_ceil(n));
    let nm1 = n - 1;
    loop {
        // y = ((n-1) x + a / x^(n-1)) / n
        let p = pow(&x, nm1);
        let (q, _) = divrem(a, &p);
        let t = add(&mul_limb_add(&x, nm1, 0), &q);
        let (y, _) = divrem_limb(&t, n);
        if cmp(&y, &x) != Ordering::Less {
            return x;
        }
        x = y;
    }
}

/// `a^e`.
pub fn pow(a: &[u64], e: u64) -> Vec<u64> {
    let mut result = vec![1u64];
    let mut base = trim(a).to_vec();
    let mut e = e;
    if e == 0 {
        return result;
    }
    if base.is_empty() {
        return base;
    }
    loop {
        if e & 1 == 1 {
            result = mul(&result, &base);
        }
        e >>= 1;
        if e == 0 {
            return result;
        }
        base = mul(&base, &base);
    }
}

/// Montgomery arithmetic modulo an odd modulus `m` (normalized, len `n`).
pub struct Montgomery {
    m: Vec<u64>,
    /// `-m^{-1} mod 2^64`.
    minv: u64,
}

impl Montgomery {
    /// Prepares the context; `m` must be odd and normalized.
    pub fn new(m: &[u64]) -> Self {
        let m0 = m[0];
        let mut inv: u64 = 1;
        for _ in 0..6 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(m0.wrapping_mul(inv)));
        }
        Self {
            m: m.to_vec(),
            minv: inv.wrapping_neg(),
        }
    }

    /// Converts `a < m` into Montgomery form `a * R mod m`.
    pub fn to_mont(&self, a: &[u64]) -> Vec<u64> {
        let shifted = shl(a, 64 * self.m.len() as u64);
        divrem(&shifted, &self.m).1
    }

    /// Montgomery reduction of `t < m * R`: `t * R^{-1} mod m`.
    pub fn redc(&self, t: &[u64]) -> Vec<u64> {
        let n = self.m.len();
        let mut buf = vec![0u64; 2 * n + 1];
        buf[..t.len()].copy_from_slice(t);
        for i in 0..n {
            let u = buf[i].wrapping_mul(self.minv);
            let mut carry = 0u128;
            for j in 0..n {
                let s = u128::from(buf[i + j]) + u128::from(u) * u128::from(self.m[j]) + carry;
                buf[i + j] = s as u64;
                carry = s >> 64;
            }
            let mut k = i + n;
            while carry != 0 {
                let s = u128::from(buf[k]) + carry;
                buf[k] = s as u64;
                carry = s >> 64;
                k += 1;
            }
        }
        let mut r = buf[n..].to_vec();
        normalize(&mut r);
        if cmp(&r, &self.m) != Ordering::Less {
            sub_assign(&mut r, &self.m);
        }
        r
    }

    /// Montgomery product.
    pub fn mul(&self, a: &[u64], b: &[u64]) -> Vec<u64> {
        self.redc(&mul(a, b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_u128(v: u128) -> Vec<u64> {
        let mut r = vec![v as u64, (v >> 64) as u64];
        normalize(&mut r);
        r
    }

    #[test]
    fn small_arith() {
        let a = from_u128(0xFFFF_FFFF_FFFF_FFFF_FFFF);
        let b = from_u128(0x1_0000_0001);
        let p = mul(&a, &b);
        let (q, r) = divrem(&p, &b);
        assert_eq!(q, a);
        assert!(r.is_empty());
        assert_eq!(sub(&add(&a, &b), &b), a);
        assert_eq!(shr(&shl(&a, 77), 77), a);
        assert_eq!(isqrt(&from_u128(1 << 100)), from_u128(1 << 50));
        assert_eq!(iroot(&from_u128(1000), 3), vec![10]);
        assert_eq!(iroot(&from_u128(999), 3), vec![9]);
    }

    #[test]
    fn large_division_identity() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for (la, lb) in [(700, 250), (1200, 600), (300, 299), (500, 2)] {
            let a: Vec<u64> = (0..la).map(|_| next()).collect();
            let b: Vec<u64> = (0..lb).map(|_| next()).collect();
            let (q, r) = divrem(&a, &b);
            assert_eq!(cmp(&r, &b), Ordering::Less);
            assert_eq!(add(&mul(&q, &b), &r), trim(&a).to_vec());
            let sq = mul(&a, &a);
            assert_eq!(isqrt(&sq), trim(&a).to_vec());
        }
    }

    #[test]
    fn montgomery_matches_plain() {
        let m = vec![0xFFFF_FFFF_FFFF_FFC5u64, 0x1234_5678, 0xdead_beef_0000_0001];
        let ctx = Montgomery::new(&m);
        let a = vec![12345u64, 678, 9];
        let b = vec![u64::MAX, 77, 1];
        let prod = ctx.redc(&ctx.mul(&ctx.to_mont(&a), &ctx.to_mont(&b)));
        assert_eq!(prod, divrem(&mul(&a, &b), &m).1);
    }

    #[test]
    fn karatsuba_matches_schoolbook() {
        let mut x = 0x1234_5678_9abc_def1u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for (la, lb) in [(40, 40), (100, 33), (77, 64), (200, 35), (65, 64)] {
            let a: Vec<u64> = (0..la).map(|_| next()).collect();
            let b: Vec<u64> = (0..lb).map(|_| next()).collect();
            assert_eq!(mul(&a, &b), schoolbook(&a, &b));
            let p = mul(&a, &b);
            let (q, r) = divrem(&p, &b);
            assert_eq!(q, a);
            assert!(r.is_empty());
        }
    }
}
