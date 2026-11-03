//! Range reduction: `php_random_range32`, `php_random_range64`,
//! `php_random_range` and the legacy `RAND_RANGE_BADSCALING` scaling.

use crate::engine::Engine;
use crate::error::{RandomError, Result};
use crate::RANGE_ATTEMPTS;

/// `PHP_MT_RAND_MAX` (`mt_getrandmax()`).
pub const MT_RAND_MAX: i64 = 0x7FFF_FFFF;

fn gather32<E: Engine + ?Sized>(engine: &mut E) -> Result<u32> {
    let mut result = 0u32;
    let mut total = 0usize;
    while total < 4 {
        let r = engine.generate()?;
        #[allow(clippy::cast_possible_truncation)]
        let part = r.value as u32;
        result |= part << (total * 8);
        total += r.size;
    }
    Ok(result)
}

fn gather64<E: Engine + ?Sized>(engine: &mut E) -> Result<u64> {
    let mut result = 0u64;
    let mut total = 0usize;
    while total < 8 {
        let r = engine.generate()?;
        result |= r.value << (total * 8);
        total += r.size;
    }
    Ok(result)
}

/// `php_random_range32`: uniform integer in `[0, umax]` from 32 bits of
/// engine output, rejecting values above the largest multiple of the range.
pub fn range32<E: Engine + ?Sized>(engine: &mut E, umax: u32) -> Result<u32> {
    let mut result = gather32(engine)?;
    if umax == u32::MAX {
        return Ok(result);
    }
    let umax = umax + 1;
    if umax.is_power_of_two() {
        return Ok(result & (umax - 1));
    }
    let limit = u32::MAX - (u32::MAX % umax) - 1;
    let mut count = 0u32;
    while result > limit {
        count += 1;
        if count > RANGE_ATTEMPTS {
            return Err(RandomError::too_many_attempts());
        }
        result = gather32(engine)?;
    }
    Ok(result % umax)
}

/// `php_random_range64`: like [`range32`] with 64 bits of engine output
/// (several outputs of a 32-bit engine are concatenated, first one lowest).
pub fn range64<E: Engine + ?Sized>(engine: &mut E, umax: u64) -> Result<u64> {
    let mut result = gather64(engine)?;
    if umax == u64::MAX {
        return Ok(result);
    }
    let umax = umax + 1;
    if umax.is_power_of_two() {
        return Ok(result & (umax - 1));
    }
    let limit = u64::MAX - (u64::MAX % umax) - 1;
    let mut count = 0u32;
    while result > limit {
        count += 1;
        if count > RANGE_ATTEMPTS {
            return Err(RandomError::too_many_attempts());
        }
        result = gather64(engine)?;
    }
    Ok(result % umax)
}

/// `php_random_range`: uniform integer in `[min, max]` (`min <= max`); uses
/// [`range32`] when `max - min` fits in 32 bits, else [`range64`].
#[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
pub fn range<E: Engine + ?Sized>(engine: &mut E, min: i64, max: i64) -> Result<i64> {
    let umax = (max as u64).wrapping_sub(min as u64);
    if let Ok(umax32) = u32::try_from(umax) {
        let r = range32(engine, umax32)?;
        return Ok(u64::from(r).wrapping_add(min as u64) as i64);
    }
    Ok(range64(engine, umax)?.wrapping_add(min as u64) as i64)
}

/// The inlined `RAND_RANGE_BADSCALING` of `php_mt_rand_common` and
/// `Randomizer::getInt()` for `MT_RAND_PHP`: scales a 31-bit value `r`
/// (`mt_rand() >> 1`) into `[min, max]` through doubles.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
pub fn bad_scaling(r: u64, min: i64, max: i64) -> i64 {
    let span = (max as f64) - (min as f64) + 1.0;
    let offset = (span * (r as f64 / (MT_RAND_MAX as f64 + 1.0))) as u64;
    offset.wrapping_add(min as u64) as i64
}
