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

/// Numbers the temporary files of one process, so threads writing the
/// same entry do not share one.
static TEMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Writes a script's payload to the file cache.
///
/// The entry is written to a temporary file next to it and renamed into
/// place, so a process reading the entry sees either the old or the new
/// file, never a partly written one. `zend_file_cache_script_store` gets
/// the same from `O_EXCL` and `flock`, but it never replaces an existing
/// entry; here an entry left by other compiler settings must be replaced.
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

    let seq = TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut temp_name = bin_path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    temp_name.push(format!(".{}.{seq}.tmp", std::process::id()));
    let temp_path = bin_path.with_file_name(temp_name);

    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
    {
        Ok(f) => f,
        Err(e) => {
            let msg = format!(
                "opcache cannot create file '{}', {}",
                bin_path.display(),
                strerror(&e)
            );
            return Err((msg, e));
        }
    };
    let written = std::io::Write::write_all(&mut file, &bytes);
    drop(file);
    match written.and_then(|()| std::fs::rename(&temp_path, &bin_path)) {
        Ok(()) => Ok(bin_path),
        Err(e) => {
            let msg = format!(
                "opcache cannot write to file '{}': {}",
                bin_path.display(),
                strerror(&e)
            );
            let _ = std::fs::remove_file(&temp_path);
            Err((msg, e))
        }
    }
}

/// Reads a script's payload from the file cache.
/// Returns `Ok(Some(payload))` on cache hit, `Ok(None)` on cache miss or stale,
/// and `Err(msg)` on a corrupted file. A stale or corrupted entry is
/// removed unless the cache is read-only (`opcache.file_cache_read_only`).
pub fn read_file_cache(
    cache_dir: &Path,
    script_path: &Path,
    expected_mtime: i64,
    validate_timestamps: bool,
    consistency_checks: bool,
    read_only: bool,
) -> Result<Option<Vec<u8>>, String> {
    let bin_path = get_bin_path(cache_dir, script_path);
    let Ok(bytes) = std::fs::read(&bin_path) else {
        return Ok(None);
    };
    let discard = || {
        if !read_only {
            let _ = std::fs::remove_file(&bin_path);
        }
    };
    let corrupted = |what: &str| {
        discard();
        Err(format!(
            "opcache cannot read from file '{}' ({what})",
            bin_path.display()
        ))
    };

    let Some(header) = bytes.get(..HEADER_SIZE) else {
        return corrupted("info");
    };
    if &header[0..8] != MAGIC {
        return corrupted("wrong header");
    }
    if &header[8..40] != system_id().as_bytes() {
        return corrupted("wrong \"system_id\"");
    }

    let field = |range: std::ops::Range<usize>| {
        let mut buf = [0_u8; 8];
        buf[..range.len()].copy_from_slice(&header[range]);
        buf
    };
    let mtime = i64::from_le_bytes(field(40..48));
    if validate_timestamps && mtime != expected_mtime {
        discard();
        return Ok(None);
    }

    let checksum = u32::try_from(u64::from_le_bytes(field(48..52))).unwrap_or(u32::MAX);
    let payload = usize::try_from(u64::from_le_bytes(field(52..60)))
        .ok()
        .and_then(|len| bytes.get(HEADER_SIZE..)?.get(..len));
    let Some(payload) = payload else {
        return corrupted("mem");
    };
    if consistency_checks {
        let actual = adler32(payload);
        if actual != checksum {
            discard();
            // Sic: php-src writes "excepted".
            return Err(format!(
                "corrupted file '{}' excepted checksum: 0x{checksum:08x} actual checksum: 0x{actual:08x}",
                bin_path.display()
            ));
        }
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
    use super::{adler32, compute_system_id, get_bin_path, read_file_cache, write_file_cache};
    use std::path::{Path, PathBuf};

    /// A fresh, empty cache directory for one test.
    fn cache_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("buzz-file-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create cache dir");
        dir
    }

    fn read(dir: &Path, read_only: bool) -> Result<Option<Vec<u8>>, String> {
        read_file_cache(dir, Path::new("/app/a.php"), 7, true, true, read_only)
    }

    #[test]
    fn write_renames_into_place_and_leaves_no_temporary_file() {
        let dir = cache_dir("write");
        let script = Path::new("/app/a.php");
        let bin = write_file_cache(&dir, script, 7, b"first").expect("write");
        assert_eq!(read(&dir, false), Ok(Some(b"first".to_vec())));
        // An existing entry is replaced.
        write_file_cache(&dir, script, 7, b"second").expect("rewrite");
        assert_eq!(read(&dir, false), Ok(Some(b"second".to_vec())));
        let names: Vec<_> = std::fs::read_dir(bin.parent().expect("parent"))
            .expect("list")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names, [bin.file_name().expect("name").to_os_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncated_entry_is_reported_and_removed_unless_read_only() {
        let dir = cache_dir("truncated");
        let bin = write_file_cache(&dir, Path::new("/app/a.php"), 7, b"payload").expect("write");
        let full = std::fs::read(&bin).expect("read back");
        std::fs::write(&bin, &full[..full.len() - 2]).expect("truncate");

        let err = read(&dir, true).expect_err("truncated");
        assert!(err.ends_with("(mem)"), "{err}");
        assert!(bin.exists(), "read-only keeps the entry");

        let err = read(&dir, false).expect_err("truncated");
        assert!(err.ends_with("(mem)"), "{err}");
        assert!(!bin.exists(), "a writable cache drops the entry");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checksum_mismatch_is_reported_like_php_src() {
        let dir = cache_dir("checksum");
        let bin = write_file_cache(&dir, Path::new("/app/a.php"), 7, b"payload").expect("write");
        let mut bytes = std::fs::read(&bin).expect("read back");
        *bytes.last_mut().expect("payload byte") ^= 1;
        std::fs::write(&bin, &bytes).expect("corrupt");
        let err = read(&dir, false).expect_err("corrupted");
        assert!(err.starts_with("corrupted file '"), "{err}");
        assert!(err.contains("excepted checksum: 0x"), "{err}");
        assert!(!bin.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_entry_is_a_miss() {
        let dir = cache_dir("stale");
        write_file_cache(&dir, Path::new("/app/a.php"), 6, b"old").expect("write");
        assert_eq!(read(&dir, true), Ok(None));
        assert!(get_bin_path(&dir, Path::new("/app/a.php")).exists());
        assert_eq!(read(&dir, false), Ok(None));
        assert!(!get_bin_path(&dir, Path::new("/app/a.php")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

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
