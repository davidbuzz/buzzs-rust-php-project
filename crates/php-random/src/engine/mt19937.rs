// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `Random\Engine\Mt19937` (`ext/random/engine_mt19937.c`).

use super::{Engine, Generated};
use crate::error::{RandomError, Result};
use crate::state::{hex_le_to_u32, u32_to_hex_le, StateArray, StateValue};

/// Length of the state vector.
pub const N: usize = 624;
const N_U32: u32 = 624;
const N_I64: i64 = 624;
const M: usize = 397;

/// The `E_DEPRECATED` message emitted when `MT_RAND_PHP` is selected.
pub const MT_RAND_PHP_DEPRECATION: &str = "The MT_RAND_PHP variant of Mt19937 is deprecated";

/// `enum php_random_mt19937_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mt19937Mode {
    /// `MT_RAND_MT19937` (0): the standard, correct Mersenne Twister.
    #[default]
    Mt19937,
    /// `MT_RAND_PHP` (1): PHP's historical buggy twist (deprecated).
    Php,
}

impl Mt19937Mode {
    /// The value of the `MT_RAND_*` constant.
    pub const fn as_long(self) -> i64 {
        match self {
            Self::Mt19937 => 0,
            Self::Php => 1,
        }
    }

    /// Parses a mode argument for `Random\Engine\Mt19937::__construct()`:
    /// anything but 0 or 1 is a `ValueError`.
    pub fn from_constructor_arg(mode: i64) -> Result<Self> {
        match mode {
            0 => Ok(Self::Mt19937),
            1 => Ok(Self::Php),
            _ => Err(RandomError::argument_value(
                "Random\\Engine\\Mt19937::__construct",
                2,
                "mode",
                "must be either MT_RAND_MT19937 or MT_RAND_PHP",
            )),
        }
    }

    /// Parses a mode argument for `mt_srand()`/`srand()`: 1 selects
    /// `MT_RAND_PHP`, every other value means `MT_RAND_MT19937`.
    pub const fn from_srand_arg(mode: i64) -> Self {
        if mode == 1 {
            Self::Php
        } else {
            Self::Mt19937
        }
    }
}

/// Mersenne Twister state (`php_random_status_state_mt19937`).
#[derive(Clone, PartialEq, Eq)]
pub struct Mt19937 {
    state: [u32; N],
    count: u32,
    mode: Mt19937Mode,
}

impl std::fmt::Debug for Mt19937 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mt19937")
            .field("count", &self.count)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

#[inline]
fn mix_bits(u: u32, v: u32) -> u32 {
    (u & 0x8000_0000) | (v & 0x7FFF_FFFF)
}

#[inline]
fn twist(m: u32, u: u32, v: u32) -> u32 {
    m ^ (mix_bits(u, v) >> 1) ^ ((v & 1).wrapping_neg() & 0x9908_b0df)
}

#[inline]
fn twist_php(m: u32, u: u32, v: u32) -> u32 {
    m ^ (mix_bits(u, v) >> 1) ^ ((u & 1).wrapping_neg() & 0x9908_b0df)
}

impl Mt19937 {
    /// `php_random_mt19937_seed32` with the given mode.
    pub fn new(seed: u32, mode: Mt19937Mode) -> Self {
        let mut mt = Self {
            state: [0; N],
            count: 0,
            mode,
        };
        mt.seed(seed);
        mt
    }

