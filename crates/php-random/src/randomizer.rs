//! `Random\Randomizer` (ext/random/randomizer.c).
//!
//! The methods are generic over any [`Engine`]; the VM keeps the engine
//! object and wraps it (e.g. `Randomizer::new(&mut engine)`) per call.

use crate::engine::Engine;
use crate::error::{RandomError, Result};
use crate::range::bad_scaling;
use crate::shuffle::{pick_keys, shuffle_slice, ArraySlots};
use crate::{gammasection, RANGE_ATTEMPTS};

const CLASS: &str = "Random\\Randomizer";

/// `Random\IntervalBoundary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum IntervalBoundary {
    /// `[min, max)` (the default).
    #[default]
    ClosedOpen,
    /// `[min, max]`.
    ClosedClosed,
    /// `(min, max]`.
    OpenClosed,
    /// `(min, max)`.
    OpenOpen,
}

impl IntervalBoundary {
    /// The enum case name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ClosedOpen => "ClosedOpen",
            Self::ClosedClosed => "ClosedClosed",
            Self::OpenClosed => "OpenClosed",
            Self::OpenOpen => "OpenOpen",
        }
    }
}

fn arg_error(method: &str, arg: u32, name: &str, msg: &str) -> RandomError {
    RandomError::argument_value(&format!("{CLASS}::{method}"), arg, name, msg)
}

/// A `Random\Randomizer` bound to an engine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Randomizer<E> {
    /// The engine (`$randomizer->engine`).
    pub engine: E,
}

impl<E: Engine> Randomizer<E> {
    /// Wraps an engine.
    pub const fn new(engine: E) -> Self {
        Self { engine }
    }

    /// `nextInt()`: one engine output shifted right by one bit.
    #[allow(clippy::cast_possible_wrap)]
    pub fn next_int(&mut self) -> Result<i64> {
        let r = self.engine.generate()?;
        if r.size > 8 {
            return Err(RandomError::RandomException(
                "Generated value exceeds size of int".into(),
            ));
        }
        Ok((r.value >> 1) as i64)
    }

    /// `getInt(int $min, int $max)`.
    pub fn get_int(&mut self, min: i64, max: i64) -> Result<i64> {
        if max < min {
            return Err(arg_error(
                "getInt",
                2,
                "max",
                "must be greater than or equal to argument #1 ($min)",
            ));
        }
        if self.engine.is_mt19937_legacy() {
            let r = self.engine.generate()?.value >> 1;
            return Ok(bad_scaling(r, min, max));
        }
        self.engine.range(min, max)
    }

