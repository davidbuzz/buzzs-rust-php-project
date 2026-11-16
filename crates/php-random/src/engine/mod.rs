// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The engine abstraction (`php_random_algo`) and the built-in engines.

mod mt19937;
mod pcg;
mod secure;
mod user;
mod xoshiro;

pub use mt19937::{Mt19937, Mt19937Mode, MT_RAND_PHP_DEPRECATION, N as MT_N};
pub use pcg::PcgOneseq128XslRr64;
pub use secure::Secure;
pub use user::{generated_from_bytes, UserEngine, UserGenerate};
pub use xoshiro::Xoshiro256StarStar;

use crate::error::{RandomError, Result};
use crate::state::StateValue;

/// The `string|int|null $seed` argument of the PCG and Xoshiro constructors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seed<'a> {
    /// `null`: seed from the CSPRNG.
    Random,
    /// An integer seed.
    Int(i64),
    /// A binary seed string.
    Bytes(&'a [u8]),
}

/// One engine step: `php_random_result`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generated {
    /// The generated bits (only the low `size * 8` bits are meaningful).
    pub value: u64,
    /// Number of bytes produced, `1..=8` (4 for Mt19937, 8 for the others).
    pub size: usize,
}

impl Generated {
    /// A 32-bit result.
    pub const fn u32(value: u32) -> Self {
        Self {
            value: value as u64,
            size: 4,
        }
    }

    /// A 64-bit result.
    pub const fn u64(value: u64) -> Self {
        Self { value, size: 8 }
    }

    /// The little-endian byte string `Random\Engine::generate()` returns.
    pub fn to_bytes(self) -> Vec<u8> {
        self.value.to_le_bytes()[..self.size.min(8)].to_vec()
    }
}

/// A random number engine (`php_random_algo` + its state).
///
/// `range` has a default implementation equal to `php_random_range`; only
/// [`Secure`] overrides it (it uses `php_random_int`).
pub trait Engine {
    /// Produces the next output.
    fn generate(&mut self) -> Result<Generated>;

    /// Uniform integer in `[min, max]` (`algo->range`). Callers guarantee
    /// `min <= max`.
    fn range(&mut self, min: i64, max: i64) -> Result<i64> {
        crate::range::range(self, min, max)
    }

    /// `true` for an [`Mt19937`] running in the legacy `MT_RAND_PHP` mode;
    /// `Randomizer::getInt()` then uses the biased legacy scaling.
    fn is_mt19937_legacy(&self) -> bool {
        false
    }

    /// `Random\Engine\*::generate()`: the output as a little-endian string.
    fn generate_bytes(&mut self) -> Result<Vec<u8>> {
        Ok(self.generate()?.to_bytes())
    }
}

impl<E: Engine + ?Sized> Engine for &mut E {
    fn generate(&mut self) -> Result<Generated> {
        (**self).generate()
    }
    fn range(&mut self, min: i64, max: i64) -> Result<i64> {
        (**self).range(min, max)
    }
    fn is_mt19937_legacy(&self) -> bool {
        (**self).is_mt19937_legacy()
    }
}

impl<E: Engine + ?Sized> Engine for Box<E> {
    fn generate(&mut self) -> Result<Generated> {
        (**self).generate()
    }
    fn range(&mut self, min: i64, max: i64) -> Result<i64> {
        (**self).range(min, max)
    }
    fn is_mt19937_legacy(&self) -> bool {
        (**self).is_mt19937_legacy()
    }
}

/// Which internal engine class an engine object is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineKind {
    /// `Random\Engine\Mt19937`
    Mt19937,
    /// `Random\Engine\PcgOneseq128XslRr64`
    PcgOneseq128XslRr64,
    /// `Random\Engine\Xoshiro256StarStar`
    Xoshiro256StarStar,
    /// `Random\Engine\Secure`
    Secure,
}

