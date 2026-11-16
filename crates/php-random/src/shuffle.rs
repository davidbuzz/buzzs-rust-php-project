// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Array/string shuffling and key picking shared by `shuffle()`,
//! `str_shuffle()`, `array_rand()` and the `Randomizer` methods
//! (`php_array_data_shuffle`, `php_binary_string_shuffle`,
//! `php_array_pick_keys` in ext/standard).

use crate::engine::Engine;
use crate::error::{RandomError, Result};
use crate::RANGE_ATTEMPTS;

/// Fisher–Yates from the end, exactly PHP's loop: for `n_left` from
/// `len - 1` down to 1, draw `j = range(0, n_left)` and call `swap(n_left, j)`
/// when they differ. `swap` receives `(n_left, j)` with `j < n_left`.
///
/// On an engine error the swaps performed so far are kept, like in PHP.
pub fn shuffle_with<E: Engine + ?Sized>(
    engine: &mut E,
    len: usize,
    mut swap: impl FnMut(usize, usize),
) -> Result<()> {
    if len < 2 {
        return Ok(());
    }
    for n_left in (1..len).rev() {
        #[allow(clippy::cast_possible_wrap)]
        let j = engine.range(0, n_left as i64)?;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let j = j as usize;
        if j != n_left {
            swap(n_left, j);
        }
    }
    Ok(())
}

/// Shuffles a slice in place (`php_array_data_shuffle` on the packed,
/// compacted values / `php_binary_string_shuffle`).
pub fn shuffle_slice<E: Engine + ?Sized, T>(engine: &mut E, items: &mut [T]) -> Result<()> {
    shuffle_with(engine, items.len(), |a, b| items.swap(a, b))
}

/// The hash-table layout information `php_array_pick_keys` depends on.
///
/// PHP's hash tables keep deleted slots ("holes") until they are
/// compacted; `array_rand()` samples raw slots, so the result depends on
/// the layout and not only on the logical contents.
pub trait ArraySlots {
    /// `zend_hash_num_elements`: number of live elements.
    fn num_elements(&self) -> u32;
    /// `nNumUsed`: number of slots in use, including holes.
    fn num_used(&self) -> u32;
    /// Whether the slot at `pos` (`0..num_used`) holds a live element.
    fn is_slot_used(&self, pos: u32) -> bool;
}

/// A hole-free array of `n` elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dense(pub u32);

impl ArraySlots for Dense {
    fn num_elements(&self) -> u32 {
        self.0
    }
    fn num_used(&self) -> u32 {
        self.0
    }
    fn is_slot_used(&self, _pos: u32) -> bool {
        true
    }
}

/// An explicit slot map: `true` = live element, `false` = hole.
impl ArraySlots for [bool] {
    fn num_elements(&self) -> u32 {
        u32::try_from(self.iter().filter(|&&b| b).count()).unwrap_or(u32::MAX)
    }
    fn num_used(&self) -> u32 {
        u32::try_from(self.len()).unwrap_or(u32::MAX)
    }
    fn is_slot_used(&self, pos: u32) -> bool {
        self.get(pos as usize).copied().unwrap_or(false)
    }
}

impl ArraySlots for Vec<bool> {
    fn num_elements(&self) -> u32 {
        self.as_slice().num_elements()
    }
    fn num_used(&self) -> u32 {
        self.as_slice().num_used()
    }
    fn is_slot_used(&self, pos: u32) -> bool {
        self.as_slice().is_slot_used(pos)
    }
}

/// Live slot positions in iteration order.
fn live_slots<A: ArraySlots + ?Sized>(arr: &A) -> impl Iterator<Item = u32> + '_ {
    (0..arr.num_used()).filter(move |&p| arr.is_slot_used(p))
}

