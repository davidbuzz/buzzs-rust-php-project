// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Unsigned big-integer helpers on base-10⁹ limbs.
//!
//! libbcmath stores numbers as one decimal digit per byte and converts them
//! to `BC_VECTOR` chunks for multiplication and division. Every caller only
//! depends on the *exact* integer result, so this module uses its own limb
//! size (10⁹, little-endian `u32` limbs) and textbook algorithms: schoolbook
//! and Karatsuba multiplication and Knuth's Algorithm D for division.

/// Limb base.
const BASE: u64 = 1_000_000_000;
/// Decimal digits per limb.
const LIMB_DIGITS: usize = 9;
/// Operand size (in limbs) above which Karatsuba multiplication is used.
const KARATSUBA_THRESHOLD: usize = 48;

/// Converts big-endian decimal digits (values `0..=9`) into little-endian limbs.
pub fn from_digits(digits: &[u8]) -> Vec<u32> {
    let mut limbs = Vec::with_capacity(digits.len() / LIMB_DIGITS + 1);
    let mut end = digits.len();
    while end > 0 {
        let start = end.saturating_sub(LIMB_DIGITS);
        let limb = digits[start..end]
            .iter()
            .fold(0u32, |acc, &d| acc * 10 + u32::from(d));
        limbs.push(limb);
        end = start;
    }
    trim(&mut limbs);
    limbs
}

/// Writes `limbs` as big-endian decimal digits right-aligned into `out`,
/// zero-filling the remaining leading positions.
///
/// # Panics
/// Panics (debug builds) if the value does not fit in `out`.
pub fn write_digits(limbs: &[u32], out: &mut [u8]) {
    out.fill(0);
    let mut pos = out.len();
    for (i, &limb) in limbs.iter().enumerate() {
        let mut v = limb;
        let is_last = i + 1 == limbs.len();
        for _ in 0..LIMB_DIGITS {
            if pos == 0 {
                debug_assert!(v == 0, "value does not fit in the output buffer");
                return;
            }
            if is_last && v == 0 {
                break;
            }
            pos -= 1;
            // `v % 10` is always below 10, so the narrowing is lossless.
            out[pos] = u8::try_from(v % 10).unwrap_or(0);
            v /= 10;
        }
    }
}

/// Number of significant decimal digits of the value (0 for zero).
pub fn digit_count(limbs: &[u32]) -> usize {
    match limbs.last() {
        None => 0,
        Some(&top) => {
            let mut top_digits = 0;
            let mut v = top;
            while v > 0 {
                top_digits += 1;
                v /= 10;
            }
            (limbs.len() - 1) * LIMB_DIGITS + top_digits
        }
    }
}

/// Removes high zero limbs (zero becomes the empty vector).
pub fn trim(limbs: &mut Vec<u32>) {
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
}

fn narrow(v: u64) -> u32 {
    // Callers only pass values below BASE.
    u32::try_from(v).unwrap_or(u32::MAX)
}

#[allow(clippy::many_single_char_names)]
/// Schoolbook product into `out` (which must be zeroed and `a.len() + b.len()` long).
fn mul_schoolbook(a: &[u32], b: &[u32], out: &mut [u32]) {
    for (i, &x) in a.iter().enumerate() {
        if x == 0 {
            continue;
        }
        let x = u64::from(x);
        let mut carry = 0u64;
        for (j, &y) in b.iter().enumerate() {
            let t = u64::from(out[i + j]) + x * u64::from(y) + carry;
            out[i + j] = narrow(t % BASE);
            carry = t / BASE;
        }
        let mut k = i + b.len();
        while carry > 0 {
            let t = u64::from(out[k]) + carry;
            out[k] = narrow(t % BASE);
            carry = t / BASE;
            k += 1;
        }
    }
}

/// `a + b` (any lengths).
fn add(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(long.len() + 1);
    let mut carry = 0u64;
    for (i, &x) in long.iter().enumerate() {
        let t = u64::from(x) + u64::from(short.get(i).copied().unwrap_or(0)) + carry;
        out.push(narrow(t % BASE));
        carry = t / BASE;
    }
    if carry > 0 {
        out.push(narrow(carry));
    }
    out
}

/// `a -= b` where `a >= b`.
fn sub_assign(a: &mut [u32], b: &[u32]) {
    let mut borrow = 0u64;
    for (i, limb) in a.iter_mut().enumerate() {
        if i >= b.len() && borrow == 0 {
            break;
        }
        let sub = u64::from(b.get(i).copied().unwrap_or(0)) + borrow;
        let cur = u64::from(*limb);
        if cur >= sub {
            *limb = narrow(cur - sub);
            borrow = 0;
        } else {
            *limb = narrow(cur + BASE - sub);
            borrow = 1;
        }
    }
    debug_assert_eq!(borrow, 0, "sub_assign underflow");
}

