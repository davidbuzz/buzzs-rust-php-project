//! `ext/opcache` core for phprust: the compiled-script cache behind
//! `opcache.enable`, its file cache, invalidation and status.
//!
//! ref: ext/opcache/ZendAccelerator.c, ext/opcache/zend_accelerator_module.c,
//! ext/opcache/zend_file_cache.c, ext/opcache/zend_persist.c

pub mod file_cache;

use std::collections::HashMap;

/// Preload statistics tracking for opcache.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreloadStats {
    pub memory_consumption: usize,
    pub functions: Vec<String>,
    pub classes: Vec<String>,
    pub scripts: Vec<String>,
}

/// A cached script entry in the opcache.
#[derive(Debug, Clone)]
pub struct ScriptEntry<T> {
    /// Canonical resolved path of the script.
    pub full_path: String,
    /// Modification time (in seconds since epoch) when compiled.
    pub timestamp: i64,
    /// Timestamp of last stat revalidation against the filesystem.
    pub last_revalidate: i64,
    /// Timestamp of last execution.
    pub last_used: i64,
    /// Hit count for this specific script.
    pub hits: u64,
    /// Estimated memory consumption in bytes.
    pub memory_consumption: usize,
    /// The compiled unit.
    pub unit: T,
}

/// The result of looking up a script in the cache.
#[derive(Debug)]
pub enum CacheLookup<'a, T> {
    /// Script was found and is valid.
    Hit(&'a T),
    /// Script was in cache but file modified on disk; entry was evicted.
    Stale,
    /// Script was not in cache.
    Miss,
}

/// The in-memory compiled script cache.
#[derive(Debug)]
pub struct ScriptCache<T> {
    /// Cached scripts keyed by full canonical path.
    pub scripts: HashMap<String, ScriptEntry<T>>,
    /// Timestamp when this cache was started.
    pub start_time: i64,
    /// Timestamp of last cache reset (0 if never reset).
    pub last_restart_time: i64,
    /// Global cache hit counter.
    pub hits: u64,
    /// Global cache miss counter.
    pub misses: u64,
    /// Number of manual cache restarts (`opcache_reset()`).
    pub manual_restarts: u64,
    /// Maximum number of accelerated files.
    pub max_accelerated_files: usize,
    /// Preload statistics, if preloading was executed.
    pub preload_stats: Option<PreloadStats>,
}

impl<T> ScriptCache<T> {
    /// Creates a new script cache.
    pub fn new(max_accelerated_files: usize) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Self {
            scripts: HashMap::new(),
            start_time: now,
            last_restart_time: 0,
            hits: 0,
            misses: 0,
            manual_restarts: 0,
            max_accelerated_files,
            preload_stats: None,
        }
    }

    /// Resets the cache, evicting all cached scripts (preserving preloaded scripts if present).
    pub fn reset(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Some(ref pstats) = self.preload_stats {
            self.scripts.retain(|path, _| pstats.scripts.contains(path));
        } else {
            self.scripts.clear();
        }
        self.last_restart_time = now;
        self.manual_restarts += 1;
        self.hits = 0;
        self.misses = 0;
    }

    /// Looks up a script by path, performing timestamp revalidation if needed.
    pub fn get(
        &mut self,
        path: &str,
        current_mtime: i64,
        validate_timestamps: bool,
        revalidate_freq: i64,
        now: i64,
    ) -> CacheLookup<'_, T> {
        let is_stale = if let Some(entry) = self.scripts.get(path) {
            validate_timestamps
                && (now - entry.last_revalidate) >= revalidate_freq
                && entry.timestamp != current_mtime
        } else {
            false
        };
        if is_stale {
            self.scripts.remove(path);
            self.misses += 1;
            return CacheLookup::Stale;
        }
        if let Some(entry) = self.scripts.get_mut(path) {
            if validate_timestamps && (now - entry.last_revalidate) >= revalidate_freq {
                entry.last_revalidate = now;
            }
            entry.hits += 1;
            entry.last_used = now;
            self.hits += 1;
            return CacheLookup::Hit(&entry.unit);
        }
        self.misses += 1;
        CacheLookup::Miss
    }

    /// Inserts a newly compiled script into the cache.
    pub fn insert(&mut self, path: String, mtime: i64, memory: usize, now: i64, unit: T) {
        let entry = ScriptEntry {
            full_path: path.clone(),
            timestamp: mtime,
            last_revalidate: now,
            last_used: now,
            hits: 0,
            memory_consumption: memory,
            unit,
        };
        self.scripts.insert(path, entry);
    }

    /// Invalidates a cached script. Returns true if an entry was invalidated.
    pub fn invalidate(
        &mut self,
        path: &str,
        force: bool,
        current_mtime: Option<i64>,
        validate_timestamps: bool,
    ) -> bool {
        if let Some(entry) = self.scripts.get(path) {
            if force || !validate_timestamps || current_mtime.map_or(true, |m| m != entry.timestamp)
            {
                self.scripts.remove(path);
                return true;
            }
            return false;
        }
        false
    }

    /// Returns whether the script is currently in cache.
    pub fn is_cached(&self, path: &str) -> bool {
        self.scripts.contains_key(path)
    }

    /// Returns the number of cached scripts.
    pub fn len(&self) -> usize {
        self.scripts.len()
    }

    /// Returns whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.scripts.is_empty()
    }

    /// Returns the total memory consumed by cached scripts.
    pub fn total_memory_consumption(&self) -> usize {
        self.scripts.values().map(|e| e.memory_consumption).sum()
    }

    /// Returns the hit rate percentage (0.0 to 100.0).
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            (self.hits as f64 / total as f64) * 100.0
        }
    }

    /// Returns the preload statistics if preloading was executed.
    pub fn preload_stats(&self) -> Option<&PreloadStats> {
        self.preload_stats.as_ref()
    }

    /// Sets the preload statistics.
    pub fn set_preload_stats(&mut self, stats: PreloadStats) {
        self.preload_stats = Some(stats);
    }
}