/// `php_array_pick_keys` (non-silent): picks `num_req` distinct elements
/// and returns their **slot positions** in iteration order (exactly one
/// position when `num_req == 1`). `func` is the PHP function name used in
/// the error messages (`"array_rand"` or `"Random\\Randomizer::pickArrayKeys"`).
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn pick_keys<E: Engine + ?Sized, A: ArraySlots + ?Sized>(
    engine: &mut E,
    arr: &A,
    num_req: i64,
    func: &str,
) -> Result<Vec<u32>> {
    let num_avail = arr.num_elements();
    if num_avail == 0 {
        return Err(RandomError::argument_value(
            func,
            1,
            "array",
            "must not be empty",
        ));
    }
    let num_used = arr.num_used();

    if num_req == 1 {
        if num_avail < num_used - (num_used >> 1) {
            // Sparse table: pick an ordinal and scan for it.
            let randval = engine.range(0, i64::from(num_avail) - 1)?;
            if let Some(pos) = live_slots(arr).nth(randval as usize) {
                return Ok(vec![pos]);
            }
        }
        // Sample raw slots until a live one is hit.
        loop {
            let randval = engine.range(0, i64::from(num_used) - 1)? as u32;
            if arr.is_slot_used(randval) {
                return Ok(vec![randval]);
            }
        }
    }

    if num_req <= 0 || num_req > i64::from(num_avail) {
        return Err(RandomError::argument_value(
            func,
            2,
            "num",
            "must be between 1 and the number of elements in argument #1 ($array)",
        ));
    }

    let mut num_req = num_req as u32;
    let negative = num_req > (num_avail >> 1);
    if negative {
        num_req = num_avail - num_req;
    }

    let mut bitset = vec![false; num_avail as usize];
    let mut i = num_req;
    let mut failures = 0u32;
    while i > 0 {
        let randval = engine.range(0, i64::from(num_avail) - 1)? as usize;
        if bitset[randval] {
            failures += 1;
            if failures > RANGE_ATTEMPTS {
                return Err(RandomError::too_many_attempts());
            }
        } else {
            bitset[randval] = true;
            i -= 1;
            failures = 0;
        }
    }

    Ok(live_slots(arr)
        .zip(bitset)
        .filter(|&(_, picked)| picked != negative)
        .map(|(pos, _)| pos)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Mt19937, Mt19937Mode};

    #[test]
    fn errors() {
        let mut e = Mt19937::new(1, Mt19937Mode::Mt19937);
        assert_eq!(
            pick_keys(&mut e, &Dense(0), 1, "array_rand")
                .unwrap_err()
                .message(),
            "array_rand(): Argument #1 ($array) must not be empty"
        );
        assert_eq!(
            pick_keys(&mut e, &Dense(3), 4, "array_rand").unwrap_err().message(),
            "array_rand(): Argument #2 ($num) must be between 1 and the number of elements in argument #1 ($array)"
        );
        assert!(pick_keys(&mut e, &Dense(3), 0, "f").is_err());
        assert!(pick_keys(&mut e, &Dense(3), -1, "f").is_err());
    }

    #[test]
    fn picks_are_sorted_and_distinct() {
        let mut e = Mt19937::new(7, Mt19937Mode::Mt19937);
        for n in 1..=10usize {
            let v = pick_keys(&mut e, &Dense(10), i64::try_from(n).unwrap(), "f").unwrap();
            assert_eq!(v.len(), n);
            assert!(v.windows(2).all(|w| w[0] < w[1]));
        }
        let holes = vec![false, true, false, false, false, true];
        for _ in 0..20 {
            let v = pick_keys(&mut e, holes.as_slice(), 1, "f").unwrap();
            assert!(v[0] == 1 || v[0] == 5);
        }
        assert_eq!(pick_keys(&mut e, &holes, 2, "f").unwrap(), vec![1, 5]);
    }

    #[test]
    fn shuffle_small() {
        let mut e = Mt19937::new(7, Mt19937Mode::Mt19937);
        let mut v: Vec<u8> = vec![];
        shuffle_slice(&mut e, &mut v).unwrap();
        let mut v = vec![1];
        shuffle_slice(&mut e, &mut v).unwrap();
        assert_eq!(v, vec![1]);
    }
}
