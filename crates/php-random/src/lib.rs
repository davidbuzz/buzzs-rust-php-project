//! A bit-exact Rust port of PHP's `ext/random`.
//!
//! * [`engine`]: the engines (`Random\Engine\Mt19937`,
//!   `PcgOneseq128XslRr64`, `Xoshiro256StarStar`, `Secure`, userland
//!   engines via [`engine::UserGenerate`]) behind the [`Engine`] trait
//!   (`php_random_algo`), with their `__serialize()` state formats.
//! * [`range`]: `php_random_range32/64` and the legacy `MT_RAND_PHP` scaling.
//! * [`gammasection`]: the γ-section float algorithm of `getFloat()`.
//! * [`randomizer`]: every `Random\Randomizer` method.
//! * [`shuffle`]: the shuffling/key-picking algorithms shared with
//!   `shuffle()`, `str_shuffle()` and `array_rand()`.
//! * [`legacy`]: the request-global state and `mt_rand()`, `rand()`,
//!   `lcg_value()`, `uniqid()` & co.
//! * [`csprng`]: `random_bytes()` / `random_int()`.
//!
//! The crate does not depend on the runtime's value types: inputs and
//! outputs are integers, floats, byte strings and slices, and every PHP
//! throwable is described by a [`RandomError`] carrying PHP's exact message.

pub mod csprng;
pub mod engine;
pub mod error;
pub mod gammasection;
pub mod legacy;
pub mod randomizer;
pub mod range;
pub mod shuffle;
pub mod state;

pub use engine::{
    Engine, EngineKind, Generated, Mt19937, Mt19937Mode, NativeEngine, PcgOneseq128XslRr64, Secure,
    Seed, UserEngine, UserGenerate, Xoshiro256StarStar,
};
pub use error::{RandomError, Result};
pub use legacy::RandomGlobals;
pub use randomizer::{IntervalBoundary, Randomizer};
pub use shuffle::{ArraySlots, Dense};
pub use state::{StateArray, StateValue};

/// `PHP_RANDOM_RANGE_ATTEMPTS`: rejection rounds before giving up with a
/// `Random\BrokenRandomEngineError`.
pub const RANGE_ATTEMPTS: u32 = 50;

/// Value of the `MT_RAND_MT19937` constant.
pub const MT_RAND_MT19937: i64 = 0;
/// Value of the (deprecated) `MT_RAND_PHP` constant.
pub const MT_RAND_PHP: i64 = 1;
