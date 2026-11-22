//! The part of opcache's shared memory that the processes forked after its
//! startup see together: the paths of the scripts any of them cached.
//!
//! php-src maps its shared memory before the first request
//! (`accel_startup`), so a process forked from that one (`pcntl_fork()`, a
//! pre-forking server) shares the script table: a script its child cached
//! is listed by the parent's `opcache_get_status()`. The compiled scripts
//! here stay per process; what is shared is this index, a file created at
//! the first compile with opcache on and unlinked at once, so that it lives
//! exactly as long as a process holding it, leaves nothing behind, and is
//! inherited by every process forked afterwards. Each record is one line:
//! `+pid path` (cached by process `pid`), `-pid path` (invalidated); a reset
//! empties the file. A process lists what the others cached
//! ([`cached_elsewhere`]): its own cache answers for itself.
//!
//! ref: ext/opcache/ZendAccelerator.c (`accel_startup`, `zend_accel_hash`)

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::sync::OnceLock;

static INDEX: OnceLock<Option<File>> = OnceLock::new();

/// The restart log: one byte a manual restart, so its length is
/// `ZCSG(manual_restarts)` for every process sharing it.
static RESTARTS: OnceLock<Option<File>> = OnceLock::new();

/// Opens the index once a process (a forked child keeps its parent's).
pub fn init() {
    INDEX.get_or_init(open);
}

fn open() -> Option<File> {
    let dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    for attempt in 0..8u32 {
        let path = dir.join(format!(
            "php-opcache-index-{}-{nanos}-{attempt}",
            std::process::id()
        ));
        match OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                // Unlinked at once: the open descriptors keep it.
                let _ = std::fs::remove_file(&path);
                return Some(file);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
    }
    None
}

/// Appends one record (each `write` of an append-mode file lands whole at
/// its end, whichever process makes it).
fn record(kind: u8, path: &str) {
    if path.contains('\n') {
        return;
    }
    if let Some(Some(file)) = INDEX.get() {
        let mut line = format!("{} ", std::process::id()).into_bytes();
        line.insert(0, kind);
        line.extend_from_slice(path.as_bytes());
        line.push(b'\n');
        let _ = (&*file).write_all(&line);
    }
}

/// Opens the restart log once a process, at module startup: the
/// processes forked from this one (a pre-forking server's workers) share
/// it, as they share the restart counters of php-src's shared memory
/// (GH-8739).
pub fn init_restarts() {
    RESTARTS.get_or_init(open);
}

/// A manual restart (`opcache_reset()`), counted for every process that
/// shares the log (each `write` of an append-mode file lands whole).
pub fn record_manual_restart() {
    if let Some(Some(file)) = RESTARTS.get() {
        let _ = (&*file).write_all(b"r");
    }
}

/// The manual restarts of all processes sharing the restart log, or `None`
/// when it is not open.
#[must_use]
pub fn manual_restarts() -> Option<u64> {
    match RESTARTS.get() {
        Some(Some(file)) => file.metadata().ok().map(|m| m.len()),
        _ => None,
    }
}

/// `path` was cached (`cache_script_in_shared_memory`).
pub fn record_cached(path: &str) {
    record(b'+', path);
}

/// `path` was invalidated (`opcache_invalidate()`).
pub fn record_invalidated(path: &str) {
    record(b'-', path);
}

/// The cache was reset (`opcache_reset()`): no script is cached anywhere.
pub fn record_reset() {
    if let Some(Some(file)) = INDEX.get() {
        let _ = file.set_len(0);
    }
}

/// The paths cached by the other processes sharing the index, in the order
/// they were first cached, without those invalidated since (by any).
#[must_use]
pub fn cached_elsewhere() -> Vec<String> {
    let Some(Some(file)) = INDEX.get() else {
        return Vec::new();
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return Vec::new();
    };
    let mut buf = vec![0u8; usize::try_from(len).unwrap_or(0)];
    if file.read_exact_at(&mut buf, 0).is_err() {
        return Vec::new();
    }
    parse(&buf, std::process::id())
}

/// The paths the records `buf` leave cached by processes other than `me`.
fn parse(buf: &[u8], me: u32) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for line in buf.split(|&c| c == b'\n') {
        let Some((&kind, rest)) = line.split_first() else {
            continue;
        };
        let rest = String::from_utf8_lossy(rest);
        let Some((pid, path)) = rest.split_once(' ') else {
            continue;
        };
        let (Ok(pid), path) = (pid.parse::<u32>(), path.to_string()) else {
            continue;
        };
        match kind {
            b'+' if pid != me && !paths.contains(&path) => paths.push(path),
            b'-' => paths.retain(|p| *p != path),
            _ => {}
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records are read back in order; an invalidation (by any process)
    /// removes a path, a later record adds it again, a duplicate is listed
    /// once, and what process `me` cached itself is not listed.
    #[test]
    fn records_and_their_paths() {
        assert_eq!(parse(b"", 1), Vec::<String>::new());
        assert_eq!(
            parse(
                b"+2 /a.php\n+2 /b.php\n+3 /a.php\n-1 /b.php\n+2 /c.php\n+1 /own.php\n",
                1
            ),
            vec!["/a.php".to_string(), "/c.php".to_string()]
        );
        assert_eq!(
            parse(b"-2 /a.php\n+2 /a.php\n", 1),
            vec!["/a.php".to_string()]
        );
        // A torn last line is still a path; malformed lines are skipped.
        assert_eq!(parse(b"?2 x\n+x /e\n+2 /d", 1), vec!["/d".to_string()]);
        assert_eq!(
            parse(b"+2 /with space.php\n", 1),
            vec!["/with space.php".to_string()]
        );
    }

    /// The index file: what is recorded is listed, a reset empties it, and
    /// nothing is left in the temporary directory.
    #[test]
    fn the_index_file() {
        init();
        record_reset();
        record_cached("/tmp/one.php");
        record_cached("/tmp/two.php");
        record_invalidated("/tmp/one.php");
        record_cached("/tmp/bad\nname.php");
        // This process's own records are not listed for it.
        assert!(cached_elsewhere().is_empty());
        let mut file = INDEX.get().and_then(Option::as_ref).expect("index");
        std::io::Write::write_all(&mut file, b"+1 /tmp/three.php\n").expect("write");
        assert_eq!(cached_elsewhere(), vec!["/tmp/three.php".to_string()]);
        record_reset();
        assert!(cached_elsewhere().is_empty());
        let leftover = std::fs::read_dir(std::env::temp_dir())
            .map(|d| {
                d.filter_map(Result::ok).any(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with(&format!("php-opcache-index-{}-", std::process::id()))
                })
            })
            .unwrap_or(false);
        assert!(!leftover);
    }
}
