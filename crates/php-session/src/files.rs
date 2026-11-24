// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `files` save handler (`ext/session/mod_files.c`).
//!
//! `session.save_path` is `"[N;[MODE;]]/path"`: `N` levels of
//! sub-directories named after the first characters of the ID and the
//! octal creation `MODE` (default `0600`). Data lives in `sess_<id>`,
//! locked with `flock(LOCK_EX)` for the lifetime of the open session.

use crate::handler::{SaveHandler, SaveModule};
use crate::host::SessionHost;
use crate::sid::valid_key;
use crate::state::warn;
use php_runtime::ErrorLevel;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// `FILE_PREFIX`.
pub const FILE_PREFIX: &str = "sess_";
/// `MAXPATHLEN`.
pub const MAXPATHLEN: usize = 4096;

/// The `files` module (`ps_mod_files`).
#[derive(Debug, Default, Clone, Copy)]
pub struct FilesModule;

/// The opened `files` handler (`ps_files`).
#[derive(Debug)]
pub struct FilesHandler {
    last_key: Option<Vec<u8>>,
    basedir: Vec<u8>,
    dirdepth: usize,
    st_size: u64,
    filemode: u32,
    file: Option<File>,
}

/// `strtol(s, NULL, base)` with overflow detection (`errno == ERANGE`).
fn strtol(s: &[u8], base: u32) -> (i64, bool) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let mut v: i128 = 0;
    let mut overflow = false;
    while i < s.len() {
        let Some(d) = char::from(s[i]).to_digit(base) else {
            break;
        };
        v = v * i128::from(base) + i128::from(d);
        if v > i128::from(i64::MAX) + 1 {
            overflow = true;
            v = i128::from(i64::MAX) + 1;
        }
        i += 1;
    }
    let v = if neg { -v } else { v };
    match i64::try_from(v) {
        Ok(n) => (n, overflow),
        Err(_) => (if neg { i64::MIN } else { i64::MAX }, true),
    }
}

/// The text of `strerror(errno)` (without Rust's `" (os error N)"`).
fn strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(p) => s[..p].to_string(),
        None => s,
    }
}

fn to_path(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

fn path_bytes(p: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        p.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        p.to_string_lossy().into_owned().into_bytes()
    }
}

/// The real and effective uid of this process, read from
/// `/proc/self/status` (std has no `getuid`).
fn process_uids() -> Option<(u32, u32)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    let mut it = line[4..].split_whitespace().map(str::parse::<u32>);
    Some((it.next()?.ok()?, it.next()?.ok()?))
}

impl SaveModule for FilesModule {
    fn name(&self) -> &'static str {
        "files"
    }

    fn open(
        &self,
        host: &mut dyn SessionHost,
        save_path: &[u8],
        _session_name: &[u8],
    ) -> Option<Box<dyn SaveHandler>> {
        let used: Vec<u8> = if save_path.is_empty() {
            let tmp = path_bytes(&host.temp_dir());
            if host.open_basedir_denied(&tmp) {
                return None;
            }
            tmp
        } else {
            save_path.to_vec()
        };
        // Split "N;MODE;path" (at most two separators are significant).
        let mut argv: Vec<&[u8]> = Vec::new();
        let mut last = 0;
        let mut search = 0;
        while let Some(off) = used[search..].iter().position(|&c| c == b';') {
            argv.push(&used[last..]);
            last = search + off + 1;
            search = last;
            if argv.len() > 1 {
                break;
            }
        }
        argv.push(&used[last..]);
        let mut dirdepth = 0usize;
        let mut filemode = 0o600u32;
        if argv.len() > 1 {
            let (v, overflow) = strtol(argv[0], 10);
            if overflow {
                plain_warning(host, "The first parameter in session.save_path is invalid");
                return None;
            }
            // (size_t) cast: negative depths become huge and fail later.
            dirdepth = usize::try_from(v).unwrap_or(usize::MAX);
        }
        if argv.len() > 2 {
            let (v, overflow) = strtol(argv[1], 8);
            if overflow || !(0..=0o7777).contains(&v) {
                plain_warning(host, "The second parameter in session.save_path is invalid");
                return None;
            }
            filemode = u32::try_from(v).unwrap_or(0o600);
        }
        Some(Box::new(FilesHandler {
            last_key: None,
            basedir: argv[argv.len() - 1].to_vec(),
            dirdepth,
            st_size: 0,
            filemode,
            file: None,
        }))
    }
}

