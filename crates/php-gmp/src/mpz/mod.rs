//! A signed arbitrary-precision integer with GMP `mpz_t` semantics.

mod bits;
mod convert;
pub(crate) mod mag;
mod theory;

pub(crate) use convert::is_c_space;
pub use convert::{Endian, WordOrder};

use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, Mul, Neg, Sub};

/// A signed arbitrary-precision integer.
///
/// The magnitude is stored as little-endian 64-bit limbs without high zero
/// limbs; zero is never negative.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
#[must_use]
pub struct Mpz {
    neg: bool,
    mag: Vec<u64>,
}

/// Rounding mode for the division family (`GMP_ROUND_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Round {
    /// Truncate toward zero (`tdiv`).
    Zero,
    /// Round toward +infinity (`cdiv`).
    PlusInf,
    /// Round toward -infinity (`fdiv`).
    MinusInf,
}

impl Mpz {
    /// Zero.
    pub const fn zero() -> Self {
        Self {
            neg: false,
            mag: Vec::new(),
        }
    }

    /// Builds a value from a sign and magnitude limbs (normalizing them).
    pub fn from_parts(neg: bool, mut mag: Vec<u64>) -> Self {
        mag::normalize(&mut mag);
        let neg = neg && !mag.is_empty();
        Self { neg, mag }
    }

    /// Magnitude limbs (little-endian, normalized).
    pub fn limbs(&self) -> &[u64] {
        &self.mag
    }

    /// `mpz_sgn`: -1, 0 or 1.
    pub fn sign(&self) -> i32 {
        if self.mag.is_empty() {
            0
        } else if self.neg {
            -1
        } else {
            1
        }
    }

