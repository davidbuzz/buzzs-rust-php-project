// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The compiler hooks phar installs (`phar_compile_file()`,
//! `phar_resolve_path()` in `phar.c`): what to compile when a phar is
//! included directly (`include 'app.phar'`), and include-path resolution
//! relative to the running phar.

use crate::consts::FILE_COMPRESSION_MASK;
use crate::registry as reg;
use crate::util::find;

/// What the compiler should read for an included file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileSource {
    /// Compile the file as usual.
    Original,
    /// Compile this `phar://` URL instead (the stub of a tar/zip phar),
    /// keeping the original file name.
    Stub(Vec<u8>),
    /// Compile these bytes (the decompressed image of a gz/bz2 phar; the
    /// compiler stops at `__HALT_COMPILER();`).
    Bytes(Vec<u8>),
}

/// `phar_has_marker_in_filename()`: the basename contains `.phar` followed
/// by the end or another `.`.
pub fn has_marker_in_filename(filename: &[u8]) -> bool {
    let base = match filename.iter().rposition(|&c| c == b'/') {
        Some(p) => &filename[p + 1..],
        None => filename,
    };
    let mut rest = base;
    while let Some(p) = find(rest, b".phar") {
        let after = rest.get(p + 5).copied();
        if matches!(after, None | Some(b'.')) {
            return true;
        }
        rest = &rest[p + 5..];
    }
    false
}

/// `phar_compile_file()`: decides how to compile `filename`.
pub fn compile_source(filename: &[u8]) -> CompileSource {
    if !has_marker_in_filename(filename) || find(filename, b"://").is_some() {
        return CompileSource::Original;
    }
    let Ok(phar) = reg::open_from_filename(filename, None, false) else {
        return CompileSource::Original;
    };
    let mut a = phar.borrow_mut();
    if a.is_zip || a.is_tar {
        let mut n = b"phar://".to_vec();
        n.extend_from_slice(filename);
        n.extend_from_slice(b"/.phar/stub.php");
        return CompileSource::Stub(n);
    }
    if a.flags & FILE_COMPRESSION_MASK != 0 {
        let len = usize::try_from(a.halt_offset).unwrap_or(0) + 32;
        if let Some(img) = a.open_archive_fp() {
            return CompileSource::Bytes(img[..len.min(img.len())].to_vec());
        }
    }
    CompileSource::Original
}

/// `phar_resolve_path()`: resolves an include path relative to the
/// running phar; `None` means "use the normal resolution".
pub fn resolve_path(
    filename: &[u8],
    executed: Option<&[u8]>,
    include_path: &[u8],
) -> Option<Vec<u8>> {
    reg::find_in_include_path(filename, executed?, include_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers() {
        assert!(has_marker_in_filename(b"/x/app.phar"));
        assert!(has_marker_in_filename(b"/x/app.phar.gz"));
        assert!(!has_marker_in_filename(b"/x/app.pharx"));
        assert!(!has_marker_in_filename(b"/x.phar/app.php"));
        assert_eq!(
            compile_source(b"/nonexistent/a.php"),
            CompileSource::Original
        );
        assert_eq!(
            compile_source(b"phar:///x.phar/a.php"),
            CompileSource::Original
        );
    }
}
