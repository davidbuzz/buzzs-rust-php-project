//! The `glob://` wrapper (`main/streams/glob_wrapper.c`).

use crate::context::ContextRef;
use crate::glob::{php_glob, GlobError};
use crate::paths;
use crate::stream::{flags, OpFailed, Stream, StreamOps, StreamState};
use crate::wrapper::{OpenError, Wrapper, STREAM_DISABLE_OPEN_BASEDIR};

/// Directory stream over glob results (`php_glob_stream_ops`).
#[derive(Debug, Default)]
pub struct GlobOps {
    results: Vec<Vec<u8>>,
    index: usize,
    path: Option<Vec<u8>>,
    pattern: Vec<u8>,
    append: bool,
}

fn split_dir(path: &[u8]) -> (Vec<u8>, &[u8]) {
    match path.iter().rposition(|&c| c == b'/') {
        Some(pos) => {
            let file = &path[pos + 1..];
            let dir_len = if pos + 1 > 1 { pos } else { pos + 1 };
            (path[..dir_len].to_vec(), file)
        }
        None => (Vec::new(), path),
    }
}

impl GlobOps {
    /// `php_glob_stream_get_path()`: the directory of the current entry.
    pub fn path(&self) -> Option<&[u8]> {
        self.path.as_deref()
    }

    /// `php_glob_stream_get_pattern()`: the file pattern part.
    pub fn pattern(&self) -> &[u8] {
        &self.pattern
    }

    /// `php_glob_stream_get_count()`.
    pub fn count(&self) -> usize {
        self.results.len()
    }
}

impl StreamOps for GlobOps {
    fn label(&self) -> &'static str {
        "glob"
    }

    fn read(&mut self, _st: &mut StreamState, _buf: &mut [u8]) -> Result<usize, OpFailed> {
        Err(OpFailed)
    }

    fn can_write(&self) -> bool {
        false
    }

    fn readdir(&mut self, _st: &mut StreamState) -> Option<Vec<u8>> {
        if self.index < self.results.len() {
            let entry = self.results[self.index].clone();
            let (dir, file) = split_dir(&entry);
            if self.append {
                self.path = Some(dir);
            }
            self.index += 1;
            return Some(file.to_vec());
        }
        self.path = None;
        None
    }

    fn can_seek(&self) -> bool {
        true
    }

    fn seek(
        &mut self,
        _st: &mut StreamState,
        _offset: i64,
        _whence: i32,
        _newpos: &mut i64,
    ) -> bool {
        self.index = 0;
        self.path = None;
        true
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// The `glob://` wrapper.
#[derive(Debug, Default)]
pub struct GlobWrapper;

impl Wrapper for GlobWrapper {
    fn label(&self) -> &'static str {
        "glob"
    }

    fn has_opener(&self) -> bool {
        false
    }

    fn open(
        &self,
        _path: &[u8],
        _mode: &str,
        _options: u32,
        _opened_path: &mut Option<Vec<u8>>,
        _context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        Err(OpenError::silent())
    }

    fn has_dir_opener(&self) -> bool {
        true
    }

    fn opendir(
        &self,
        path: &[u8],
        options: u32,
        _context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        let path = path.strip_prefix(b"glob://").unwrap_or(path);
        let mut results = match php_glob(path, 0) {
            Ok(r) => r,
            Err(GlobError::NoMatch) => Vec::new(),
            Err(_) => return Err(OpenError::silent()),
        };
        if options & STREAM_DISABLE_OPEN_BASEDIR == 0 {
            results.retain(|p| paths::check_open_basedir_ex(p, false));
        }
        let (_, pattern) = split_dir(path);
        let first = results.first().cloned().unwrap_or_else(|| path.to_vec());
        let (dir, _) = split_dir(&first);
        let ops = GlobOps {
            pattern: pattern.to_vec(),
            path: Some(dir),
            results,
            index: 0,
            append: true,
        };
        let mut s = Stream::new(Box::new(ops), "r");
        s.add_flags(flags::NO_BUFFER | flags::IS_DIR);
        Ok(s)
    }
}
