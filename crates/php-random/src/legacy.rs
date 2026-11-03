//! The global (per-request) random state and the procedural API:
//! `mt_srand`/`srand`, `mt_rand`/`rand`, `mt_getrandmax`/`getrandmax`,
//! `lcg_value`, `shuffle`, `str_shuffle`, `array_rand` and `uniqid`
//! (ext/random/random.c, ext/standard/{array,string,uniqid}.c).

use std::time::{SystemTime, UNIX_EPOCH};

use crate::engine::{Engine, Mt19937, Mt19937Mode};
use crate::error::{RandomError, Result};
use crate::range::{bad_scaling, MT_RAND_MAX};
use crate::shuffle::{pick_keys, shuffle_slice, shuffle_with, ArraySlots};

/// The `E_DEPRECATED` message of `lcg_value()` (PHP 8.4+).
pub const LCG_VALUE_DEPRECATION: &str =
    "Function lcg_value() is deprecated since 8.4, use \\Random\\Randomizer::getFloat() instead";

/// Module globals of ext/random (`zend_random_globals`) plus `uniqid()`'s
/// previous timestamp. The VM keeps one per request.
#[derive(Debug, Clone)]
pub struct RandomGlobals {
    mt: Mt19937,
    mt_seeded: bool,
    lcg: [i32; 2],
    lcg_seeded: bool,
    fallback_counter: u64,
    prev_tv: (i64, i64),
}

impl Default for RandomGlobals {
    fn default() -> Self {
        Self::new()
    }
}

impl RandomGlobals {
    /// Fresh request state (`RINIT`): nothing seeded yet.
    pub fn new() -> Self {
        Self {
            mt: Mt19937::new(0, Mt19937Mode::Mt19937),
            mt_seeded: false,
            lcg: [0; 2],
            lcg_seeded: false,
            fallback_counter: 0,
            prev_tv: (0, 0),
        }
    }

    /// `php_random_generate_fallback_seed`: a non-cryptographic seed used
    /// only when the CSPRNG is unavailable (time, pid and a counter mixed
    /// with splitmix64 instead of PHP's SHA-1 construction).
    pub fn fallback_seed(&mut self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        self.fallback_counter = self.fallback_counter.wrapping_add(1);
        #[allow(clippy::cast_possible_truncation)]
        let mut x = (now as u64)
            ^ ((now >> 64) as u64)
            ^ (u64::from(std::process::id()) << 32)
            ^ self.fallback_counter.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }

