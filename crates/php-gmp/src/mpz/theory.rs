//! Number-theoretic functions (gcd, modular arithmetic, roots, primality).

use super::{mag, Mpz, Round};
use std::cmp::Ordering;

/// Below this bound GMP reports Baillie-PSW probable primes as definitely
/// prime (`mpz_probab_prime_p` returns 2): 35 * 2^46.
pub const BPSW_DEFINITE_LIMIT: u64 = 35 << 46;

const SMALL_PRIMES: [u64; 54] = [
    2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97,
    101, 103, 107, 109, 113, 127, 131, 137, 139, 149, 151, 157, 163, 167, 173, 179, 181, 191, 193,
    197, 199, 211, 223, 227, 229, 233, 239, 241, 251,
];

fn is_prime_u64_small(n: u64) -> bool {
    if n < 2 {
        return false;
    }
    if n < 4 {
        return true;
    }
    if n % 2 == 0 {
        return false;
    }
    let mut d = 3;
    while d * d <= n {
        if n % d == 0 {
            return false;
        }
        d += 2;
    }
    true
}

impl Mpz {
    /// `mpz_gcd` (non-negative).
    pub fn gcd(&self, other: &Self) -> Self {
        let mut a = self.abs();
        let mut b = other.abs();
        while !b.is_zero() {
            let r = Self::from_parts(false, mag::divrem(&a.mag, &b.mag).1);
            a = b;
            b = r;
        }
        a
    }