fn plain_warning(host: &mut dyn SessionHost, msg: &str) {
    if let Err(e) = host.diagnostic(ErrorLevel::Warning, msg) {
        host.session().set_exception(e);
    }
}

impl FilesHandler {
    /// `ps_files_path_create()`.
    #[must_use]
    pub fn path_for(&self, key: &[u8]) -> Option<Vec<u8>> {
        let needed = self
            .basedir
            .len()
            .checked_add(self.dirdepth.checked_mul(2)?)?
            .checked_add(key.len() + 5 + FILE_PREFIX.len() + 1)?;
        if key.len() <= self.dirdepth || MAXPATHLEN < needed {
            return None;
        }
        let mut buf = self.basedir.clone();
        buf.push(b'/');
        for &c in &key[..self.dirdepth] {
            buf.push(c);
            buf.push(b'/');
        }
        buf.extend_from_slice(FILE_PREFIX.as_bytes());
        buf.extend_from_slice(key);
        Some(buf)
    }

    fn close_file(&mut self) {
        // Dropping the descriptor releases the flock.
        self.file = None;
    }

    /// `ps_files_open()`.
    fn open_key(&mut self, host: &mut dyn SessionHost, key: &[u8]) {
        if self.file.is_some() && self.last_key.as_deref() == Some(key) {
            return;
        }
        self.last_key = None;
        self.close_file();
        if !valid_key(key) {
            warn(
                host,
                ErrorLevel::Warning,
                "Session ID is too long or contains illegal characters. Only the A-Z, a-z, 0-9, \
                 \"-\", and \",\" characters are allowed",
            );
            return;
        }
        let Some(path) = self.path_for(key) else {
            warn(
                host,
                ErrorLevel::Warning,
                &format!(
                    "Failed to create session data file path. Too short session ID, invalid \
                     save_path or path length exceeds {MAXPATHLEN} characters"
                ),
            );
            return;
        };
        self.last_key = Some(key.to_vec());
        let p = to_path(&path);
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            // `O_CREAT | O_RDWR | O_NOFOLLOW`: a symlink planted at the
            // session's path is refused by the same `open()` (ELOOP), with
            // no window between a check and the open.
            opts.mode(self.filemode).custom_flags(libc::O_NOFOLLOW);
        }
        let opened = opts.open(&p);
        match opened {
            Ok(file) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt as _;
                    if let (Ok(meta), Some((uid, euid))) = (file.metadata(), process_uids()) {
                        let owner = meta.uid();
                        if owner != 0 && owner != uid && owner != euid && uid != 0 {
                            warn(
                                host,
                                ErrorLevel::Warning,
                                "Session data file is not created by your uid",
                            );
                            return;
                        }
                    }
                }
                // flock(LOCK_EX); EINTR is retried by std.
                #[allow(clippy::incompatible_msrv)] // the workspace toolchain is 1.95
                let _ = file.lock();
                self.file = Some(file);
            }
            Err(e) => {
                warn(
                    host,
                    ErrorLevel::Warning,
                    &format!(
                        "open({}, O_RDWR) failed: {} ({})",
                        String::from_utf8_lossy(&path),
                        strerror(&e),
                        e.raw_os_error().unwrap_or(0)
                    ),
                );
            }
        }
    }

    /// `ps_files_write()`.
    fn write_data(&mut self, host: &mut dyn SessionHost, key: &[u8], val: &[u8]) -> bool {
        self.open_key(host, key);
        let Some(file) = self.file.as_mut() else {
            return false;
        };
        // Truncate before writing (GHSA-fm2m-fjf5-v3cp, CVE-2026-126714):
        // a write cut short then leaves a short file, never the new data
        // followed by the end of the old session, which `unserialize()`
        // would read as one.
        if let Err(e) = file.set_len(0) {
            warn(
                host,
                ErrorLevel::Warning,
                &format!(
                    "ftruncate() failed: {} ({})",
                    strerror(&e),
                    e.raw_os_error().unwrap_or(0)
                ),
            );
            return false;
        }
        let res = file
            .seek(SeekFrom::Start(0))
            .and_then(|_| file.write_all(val));
        if let Err(e) = res {
            warn(
                host,
                ErrorLevel::Warning,
                &format!(
                    "Write failed: {} ({})",
                    strerror(&e),
                    e.raw_os_error().unwrap_or(0)
                ),
            );
            return false;
        }
        self.st_size = val.len() as u64;
        true
    }

    /// `ps_files_key_exists()`.
    fn key_exists(&self, key: &[u8]) -> bool {
        self.path_for(key)
            .is_some_and(|p| std::fs::metadata(to_path(&p)).is_ok())
    }
}