    fn seed_default_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        if crate::csprng::fill(&mut b).is_ok() {
            u32::from_le_bytes(b)
        } else {
            #[allow(clippy::cast_possible_truncation)]
            let s = self.fallback_seed() as u32;
            s
        }
    }

    /// `php_random_default_engine()`: the global Mt19937, seeded from the
    /// CSPRNG on first use. `shuffle()`, `array_rand()`, `str_shuffle()` and
    /// `mt_rand()` all draw from it.
    pub fn default_engine(&mut self) -> &mut Mt19937 {
        if !self.mt_seeded {
            let seed = self.seed_default_u32();
            self.mt = Mt19937::new(seed, Mt19937Mode::Mt19937);
            self.mt_seeded = true;
        }
        &mut self.mt
    }

    /// Whether the global Mt19937 has been seeded in this request.
    pub fn is_mt_seeded(&self) -> bool {
        self.mt_seeded
    }

    /// `mt_srand(?int $seed = null, int $mode = MT_RAND_MT19937)` (also
    /// `srand()`). Returns the `E_DEPRECATED` message to emit (before
    /// anything else happens) when `$mode` is `MT_RAND_PHP`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn mt_srand(&mut self, seed: Option<i64>, mode: i64) -> Option<&'static str> {
        let mode = Mt19937Mode::from_srand_arg(mode);
        let seed = match seed {
            Some(s) => s as u32,
            None => self.seed_default_u32(),
        };
        self.mt = Mt19937::new(seed, mode);
        self.mt_seeded = true;
        (mode == Mt19937Mode::Php).then_some(crate::engine::MT_RAND_PHP_DEPRECATION)
    }

    /// `php_mt_rand()`: the next raw 32-bit output of the global engine.
    pub fn php_mt_rand(&mut self) -> u32 {
        self.default_engine().next_u32()
    }

    /// `mt_rand()` / `rand()` without arguments: 31 bits.
    pub fn mt_rand(&mut self) -> i64 {
        i64::from(self.php_mt_rand() >> 1)
    }

    /// `php_mt_rand_range`: unbiased `[min, max]` from the global engine.
    pub fn mt_rand_range_unchecked(&mut self, min: i64, max: i64) -> i64 {
        crate::range::range(self.default_engine(), min, max).unwrap_or(min)
    }

    /// `php_mt_rand_common`: unbiased in `MT_RAND_MT19937` mode, legacy
    /// double scaling in `MT_RAND_PHP` mode.
    pub fn mt_rand_common(&mut self, min: i64, max: i64) -> i64 {
        if self.default_engine().mode() == Mt19937Mode::Mt19937 {
            return self.mt_rand_range_unchecked(min, max);
        }
        let r = u64::from(self.php_mt_rand() >> 1);
        bad_scaling(r, min, max)
    }

    /// `mt_rand(int $min, int $max)`.
    pub fn mt_rand_range(&mut self, min: i64, max: i64) -> Result<i64> {
        if max < min {
            return Err(RandomError::argument_value(
                "mt_rand",
                2,
                "max",
                "must be greater than or equal to argument #1 ($min)",
            ));
        }
        Ok(self.mt_rand_common(min, max))
    }

    /// `rand(int $min, int $max)`: like `mt_rand()` but swaps reversed bounds.
    pub fn rand_range(&mut self, min: i64, max: i64) -> i64 {
        if max < min {
            self.mt_rand_common(max, min)
        } else {
            self.mt_rand_common(min, max)
        }
    }

    /// `mt_getrandmax()` / `getrandmax()`.
    pub const fn mt_getrandmax() -> i64 {
        MT_RAND_MAX
    }

    /// Seeds the combined LCG explicitly (PHP seeds it from the CSPRNG on
    /// first use; this makes `lcg_value()` deterministic for tests).
    pub fn seed_combined_lcg(&mut self, s1: i32, s2: i32) {
        self.lcg = [s1, s2];
        self.lcg_seeded = true;
    }

    /// `php_combined_lcg()` / `lcg_value()`: a float in `(0, 1)`. The VM
    /// must emit [`LCG_VALUE_DEPRECATION`] for the userland function.
    pub fn lcg_value(&mut self) -> f64 {
        if !self.lcg_seeded {
            let mut b = [0u8; 8];
            let seed = if crate::csprng::fill(&mut b).is_ok() {
                u64::from_le_bytes(b)
            } else {
                self.fallback_seed()
            };
            #[allow(clippy::cast_possible_truncation)]
            self.seed_combined_lcg(seed as i32, (seed >> 32) as i32);
        }
        self.lcg[0] = modmult(53668, 40014, 12211, 2_147_483_563, self.lcg[0]);
        self.lcg[1] = modmult(52774, 40692, 3791, 2_147_483_399, self.lcg[1]);
        let mut z = self.lcg[0].wrapping_sub(self.lcg[1]);
        if z < 1 {
            z = z.wrapping_add(2_147_483_562);
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_precision_loss)]
        let v = (i64::from(z) as u64) as f64 * 4.656_613e-10;
        v
    }

    /// `shuffle(array &$array)` on the array's values (iteration order);
    /// the VM rebuilds the array as a list afterwards.
    pub fn shuffle<T>(&mut self, values: &mut [T]) {
        // The Mt19937 engine cannot fail.
        let _ = shuffle_slice(self.default_engine(), values);
    }

    /// Like [`RandomGlobals::shuffle`] with a swap callback (for arrays with
    /// iterators that must be updated on every swap).
    pub fn shuffle_with(&mut self, len: usize, swap: impl FnMut(usize, usize)) {
        let _ = shuffle_with(self.default_engine(), len, swap);
    }

    /// `str_shuffle(string $string)`.
    pub fn str_shuffle(&mut self, s: &[u8]) -> Vec<u8> {
        let mut v = s.to_vec();
        let _ = shuffle_slice(self.default_engine(), &mut v);
        v
    }

    /// `array_rand(array $array, int $num = 1)`: slot positions of the
    /// picked keys (one position when `$num == 1`, in which case PHP returns
    /// the key itself rather than an array).
    pub fn array_rand<A: ArraySlots + ?Sized>(&mut self, array: &A, num: i64) -> Result<Vec<u32>> {
        pick_keys(self.default_engine(), array, num, "array_rand")
    }

    /// `uniqid(string $prefix = "", bool $more_entropy = false)` using the
    /// system clock (busy-waits until the microsecond changes, like PHP).
    pub fn uniqid(&mut self, prefix: &[u8], more_entropy: bool) -> Vec<u8> {
        let (sec, usec) = loop {
            let d = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            let tv = (
                i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
                i64::from(d.subsec_micros()),
            );
            if tv != self.prev_tv {
                break tv;
            }
        };
        self.prev_tv = (sec, usec);
        let entropy = more_entropy.then(|| {
            let mut b = [0u8; 4];
            if crate::csprng::fill(&mut b).is_ok() {
                u32::from_le_bytes(b)
            } else {
                #[allow(clippy::cast_possible_truncation)]
                let s = self.fallback_seed() as u32;
                s
            }
        });
        #[allow(clippy::cast_possible_truncation)]
        format_uniqid(prefix, sec as i32, (usec % 0x10_0000) as i32, entropy)
    }
}

/// `PHP_COMBINED_LCG_MODMULT`: `s = (s * b) % m` via Schrage's method.
#[allow(clippy::many_single_char_names)]
fn modmult(a: i32, b: i32, c: i32, m: i32, s: i32) -> i32 {
    let q = s / a;
    let s = b.wrapping_mul(s - a * q).wrapping_sub(c.wrapping_mul(q));
    if s < 0 {
        s.wrapping_add(m)
    } else {
        s
    }
}

/// The formatting half of `uniqid()`: `%s%08x%05x` plus, with more
/// entropy, `%.8F` of `bytes / UINT32_MAX * 10`.
#[allow(clippy::cast_sign_loss)]
pub fn format_uniqid(prefix: &[u8], sec: i32, usec: i32, entropy: Option<u32>) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(format!("{:08x}{:05x}", sec as u32, usec as u32).as_bytes());
    if let Some(bytes) = entropy {
        let seed = (f64::from(bytes) / f64::from(u32::MAX)) * 10.0;
        out.extend_from_slice(format!("{seed:.8}").as_bytes());
    }
    out
}

impl Engine for RandomGlobals {
    fn generate(&mut self) -> Result<crate::engine::Generated> {
        self.default_engine().generate()
    }

    fn is_mt19937_legacy(&self) -> bool {
        self.mt_seeded && self.mt.mode() == Mt19937Mode::Php
    }
}