    /// Whether the value is zero.
    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }

    /// Whether the value is negative.
    pub fn is_negative(&self) -> bool {
        self.neg
    }

    /// Whether the value is odd.
    pub fn is_odd(&self) -> bool {
        self.mag.first().is_some_and(|l| l & 1 == 1)
    }

    /// `|self|`.
    pub fn abs(&self) -> Self {
        Self {
            neg: false,
            mag: self.mag.clone(),
        }
    }

    /// Number of significant bits of `|self|` (0 for zero).
    pub fn bit_len(&self) -> u64 {
        mag::bit_len(&self.mag)
    }

    /// Compares absolute values.
    pub fn cmp_abs(&self, other: &Self) -> Ordering {
        mag::cmp(&self.mag, &other.mag)
    }

    /// `mpz_fits_slong_p`.
    pub fn fits_i64(&self) -> bool {
        self.to_i64().is_some()
    }

    /// `mpz_fits_ulong_p`.
    pub fn fits_u64(&self) -> bool {
        !self.neg && self.mag.len() <= 1
    }

    /// Exact conversion to `i64`.
    pub fn to_i64(&self) -> Option<i64> {
        match self.mag.len() {
            0 => Some(0),
            1 => {
                let m = self.mag[0];
                if self.neg {
                    if m <= 1u64 << 63 {
                        Some(0i64.wrapping_sub_unsigned(m))
                    } else {
                        None
                    }
                } else {
                    i64::try_from(m).ok()
                }
            }
            _ => None,
        }
    }

    /// Exact conversion to `u64`.
    pub fn to_u64(&self) -> Option<u64> {
        if self.fits_u64() {
            Some(self.mag.first().copied().unwrap_or(0))
        } else {
            None
        }
    }

    /// `mpz_get_ui`: the low limb of `|self|`.
    pub fn low_u64(&self) -> u64 {
        self.mag.first().copied().unwrap_or(0)
    }

    /// `mpz_get_si`: the low 63 bits of `|self|` with the sign applied
    /// (two's complement style for negative values), as GMP does.
    pub fn get_si(&self) -> i64 {
        let zl = self.low_u64();
        let low = i64::try_from(zl & (i64::MAX as u64)).unwrap_or(0);
        match self.sign() {
            1 => low,
            -1 => -1 - i64::try_from(zl.wrapping_sub(1) & (i64::MAX as u64)).unwrap_or(0),
            _ => 0,
        }
    }

    /// `mpz_get_d`: conversion to `f64` truncating toward zero
    /// (infinite when out of range).
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_wrap)]
    pub fn to_f64(&self) -> f64 {
        let bits = self.bit_len();
        let v = if bits == 0 {
            0.0
        } else if bits <= 64 {
            // Exact up to 53 bits; truncate the rest explicitly.
            let m = self.mag[0];
            let drop = bits.saturating_sub(53);
            ((m >> drop) as f64) * (2f64).powi(i32::try_from(drop).unwrap_or(0))
        } else if bits > 1024 {
            f64::INFINITY
        } else {
            let top = mag::shr(&self.mag, bits - 53);
            let m = top[0] as f64;
            let e = i32::try_from(bits - 53).unwrap_or(i32::MAX);
            // m < 2^53 and e <= 971, so the product is exact or overflows.
            let half = e / 2;
            m * (2f64).powi(half) * (2f64).powi(e - half)
        };
        if self.neg {
            -v
        } else {
            v
        }
    }

    /// Truncating conversion from `f64` (`mpz_set_d`); NaN/inf yield `None`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn from_f64(v: f64) -> Option<Self> {
        if !v.is_finite() {
            return None;
        }
        let t = v.trunc();
        let neg = t < 0.0;
        let a = t.abs();
        if a < 1.0 {
            return Some(Self::zero());
        }
        let bits = a.to_bits();
        let exp = i64::try_from((bits >> 52) & 0x7FF).unwrap_or(0) - 1075;
        let mant = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
        let mag = if exp >= 0 {
            mag::shl(&[mant], exp as u64)
        } else {
            vec![mant >> (-exp)]
        };
        Some(Self::from_parts(neg, mag))
    }

    /// `2^n`.
    pub fn pow2(n: u64) -> Self {
        Self::from_parts(false, mag::shl(&[1], n))
    }

    /// `self^e` (`mpz_pow_ui`).
    pub fn pow(&self, e: u64) -> Self {
        Self::from_parts(self.neg && e & 1 == 1, mag::pow(&self.mag, e))
    }

    /// `self * 2^n` (`mpz_mul_2exp`).
    pub fn shl(&self, n: u64) -> Self {
        Self::from_parts(self.neg, mag::shl(&self.mag, n))
    }

    /// `floor(self / 2^n)` (`mpz_fdiv_q_2exp`).
    pub fn shr_floor(&self, n: u64) -> Self {
        let q = mag::shr(&self.mag, n);
        if self.neg && mag::low_bits_nonzero(&self.mag, n) {
            Self::from_parts(true, mag::add(&q, &[1]))
        } else {
            Self::from_parts(self.neg, q)
        }
    }

    /// `trunc(self / 2^n)` (`mpz_tdiv_q_2exp`).
    pub fn shr_trunc(&self, n: u64) -> Self {
        Self::from_parts(self.neg, mag::shr(&self.mag, n))
    }

    /// Quotient and remainder with the given rounding; `d` must be non-zero.
    ///
    /// The remainder has the sign of `d` (floor), opposite of `d` (ceil) or of
    /// `self` (trunc), as in GMP.
    pub fn div_qr(&self, d: &Self, round: Round) -> (Self, Self) {
        assert!(!d.is_zero(), "division by zero");
        let (q, r) = mag::divrem(&self.mag, &d.mag);
        let qneg = self.neg != d.neg;
        let mut q = Self::from_parts(qneg, q);
        let mut r = Self::from_parts(self.neg, r);
        if !r.is_zero() {
            let adjust = match round {
                Round::Zero => false,
                // Floor: adjust when the exact quotient is negative.
                Round::MinusInf => qneg,
                Round::PlusInf => !qneg,
            };
            if adjust {
                if qneg {
                    q = &q - &Self::from(1);
                } else {
                    q = &q + &Self::from(1);
                }
                // r = self - q*d
                r = self - &(&q * d);
            }
        }
        (q, r)
    }

    /// Quotient only.
    pub fn div_q(&self, d: &Self, round: Round) -> Self {
        self.div_qr(d, round).0
    }

    /// Remainder only.
    pub fn div_r(&self, d: &Self, round: Round) -> Self {
        self.div_qr(d, round).1
    }

    /// `mpz_mod`: non-negative remainder modulo `|d|`.
    pub fn modulo(&self, d: &Self) -> Self {
        let r = self.div_r(d, Round::Zero);
        if r.neg {
            &r + &d.abs()
        } else {
            r
        }
    }

    /// `mpz_divexact`, including GMP's 2-adic result for inexact divisions.
    pub fn divexact(&self, d: &Self) -> Self {
        assert!(!d.is_zero(), "division by zero");
        let (nn, dn) = (self.mag.len(), d.mag.len());
        if nn < dn {
            return Self::zero();
        }
        let qn = nn - dn + 1;
        let z = d.mag.iter().take_while(|l| **l == 0).count();
        let shift = u64::from(d.mag[z].trailing_zeros());
        let total = 64 * z as u64 + shift;
        let n1 = mag::shr(&self.mag, total);
        let d1 = mag::shr(&d.mag, total);
        // q = n1 * d1^{-1} mod B^qn
        let inv = inverse_mod_pow2(&d1, qn);
        let mut q = mag::mul(&n1[..n1.len().min(qn)], &inv);
        q.truncate(qn);
        Self::from_parts(self.neg != d.neg, q)
    }
}

