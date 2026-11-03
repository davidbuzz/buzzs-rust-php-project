//! Port of PHP's `ext/gmp` on top of a pure-Rust big integer engine.

pub mod mpz;
pub mod random;

pub use mpz::{Endian, Mpz, Round, WordOrder};
