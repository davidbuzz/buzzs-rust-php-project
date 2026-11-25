// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The per-request compiled pattern cache and the `pcre.*` INI settings
//! (`PCRE_G(pcre_cache)`, `pcre.backtrack_limit`, `pcre.recursion_limit`,
//! `pcre.jit`).

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

use crate::error::CompileError;
use crate::ffi::Tables;
use crate::regex::{JitOutcome, Regex};

/// `PCRE_CACHE_SIZE`.
pub const PCRE_CACHE_SIZE: usize = 4096;

/// Match-time settings, copied out of [`Pcre`] for each call so that user
/// callbacks may re-enter the extension while a match is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchConfig {
    /// `pcre.backtrack_limit` as handed to `pcre2_set_match_limit()`.
    pub backtrack_limit: u32,
    /// `pcre.recursion_limit` as handed to `pcre2_set_depth_limit()`.
    pub recursion_limit: u32,
    /// `pcre.jit`: whether the shared JIT stack is attached to the match context.
    pub jit: bool,
    /// The caller guarantees the subject is valid UTF-8 (PHP's
    /// `IS_STR_VALID_UTF8` string flag), so `/u` patterns may skip PCRE2's
    /// per-call validation (`PCRE2_NO_UTF_CHECK`).  A start offset that
    /// points into a continuation byte is still checked, so
    /// `PREG_BAD_UTF8_OFFSET_ERROR` is reported exactly as without the hint.
    /// Not an INI setting: set it per call with
    /// [`MatchConfig::with_known_valid_utf8`].  Passing `true` for an invalid
    /// subject is a logic error (PCRE2 behaviour is then undefined), so only
    /// pass what a validation actually established.
    pub known_valid_utf8: bool,
}

impl MatchConfig {
    /// Builds a config from the raw INI integers; PHP truncates them with a
    /// `(uint32_t)` cast, which is reproduced here.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn from_ini(backtrack_limit: i64, recursion_limit: i64, jit: bool) -> Self {
        Self {
            backtrack_limit: backtrack_limit as u32,
            recursion_limit: recursion_limit as u32,
            jit,
            known_valid_utf8: false,
        }
    }

    /// A copy of this config carrying the per-call "subject is known to be
    /// valid UTF-8" hint (see [`MatchConfig::known_valid_utf8`]).
    #[must_use]
    pub fn with_known_valid_utf8(self, known_valid_utf8: bool) -> Self {
        Self {
            known_valid_utf8,
            ..self
        }
    }
}

impl Default for MatchConfig {
    /// PHP's defaults: `1000000`, `100000`, JIT on.
    fn default() -> Self {
        Self {
            backtrack_limit: 1_000_000,
            recursion_limit: 100_000,
            jit: true,
            known_valid_utf8: false,
        }
    }
}

/// `zend_inline_hash_func()`: PHP's string hash (DJBX33A over the bytes,
/// the top bit set), the hash the cache is keyed by.
///
/// It is the hash a PHP string carries (`ZSTR_H`), so a binding holding
/// the pattern as such a string passes its cached hash to
/// [`Pcre::cached`] instead of hashing the pattern again on every call,
/// as `pcre_get_compiled_regex_cache_ex()` looks the pattern up with
/// `zend_hash_find(&PCRE_G(pcre_cache), regex)`.
#[must_use]
pub fn pattern_hash(bytes: &[u8]) -> u64 {
    #[inline]
    fn step(hash: u64, b: u8) -> u64 {
        hash.wrapping_mul(33).wrapping_add(u64::from(b))
    }
    let mut hash: u64 = 5381;
    let mut chunks = bytes.chunks_exact(8);
    for c in &mut chunks {
        for &b in c {
            hash = step(hash, b);
        }
    }
    for &b in chunks.remainder() {
        hash = step(hash, b);
    }
    hash | 0x8000_0000_0000_0000
}

/// The hasher of the cache's `u64` keys (already [`pattern_hash`]es): a
/// 128-bit multiply fold, so that the high bits the table's probe tags
/// come from depend on every bit of the DJBX33A value (whose top bit is
/// always set).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PatternHasher(u64);

/// The multiplier of the fold (`rustc-hash` 2's `K`).
const FOLD: u64 = 0xf1_35_7a_ea_2e_62_a9_c5;

impl Hasher for PatternHasher {
    #[allow(clippy::cast_possible_truncation)] // folds the 128-bit product
    fn finish(&self) -> u64 {
        let p = u128::from(self.0) * u128::from(FOLD);
        (p as u64) ^ ((p >> 64) as u64)
    }

