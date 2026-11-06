//! A fast, unseeded hasher for the VM's tables of declared names: the
//! registration tables (the attribution of functions, classes and constants
//! to extensions) and the per-class member tables
//! ([`crate::class::NameMap`]).
//!
//! Their keys are the names of symbols and members, not request data, so
//! they need no protection against chosen-key collisions. `SipHash`, the
//! standard library's default, took about 15 % of start-up instructions and
//! about 280 instructions per class-member lookup. This is rustc's
//! `FxHasher` word mix, finished with a folded 128-bit multiply so that the
//! low bits hashbrown picks buckets with depend on every input byte (a
//! plain multiply only carries differences upwards).
//!
//! Iteration order is fixed instead of random per process; as with any
//! hash map, nothing may depend on it (the readers sort or keep a separate
//! registration-order list).

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// The multiplier of rustc's `FxHasher` (64-bit).
const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

/// The multiplier of the final fold (`rustc-hash` 2's `K`).
const FOLD: u64 = 0xf1_35_7a_ea_2e_62_a9_c5;

/// rustc's `FxHasher`: rotate, xor and multiply per 8-byte word. Public
/// through [`crate::class::FxHasher`] so that other crates can name the
/// type of a [`crate::class::NameMap`].
#[derive(Clone, Copy, Debug, Default)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            let mut word = [0u8; 8];
            word.copy_from_slice(c);
            self.add(u64::from_le_bytes(word));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut word = [0u8; 8];
            word[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(word));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(u64::try_from(i).unwrap_or(u64::MAX));
    }

    #[inline]
    #[allow(clippy::cast_possible_truncation)] // folds the 128-bit product
    fn finish(&self) -> u64 {
        let p = u128::from(self.hash) * u128::from(FOLD);
        (p as u64) ^ ((p >> 64) as u64)
    }
}

/// A `HashMap` with [`FxHasher`].
pub(crate) type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;

#[cfg(test)]
mod tests {
    use super::{FastMap, FxHasher};
    use php_runtime::ZStr;
    use std::hash::{BuildHasher, BuildHasherDefault, Hash, Hasher};

    fn hash_of<T: Hash + ?Sized>(v: &T) -> u64 {
        BuildHasherDefault::<FxHasher>::default().hash_one(v)
    }

    #[test]
    #[allow(clippy::mutable_key_type)] // `ZStr` keys hash and compare by bytes only
    fn equal_bytes_hash_equal_through_borrow() {
        // `ZStr` hashes like `[u8]`, so `&[u8]` lookups find `ZStr` keys.
        let z = ZStr::from("strlen");
        assert_eq!(hash_of(&z), hash_of(b"strlen".as_slice()));
        assert_eq!(hash_of(&b"strlen".to_vec()), hash_of(b"strlen".as_slice()));
        let mut m: FastMap<ZStr, usize> = FastMap::default();
        m.insert(z, 7);
        assert_eq!(m.get(b"strlen".as_slice()), Some(&7));
        assert_eq!(m.get(b"strlen2".as_slice()), None);
    }

    #[test]
    fn hashes_depend_on_every_byte_and_the_length() {
        let names = [
            "",
            "a",
            "b",
            "ab",
            "ba",
            "abcdefgh",
            "abcdefgi",
            "bbcdefgh",
            "abcdefgh1",
            "abcdefgh2",
            "abcdefg",
            "abcdefg\0",
        ];
        for (i, x) in names.iter().enumerate() {
            for y in &names[i + 1..] {
                assert_ne!(hash_of(x.as_bytes()), hash_of(y.as_bytes()), "{x:?} {y:?}");
            }
        }
    }

    #[test]
    fn low_bits_spread_over_similar_names() {
        // hashbrown picks buckets with the low bits: names that differ only
        // in their last byte must not share them.
        let mut low = std::collections::HashSet::new();
        for c in b'a'..=b'z' {
            let name = [b'e', b'_', b'u', b's', b'e', b'r', b'_', c];
            low.insert(hash_of(name.as_slice()) & 0xff);
        }
        assert!(low.len() > 16, "{}", low.len());
    }

    #[test]
    fn hasher_is_deterministic() {
        let mut a = FxHasher::default();
        let mut b = FxHasher::default();
        a.write(b"ReflectionClass");
        b.write(b"ReflectionClass");
        assert_eq!(a.finish(), b.finish());
    }
}