/// Inverse of an odd magnitude modulo `B^n` (Newton/Hensel lifting).
fn inverse_mod_pow2(d: &[u64], n: usize) -> Vec<u64> {
    // Limb inverse first.
    let d0 = d[0];
    let mut x: u64 = 1;
    for _ in 0..6 {
        x = x.wrapping_mul(2u64.wrapping_sub(d0.wrapping_mul(x)));
    }
    let mut inv = vec![x];
    let mut prec = 1usize;
    while prec < n {
        prec = (prec * 2).min(n);
        // inv = inv * (2 - d*inv) mod B^prec
        let dt = &d[..d.len().min(prec)];
        let mut t = mag::mul(dt, &inv);
        t.resize(prec, 0);
        // two_minus = 2 - t mod B^prec  ==  (!t + 1) + 2 ... compute B^prec - t + 2
        let mut neg_t: Vec<u64> = t.iter().map(|l| !l).collect();
        let mut carry = 3u64; // !t + 1 + 2
        for l in &mut neg_t {
            let (s, c) = l.overflowing_add(carry);
            *l = s;
            carry = u64::from(c);
            if carry == 0 {
                break;
            }
        }
        let mut r = mag::mul(&inv, &neg_t);
        r.resize(prec, 0);
        mag::normalize(&mut r);
        inv = r;
        inv.resize(prec, 0);
    }
    inv.truncate(n);
    mag::normalize(&mut inv);
    inv
}

impl From<i64> for Mpz {
    fn from(v: i64) -> Self {
        Self::from_parts(v < 0, vec![v.unsigned_abs()])
    }
}

impl From<u64> for Mpz {
    fn from(v: u64) -> Self {
        Self::from_parts(false, vec![v])
    }
}

impl From<i32> for Mpz {
    fn from(v: i32) -> Self {
        Self::from(i64::from(v))
    }
}

impl Ord for Mpz {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.neg, other.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => mag::cmp(&self.mag, &other.mag),
            (true, true) => mag::cmp(&other.mag, &self.mag),
        }
    }
}

impl PartialOrd for Mpz {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Mpz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_str_radix(10))
    }
}

impl fmt::Debug for Mpz {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Mpz({self})")
    }
}

fn add_signed(a: &Mpz, b: &Mpz, b_neg: bool) -> Mpz {
    if a.neg == b_neg {
        Mpz::from_parts(a.neg, mag::add(&a.mag, &b.mag))
    } else {
        match mag::cmp(&a.mag, &b.mag) {
            Ordering::Equal => Mpz::zero(),
            Ordering::Greater => Mpz::from_parts(a.neg, mag::sub(&a.mag, &b.mag)),
            Ordering::Less => Mpz::from_parts(b_neg, mag::sub(&b.mag, &a.mag)),
        }
    }
}