    fn write(&mut self, bytes: &[u8]) {
        // Only `u64` keys are hashed (`write_u64`); this keeps the trait
        // total.
        self.0 = pattern_hash(bytes) ^ self.0.rotate_left(5);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.0 = i;
    }
}

/// One cached pattern.
#[derive(Debug)]
struct Entry {
    key: Box<[u8]>,
    regex: Regex,
}

/// The compiled-pattern cache.  Like PHP it is keyed by the exact pattern
/// bytes (prefixed with the `LC_CTYPE` name for locale-aware patterns),
/// found by their [`pattern_hash`], and, when it reaches
/// [`PCRE_CACHE_SIZE`] entries, drops the oldest `PCRE_CACHE_SIZE / 8`
/// entries that are not currently in use.
#[derive(Debug, Default)]
pub struct RegexCache {
    /// The entries by hash; more than one only when two keys collide.
    map: HashMap<u64, Vec<Entry>, BuildHasherDefault<PatternHasher>>,
    /// The keys (and their hashes) in insertion order, for eviction.
    order: Vec<(u64, Box<[u8]>)>,
}

impl RegexCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of cached patterns.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Looks a key up.
    pub fn get(&self, key: &[u8]) -> Option<Regex> {
        self.get_hashed(key, pattern_hash(key))
    }

    /// Looks a key up by its [`pattern_hash`] `hash`.
    #[inline]
    pub fn get_hashed(&self, key: &[u8], hash: u64) -> Option<Regex> {
        debug_assert_eq!(hash, pattern_hash(key));
        self.map
            .get(&hash)?
            .iter()
            .find(|e| *e.key == *key)
            .map(|e| e.regex.clone())
    }

    /// Whether `key` is cached.
    pub fn contains(&self, key: &[u8]) -> bool {
        self.get(key).is_some()
    }

    /// Stores a freshly compiled regex, evicting first if the cache is full
    /// (`pcre_clean_cache`).
    pub fn insert(&mut self, key: Vec<u8>, regex: Regex) {
        if self.order.len() >= PCRE_CACHE_SIZE {
            self.clean();
        }
        let hash = pattern_hash(&key);
        let bucket = self.map.entry(hash).or_default();
        if let Some(e) = bucket.iter_mut().find(|e| *e.key == *key) {
            e.regex = regex;
            return;
        }
        let key = key.into_boxed_slice();
        self.order.push((hash, key.clone()));
        bucket.push(Entry { key, regex });
    }

    /// `pcre_clean_cache()`: drops the oldest `PCRE_CACHE_SIZE / 8`
    /// entries that are not in use.
    fn clean(&mut self) {
        let mut num_clean = PCRE_CACHE_SIZE / 8;
        let map = &mut self.map;
        self.order.retain(|(hash, key)| {
            if num_clean == 0 {
                return true;
            }
            let Some(bucket) = map.get_mut(hash) else {
                return false;
            };
            let Some(i) = bucket.iter().position(|e| e.key == *key) else {
                return false;
            };
            if bucket[i].regex.in_use() {
                return true;
            }
            bucket.swap_remove(i);
            if bucket.is_empty() {
                map.remove(hash);
            }
            num_clean -= 1;
            false
        });
    }

    /// Drops every entry.
    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

/// Per-request PCRE state owned by the VM: cache, INI settings, the
/// `preg_last_error()` value and warnings produced while compiling.
///
/// The `preg_*` operations themselves are free functions taking a
/// [`Regex`] and a [`MatchConfig`] (see the crate docs), so a callback run
/// from inside `preg_replace_callback()` can borrow this struct again.
#[derive(Debug)]
pub struct Pcre {
    cache: RegexCache,
    config: MatchConfig,
    last_error: crate::PregError,
    char_tables: HashMap<Vec<u8>, Arc<Tables>>,
    warnings: Vec<Vec<u8>>,
}

impl Default for Pcre {
    fn default() -> Self {
        Self::new()
    }
}

impl Pcre {
    /// `RSHUTDOWN`: the request's state goes (the error code, pending
    /// warnings, the `pcre.*` settings, read again by the next request);
    /// the compiled patterns and the locale tables stay, as
    /// `PCRE_G(pcre_cache)` lives as long as the process.
    pub fn end_request(&mut self) {
        self.config = MatchConfig::default();
        self.last_error = crate::PregError::NoError;
        self.warnings.clear();
    }