impl EngineKind {
    /// The fully-qualified PHP class name.
    pub const fn class_name(self) -> &'static str {
        match self {
            Self::Mt19937 => "Random\\Engine\\Mt19937",
            Self::PcgOneseq128XslRr64 => "Random\\Engine\\PcgOneseq128XslRr64",
            Self::Xoshiro256StarStar => "Random\\Engine\\Xoshiro256StarStar",
            Self::Secure => "Random\\Engine\\Secure",
        }
    }
}

/// Any of the built-in (internal-class) engines, so the VM can store one
/// value per engine object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeEngine {
    /// Mersenne Twister (boxed: its state is 2.5 KiB).
    Mt19937(Box<Mt19937>),
    /// PCG 128-bit one-sequence XSL-RR 64.
    Pcg(PcgOneseq128XslRr64),
    /// xoshiro256**.
    Xoshiro(Xoshiro256StarStar),
    /// The OS CSPRNG.
    Secure(Secure),
}

impl NativeEngine {
    /// The engine's class.
    pub const fn kind(&self) -> EngineKind {
        match self {
            Self::Mt19937(_) => EngineKind::Mt19937,
            Self::Pcg(_) => EngineKind::PcgOneseq128XslRr64,
            Self::Xoshiro(_) => EngineKind::Xoshiro256StarStar,
            Self::Secure(_) => EngineKind::Secure,
        }
    }

    /// `algo->serialize`: the state array of `__serialize()`/`__debugInfo()`
    /// (`None` for engines without serializable state, i.e. `Secure`).
    pub fn serialize_state(&self) -> Option<Vec<StateValue>> {
        match self {
            Self::Mt19937(e) => Some(e.serialize_state()),
            Self::Pcg(e) => Some(e.serialize_state()),
            Self::Xoshiro(e) => Some(e.serialize_state()),
            Self::Secure(_) => None,
        }
    }

    /// `algo->unserialize`; `false` when the data is rejected. On failure the
    /// engine state is left untouched.
    pub fn unserialize_state(&mut self, data: &crate::state::StateArray) -> bool {
        match self {
            Self::Mt19937(e) => e.unserialize_state(data),
            Self::Pcg(e) => e.unserialize_state(data),
            Self::Xoshiro(e) => e.unserialize_state(data),
            Self::Secure(_) => false,
        }
    }

    /// Full `__unserialize(array $data)` validation of the outer
    /// `[members, state]` array, given the number of elements of `$data`,
    /// whether index 0 is an array, and the state array at index 1 (if it is
    /// an array). Loading the members into the object is the VM's job (and
    /// it must raise [`RandomError::invalid_serialization`] if that fails).
    pub fn unserialize(
        &mut self,
        outer_len: usize,
        members_is_array: bool,
        state: Option<&crate::state::StateArray>,
    ) -> Result<()> {
        let class = self.kind().class_name();
        if outer_len != 2 || !members_is_array {
            return Err(RandomError::invalid_serialization(class));
        }
        match state {
            Some(s) if self.unserialize_state(s) => Ok(()),
            _ => Err(RandomError::invalid_serialization(class)),
        }
    }
}

impl Engine for NativeEngine {
    fn generate(&mut self) -> Result<Generated> {
        match self {
            Self::Mt19937(e) => e.generate(),
            Self::Pcg(e) => e.generate(),
            Self::Xoshiro(e) => e.generate(),
            Self::Secure(e) => e.generate(),
        }
    }

    fn range(&mut self, min: i64, max: i64) -> Result<i64> {
        match self {
            Self::Mt19937(e) => e.range(min, max),
            Self::Pcg(e) => e.range(min, max),
            Self::Xoshiro(e) => e.range(min, max),
            Self::Secure(e) => e.range(min, max),
        }
    }

    fn is_mt19937_legacy(&self) -> bool {
        match self {
            Self::Mt19937(e) => e.is_mt19937_legacy(),
            _ => false,
        }
    }
}