/// `out[offset..] += v`.
fn add_at(out: &mut [u32], v: &[u32], offset: usize) {
    let mut carry = 0u64;
    let mut i = 0;
    while i < v.len() || carry > 0 {
        let idx = offset + i;
        if idx >= out.len() {
            debug_assert!(carry == 0 && i >= v.len(), "add_at overflow");
            break;
        }
        let t = u64::from(out[idx]) + u64::from(v.get(i).copied().unwrap_or(0)) + carry;
        out[idx] = narrow(t % BASE);
        carry = t / BASE;
        i += 1;
    }
}

#[allow(clippy::many_single_char_names)] // Karatsuba's customary names.
fn mul_into(a: &[u32], b: &[u32], out: &mut [u32]) {
    if a.len() < KARATSUBA_THRESHOLD || b.len() < KARATSUBA_THRESHOLD {
        mul_schoolbook(a, b, out);
        return;
    }
    // Karatsuba on the split point of the longer operand.
    let half = a.len().max(b.len()) / 2;
    let (a0, a1) = a.split_at(half.min(a.len()));
    let (b0, b1) = b.split_at(half.min(b.len()));
    if a1.is_empty() || b1.is_empty() {
        // Very unbalanced operands: split the longer one into chunks.
        let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
        let mut start = 0;
        while start < long.len() {
            let end = (start + short.len()).min(long.len());
            let mut tmp = vec![0u32; end - start + short.len()];
            mul_into(&long[start..end], short, &mut tmp);
            add_at(out, &tmp, start);
            start = end;
        }
        return;
    }
    let mut z0 = vec![0u32; a0.len() + b0.len()];
    mul_into(a0, b0, &mut z0);
    let mut z2 = vec![0u32; a1.len() + b1.len()];
    mul_into(a1, b1, &mut z2);
    let sa = add(a0, a1);
    let sb = add(b0, b1);
    let mut z1 = vec![0u32; sa.len() + sb.len()];
    mul_into(&sa, &sb, &mut z1);
    sub_assign(&mut z1, &z0);
    sub_assign(&mut z1, &z2);
    add_at(out, &z0, 0);
    let mut z1t = z1;
    trim(&mut z1t);
    add_at(out, &z1t, half);
    let mut z2t = z2;
    trim(&mut z2t);
    add_at(out, &z2t, 2 * half);
}

/// Exact product `a * b`.
pub fn mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; a.len() + b.len()];
    mul_into(a, b, &mut out);
    trim(&mut out);
    out
}

/// `base ^ exponent` by binary exponentiation (`exponent > 0`).
pub fn pow(base: &[u32], mut exponent: u64) -> Vec<u32> {
    let mut result: Vec<u32> = vec![1];
    let mut b = base.to_vec();
    loop {
        if exponent & 1 == 1 {
            result = mul(&result, &b);
        }
        exponent >>= 1;
        if exponent == 0 {
            break;
        }
        b = mul(&b, &b);
    }
    result
}

/// Truncating quotient `n / d` (`d` non-zero, both trimmed).
pub fn div(n: &[u32], d: &[u32]) -> Vec<u32> {
    debug_assert!(!d.is_empty(), "division by zero");
    if n.len() < d.len() {
        return Vec::new();
    }
    if d.len() == 1 {
        let dv = u64::from(d[0]);
        let mut q = vec![0u32; n.len()];
        let mut rem = 0u64;
        for i in (0..n.len()).rev() {
            let cur = rem * BASE + u64::from(n[i]);
            q[i] = narrow(cur / dv);
            rem = cur % dv;
        }
        trim(&mut q);
        return q;
    }
    knuth_div(n, d)
}

fn mul_small(v: &[u32], f: u64) -> Vec<u32> {
    let mut out = Vec::with_capacity(v.len() + 1);
    let mut carry = 0u64;
    for &x in v {
        let t = u64::from(x) * f + carry;
        out.push(narrow(t % BASE));
        carry = t / BASE;
    }
    out.push(narrow(carry));
    out
}

