//! `shuffle`, `array_rand` and `str_shuffle`. The random engine is supplied
//! as a [`RangeFn`] (the engine's `range(min, max)`, e.g. the default
//! `Mt19937` engine's `php_mt_rand_range`), so the exact PHP sequences are
//! reproduced when the VM plugs in `php-random`.

use crate::util::{arg_value_error, with_array};
use crate::RangeFn;
use php_runtime::{ArrayKey, HashTable, PhpRef, RtResult, RuntimeError, Value, ZArray};

/// `PHP_RANDOM_RANGE_ATTEMPTS`.
pub const RANGE_ATTEMPTS: u32 = 50;

fn len_i64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn idx(n: i64) -> usize {
    usize::try_from(n).unwrap_or(0)
}

/// The Fisher–Yates loop shared by `php_array_data_shuffle` and
/// `php_binary_string_shuffle`. Stops at the first engine error, leaving the
/// slice partially shuffled.
fn fisher_yates<T>(v: &mut [T], rng: &mut RangeFn<'_>) -> RtResult<()> {
    let mut n_left = len_i64(v.len());
    if n_left <= 1 {
        return Ok(());
    }
    loop {
        n_left -= 1;
        if n_left == 0 {
            return Ok(());
        }
        let rnd = rng(0, n_left)?;
        if rnd != n_left {
            v.swap(idx(n_left), idx(rnd));
        }
    }
}

/// `php_array_data_shuffle` on a table: the values are shuffled and the
/// table becomes a list (`0..n`).
pub fn shuffle_table(ht: &mut HashTable, rng: &mut RangeFn<'_>) -> RtResult<()> {
    let n = ht.len();
    if n < 1 {
        return Ok(());
    }
    let mut vals: Vec<Value> = ht.values().cloned().collect();
    let res = fisher_yates(&mut vals, rng);
    let repl = ZArray::from_values(vals);
    let _ = ht.splice(0, len_i64(n), Some(&repl), false);
    ht.set_internal_pointer(0);
    res
}

/// `shuffle(array &$array): true`.
pub fn shuffle(array: &PhpRef, rng: &mut RangeFn<'_>) -> RtResult<()> {
    with_array(array, "shuffle", 1, "array", |a| {
        if a.is_empty() {
            return Ok(());
        }
        shuffle_table(a.make_mut(), rng)
    })
}

/// `str_shuffle(string $string): string` (`php_binary_string_shuffle`).
pub fn str_shuffle(s: &[u8], rng: &mut RangeFn<'_>) -> RtResult<Vec<u8>> {
    let mut v = s.to_vec();
    fisher_yates(&mut v, rng)?;
    Ok(v)
}

/// `php_array_pick_keys` / `array_rand(array $array, int $num = 1): int|string|array`.
pub fn array_rand(array: &HashTable, num: i64, rng: &mut RangeFn<'_>) -> RtResult<Value> {
    const F: &str = "array_rand";
    let num_avail = len_i64(array.len());
    if num_avail == 0 {
        return Err(arg_value_error(F, 1, Some("array"), "must not be empty"));
    }
    if num == 1 {
        let used = i64::from(array.used());
        if num_avail < used - (used >> 1) {
            let randval = rng(0, num_avail - 1)?;
            if let Some((k, _)) = array.iter().nth(idx(randval)) {
                return Ok(k.to_value());
            }
        }
        loop {
            let randval = rng(0, used - 1)?;
            let pos = u32::try_from(randval).unwrap_or(u32::MAX);
            if let Some((k, _)) = array.at(pos) {
                return Ok(k.to_value());
            }
        }
    }
    if num <= 0 || num > num_avail {
        return Err(arg_value_error(
            F,
            2,
            Some("num"),
            "must be between 1 and the number of elements in argument #1 ($array)",
        ));
    }
    let mut num_req = num;
    let negative = num_req > (num_avail >> 1);
    if negative {
        num_req = num_avail - num_req;
    }
    let mut bitset = vec![false; idx(num_avail)];
    let mut i = num_req;
    let mut failures = 0u32;
    while i > 0 {
        let randval = rng(0, num_avail - 1)?;
        let last = bitset.len() - 1;
        let slot = &mut bitset[idx(randval).min(last)];
        if *slot {
            failures += 1;
            if failures > RANGE_ATTEMPTS {
                return Err(RuntimeError::Error(format!(
                    "Failed to generate an acceptable random number in {RANGE_ATTEMPTS} attempts"
                )));
            }
        } else {
            *slot = true;
            i -= 1;
            failures = 0;
        }
    }
    let mut out = HashTable::with_capacity(idx(num));
    for (j, k) in array.keys().enumerate() {
        if bitset[j] ^ negative {
            let _ = out.push(match k {
                ArrayKey::Int(i) => Value::Int(*i),
                ArrayKey::Str(s) => Value::Str(s.clone()),
            });
        }
    }
    Ok(Value::Array(ZArray::from_table(out)))
}
