//! `basename()`, `dirname()` and `pathinfo()` (`ext/standard/string.c`,
//! `zend_dirname()`), POSIX variants with an ASCII-compatible locale.

/// `PATHINFO_DIRNAME`.
pub const PATHINFO_DIRNAME: i64 = 1;
/// `PATHINFO_BASENAME`.
pub const PATHINFO_BASENAME: i64 = 2;
/// `PATHINFO_EXTENSION`.
pub const PATHINFO_EXTENSION: i64 = 4;
/// `PATHINFO_FILENAME`.
pub const PATHINFO_FILENAME: i64 = 8;
/// `PATHINFO_ALL`.
pub const PATHINFO_ALL: i64 = 15;

/// `php_basename()`: strips trailing slashes and leading directories,
/// then removes `suffix` when it is a proper suffix of the result.
pub fn basename(s: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut end = s.len();
    while end > 0 && s[end - 1] == b'/' {
        end -= 1;
    }
    if end == 0 {
        return Vec::new();
    }
    let mut start = end - 1;
    while start > 0 && s[start - 1] != b'/' {
        start -= 1;
    }
    if !suffix.is_empty() && suffix.len() < end - start && s[start..end].ends_with(suffix) {
        end -= suffix.len();
    }
    s[start..end].to_vec()
}

/// `zend_dirname()`.
pub fn dirname(path: &[u8]) -> Vec<u8> {
    if path.is_empty() {
        return Vec::new();
    }
    let mut end = isize::try_from(path.len()).unwrap_or(isize::MAX) - 1;
    let at = |i: isize| path[usize::try_from(i).unwrap_or(0)];
    while end >= 0 && at(end) == b'/' {
        end -= 1;
    }
    if end < 0 {
        return b"/".to_vec();
    }
    while end >= 0 && at(end) != b'/' {
        end -= 1;
    }
    if end < 0 {
        return b".".to_vec();
    }
    while end >= 0 && at(end) == b'/' {
        end -= 1;
    }
    if end < 0 {
        return b"/".to_vec();
    }
    path[..usize::try_from(end + 1).unwrap_or(0)].to_vec()
}

/// `dirname($path, $levels)`; `None` when `levels < 1` (PHP throws
/// `ValueError: dirname(): Argument #2 ($levels) must be greater than or
/// equal to 1`).
pub fn dirname_levels(path: &[u8], levels: i64) -> Option<Vec<u8>> {
    if levels < 1 {
        return None;
    }
    if levels == 1 {
        return Some(dirname(path));
    }
    let mut ret = path.to_vec();
    let mut levels = levels;
    loop {
        let before = ret.len();
        ret = dirname(&ret);
        levels -= 1;
        if ret.len() >= before || levels == 0 {
            break;
        }
    }
    Some(ret)
}

/// The parts computed by `pathinfo()`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathInfo {
    /// `dirname`, absent for an empty path.
    pub dirname: Option<Vec<u8>>,
    /// `basename`.
    pub basename: Vec<u8>,
    /// `extension`, absent when the basename has no dot.
    pub extension: Option<Vec<u8>>,
    /// `filename`.
    pub filename: Vec<u8>,
}

/// `pathinfo()` copies the dirname as a C string (stops at NUL).
fn c_str(mut v: Vec<u8>) -> Vec<u8> {
    if let Some(p) = v.iter().position(|&c| c == 0) {
        v.truncate(p);
    }
    v
}

/// `pathinfo($path)` with `PATHINFO_ALL`.
pub fn pathinfo(path: &[u8]) -> PathInfo {
    let d = c_str(dirname(path));
    let basename = basename(path, b"");
    let dot = basename.iter().rposition(|&c| c == b'.');
    PathInfo {
        dirname: (!d.is_empty() && d[0] != 0).then_some(d),
        extension: dot.map(|p| basename[p + 1..].to_vec()),
        filename: dot.map_or_else(|| basename.clone(), |p| basename[..p].to_vec()),
        basename,
    }
}

/// Errors of `pathinfo()`'s `$flags` validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathinfoFlagError {
    /// "must be one of the PATHINFO_* constants".
    OutOfRange,
    /// "must be only one of the PATHINFO_* constants".
    Multiple,
}

impl PathinfoFlagError {
    /// The `ValueError` message suffix (after `pathinfo(): Argument #2
    /// ($flags) `).
    pub fn message(self) -> &'static str {
        match self {
            PathinfoFlagError::OutOfRange => "must be one of the PATHINFO_* constants",
            PathinfoFlagError::Multiple => "must be only one of the PATHINFO_* constants",
        }
    }
}

/// `pathinfo($path, $flag)` for a single flag: the string result.
pub fn pathinfo_part(path: &[u8], opt: i64) -> Result<Vec<u8>, PathinfoFlagError> {
    if !(PATHINFO_DIRNAME..=PATHINFO_ALL).contains(&opt) {
        return Err(PathinfoFlagError::OutOfRange);
    }
    if opt < PATHINFO_ALL && opt & (opt - 1) != 0 {
        return Err(PathinfoFlagError::Multiple);
    }
    let info = pathinfo(path);
    Ok(match opt {
        PATHINFO_DIRNAME => c_str(dirname(path)),
        PATHINFO_BASENAME => info.basename,
        PATHINFO_EXTENSION => info.extension.unwrap_or_default(),
        _ => info.filename,
    })
}

#[cfg(test)]
mod tests;
