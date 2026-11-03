//! Port of PHP's `ext/gmp` on top of a pure-Rust big integer engine.
//!
//! * [`mpz`] — [`Mpz`], an arbitrary-precision signed integer reproducing
//!   GMP's `mpz_*` semantics (division rounding modes, two's complement bit
//!   operations, `mpz_get_si`/`mpz_get_d`, `mpz_set_str`/`mpz_get_str`,
//!   import/export, gcdext cofactors, primality certainty levels, ...).
//! * [`random`] — GMP's Mersenne Twister, so seeded `gmp_random_*` output
//!   matches PHP exactly.
//! * [`php`] — the userland functions, the `GMP` class casts,
//!   (un)serialization helpers and operator overloading with PHP's exact
//!   error messages.

pub mod mpz;
pub mod php;
pub mod random;

pub use mpz::{Endian, Mpz, Round, WordOrder};
pub use php::{GmpError, Num};