/// `ps_files_cleanup_dir()`: deletes `sess_*` files older than
/// `maxlifetime`, descending `depth` directory levels first. Returns the
/// number of deleted files or `-1` when the directory cannot be opened.
fn cleanup_dir(host: &mut dyn SessionHost, dirname: &[u8], maxlifetime: i64, depth: usize) -> i64 {
    let dir = match std::fs::read_dir(to_path(dirname)) {
        Ok(d) => d,
        Err(e) => {
            warn(
                host,
                ErrorLevel::Notice,
                &format!(
                    "ps_files_cleanup_dir: opendir({}) failed: {} ({})",
                    String::from_utf8_lossy(dirname),
                    strerror(&e),
                    e.raw_os_error().unwrap_or(0)
                ),
            );
            return -1;
        }
    };
    if dirname.len() >= MAXPATHLEN {
        warn(
            host,
            ErrorLevel::Notice,
            &format!(
                "ps_files_cleanup_dir: dirname({}) is too long",
                String::from_utf8_lossy(dirname)
            ),
        );
        return -1;
    }
    let mut nrdels = 0;
    for entry in dir.flatten() {
        let name = path_bytes(Path::new(&entry.file_name()));
        if dirname.len() + 1 + name.len() >= MAXPATHLEN {
            continue;
        }
        let mut full = dirname.to_vec();
        full.push(b'/');
        full.extend_from_slice(&name);
        let Ok(meta) = std::fs::metadata(to_path(&full)) else {
            continue;
        };
        if depth == 0 {
            if !name.starts_with(FILE_PREFIX.as_bytes()) {
                continue;
            }
            let now = host.now();
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            if now - mtime > maxlifetime {
                let _ = std::fs::remove_file(to_path(&full));
                nrdels += 1;
            }
        } else if meta.is_dir() {
            let n = cleanup_dir(host, &full, maxlifetime, depth - 1);
            if n >= 0 {
                nrdels += n;
            }
        }
    }
    nrdels
}

impl SaveHandler for FilesHandler {
    fn close(&mut self, _host: &mut dyn SessionHost) -> bool {
        self.close_file();
        self.last_key = None;
        true
    }

