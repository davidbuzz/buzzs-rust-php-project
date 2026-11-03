//! Port of GMP's Mersenne Twister random state (`gmp_randinit_mt`,
//! `gmp_randseed`, `mpz_urandomb`, `mpz_urandomm`), so that seeded
//! `gmp_random_*` sequences match PHP bit for bit.

use crate::mpz::{mag, Mpz};
use std::cmp::Ordering;

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_B0DF;
const TEMPER_MASK_B: u32 = 0x9D2C_5680;
const TEMPER_MASK_C: u32 = 0xEFC6_0000;
const WARM_UP: usize = 2000;
const MAX_URANDOMM_ITER: u32 = 80;

/// A GMP Mersenne Twister state.
#[derive(Clone)]
pub struct MtState {
    mt: [u32; N],
    mti: usize,
}

impl std::fmt::Debug for MtState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MtState")
            .field("mti", &self.mti)
            .finish_non_exhaustive()
    }
}

fn recalc_buffer(mt: &mut [u32; N]) {
    let twist = |y: u32| (y >> 1) ^ if y & 1 != 0 { MATRIX_A } else { 0 };
    for kk in 0..N - M {
        let y = (mt[kk] & 0x8000_0000) | (mt[kk + 1] & 0x7FFF_FFFF);
        mt[kk] = mt[kk + M] ^ twist(y);
    }
    for kk in N - M..N - 1 {
        let y = (mt[kk] & 0x8000_0000) | (mt[kk + 1] & 0x7FFF_FFFF);
        mt[kk] = mt[kk + M - N] ^ twist(y);
    }
    let y = (mt[N - 1] & 0x8000_0000) | (mt[0] & 0x7FFF_FFFF);
    mt[N - 1] = mt[M - 1] ^ twist(y);
}

/// `b^1074888996 mod (2^19937 - 20023)` with GMP's partial reduction.
fn mangle_seed(r: &Mpz) -> Mpz {
    let b = r.clone();
    let mut r = r.clone();
    let mut e: u64 = 0x4011_8124;
    let mut bit: u64 = 0x2000_0000;
    let k = Mpz::from(20023);
    let reduce = |r: &mut Mpz| loop {
        let t = r.shr_trunc(19937);
        if t.is_zero() {
            break;
        }
        let low = Mpz::from_parts(false, low_bits(r.limbs(), 19937));
        *r = &low + &(&t * &k);
    };
    loop {
        r = &r * &r;
        reduce(&mut r);
        if e & bit != 0 {
            e ^= bit;
            r = &r * &b;
            reduce(&mut r);
        }
        bit >>= 1;
        if bit == 0 {
            break;
        }
    }
    r
}

fn low_bits(a: &[u64], bits: usize) -> Vec<u64> {
    let full = bits / 64;
    let mut v: Vec<u64> = a.iter().take(full + 1).copied().collect();
    if v.len() > full {
        v[full] &= (1u64 << (bits % 64)) - 1;
    }
    v
}

impl MtState {
    /// A state seeded with `gmp_randseed(seed)`.
    pub fn new(seed: &Mpz) -> Self {
        let mut s = Self { mt: [0; N], mti: 0 };
        s.seed(seed);
        s
    }

    /// A state seeded with `gmp_randseed_ui(seed)` (how PHP initializes the
    /// generator from random bytes before the first `gmp_random_*` call).
    pub fn from_u64(seed: u64) -> Self {
        Self::new(&Mpz::from(seed))
    }

    /// `gmp_randseed`: reseeds from an arbitrary integer.
    pub fn seed(&mut self, seed: &Mpz) {
        let modulus = &Mpz::pow2(19937) - &Mpz::from(20027);
        let seed1 = &seed.modulo(&modulus) + &Mpz::from(2);
        let mut seed1 = mangle_seed(&seed1);
        self.mt[0] = if seed1.test_bit(19936) {
            0x8000_0000
        } else {
            0
        };
        seed1.clear_bit(19936);
        let limbs = seed1.limbs();
        let mut words = Vec::with_capacity(limbs.len() * 2);
        for l in limbs {
            words.push(*l as u32);
            words.push((*l >> 32) as u32);
        }
        while words.last() == Some(&0) {
            words.pop();
        }
        for i in 1..N {
            self.mt[i] = words.get(i - 1).copied().unwrap_or(0);
        }
        for _ in 0..WARM_UP / N {
            recalc_buffer(&mut self.mt);
        }
        self.mti = WARM_UP % N;
    }

    fn next_u32(&mut self) -> u32 {
        if self.mti >= N {
            recalc_buffer(&mut self.mt);
            self.mti = 0;
        }
        let mut y = self.mt[self.mti];
        self.mti += 1;
        y ^= y >> 11;
        y ^= (y << 7) & TEMPER_MASK_B;
        y ^= (y << 15) & TEMPER_MASK_C;
        y ^= y >> 18;
        y
    }

    /// `_gmp_rand`: `nbits` random bits as limbs (not normalized).
    fn rand_limbs(&mut self, nbits: u64) -> Vec<u64> {
        let nlimbs = usize::try_from(nbits / 64).expect("too many random bits");
        let rbits = nbits % 64;
        let mut dest = Vec::with_capacity(nlimbs + 1);
        for _ in 0..nlimbs {
            let lo = u64::from(self.next_u32());
            let hi = u64::from(self.next_u32());
            dest.push(lo | (hi << 32));
        }
        if rbits != 0 {
            let v = if rbits < 32 {
                u64::from(self.next_u32()) & ((1u64 << rbits) - 1)
            } else {
                let mut v = u64::from(self.next_u32());
                if rbits > 32 {
                    let hi = u64::from(self.next_u32()) & ((1u64 << (rbits - 32)) - 1);
                    v |= hi << 32;
                }
                v
            };
            dest.push(v);
        }
        dest
    }

    /// `mpz_urandomb`: uniform in `[0, 2^nbits)`.
    pub fn urandomb(&mut self, nbits: u64) -> Mpz {
        Mpz::from_parts(false, self.rand_limbs(nbits))
    }

    /// `mpz_urandomm`: uniform in `[0, n)` for non-zero `n` (sign ignored).
    pub fn urandomm(&mut self, n: &Mpz) -> Mpz {
        assert!(!n.is_zero(), "urandomm by zero");
        let np = n.limbs();
        let size = np.len();
        let pow2 = np[size - 1].is_power_of_two() && np[..size - 1].iter().all(|l| *l == 0);
        let nbits = n.bit_len() - u64::from(pow2);
        if nbits == 0 {
            return Mpz::zero();
        }
        let mut count = MAX_URANDOMM_ITER;
        loop {
            let mut r = self.rand_limbs(nbits);
            r.resize(size, 0);
            let cmp = mag::cmp(&r, np);
            if cmp == Ordering::Less {
                return Mpz::from_parts(false, r);
            }
            count -= 1;
            if count == 0 {
                return Mpz::from_parts(false, mag::sub(&r, np));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_seed_zero_sequence() {
        let mut st = MtState::new(&Mpz::zero());
        assert_eq!(st.urandomb(10).to_string(), "107");
        assert_eq!(
            st.urandomb(100).to_string(),
            "576055025228722307492589900056"
        );
    }
}
