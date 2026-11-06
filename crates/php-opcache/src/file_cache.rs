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
pub fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Returns the system id.
pub fn system_id() -> &'static str {
    SYSTEM_ID
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
    cache_dir.join(SYSTEM_ID).join(parent).join(file_name)
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
                e
            );
            return Err((msg, e));
        }
    }

    let checksum = adler32(payload);
    let mut bytes = Vec::with_capacity(HEADER_SIZE + payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(SYSTEM_ID.as_bytes());
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
                e
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

    if &bytes[8..40] != SYSTEM_ID.as_bytes() {
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
                    && &bytes[8..40] == SYSTEM_ID.as_bytes()
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