    fn read(
        &mut self,
        host: &mut dyn SessionHost,
        key: &[u8],
        _maxlifetime: i64,
    ) -> Option<Vec<u8>> {
        self.open_key(host, key);
        let file = self.file.as_mut()?;
        let meta = file.metadata().ok()?;
        self.st_size = meta.len();
        if meta.len() == 0 {
            return Some(Vec::new());
        }
        let mut buf = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
        let res = file
            .seek(SeekFrom::Start(0))
            .and_then(|_| file.read_to_end(&mut buf));
        match res {
            Ok(n) if n as u64 == meta.len() => Some(buf),
            Ok(_) => {
                warn(
                    host,
                    ErrorLevel::Warning,
                    "Read returned less bytes than requested",
                );
                None
            }
            Err(e) => {
                warn(
                    host,
                    ErrorLevel::Warning,
                    &format!(
                        "Read failed: {} ({})",
                        strerror(&e),
                        e.raw_os_error().unwrap_or(0)
                    ),
                );
                None
            }
        }
    }

    fn write(
        &mut self,
        host: &mut dyn SessionHost,
        key: &[u8],
        val: &[u8],
        _maxlifetime: i64,
    ) -> bool {
        self.write_data(host, key, val)
    }

    fn destroy(&mut self, _host: &mut dyn SessionHost, key: &[u8]) -> bool {
        let Some(path) = self.path_for(key) else {
            return false;
        };
        if self.file.is_some() {
            self.close_file();
            let p = to_path(&path);
            if std::fs::remove_file(&p).is_err() && p.exists() {
                return false;
            }
        }
        true
    }

    fn gc(&mut self, host: &mut dyn SessionHost, maxlifetime: i64) -> i64 {
        let basedir = self.basedir.clone();
        cleanup_dir(host, &basedir, maxlifetime, self.dirdepth)
    }

    fn create_sid(&mut self, host: &mut dyn SessionHost) -> Option<Vec<u8>> {
        let mut maxfail = 3;
        loop {
            match crate::state::create_default_id(host) {
                Some(sid) if !self.key_exists(&sid) => return Some(sid),
                _ => {
                    maxfail -= 1;
                    if maxfail < 0 || host.session().has_exception() {
                        return None;
                    }
                }
            }
        }
    }

    fn validate_sid(&mut self, _host: &mut dyn SessionHost, key: &[u8]) -> bool {
        self.key_exists(key)
    }

    fn update_timestamp(
        &mut self,
        host: &mut dyn SessionHost,
        key: &[u8],
        val: &[u8],
        _maxlifetime: i64,
    ) -> bool {
        let Some(path) = self.path_for(key) else {
            return false;
        };
        let touched = File::options()
            .write(true)
            .open(to_path(&path))
            .and_then(|f| f.set_modified(SystemTime::now()));
        if touched.is_err() {
            // New session ID: create the data file.
            return self.write_data(host, key, val);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(basedir: &str, dirdepth: usize) -> FilesHandler {
        FilesHandler {
            last_key: None,
            basedir: basedir.as_bytes().to_vec(),
            dirdepth,
            st_size: 0,
            filemode: 0o600,
            file: None,
        }
    }

    #[test]
    fn paths() {
        let h = handler("/tmp/s", 0);
        assert_eq!(h.path_for(b"abc").unwrap(), b"/tmp/s/sess_abc");
        let h = handler("/tmp/s", 2);
        assert_eq!(h.path_for(b"abc").unwrap(), b"/tmp/s/a/b/sess_abc");
        assert!(h.path_for(b"ab").is_none());
        let h = handler("/tmp/s", usize::MAX);
        assert!(h.path_for(b"abc").is_none());
        let long = "x".repeat(4090);
        assert!(handler(&long, 0).path_for(b"abc").is_none());
    }

    #[test]
    fn strtol_bases() {
        assert_eq!(strtol(b"2", 10), (2, false));
        assert_eq!(strtol(b"0644", 8), (0o644, false));
        assert_eq!(strtol(b"999999999999999999999", 10), (i64::MAX, true));
        assert_eq!(strtol(b"-1", 10), (-1, false));
        assert_eq!(strtol(b"x", 10), (0, false));
    }

    #[test]
    fn errno_text() {
        let e = std::io::Error::from_raw_os_error(2);
        assert_eq!(strerror(&e), "No such file or directory");
    }
}