    /// `new Random\Engine\Mt19937($seed, $mode)` (the seed is truncated to
    /// 32 bits like the C code does; `None` seeds from the CSPRNG). The
    /// caller must emit [`MT_RAND_PHP_DEPRECATION`] when `mode` is
    /// `MT_RAND_PHP` (the mode is validated first).
    pub fn construct(seed: Option<i64>, mode: i64) -> Result<Self> {
        let mode = Mt19937Mode::from_constructor_arg(mode)?;
        let seed = if let Some(s) = seed {
            s
        } else {
            let mut b = [0u8; 8];
            crate::csprng::fill(&mut b).map_err(|_| {
                RandomError::RandomException("Failed to generate a random seed".into())
            })?;
            i64::from_le_bytes(b)
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Ok(Self::new(seed as u32, mode))
    }

    /// Reseeds (`php_random_mt19937_seed32`), keeping the mode.
    pub fn seed(&mut self, seed: u32) {
        self.state[0] = seed;
        for i in 1..N {
            let prev = self.state[i - 1];
            // `i < 624`, so the cast is lossless.
            #[allow(clippy::cast_possible_truncation)]
            let idx = i as u32;
            self.state[i] = 1_812_433_253u32
                .wrapping_mul(prev ^ (prev >> 30))
                .wrapping_add(idx);
        }
        self.count = N_U32;
        self.reload();
    }

    /// The current mode.
    pub fn mode(&self) -> Mt19937Mode {
        self.mode
    }

    /// Changes the mode without reseeding (the next reload uses it).
    pub fn set_mode(&mut self, mode: Mt19937Mode) {
        self.mode = mode;
    }

    fn reload(&mut self) {
        let s = &mut self.state;
        let tw: fn(u32, u32, u32) -> u32 = match self.mode {
            Mt19937Mode::Mt19937 => twist,
            Mt19937Mode::Php => twist_php,
        };
        for i in 0..N - M {
            s[i] = tw(s[i + M], s[i], s[i + 1]);
        }
        for i in N - M..N - 1 {
            s[i] = tw(s[i + M - N], s[i], s[i + 1]);
        }
        s[N - 1] = tw(s[M - 1], s[N - 1], s[0]);
        self.count = 0;
    }

    /// Next 32-bit output.
    pub fn next_u32(&mut self) -> u32 {
        if self.count as usize >= N {
            self.reload();
        }
        let mut s1 = self.state[self.count as usize];
        self.count += 1;
        s1 ^= s1 >> 11;
        s1 ^= (s1 << 7) & 0x9d2c_5680;
        s1 ^= (s1 << 15) & 0xefc6_0000;
        s1 ^ (s1 >> 18)
    }

    /// The serialized state: 624 hex words, the position and the mode.
    pub fn serialize_state(&self) -> Vec<StateValue> {
        let mut v: Vec<StateValue> = self
            .state
            .iter()
            .map(|&w| StateValue::Str(u32_to_hex_le(w).into_bytes()))
            .collect();
        v.push(StateValue::Int(i64::from(self.count)));
        v.push(StateValue::Int(self.mode.as_long()));
        v
    }

    /// Restores a state produced by [`Mt19937::serialize_state`]; returns
    /// `false` (leaving `self` untouched) if the data is invalid.
    pub fn unserialize_state(&mut self, data: &StateArray) -> bool {
        if data.len() != N + 2 {
            return false;
        }
        let mut state = [0u32; N];
        for (i, slot) in state.iter_mut().enumerate() {
            #[allow(clippy::cast_possible_wrap)]
            let Some(hex) = data.str_of_len(i as i64, 8) else {
                return false;
            };
            let Some(w) = hex_le_to_u32(hex) else {
                return false;
            };
            *slot = w;
        }
        // The C code stores the zend_long into uint32_t / the enum first.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let Some(count) = data.int(N_I64).map(|c| c as u32) else {
            return false;
        };
        if count as usize > N {
            return false;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let mode = match data.int(N_I64 + 1).map(|m| m as u32) {
            Some(0) => Mt19937Mode::Mt19937,
            Some(1) => Mt19937Mode::Php,
            _ => return false,
        };
        self.state = state;
        self.count = count;
        self.mode = mode;
        true
    }
}

impl Engine for Mt19937 {
    fn generate(&mut self) -> Result<Generated> {
        Ok(Generated::u32(self.next_u32()))
    }

    fn is_mt19937_legacy(&self) -> bool {
        self.mode != Mt19937Mode::Mt19937
    }
}