    /// `getBytes(int $length)`.
    pub fn get_bytes(&mut self, length: i64) -> Result<Vec<u8>> {
        let len = match usize::try_from(length) {
            Ok(l) if l >= 1 => l,
            _ => return Err(arg_error("getBytes", 1, "length", "must be greater than 0")),
        };
        let mut out = Vec::with_capacity(len);
        // Fast path: whole 64-bit outputs; it is abandoned for good as soon
        // as an engine returns anything but 8 bytes.
        let mut pending = None;
        while out.len() + 8 <= len {
            let r = self.engine.generate()?;
            if r.size != 8 {
                pending = Some(r);
                break;
            }
            out.extend_from_slice(&r.value.to_le_bytes());
        }
        while out.len() < len {
            let mut r = match pending.take() {
                Some(r) => r,
                None => self.engine.generate()?,
            };
            for _ in 0..r.size {
                #[allow(clippy::cast_possible_truncation)]
                out.push(r.value as u8);
                r.value >>= 8;
                if out.len() >= len {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// `getBytesFromString(string $string, int $length)`.
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap
    )]
    pub fn get_bytes_from_string(&mut self, source: &[u8], length: i64) -> Result<Vec<u8>> {
        if source.is_empty() {
            return Err(arg_error(
                "getBytesFromString",
                1,
                "string",
                "must not be empty",
            ));
        }
        let len = match usize::try_from(length) {
            Ok(l) if l >= 1 => l,
            _ => {
                return Err(arg_error(
                    "getBytesFromString",
                    2,
                    "length",
                    "must be greater than 0",
                ))
            }
        };
        let max_offset = source.len() - 1;
        let mut out = Vec::with_capacity(len);
        if max_offset > 0xff {
            while out.len() < len {
                let offset = self.engine.range(0, max_offset as i64)?;
                out.push(source[offset as usize]);
            }
            return Ok(out);
        }
        let mut mask = max_offset as u64;
        mask |= mask >> 1;
        mask |= mask >> 2;
        mask |= mask >> 4;
        mask = mask.wrapping_mul(0x0101_0101_0101_0101);
        let mut failures = 0u32;
        while out.len() < len {
            let r = self.engine.generate()?;
            let mut offsets = r.value & mask;
            for _ in 0..r.size {
                let offset = (offsets & 0xff) as usize;
                offsets >>= 8;
                if offset > max_offset {
                    failures += 1;
                    if failures > RANGE_ATTEMPTS {
                        return Err(RandomError::too_many_attempts());
                    }
                    continue;
                }
                failures = 0;
                out.push(source[offset]);
                if out.len() >= len {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// `nextFloat()`: 53 random bits scaled into `[0, 1)`.
    #[allow(clippy::cast_precision_loss)]
    pub fn next_float(&mut self) -> Result<f64> {
        let mut result = 0u64;
        let mut total = 0usize;
        while total < 8 {
            let r = self.engine.generate()?;
            result |= r.value << (total * 8);
            total += r.size;
        }
        let step = 1.0 / (1u64 << 53) as f64;
        Ok(step * (result >> 11) as f64)
    }

    /// `getFloat(float $min, float $max, IntervalBoundary $boundary)`.
    pub fn get_float(&mut self, min: f64, max: f64, boundary: IntervalBoundary) -> Result<f64> {
        if !min.is_finite() {
            return Err(arg_error("getFloat", 1, "min", "must be finite"));
        }
        if !max.is_finite() {
            return Err(arg_error("getFloat", 2, "max", "must be finite"));
        }
        let greater = || {
            arg_error(
                "getFloat",
                2,
                "max",
                "must be greater than argument #1 ($min)",
            )
        };
        match boundary {
            IntervalBoundary::ClosedOpen => {
                if max <= min {
                    return Err(greater());
                }
                gammasection::closed_open(&mut self.engine, min, max)
            }
            IntervalBoundary::ClosedClosed => {
                if max < min {
                    return Err(arg_error(
                        "getFloat",
                        2,
                        "max",
                        "must be greater than or equal to argument #1 ($min)",
                    ));
                }
                gammasection::closed_closed(&mut self.engine, min, max)
            }
            IntervalBoundary::OpenClosed => {
                if max <= min {
                    return Err(greater());
                }
                gammasection::open_closed(&mut self.engine, min, max)
            }
            IntervalBoundary::OpenOpen => {
                if max <= min {
                    return Err(greater());
                }
                let v = gammasection::open_open(&mut self.engine, min, max)?;
                if v.is_nan() {
                    return Err(RandomError::ValueError(
                        "The given interval is empty, there are no floats between argument #1 ($min) and argument #2 ($max)".into(),
                    ));
                }
                Ok(v)
            }
        }
    }

    /// `shuffleArray(array $array)` on the array's values (in iteration
    /// order); the result is a list of the permuted values.
    pub fn shuffle_array<T>(&mut self, values: &mut [T]) -> Result<()> {
        shuffle_slice(&mut self.engine, values)
    }

    /// `shuffleBytes(string $bytes)`.
    pub fn shuffle_bytes(&mut self, bytes: &[u8]) -> Result<Vec<u8>> {
        let mut v = bytes.to_vec();
        shuffle_slice(&mut self.engine, &mut v)?;
        Ok(v)
    }

    /// `pickArrayKeys(array $array, int $num)`: slot positions (see
    /// [`pick_keys`]) of the picked keys, always returned as a list.
    pub fn pick_array_keys<A: ArraySlots + ?Sized>(
        &mut self,
        array: &A,
        num: i64,
    ) -> Result<Vec<u32>> {
        pick_keys(
            &mut self.engine,
            array,
            num,
            "Random\\Randomizer::pickArrayKeys",
        )
    }
}

/// Validation of `Random\Randomizer::__unserialize(array $data)`: `$data`
/// must have exactly one element, at index 0, which is an array.
pub fn validate_unserialize(len: usize, first_is_array: bool) -> Result<()> {
    if len != 1 || !first_is_array {
        return Err(RandomError::Exception(
            "Invalid serialization data for Random\\Randomizer object".into(),
        ));
    }
    Ok(())
}
