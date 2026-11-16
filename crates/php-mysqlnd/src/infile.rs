// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `LOAD DATA LOCAL INFILE` support (ref: `mysqlnd_loaddata.c`): the
//! pluggable reader used to stream a client-side file to the server and the
//! `local_infile_directory` containment check.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::consts::{CR_UNKNOWN_ERROR, MYSQLND_EE_FILENOTFOUND};

/// Opens files requested by the server. The default implementation uses
/// the local filesystem; the PHP binding may substitute one that goes
/// through PHP streams / `open_basedir`.
pub trait LocalInfileHandler: std::fmt::Debug {
    /// `local_infile_init`: opens `filename`, or returns `(errno, message)`.
    fn open(&mut self, filename: &[u8]) -> Result<Box<dyn Read>, (u32, String)>;
}

/// The default handler (`mysqlnd_local_infile_default`).
#[derive(Debug, Default, Clone, Copy)]
pub struct FsInfile;

/// `snprintf("Can't find file '%-.64s'.")`
#[must_use]
pub fn not_found_message(filename: &[u8]) -> String {
    let shown = &filename[..filename.len().min(64)];
    format!("Can't find file '{}'.", String::from_utf8_lossy(shown))
}

impl LocalInfileHandler for FsInfile {
    fn open(&mut self, filename: &[u8]) -> Result<Box<dyn Read>, (u32, String)> {
        let path = PathBuf::from(String::from_utf8_lossy(filename).into_owned());
        match std::fs::File::open(&path) {
            Ok(f) if f.metadata().is_ok_and(|m| !m.is_dir()) => Ok(Box::new(f)),
            _ => Err((MYSQLND_EE_FILENOTFOUND, not_found_message(filename))),
        }
    }
}

/// The message used for read errors (`"Error reading file"`, `CR_UNKNOWN_ERROR`).
pub const READ_ERROR: (u32, &str) = (CR_UNKNOWN_ERROR, "Error reading file");

/// Lexically normalises `p` against the current directory (no symlink
/// resolution for the non-existing tail).
fn absolutize(p: &Path) -> PathBuf {
    let base = if p.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    let joined = base.join(p);
    if let Ok(c) = std::fs::canonicalize(&joined) {
        return c;
    }
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `php_check_specific_open_basedir(dir, filename)`: whether `filename`
/// resolves to a path inside `dir`.
#[must_use]
pub fn path_within(dir: &str, filename: &[u8]) -> bool {
    let d = absolutize(Path::new(dir));
    let f = absolutize(Path::new(&*String::from_utf8_lossy(filename)));
    f.starts_with(&d)
}

/// Whether `dir` can be opened as a directory (`php_stream_opendir`).
#[must_use]
pub fn dir_accessible(dir: &str) -> bool {
    std::fs::read_dir(dir).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found() {
        let mut h = FsInfile;
        let Err(e) = h.open(b"/nonexistent/file.csv") else {
            panic!("opened")
        };
        assert_eq!(e.0, MYSQLND_EE_FILENOTFOUND);
        assert_eq!(e.1, "Can't find file '/nonexistent/file.csv'.");
        let long = vec![b'a'; 100];
        assert_eq!(
            not_found_message(&long).len(),
            "Can't find file ''.".len() + 64
        );
    }

    #[test]
    fn containment() {
        let tmp = std::env::temp_dir();
        let t = tmp.to_string_lossy().into_owned();
        assert!(path_within(&t, format!("{t}/x.csv").as_bytes()));
        assert!(!path_within(&t, b"/etc/passwd"));
        assert!(!path_within(&t, format!("{t}/../etc/passwd").as_bytes()));
        assert!(dir_accessible(&t));
        assert!(!dir_accessible("/nonexistent-dir-xyz"));
    }
}