/// Knuth, TAOCP vol. 2, 4.3.1, Algorithm D (quotient only).
#[allow(clippy::many_single_char_names)] // Knuth's notation.
fn knuth_div(n: &[u32], d: &[u32]) -> Vec<u32> {
    let dn = d.len();
    let f = BASE / (u64::from(d[dn - 1]) + 1);
    let mut v = mul_small(d, f);
    v.truncate(dn);
    let mut u = mul_small(n, f);
    let m = n.len() - dn;
    let mut q = vec![0u32; m + 1];
    let v_top = u64::from(v[dn - 1]);
    let v_next = u64::from(v[dn - 2]);
    for j in (0..=m).rev() {
        let num = u64::from(u[j + dn]) * BASE + u64::from(u[j + dn - 1]);
        let mut qhat = num / v_top;
        let mut rhat = num % v_top;
        while qhat >= BASE || qhat * v_next > rhat * BASE + u64::from(u[j + dn - 2]) {
            qhat -= 1;
            rhat += v_top;
            if rhat >= BASE {
                break;
            }
        }
        if qhat == 0 {
            continue;
        }
        // Multiply and subtract.
        let mut borrow = 0i64;
        let mut carry = 0u64;
        for i in 0..dn {
            let p = qhat * u64::from(v[i]) + carry;
            carry = p / BASE;
            let t = i64::from(u[i + j]) - i64::try_from(p % BASE).unwrap_or(0) + borrow;
            if t < 0 {
                u[i + j] = narrow(u64::try_from(t + 1_000_000_000).unwrap_or(0));
                borrow = -1;
            } else {
                u[i + j] = narrow(u64::try_from(t).unwrap_or(0));
                borrow = 0;
            }
        }
        let t = i64::from(u[j + dn]) - i64::try_from(carry).unwrap_or(0) + borrow;
        if t < 0 {
            // qhat was one too large: add the divisor back.
            u[j + dn] = narrow(u64::try_from(t + 1_000_000_000).unwrap_or(0));
            qhat -= 1;
            let mut c = 0u64;
            for i in 0..dn {
                let s = u64::from(u[i + j]) + u64::from(v[i]) + c;
                u[i + j] = narrow(s % BASE);
                c = s / BASE;
            }
            u[j + dn] = narrow((u64::from(u[j + dn]) + c) % BASE);
        } else {
            u[j + dn] = narrow(u64::try_from(t).unwrap_or(0));
        }
        q[j] = narrow(qhat);
    }
    trim(&mut q);
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digits(s: &str) -> Vec<u8> {
        s.bytes().map(|b| b - b'0').collect()
    }

    fn render(limbs: &[u32]) -> String {
        let n = digit_count(limbs).max(1);
        let mut out = vec![0u8; n];
        write_digits(limbs, &mut out);
        out.iter().map(|d| char::from(b'0' + d)).collect()
    }

    fn big(s: &str) -> Vec<u32> {
        from_digits(&digits(s))
    }

    #[test]
    fn roundtrip() {
        for s in [
            "0",
            "1",
            "999999999",
            "1000000000",
            "123456789012345678901234567890",
        ] {
            assert_eq!(render(&big(s)), s);
        }
        assert_eq!(render(&big("000123")), "123");
        assert!(big("0000").is_empty());
    }

    #[test]
    fn multiplication() {
        assert_eq!(
            render(&mul(&big("99999999"), &big("99999999"))),
            "9999999800000001"
        );
        assert_eq!(
            render(&mul(&big("123456789123456789"), &big("987654321987654321"))),
            "121932631356500531347203169112635269"
        );
        assert!(mul(&big("0"), &big("5")).is_empty());
    }

    #[test]
    fn karatsuba_matches_schoolbook() {
        let a: String = (0..2000)
            .map(|i| char::from(b'0' + u8::try_from((i * 7 + 3) % 10).unwrap()))
            .collect();
        let b: String = (0..1500)
            .map(|i| char::from(b'0' + u8::try_from((i * 3 + 1) % 10).unwrap()))
            .collect();
        let (x, y) = (big(&a), big(&b));
        let mut slow = vec![0u32; x.len() + y.len()];
        mul_schoolbook(&x, &y, &mut slow);
        trim(&mut slow);
        assert_eq!(mul(&x, &y), slow);
        let short = big("123456789123456789123");
        let mut slow = vec![0u32; x.len() + short.len()];
        mul_schoolbook(&x, &short, &mut slow);
        trim(&mut slow);
        assert_eq!(mul(&x, &short), slow);
    }

    #[test]
    fn division() {
        assert_eq!(render(&div(&big("100"), &big("7"))), "14");
        assert_eq!(render(&div(&big("123456780000"), &big("123456789"))), "999");
        assert_eq!(
            render(&div(
                &big("121932631356500531347203169112635269"),
                &big("987654321987654321")
            )),
            "123456789123456789"
        );
        assert!(div(&big("5"), &big("123456789123")).is_empty());
        // Knuth add-back case.
        let n = big("4000000000000000000000000000");
        let d = big("2000000000000000001");
        assert_eq!(render(&div(&n, &d)), "1999999999");
    }

    #[test]
    fn power() {
        assert_eq!(
            render(&pow(&big("2"), 100)),
            "1267650600228229401496703205376"
        );
        assert_eq!(render(&pow(&big("7"), 1)), "7");
    }
}
