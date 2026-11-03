//! `Random\Engine\PcgOneseq128XslRr64` (`ext/random/engine_pcgoneseq128xslrr64.c`).

use super::{Engine, Generated, Seed};
use crate::error::{RandomError, Result};
use crate::state::{hex_le_to_u64, le_u64, u64_to_hex_le, StateArray, StateValue};

const MULT: u128 = 0x2360_ed05_1fc6_5da4_4385_df64_9fcc_f645;
const INC: u128 = 0x5851_f42d_4c95_7f2d_1405_7b7e_f767_814f;
const CLASS: &str = "Random\\Engine\\PcgOneseq128XslRr64";

/// PCG64 (one sequence, XSL-RR output) with 128-bit state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PcgOneseq128XslRr64 {
    state: u128,
}

impl PcgOneseq128XslRr64 {
    /// `php_random_pcgoneseq128xslrr64_seed128`.
    pub fn from_seed128(seed: u128) -> Self {
        let mut s = Self { state: 0 };
        s.step();
        s.state = s.state.wrapping_add(seed);
        s.step();
        s
    }

    /// Seeds from an integer (`new PcgOneseq128XslRr64(int)`).
    #[allow(clippy::cast_sign_loss)]
    pub fn from_int(seed: i64) -> Self {
        Self::from_seed128(u128::from(seed as u64))
    }

    /// Builds the engine from a raw 128-bit state (no seeding steps).
    pub const fn from_state(state: u128) -> Self {
        Self { state }
    }

    /// The raw 128-bit state.
    pub const fn state(&self) -> u128 {
        self.state
    }

    /// `new Random\Engine\PcgOneseq128XslRr64($seed)` with all of its
    /// argument validation.
    pub fn construct(seed: Seed<'_>) -> Result<Self> {
        match seed {
            Seed::Random => {
                let mut b = [0u8; 16];
                crate::csprng::fill(&mut b).map_err(|_| {
                    RandomError::RandomException("Failed to generate a random seed".into())
                })?;
                Ok(Self::from_seed128(u128::from_le_bytes(b)))
            }
            Seed::Int(i) => Ok(Self::from_int(i)),
            Seed::Bytes(b) if b.len() == 16 => {
                let hi = le_u64(&b[..8]);
                let lo = le_u64(&b[8..]);
                Ok(Self::from_seed128((u128::from(hi) << 64) | u128::from(lo)))
            }
            Seed::Bytes(_) => Err(RandomError::argument_value(
                &format!("{CLASS}::__construct"),
                1,
                "seed",
                "must be a 16 byte (128 bit) string",
            )),
        }
    }

    #[inline]
    fn step(&mut self) {
        self.state = self.state.wrapping_mul(MULT).wrapping_add(INC);
    }

    /// Next 64-bit output.
    pub fn next_u64(&mut self) -> u64 {
        self.step();
        #[allow(clippy::cast_possible_truncation)]
        let v = ((self.state >> 64) as u64) ^ (self.state as u64);
        #[allow(clippy::cast_possible_truncation)]
        let rot = (self.state >> 122) as u32;
        v.rotate_right(rot)
    }

    /// `php_random_pcgoneseq128xslrr64_advance`.
    pub fn advance(&mut self, mut advance: u64) {
        let mut cur_mult = MULT;
        let mut cur_plus = INC;
        let mut acc_mult: u128 = 1;
        let mut acc_plus: u128 = 0;
        while advance > 0 {
            if advance & 1 != 0 {
                acc_mult = acc_mult.wrapping_mul(cur_mult);
                acc_plus = acc_plus.wrapping_mul(cur_mult).wrapping_add(cur_plus);
            }
            cur_plus = cur_mult.wrapping_add(1).wrapping_mul(cur_plus);
            cur_mult = cur_mult.wrapping_mul(cur_mult);
            advance /= 2;
        }
        self.state = acc_mult.wrapping_mul(self.state).wrapping_add(acc_plus);
    }

    /// `PcgOneseq128XslRr64::jump(int $advance)`.
    pub fn jump(&mut self, advance: i64) -> Result<()> {
        let Ok(advance) = u64::try_from(advance) else {
            return Err(RandomError::argument_value(
                &format!("{CLASS}::jump"),
                1,
                "advance",
                "must be greater than or equal to 0",
            ));
        };
        self.advance(advance);
        Ok(())
    }

    /// The serialized state: `[hex(hi), hex(lo)]`.
    pub fn serialize_state(&self) -> Vec<StateValue> {
        #[allow(clippy::cast_possible_truncation)]
        let (hi, lo) = ((self.state >> 64) as u64, self.state as u64);
        vec![
            StateValue::Str(u64_to_hex_le(hi).into_bytes()),
            StateValue::Str(u64_to_hex_le(lo).into_bytes()),
        ]
    }

    /// Restores a serialized state; `false` (state untouched) if invalid.
    pub fn unserialize_state(&mut self, data: &StateArray) -> bool {
        if data.len() != 2 {
            return false;
        }
        let mut u = [0u64; 2];
        for (i, slot) in (0i64..).zip(u.iter_mut()) {
            match data.str_of_len(i, 16).and_then(hex_le_to_u64) {
                Some(v) => *slot = v,
                None => return false,
            }
        }
        self.state = (u128::from(u[0]) << 64) | u128::from(u[1]);
        true
    }
}

impl Engine for PcgOneseq128XslRr64 {
    fn generate(&mut self) -> Result<Generated> {
        Ok(Generated::u64(self.next_u64()))
    }
}