    /// `mpz_lcm` (non-negative; 0 if either operand is 0).
    pub fn lcm(&self, other: &Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return Self::zero();
        }
        let g = self.gcd(other);
        let q = Self::from_parts(false, mag::divrem(&self.mag, &g.mag).0);
        Self::from_parts(false, mag::mul(&q.mag, &other.mag))
    }

    /// `mpz_gcdext`: `(g, s, t)` with `a*s + b*t = g`, normalized exactly
    /// like GMP (`|s| < |b|/(2g)`, `|t| < |a|/(2g)` plus the documented
    /// exceptional cases).
    pub fn gcdext(&self, other: &Self) -> (Self, Self, Self) {
        let (a, b) = (self, other);
        let sgn = |x: &Self| Self::from(x.sign());
        if a.cmp_abs(b) == Ordering::Equal {
            return (b.abs(), Self::zero(), sgn(b));
        }
        if b.is_zero() {
            return (a.abs(), sgn(a), Self::zero());
        }
        if a.is_zero() {
            return (b.abs(), Self::zero(), sgn(b));
        }
        // Extended Euclid on |a|, |b| tracking the coefficient of |a|.
        let (mut r0, mut r1) = (a.abs(), b.abs());
        let (mut s0, mut s1) = (Self::from(1), Self::zero());
        while !r1.is_zero() {
            let (q, r) = r0.div_qr(&r1, Round::Zero);
            let s2 = &s0 - &(&q * &s1);
            r0 = r1;
            r1 = r;
            s0 = s1;
            s1 = s2;
        }
        let g = r0;
        // s0 * |a| ≡ g (mod |b|); reduce into the symmetric range.
        let bg = Self::from_parts(false, mag::divrem(&b.mag, &g.mag).0);
        let two_g = g.shl(1);
        let mut s = if a.neg { -&s0 } else { s0 };
        if b.cmp_abs(&two_g) == Ordering::Equal {
            s = sgn(a);
        } else if a.cmp_abs(&two_g) == Ordering::Equal {
            // t = sgn(b); s = (g - t*b) / a
            let t = sgn(b);
            let s = (&g - &(&t * b)).div_q(a, Round::Zero);
            return (g, s, t);
        } else {
            s = s.modulo(&bg);
            // Choose the representative with |s| < |b|/(2g).
            if s.shl(1) > bg {
                s = &s - &bg;
            }
        }
        let t = (&g - &(&s * a)).div_q(b, Round::Zero);
        (g, s, t)
    }

    /// `mpz_invert`: inverse of `self` modulo `|m|` in `[0, |m|)`, or `None`.
    pub fn invert(&self, m: &Self) -> Option<Self> {
        let (g, s, _) = self.gcdext(m);
        if g != Self::from(1) {
            return None;
        }
        if s.neg {
            Some(&s + &m.abs())
        } else {
            Some(s)
        }
    }

    /// `mpz_jacobi` / `mpz_kronecker` / `mpz_legendre` (GMP evaluates all
    /// three as the Kronecker symbol).
    pub fn kronecker(&self, b: &Self) -> i32 {
        let a = self;
        if b.is_zero() {
            return i32::from(a.cmp_abs(&Self::from(1)) == Ordering::Equal);
        }
        if !a.is_odd() && !b.is_odd() {
            return 0;
        }
        let v = b.trailing_zeros();
        let mut bb = b.abs().shr_trunc(v);
        let mut k = 1;
        if v % 2 == 1 {
            let a8 = a.modulo(&Self::from(8)).low_u64();
            if a8 == 3 || a8 == 5 {
                k = -k;
            }
        }
        if b.neg && a.neg {
            k = -k;
        }
        // Jacobi symbol (a / bb) with bb odd positive.
        let mut aa = a.modulo(&bb);
        while !aa.is_zero() {
            let z = aa.trailing_zeros();
            aa = aa.shr_trunc(z);
            if z % 2 == 1 {
                let b8 = bb.low_u64() & 7;
                if b8 == 3 || b8 == 5 {
                    k = -k;
                }
            }
            if aa.low_u64() & 3 == 3 && bb.low_u64() & 3 == 3 {
                k = -k;
            }
            std::mem::swap(&mut aa, &mut bb);
            aa = aa.modulo(&bb);
        }
        if bb == Self::from(1) {
            k
        } else {
            0
        }
    }

    /// `mpz_powm` for a non-negative exponent and non-zero modulus: result in
    /// `[0, |m|)`.
    pub fn powm(&self, e: &Self, m: &Self) -> Self {
        assert!(!m.is_zero() && !e.neg, "invalid powm arguments");
        let m = m.abs();
        if m == Self::from(1) {
            return Self::zero();
        }
        let base = self.modulo(&m);
        if e.is_zero() {
            return Self::from(1);
        }
        if m.is_odd() && m.limbs().len() > 1 {
            return powm_montgomery(&base, e, &m);
        }
        // Left-to-right fixed 4-bit window with plain reduction.
        let mut table = vec![Self::from(1), base.clone()];
        for i in 2..16 {
            table.push((&table[i - 1] * &base).modulo(&m));
        }
        let mut result = Self::from(1);
        for w in exponent_windows(e) {
            for _ in 0..4 {
                result = (&result * &result).modulo(&m);
            }
            if w != 0 {
                result = (&result * &table[w]).modulo(&m);
            }
        }
        result
    }

    /// `mpz_sqrt` of a non-negative value.
    pub fn sqrt(&self) -> Self {
        Self::from_parts(false, mag::isqrt(&self.mag))
    }

    /// `mpz_sqrtrem` of a non-negative value.
    pub fn sqrtrem(&self) -> (Self, Self) {
        let r = self.sqrt();
        let rem = self - &(&r * &r);
        (r, rem)
    }

    /// `mpz_root`: `n`-th root truncated toward zero (`n >= 1`; negative
    /// values require odd `n`).
    pub fn root(&self, n: u64) -> Self {
        Self::from_parts(self.neg, mag::iroot(&self.mag, n))
    }

    /// `mpz_rootrem`: root and `self - root^n`.
    pub fn rootrem(&self, n: u64) -> (Self, Self) {
        let r = self.root(n);
        let rem = self - &r.pow(n);
        (r, rem)
    }

    /// `mpz_perfect_square_p`.
    pub fn is_perfect_square(&self) -> bool {
        if self.neg {
            return false;
        }
        let r = self.sqrt();
        &r * &r == *self
    }

    /// `mpz_perfect_power_p` (0, 1 and -1 are perfect powers; negative
    /// values only as odd powers).
    pub fn is_perfect_power(&self) -> bool {
        if self.cmp_abs(&Self::from(1)) != Ordering::Greater {
            return true;
        }
        let n = self.abs();
        let bits = n.bit_len();
        let mut e = 2u64;
        while e <= bits {
            if is_prime_u64_small(e) && !(self.neg && e == 2) {
                let r = n.root(e);
                if r.pow(e) == n {
                    return true;
                }
            }
            e += 1;
        }
        false
    }

    /// `mpz_probab_prime_p`: 2 = definitely prime, 1 = probably prime,
    /// 0 = composite.  The repetition count does not change the outcome
    /// because GMP always runs a Baillie-PSW test first.
    pub fn probab_prime(&self, _reps: i32) -> i32 {
        if self.cmp_abs(&Self::from(1_000_000u64)) != Ordering::Greater {
            return if is_prime_u64_small(self.low_u64()) {
                2
            } else {
                0
            };
        }
        let n = self.abs();
        if !n.is_odd() {
            return 0;
        }
        if !bpsw(&n) {
            return 0;
        }
        if n < Self::from(BPSW_DEFINITE_LIMIT) {
            2
        } else {
            1
        }
    }

    /// `mpz_nextprime`: the smallest prime greater than `self` (2 for
    /// anything below 2).
    pub fn next_prime(&self) -> Self {
        if *self < Self::from(2) {
            return Self::from(2);
        }
        let mut p = self + &Self::from(1);
        if !p.is_odd() {
            if p == Self::from(2) {
                return p;
            }
            p = &p + &Self::from(1);
        }
        let two = Self::from(2);
        while p.probab_prime(25) == 0 {
            p = &p + &two;
        }
        p
    }

    /// `mpz_prevprime`: the largest prime smaller than `self` and GMP's
    /// certainty (2 = definitely, 1 = probably); `None` when `self <= 2`.
    pub fn prev_prime(&self) -> Option<(Self, i32)> {
        if *self <= Self::from(2) {
            return None;
        }
        if *self == Self::from(3) {
            return Some((Self::from(2), 2));
        }
        let two = Self::from(2);
        let mut p = self - &Self::from(1);
        if !p.is_odd() {
            p = &p - &Self::from(1);
        }
        loop {
            let r = p.probab_prime(25);
            if r != 0 {
                return Some((p, r));
            }
            p = &p - &two;
        }
    }

    /// `mpz_fac_ui`.
    pub fn factorial(n: u64) -> Self {
        fn product(lo: u64, hi: u64) -> Vec<u64> {
            // Product of lo..=hi.
            if hi - lo < 16 {
                let mut acc = vec![1u64];
                let mut small: u64 = 1;
                for i in lo..=hi {
                    if let Some(v) = small.checked_mul(i) {
                        small = v;
                    } else {
                        acc = mag::mul_limb_add(&acc, small, 0);
                        small = i;
                    }
                }
                return mag::mul_limb_add(&acc, small, 0);
            }
            let mid = lo + (hi - lo) / 2;
            mag::mul(&product(lo, mid), &product(mid + 1, hi))
        }
        if n < 2 {
            return Self::from(1);
        }
        Self::from_parts(false, product(2, n))
    }

    /// `mpz_bin_ui`: binomial coefficient `self choose k` (negative `self`
    /// uses `(-1)^k * binomial(-self + k - 1, k)`).
    pub fn binomial(&self, k: u64) -> Self {
        if self.neg {
            let n2 = &(&self.abs() + &Self::from(k)) - &Self::from(1);
            let r = n2.binomial(k);
            return if k % 2 == 1 { -&r } else { r };
        }
        let kz = Self::from(k);
        if kz > *self {
            return Self::zero();
        }
        // Use the smaller of k and n - k.
        let nk = self - &kz;
        let k = if nk < kz { nk.low_u64() } else { k };
        let mut r = Self::from(1);
        let base = self - &Self::from(k);
        for i in 1..=k {
            r = &r * &(&base + &Self::from(i));
            r = Self::from_parts(false, mag::divrem_limb(&r.mag, i).0);
        }
        r
    }
}

