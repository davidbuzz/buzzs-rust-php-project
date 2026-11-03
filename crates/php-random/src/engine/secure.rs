//! `Random\Engine\Secure` (`ext/random/engine_secure.c`).

use super::{Engine, Generated};
use crate::error::Result;

/// The CSPRNG engine: every output is 8 fresh bytes from the OS, and
/// `range` is `random_int()`'s algorithm. It has no state and cannot be
/// serialized or cloned in PHP.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Secure;

impl Secure {
    /// `new Random\Engine\Secure()`.
    pub const fn new() -> Self {
        Self
    }
}

impl Engine for Secure {
    fn generate(&mut self) -> Result<Generated> {
        Ok(Generated::u64(crate::csprng::next_u64()?))
    }

    fn range(&mut self, min: i64, max: i64) -> Result<i64> {
        crate::csprng::random_int_unchecked(min, max)
    }
}
