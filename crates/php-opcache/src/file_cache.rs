// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! File cache implementation for `ext/opcache`.
//!
//! Stores compiled scripts on disk under `opcache.file_cache` directory.
//! Path structure: `<file_cache_dir>/<system_id>/<canonical_path>.bin`
//! Header: magic `OPCACHE\0`, 32-char hex system id, 64-bit timestamp,
//! 32-bit Adler-32 checksum, 64-bit payload length.
//!
//! Ref: `ext/opcache/zend_file_cache.c`

use std::path::{Path, PathBuf};

/// Magic header for opcache file cache binary files.
pub const MAGIC: &[u8; 8] = b"OPCACHE\0";

/// 32-character hexadecimal system id, matching php-src's `zend_system_id`.
pub const SYSTEM_ID: &str = "817478635848e894c2598be6984e49ef";

/// Header size: 8 (magic) + 32 (system_id) + 8 (mtime) + 4 (checksum) + 8 (len) = 60 bytes.
pub const HEADER_SIZE: usize = 60;

/// Calculates Adler-32 checksum, matching `zend_adler32` in php-src.
///
/// The sums are reduced only every `NMAX` bytes, as `zend_adler32` and
/// zlib do: 5552 is the largest count for which `b` cannot overflow a
/// `u32` before the reduction, so the result equals reducing after every
/// byte.
pub fn adler32(data: &[u8]) -> u32 {
    const BASE: u32 = 65521;
    const NMAX: usize = 5552;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for chunk in data.chunks(NMAX) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= BASE;
        b %= BASE;
    }
    (b << 16) | a
}

/// The finalized system id, once [`set_system_id`] ran.
static FINAL_SYSTEM_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Returns the system id: the finalized one, else [`SYSTEM_ID`].
pub fn system_id() -> &'static str {
    FINAL_SYSTEM_ID.get().map_or(SYSTEM_ID, String::as_str)
}

/// Whether the system id was finalized.
pub fn system_id_is_final() -> bool {
    FINAL_SYSTEM_ID.get().is_some()
}

/// Finalizes the system id (`zend_finalize_system_id`); later calls keep
/// the first value.
pub fn set_system_id(id: String) {
    let _ = FINAL_SYSTEM_ID.set(id);
}

/// `zend_finalize_system_id`: cached scripts may refer to internal
/// functions, so the effects of `disable_functions` (and of the loaded
/// extensions) are part of the system id (GH-17733). `function_names` are
/// the internal function table's keys; their order does not matter here.
/// The result is 32 lowercase hex digits.
pub fn compute_system_id<'a>(function_names: impl IntoIterator<Item = &'a [u8]>) -> String {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut names: Vec<&[u8]> = function_names.into_iter().collect();
    names.sort_unstable();
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x8422_2325_cbf2_9ce4;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            h1 = (h1 ^ u64::from(b)).wrapping_mul(PRIME);
            h2 = (h2 ^ u64::from(b.rotate_left(3))).wrapping_mul(PRIME);
        }
        // The terminating NUL php-src hashes with each name.
        h1 = h1.wrapping_mul(PRIME);
        h2 = (h2 ^ 0xff).wrapping_mul(PRIME);
    };
    feed(SYSTEM_ID.as_bytes());
    for n in names {
        feed(n);
    }
    format!("{h1:016x}{h2:016x}")
}

/// Returns the full path to the `.bin` cache file for the given script.
pub fn get_bin_path(cache_dir: &Path, script_path: &Path) -> PathBuf {
    let mut rel = script_path;
    while let Ok(stripped) = rel.strip_prefix("/") {
        rel = stripped;
    }
    let mut file_name = rel
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    file_name.push(".bin");
    let parent = rel.parent().unwrap_or_else(|| Path::new(""));
    cache_dir.join(system_id()).join(parent).join(file_name)
}

/// `strerror(errno)`: the error text without Rust's " (os error N)".
fn strerror(e: &std::io::Error) -> String {
    let text = e.to_string();
    match text.find(" (os error") {
        Some(i) => text[..i].to_string(),
        None => text,
    }
}