    /// Fresh state with PHP's default INI values.
    pub fn new() -> Self {
        Self {
            cache: RegexCache::new(),
            config: MatchConfig::default(),
            last_error: crate::PregError::NoError,
            char_tables: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    /// The current match settings (copy them before running a match).
    pub fn config(&self) -> MatchConfig {
        self.config
    }

    /// `pcre.backtrack_limit` changed.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn set_backtrack_limit(&mut self, value: i64) {
        self.config.backtrack_limit = value as u32;
    }

    /// `pcre.recursion_limit` changed.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn set_recursion_limit(&mut self, value: i64) {
        self.config.recursion_limit = value as u32;
    }

    /// `pcre.jit` changed.  Already-cached patterns keep their JIT code, as in PHP.
    pub fn set_jit(&mut self, jit: bool) {
        self.config.jit = jit;
    }

    /// Current `pcre.jit` value (PHP may switch it off after a JIT allocation failure).
    pub fn jit(&self) -> bool {
        self.config.jit
    }

    /// `preg_last_error()`.
    pub fn last_error(&self) -> crate::PregError {
        self.last_error
    }

    /// Stores a new `preg_last_error()` value.
    pub fn set_last_error(&mut self, e: crate::PregError) {
        self.last_error = e;
    }

    /// The pattern cache.
    pub fn cache(&self) -> &RegexCache {
        &self.cache
    }