/// The exponent split into 4-bit windows, most significant first.
fn exponent_windows(e: &Mpz) -> Vec<usize> {
    let bits = e.bit_len();
    let n = bits.div_ceil(4);
    (0..n)
        .rev()
        .map(|i| {
            let mut w = 0usize;
            for b in (0..4).rev() {
                w = (w << 1) | usize::from(e.test_bit(i * 4 + b));
            }
            w
        })
        .collect()
}

/// `base^e mod m` for odd multi-limb `m`, `0 <= base < m`, `e > 0`.
fn powm_montgomery(base: &Mpz, e: &Mpz, m: &Mpz) -> Mpz {
    let ctx = mag::Montgomery::new(m.limbs());
    let one = ctx.to_mont(&[1]);
    let b = ctx.to_mont(base.limbs());
    let mut table = vec![one.clone(), b.clone()];
    for i in 2..16 {
        let next = ctx.mul(&table[i - 1], &b);
        table.push(next);
    }
    let mut acc = one;
    for w in exponent_windows(e) {
        for _ in 0..4 {
            acc = ctx.mul(&acc, &acc);
        }
        if w != 0 {
            acc = ctx.mul(&acc, &table[w]);
        }
    }
    Mpz::from_parts(false, ctx.redc(&acc))
}

/// Strong probable-prime test to base 2 plus the strong Lucas test with
/// Selfridge parameters, for odd `n > 1_000_000`.
fn bpsw(n: &Mpz) -> bool {
    for &p in &SMALL_PRIMES[1..] {
        if mag::rem_limb(&n.mag, p) == 0 {
            return false;
        }
    }
    if !miller_rabin(n, &Mpz::from(2)) {
        return false;
    }
    strong_lucas(n)
}

fn miller_rabin(n: &Mpz, a: &Mpz) -> bool {
    let one = Mpz::from(1);
    let nm1 = n - &one;
    let k = nm1.trailing_zeros();
    let q = nm1.shr_trunc(k);
    let mut y = a.powm(&q, n);
    if y == one || y == nm1 {
        return true;
    }
    for _ in 1..k {
        y = (&y * &y).modulo(n);
        if y == nm1 {
            return true;
        }
        if y == one {
            return false;
        }
    }
    false
}