/// Writes a script's payload to the file cache.
pub fn write_file_cache(
    cache_dir: &Path,
    script_path: &Path,
    mtime: i64,
    payload: &[u8],
) -> Result<PathBuf, (String, std::io::Error)> {
    let bin_path = get_bin_path(cache_dir, script_path);
    if let Some(parent) = bin_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            let msg = format!(
                "opcache cannot create directory for file '{}', {}",
                bin_path.display(),
                strerror(&e)
            );
            return Err((msg, e));
        }
    }

    let checksum = adler32(payload);
    let mut bytes = Vec::with_capacity(HEADER_SIZE + payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(system_id().as_bytes());
    bytes.extend_from_slice(&mtime.to_le_bytes());
    bytes.extend_from_slice(&checksum.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(payload);

    match std::fs::write(&bin_path, &bytes) {
        Ok(()) => Ok(bin_path),
        Err(e) => {
            let msg = format!(
                "opcache cannot write to file '{}': {}",
                bin_path.display(),
                strerror(&e)
            );
            let _ = std::fs::remove_file(&bin_path);
            Err((msg, e))
        }
    }
}

/// Reads a script's payload from the file cache.
/// Returns `Ok(Some(payload))` on cache hit, `Ok(None)` on cache miss or stale,
/// and `Err(msg)` on corrupted file.
pub fn read_file_cache(
    cache_dir: &Path,
    script_path: &Path,
    expected_mtime: i64,
    validate_timestamps: bool,
    consistency_checks: bool,
) -> Result<Option<Vec<u8>>, String> {
    let bin_path = get_bin_path(cache_dir, script_path);
    let bytes = match std::fs::read(&bin_path) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };

    if bytes.len() < HEADER_SIZE {
        let _ = std::fs::remove_file(&bin_path);
        return Err(format!(
            "opcache cannot read from file '{}' (info)",
            bin_path.display()
        ));
    }

    if &bytes[0..8] != MAGIC {
        let _ = std::fs::remove_file(&bin_path);
        return Err(format!(
            "opcache cannot read from file '{}' (wrong header)",
            bin_path.display()
        ));
    }

    if &bytes[8..40] != system_id().as_bytes() {
        let _ = std::fs::remove_file(&bin_path);
        return Err(format!(
            "opcache cannot read from file '{}' (wrong \"system_id\")",
            bin_path.display()
        ));
    }

    let mtime = i64::from_le_bytes(bytes[40..48].try_into().unwrap());
    if validate_timestamps && mtime != expected_mtime {
        let _ = std::fs::remove_file(&bin_path);
        return Ok(None);
    }

    let checksum = u32::from_le_bytes(bytes[48..52].try_into().unwrap());
    let payload_len = u64::from_le_bytes(bytes[52..60].try_into().unwrap()) as usize;
    if bytes.len() < HEADER_SIZE + payload_len {
        let _ = std::fs::remove_file(&bin_path);
        return Ok(None);
    }

    let payload = &bytes[HEADER_SIZE..HEADER_SIZE + payload_len];
    if consistency_checks && adler32(payload) != checksum {
        let _ = std::fs::remove_file(&bin_path);
        return Ok(None);
    }

    Ok(Some(payload.to_vec()))
}

/// Invalidates (unlinks) a script in the file cache.
pub fn invalidate_file_cache(cache_dir: &Path, script_path: &Path) -> bool {
    let bin_path = get_bin_path(cache_dir, script_path);
    std::fs::remove_file(&bin_path).is_ok()
}

/// Checks if a script is cached in the file cache.
pub fn is_script_cached_in_file_cache(
    cache_dir: &Path,
    script_path: &Path,
    expected_mtime: Option<i64>,
    validate_timestamps: bool,
) -> bool {
    let bin_path = get_bin_path(cache_dir, script_path);
    if !bin_path.is_file() {
        return false;
    }
    if validate_timestamps {
        if let Some(expected) = expected_mtime {
            if let Ok(bytes) = std::fs::read(&bin_path) {
                if bytes.len() >= 48
                    && &bytes[0..8] == MAGIC
                    && &bytes[8..40] == system_id().as_bytes()
                {
                    let mtime = i64::from_le_bytes(bytes[40..48].try_into().unwrap());
                    return mtime == expected;
                }
            }
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{adler32, compute_system_id};

    /// The reduction after every byte, as `zend_adler32` defines the sum.
    fn adler32_per_byte(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1_u32, 0_u32);
        for &byte in data {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }

    #[test]
    fn adler32_matches_the_per_byte_definition() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        // All-0xff input maximizes the sums; lengths around the block size.
        for len in [1, 5551, 5552, 5553, 11104, 11105, 100_000] {
            let ff = vec![0xff_u8; len];
            assert_eq!(adler32(&ff), adler32_per_byte(&ff), "0xff x {len}");
            let mixed: Vec<u8> = (0..len)
                .map(|i| u8::try_from(i * 131 % 251).expect("below 251"))
                .collect();
            assert_eq!(adler32(&mixed), adler32_per_byte(&mixed), "mixed x {len}");
        }
    }

    #[test]
    fn system_id_depends_on_the_function_set_not_its_order() {
        let all: [&[u8]; 3] = [b"strlen", b"str_repeat", b"zend_version"];
        let id = compute_system_id(all);
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(id, compute_system_id([all[2], all[0], all[1]]));
        assert_ne!(id, compute_system_id([all[0], all[1]]));
        assert_ne!(id, compute_system_id([all[0], all[2]]));
    }
}