    /// Non-fatal warnings produced by the last compile calls (JIT problems).
    /// The VM should drain these after each [`Pcre::compile`] and emit them
    /// as `E_WARNING`s, before the warning of a failed compile if any.
    pub fn take_warnings(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.warnings)
    }

    /// The cache hit of [`Pcre::compile`] (the C locale), found by the
    /// pattern's [`pattern_hash`] `hash` (the hash the caller's string
    /// already carries): no hashing, no allocation.
    #[inline]
    pub fn cached(&self, pattern: &[u8], hash: u64) -> Option<Regex> {
        self.cache.get_hashed(pattern, hash)
    }

    /// `pcre_get_compiled_regex_cache()` for the C locale.
    ///
    /// On failure the caller emits [`CompileError::message`] as an
    /// `E_WARNING`; `preg_last_error()` has already been set to
    /// `PREG_INTERNAL_ERROR` here.
    pub fn compile(&mut self, pattern: &[u8]) -> Result<Regex, CompileError> {
        self.compile_with_locale(pattern, None)
    }

    /// `pcre_get_compiled_regex_cache()` honouring `setlocale(LC_CTYPE)`.
    ///
    /// Pass `Some(name)` when PHP's `BG(ctype_string)` is set (a non-"C"
    /// `LC_CTYPE` was selected via `setlocale()`); the process' C locale must
    /// then actually be that locale, since PCRE2 builds its character tables
    /// from it.
    pub fn compile_with_locale(
        &mut self,
        pattern: &[u8],
        ctype_locale: Option<&[u8]>,
    ) -> Result<Regex, CompileError> {
        // A cache hit (the common case) allocates nothing.
        let key: Cow<'_, [u8]> = match ctype_locale {
            Some(loc) => Cow::Owned([loc, pattern].concat()),
            None => Cow::Borrowed(pattern),
        };
        if let Some(re) = self.cache.get(&key) {
            return Ok(re);
        }
        let result = self.compile_uncached(pattern, ctype_locale);
        match result {
            Ok(re) => {
                self.cache.insert(key.into_owned(), re.clone());
                Ok(re)
            }
            Err(e) => {
                self.last_error = e.last_error();
                Err(e)
            }
        }
    }

    fn compile_uncached(
        &mut self,
        pattern: &[u8],
        ctype_locale: Option<&[u8]>,
    ) -> Result<Regex, CompileError> {
        // Delimiter / modifier errors come before locale table generation.
        crate::regex::parse_pattern(pattern)?;
        let tables = match ctype_locale {
            None => None,
            Some(loc) => {
                if let Some(t) = self.char_tables.get(loc) {
                    Some(Arc::clone(t))
                } else {
                    let t =
                        Arc::new(Tables::for_current_locale().ok_or(CompileError::LocaleTables)?);
                    self.char_tables.insert(loc.to_vec(), Arc::clone(&t));
                    Some(t)
                }
            }
        };
        let (re, jit) = Regex::compile_with(pattern, self.config.jit, tables)?;
        match jit {
            JitOutcome::Disabled | JitOutcome::Ok => {}
            JitOutcome::NoMemory => {
                self.warnings.push(
                    b"Allocation of JIT memory failed, PCRE JIT will be disabled. \
This is likely caused by security restrictions. \
Either grant PHP permission to allocate executable memory, or set pcre.jit=0"
                        .to_vec(),
                );
                self.config.jit = false;
            }
            JitOutcome::Failed(rc) => {
                let mut m = b"JIT compilation failed: ".to_vec();
                m.extend_from_slice(&crate::ffi::error_message(rc));
                self.warnings.push(m);
                self.last_error = crate::PregError::Internal;
            }
        }
        Ok(re)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hits_return_same_regex() {
        let mut pcre = Pcre::new();
        let a = pcre.compile(b"/a+/").unwrap();
        let b = pcre.compile(b"/a+/").unwrap();
        assert!(Regex::ptr_eq(&a, &b));
        assert_eq!(pcre.cache().len(), 1);
    }

    #[test]
    fn failures_are_not_cached_and_set_internal_error() {
        let mut pcre = Pcre::new();
        assert!(pcre.compile(b"/(/").is_err());
        assert_eq!(pcre.last_error(), crate::PregError::Internal);
        assert!(pcre.cache().is_empty());
    }

    #[test]
    fn eviction_drops_oldest_unused() {
        let mut pcre = Pcre::new();
        pcre.set_jit(false);
        let first = pcre.compile(b"/0/").unwrap(); // stays in use
        for i in 1..PCRE_CACHE_SIZE {
            pcre.compile(format!("/{i}/").as_bytes()).unwrap();
        }
        assert_eq!(pcre.cache().len(), PCRE_CACHE_SIZE);
        pcre.compile(b"/new/").unwrap();
        assert_eq!(
            pcre.cache().len(),
            PCRE_CACHE_SIZE - PCRE_CACHE_SIZE / 8 + 1
        );
        assert!(pcre.cache().contains(b"/0/"));
        assert!(!pcre.cache().contains(b"/1/"));
        assert!(!pcre
            .cache()
            .contains(format!("/{}/", PCRE_CACHE_SIZE / 8).as_bytes()));
        assert!(pcre
            .cache()
            .contains(format!("/{}/", PCRE_CACHE_SIZE / 8 + 1).as_bytes()));
        drop(first);
    }

    #[test]
    fn unlocalised_compiles_share_one_entry() {
        let mut pcre = Pcre::new();
        let plain = pcre.compile(b"/a+/").unwrap();
        assert!(pcre.cache().contains(b"/a+/"));
        assert!(!pcre.cache().contains(b"C/a+/"));
        let again = pcre.compile_with_locale(b"/a+/", None).unwrap();
        assert!(Regex::ptr_eq(&plain, &again));
        assert_eq!(pcre.cache().len(), 1);
    }

    /// `pattern_hash` is `zend_inline_hash_func` (what `ZSTR_H` holds).
    #[test]
    fn pattern_hash_is_zend_hash() {
        const TOP: u64 = 0x8000_0000_0000_0000;
        assert_eq!(pattern_hash(b""), 5381 | TOP);
        assert_eq!(pattern_hash(b"a"), (5381 * 33 + 97) | TOP);
        let mut h: u64 = 5381;
        for &b in b"/abcdefghij/u" {
            h = h.wrapping_mul(33).wrapping_add(u64::from(b));
        }
        assert_eq!(pattern_hash(b"/abcdefghij/u"), h | TOP);
    }

    #[test]
    fn hashed_lookup_and_collisions() {
        let mut cache = RegexCache::new();
        let a = Regex::compile(b"/a/").unwrap();
        cache.insert(b"/a/".to_vec(), a.clone());
        let hit = cache.get_hashed(b"/a/", pattern_hash(b"/a/")).unwrap();
        assert!(Regex::ptr_eq(&a, &hit));
        // Two keys under one hash (forced) stay apart.
        let b = Regex::compile(b"/b/").unwrap();
        cache
            .map
            .get_mut(&pattern_hash(b"/a/"))
            .unwrap()
            .push(Entry {
                key: b"/b/".to_vec().into_boxed_slice(),
                regex: b.clone(),
            });
        let found = cache.map[&pattern_hash(b"/a/")]
            .iter()
            .find(|e| &*e.key == b"/b/")
            .map(|e| e.regex.clone())
            .unwrap();
        assert!(Regex::ptr_eq(&b, &found));
        assert!(Regex::ptr_eq(&a, &cache.get(b"/a/").unwrap()));
        // Re-inserting a key replaces its regex without a second entry.
        cache.insert(b"/a/".to_vec(), b.clone());
        assert_eq!(cache.len(), 1);
        assert!(Regex::ptr_eq(&b, &cache.get(b"/a/").unwrap()));
        cache.clear();
        assert!(cache.is_empty() && cache.get(b"/a/").is_none());
    }

    #[test]
    fn ini_truncation() {
        let c = MatchConfig::from_ini(-1, 1 << 33 | 5, false);
        assert_eq!(c.backtrack_limit, u32::MAX);
        assert_eq!(c.recursion_limit, 5);
    }
}