fn strong_lucas(n: &Mpz) -> bool {
    if n.is_perfect_square() {
        return false;
    }
    // Selfridge: first D in 5, -7, 9, -11, ... with (D/n) = -1.
    let mut d: i64 = 5;
    loop {
        let j = Mpz::from(d).kronecker(n);
        if j == -1 {
            break;
        }
        if j == 0 && Mpz::from(d).cmp_abs(n) == Ordering::Less {
            return false;
        }
        d = if d > 0 { -(d + 2) } else { -d + 2 };
    }
    let p = Mpz::from(1);
    let q = Mpz::from((1 - d) / 4);
    let np1 = n + &Mpz::from(1);
    let s = np1.trailing_zeros();
    let dd = np1.shr_trunc(s);
    // Compute U_dd, V_dd, Q^dd mod n by binary method.
    let md = Mpz::from(d).modulo(n);
    let half = |x: Mpz| -> Mpz {
        let x = if x.is_odd() { &x + n } else { x };
        x.shr_trunc(1)
    };
    let mut u = Mpz::from(1);
    let mut v = p.clone();
    let mut qk = q.modulo(n);
    let bits = dd.bit_len();
    for i in (0..bits - 1).rev() {
        // Double.
        u = (&u * &v).modulo(n);
        v = (&(&v * &v) - &qk.shl(1)).modulo(n);
        qk = (&qk * &qk).modulo(n);
        if dd.test_bit(i) {
            let nu = half((&(&p * &u) + &v).modulo(n));
            let nv = half((&(&md * &u) + &(&p * &v)).modulo(n));
            u = nu;
            v = nv;
            qk = (&qk * &q).modulo(n);
        }
    }
    if u.is_zero() || v.is_zero() {
        return true;
    }
    for _ in 1..s {
        v = (&(&v * &v) - &qk.shl(1)).modulo(n);
        if v.is_zero() {
            return true;
        }
        qk = (&qk * &qk).modulo(n);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primes() {
        let p = |s: &str| Mpz::parse(s.as_bytes(), 10).unwrap().probab_prime(10);
        assert_eq!(p("10"), 0);
        assert_eq!(p("7"), 2);
        assert_eq!(p("-31"), 2);
        assert_eq!(p("172368715471481723"), 0);
        assert_eq!(p("2305843009213693951"), 1);
        assert_eq!(p("618970019642690137449562111"), 1);
        assert_eq!(p("2462906046217313"), 2);
        assert_eq!(p("2462906046218971"), 1);
        // Strong pseudoprimes to base 2 must be rejected by the Lucas test.
        assert_eq!(p("3215031751"), 0);
        assert_eq!(p("2152302898747"), 0);
        assert_eq!(Mpz::from(1000).next_prime(), Mpz::from(1009));
        assert_eq!(Mpz::from(-5).next_prime(), Mpz::from(2));
        assert_eq!(Mpz::from(1009).prev_prime(), Some((Mpz::from(997), 2)));
    }

    #[test]
    fn gcd_family() {
        let (g, s, t) = Mpz::from(123).gcdext(&Mpz::from(45));
        assert_eq!(g, Mpz::from(3));
        assert_eq!(&(&s * &Mpz::from(123)) + &(&t * &Mpz::from(45)), g);
        assert_eq!(
            Mpz::from(123_123).invert(&Mpz::from(5_467_624)),
            Some(Mpz::from(2_293_131))
        );
        assert_eq!(Mpz::from(0).invert(&Mpz::from(28347)), None);
        assert_eq!(Mpz::from(7).kronecker(&Mpz::from(23)), -1);
        assert_eq!(Mpz::from(-12).lcm(&Mpz::from(18)), Mpz::from(36));
    }

    #[test]
    fn misc() {
        assert_eq!(Mpz::factorial(20), Mpz::from(2_432_902_008_176_640_000u64));
        assert_eq!(Mpz::from(10).binomial(3), Mpz::from(120));
        assert_eq!(Mpz::from(-3).binomial(3), Mpz::from(-10));
        assert!(Mpz::from(-8).is_perfect_power());
        assert!(!Mpz::from(-4).is_perfect_power());
        assert!(Mpz::from(-64).is_perfect_power());
        assert_eq!(Mpz::from(-27).root(3), Mpz::from(-3));
        assert_eq!(Mpz::from(-30).rootrem(3), (Mpz::from(-3), Mpz::from(-3)));
        assert_eq!(
            Mpz::from(4).powm(&Mpz::from(13), &Mpz::from(-497)),
            Mpz::from(445)
        );
    }
}
