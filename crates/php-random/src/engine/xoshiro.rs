// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `Random\Engine\Xoshiro256StarStar` (`ext/random/engine_xoshiro256starstar.c`).

use super::{Engine, Generated, Seed};
use crate::error::{RandomError, Result};
use crate::state::{hex_le_to_u64, le_u64, u64_to_hex_le, StateArray, StateValue};

const CLASS: &str = "Random\\Engine\\Xoshiro256StarStar";
const JUMP: [u64; 4] = [
    0x180e_c6d3_3cfd_0aba,
    0xd5a6_1266_f0c9_392c,
    0xa958_2618_e03f_c9aa,
    0x39ab_dc45_29b1_661c,
];
const JUMP_LONG: [u64; 4] = [
    0x76e1_5d3e_fefd_cbbf,
    0xc500_4e44_1c52_2fb3,
    0x7771_0069_854e_e241,
    0x3910_9bb0_2acb_e635,
];

/// xoshiro256** with 256-bit state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Xoshiro256StarStar {
    state: [u64; 4],
}

fn splitmix64(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut r = *seed;
    r = (r ^ (r >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    r = (r ^ (r >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    r ^ (r >> 31)
}

impl Xoshiro256StarStar {
    /// `php_random_xoshiro256starstar_seed256` (no validation).
    pub const fn from_state(state: [u64; 4]) -> Self {
        Self { state }
    }

    /// `php_random_xoshiro256starstar_seed64`: four splitmix64 outputs.
    pub fn from_seed64(mut seed: u64) -> Self {
        let mut s = [0u64; 4];
        for w in &mut s {
            *w = splitmix64(&mut seed);
        }
        Self { state: s }
    }

    /// The raw state words.
    pub const fn state(&self) -> [u64; 4] {
        self.state
    }

    /// `new Random\Engine\Xoshiro256StarStar($seed)` with all of its
    /// argument validation.
    #[allow(clippy::cast_sign_loss)]
    pub fn construct(seed: Seed<'_>) -> Result<Self> {
        match seed {
            Seed::Random => loop {
                let mut b = [0u8; 32];
                crate::csprng::fill(&mut b).map_err(|_| {
                    RandomError::RandomException("Failed to generate a random seed".into())
                })?;
                let t = words(&b);
                if t != [0; 4] {
                    return Ok(Self::from_state(t));
                }
            },
            Seed::Int(i) => Ok(Self::from_seed64(i as u64)),
            Seed::Bytes(b) if b.len() == 32 => {
                let t = words(b);
                if t == [0; 4] {
                    return Err(RandomError::argument_value(
                        &format!("{CLASS}::__construct"),
                        1,
                        "seed",
                        "must not consist entirely of NUL bytes",
                    ));
                }
                Ok(Self::from_state(t))
            }
            Seed::Bytes(_) => Err(RandomError::argument_value(
                &format!("{CLASS}::__construct"),
                1,
                "seed",
                "must be a 32 byte (256 bit) string",
            )),
        }
    }

    /// Next 64-bit output.
    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.state;
        let r = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        r
    }

    fn jump_with(&mut self, jmp: &[u64; 4]) {
        let mut acc = [0u64; 4];
        for &word in jmp {
            for b in 0..64 {
                if word & (1u64 << b) != 0 {
                    for (a, s) in acc.iter_mut().zip(self.state) {
                        *a ^= s;
                    }
                }
                self.next_u64();
            }
        }
        self.state = acc;
    }

    /// `jump()`: advances by 2^128 steps.
    pub fn jump(&mut self) {
        self.jump_with(&JUMP);
    }

    /// `jumpLong()`: advances by 2^192 steps.
    pub fn jump_long(&mut self) {
        self.jump_with(&JUMP_LONG);
    }

    /// The serialized state: four hex words.
    pub fn serialize_state(&self) -> Vec<StateValue> {
        self.state
            .iter()
            .map(|&w| StateValue::Str(u64_to_hex_le(w).into_bytes()))
            .collect()
    }

    /// Restores a serialized state; `false` (state untouched) if invalid,
    /// including the all-zero state.
    pub fn unserialize_state(&mut self, data: &StateArray) -> bool {
        if data.len() != 4 {
            return false;
        }
        let mut s = [0u64; 4];
        for (i, slot) in (0i64..).zip(s.iter_mut()) {
            match data.str_of_len(i, 16).and_then(hex_le_to_u64) {
                Some(v) => *slot = v,
                None => return false,
            }
        }
        if s == [0; 4] {
            return false;
        }
        self.state = s;
        true
    }
}

fn words(b: &[u8]) -> [u64; 4] {
    [
        le_u64(&b[0..8]),
        le_u64(&b[8..16]),
        le_u64(&b[16..24]),
        le_u64(&b[24..32]),
    ]
}

impl Engine for Xoshiro256StarStar {
    fn generate(&mut self) -> Result<Generated> {
        Ok(Generated::u64(self.next_u64()))
    }
}
