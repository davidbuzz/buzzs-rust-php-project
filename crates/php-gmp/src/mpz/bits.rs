// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Bitwise operations with GMP's infinite two's complement semantics.

use super::{mag, Mpz};

/// Two's complement limbs of `x` over `n` limbs (sign-extended).
fn twos(x: &Mpz, n: usize) -> Vec<u64> {
    let mut v = x.mag.clone();
    v.resize(n, 0);
    if x.neg {
        // !(|x| - 1)
        for l in &mut v {
            let (d, borrow) = l.overflowing_sub(1);
            *l = d;
            if !borrow {
                break;
            }
        }
        for l in &mut v {
            *l = !*l;
        }
    }
    v
}

/// Inverse of [`twos`].
fn from_twos(mut v: Vec<u64>, neg: bool) -> Mpz {
    if neg {
        for l in &mut v {
            *l = !*l;
        }
        for l in &mut v {
            let (s, c) = l.overflowing_add(1);
            *l = s;
            if !c {
                break;
            }
        }
    }
    Mpz::from_parts(neg, v)
}

/// Extends `v` with zero limbs to `len` (`MPZ_REALLOC` + `MPN_ZERO` in
/// `mpz_setbit`). A large jump gets a fresh zeroed buffer instead of
/// `resize`'s reallocation copy plus zero fill: the allocator hands out
/// fresh zero pages lazily, so `gmp_setbit($n, 2**36 - 1)` only touches the
/// limbs it copies, not the whole 8 GiB.
fn grow_zeroed(v: &mut Vec<u64>, len: usize) {
    /// 2 MiB of limbs: below this, `resize`'s amortized growth wins.
    const LARGE: usize = 1 << 18;
    if len > LARGE && len / 2 > v.capacity() {
        let mut fresh = vec![0u64; len];
        fresh[..v.len()].copy_from_slice(v);
        *v = fresh;
    } else {
        v.resize(len, 0);
    }
}

fn bitwise(a: &Mpz, b: &Mpz, f: impl Fn(u64, u64) -> u64, neg: bool) -> Mpz {
    let n = a.mag.len().max(b.mag.len()) + 1;
    let (ta, tb) = (twos(a, n), twos(b, n));
    let r = ta.iter().zip(&tb).map(|(x, y)| f(*x, *y)).collect();
    from_twos(r, neg)
}

impl Mpz {
    /// `mpz_and`.
    pub fn and(&self, other: &Self) -> Self {
        bitwise(self, other, |x, y| x & y, self.neg && other.neg)
    }

    /// `mpz_ior`.
    pub fn or(&self, other: &Self) -> Self {
        bitwise(self, other, |x, y| x | y, self.neg || other.neg)
    }

    /// `mpz_xor`.
    pub fn xor(&self, other: &Self) -> Self {
        bitwise(self, other, |x, y| x ^ y, self.neg != other.neg)
    }

    /// `mpz_com`: `-self - 1`.
    pub fn com(&self) -> Self {
        &(-self) - &Self::from(1)
    }

    /// `mpz_tstbit` (two's complement for negative values).
    pub fn test_bit(&self, index: u64) -> bool {
        let li = usize::try_from(index / 64).unwrap_or(usize::MAX);
        let bit = index % 64;
        if li >= self.mag.len() {
            return self.neg;
        }
        let t = twos(self, self.mag.len());
        (t[li] >> bit) & 1 == 1
    }

    /// `mpz_setbit`.
    pub fn set_bit(&mut self, index: u64) {
        if !self.neg {
            let li = usize::try_from(index / 64).expect("bit index too large");
            if li >= self.mag.len() {
                grow_zeroed(&mut self.mag, li + 1);
            }
            self.mag[li] |= 1 << (index % 64);
        } else if !self.test_bit(index) {
            *self = self.or(&Self::pow2(index));
        }
    }

    /// `mpz_clrbit`.
    pub fn clear_bit(&mut self, index: u64) {
        if !self.neg {
            let li = usize::try_from(index / 64).unwrap_or(usize::MAX);
            if li < self.mag.len() {
                self.mag[li] &= !(1 << (index % 64));
                mag::normalize(&mut self.mag);
            }
        } else if self.test_bit(index) {
            *self = self.and(&Self::pow2(index).com());
        }
    }