impl Add for &Mpz {
    type Output = Mpz;
    fn add(self, rhs: &Mpz) -> Mpz {
        add_signed(self, rhs, rhs.neg)
    }
}

impl Sub for &Mpz {
    type Output = Mpz;
    fn sub(self, rhs: &Mpz) -> Mpz {
        add_signed(self, rhs, !rhs.neg && !rhs.is_zero())
    }
}

impl Mul for &Mpz {
    type Output = Mpz;
    fn mul(self, rhs: &Mpz) -> Mpz {
        Mpz::from_parts(self.neg != rhs.neg, mag::mul(&self.mag, &rhs.mag))
    }
}

impl Neg for &Mpz {
    type Output = Mpz;
    fn neg(self) -> Mpz {
        Mpz::from_parts(!self.neg, self.mag.clone())
    }
}

impl Add for Mpz {
    type Output = Mpz;
    fn add(self, rhs: Mpz) -> Mpz {
        &self + &rhs
    }
}

impl Sub for Mpz {
    type Output = Mpz;
    fn sub(self, rhs: Mpz) -> Mpz {
        &self - &rhs
    }
}

impl Mul for Mpz {
    type Output = Mpz;
    fn mul(self, rhs: Mpz) -> Mpz {
        &self * &rhs
    }
}

impl Neg for Mpz {
    type Output = Mpz;
    fn neg(self) -> Mpz {
        -&self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(s: &str) -> Mpz {
        Mpz::parse(s.as_bytes(), 10).expect("valid")
    }

    #[test]
    fn division_modes() {
        let cases = [(7, 2), (-7, 2), (7, -2), (-7, -2), (6, 3), (-6, 3)];
        for (a, b) in cases {
            let (a, b) = (Mpz::from(a), Mpz::from(b));
            for round in [Round::Zero, Round::PlusInf, Round::MinusInf] {
                let (q, r) = a.div_qr(&b, round);
                assert_eq!(&(&q * &b) + &r, a);
            }
        }
        assert_eq!(
            Mpz::from(-7).div_q(&Mpz::from(2), Round::MinusInf),
            Mpz::from(-4)
        );
        assert_eq!(
            Mpz::from(-7).div_q(&Mpz::from(2), Round::PlusInf),
            Mpz::from(-3)
        );
        assert_eq!(
            Mpz::from(7).div_q(&Mpz::from(2), Round::PlusInf),
            Mpz::from(4)
        );
        assert_eq!(Mpz::from(-7).modulo(&Mpz::from(-3)), Mpz::from(2));
    }

    #[test]
    fn get_si_and_d() {
        assert_eq!(z("9223372036854775808").get_si(), 0);
        assert_eq!(z("-9223372036854775808").get_si(), i64::MIN);
        assert_eq!(z("-9223372036854775809").get_si(), -1);
        assert_eq!(z("18446744073709551615").get_si(), i64::MAX);
        assert_eq!(
            z("9007199254740993").to_f64().to_bits(),
            9_007_199_254_740_992f64.to_bits()
        );
        assert_eq!(
            z("-9007199254740995").to_f64().to_bits(),
            (-9_007_199_254_740_994f64).to_bits()
        );
    }

    #[test]
    fn divexact_inexact_matches_gmp() {
        assert_eq!(
            Mpz::from(233).divexact(&Mpz::from(7)),
            z("15811494920322472847")
        );
        assert_eq!(
            Mpz::from(1000).divexact(&Mpz::from(3)),
            z("12297829382473034744")
        );
        assert_eq!(
            Mpz::from(-1001).divexact(&Mpz::from(6)),
            z("-6148914691236517372")
        );
        assert_eq!(
            z("18446744073709551621").divexact(&z("18446744073709551617")),
            Mpz::from(5)
        );
        assert_eq!(
            Mpz::from(233).divexact(&z("23345555555555555555555555")),
            Mpz::zero()
        );
    }
}