    /// `mpz_scan0`: index of the first 0 bit at or above `start`
    /// (`u64::MAX` if there is none).
    pub fn scan0(&self, start: u64) -> u64 {
        self.scan(start, false)
    }

    /// `mpz_scan1`: index of the first 1 bit at or above `start`
    /// (`u64::MAX` if there is none).
    pub fn scan1(&self, start: u64) -> u64 {
        self.scan(start, true)
    }

    fn scan(&self, start: u64, want: bool) -> u64 {
        let n = self.mag.len();
        let total = n as u64 * 64;
        if start >= total {
            // Beyond the magnitude all bits equal the sign.
            return if self.neg == want { start } else { u64::MAX };
        }
        let t = twos(self, n);
        let mut li = usize::try_from(start / 64).unwrap_or(n);
        let mut word = if want { t[li] } else { !t[li] };
        word &= u64::MAX << (start % 64);
        loop {
            if word != 0 {
                return li as u64 * 64 + u64::from(word.trailing_zeros());
            }
            li += 1;
            if li >= n {
                return if self.neg == want { total } else { u64::MAX };
            }
            word = if want { t[li] } else { !t[li] };
        }
    }

    /// `mpz_popcount` (`u64::MAX` for negative values).
    pub fn popcount(&self) -> u64 {
        if self.neg {
            u64::MAX
        } else {
            self.mag.iter().map(|l| u64::from(l.count_ones())).sum()
        }
    }

    /// `mpz_hamdist` (`u64::MAX` when the signs differ).
    pub fn hamdist(&self, other: &Self) -> u64 {
        if self.neg != other.neg {
            return u64::MAX;
        }
        let n = self.mag.len().max(other.mag.len());
        let (a, b) = (twos(self, n), twos(other, n));
        a.iter()
            .zip(&b)
            .map(|(x, y)| u64::from((x ^ y).count_ones()))
            .sum()
    }

    /// Number of trailing zero bits of `|self|` (0 for zero).
    pub fn trailing_zeros(&self) -> u64 {
        let z = self.mag.iter().take_while(|l| **l == 0).count();
        match self.mag.get(z) {
            None => 0,
            Some(l) => z as u64 * 64 + u64::from(l.trailing_zeros()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_ops() {
        let a = Mpz::from(-12);
        let b = Mpz::from(10);
        assert_eq!(a.and(&b), Mpz::from(-0b1100 & 0b1010));
        assert_eq!(a.or(&b), Mpz::from(-0b1100 | 0b1010));
        assert_eq!(a.xor(&b), Mpz::from(-0b1100 ^ 0b1010));
        assert_eq!(a.com(), Mpz::from(11));
        assert!(a.test_bit(100));
        assert!(!a.test_bit(0));
        assert_eq!(a.scan1(0), 2);
        assert_eq!(a.scan0(2), 3);
        assert_eq!(Mpz::from(-1).scan0(5), u64::MAX);
        assert_eq!(b.scan1(4), u64::MAX);
        assert_eq!(b.scan0(4), 4);
        let mut c = Mpz::from(-1);
        c.clear_bit(70);
        assert_eq!(c, &Mpz::from(-1) - &Mpz::pow2(70));
        c.set_bit(70);
        assert_eq!(c, Mpz::from(-1));
    }

    #[test]
    fn set_bit_far_above_the_top() {
        let index = 64 * (1u64 << 19) + 5;
        let mut m = Mpz::from(5);
        m.set_bit(index);
        assert_eq!(m.bit_len(), index + 1);
        assert_eq!(m.limbs().len(), (1 << 19) + 1);
        assert!(m.test_bit(0) && !m.test_bit(1) && m.test_bit(2));
        assert!(m.limbs()[1..1 << 19].iter().all(|&l| l == 0));
        assert_eq!(m.popcount(), 3);
        // A small step above the top goes through `resize`.
        m.set_bit(index + 64);
        assert_eq!(m.bit_len(), index + 65);
        assert_eq!(m.popcount(), 4);
    }
}
