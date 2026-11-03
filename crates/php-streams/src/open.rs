//! Opening streams and directories through wrappers
//! (`_php_stream_open_wrapper_ex`, `_php_stream_opendir`, path operations).

use crate::context::ContextRef;
use crate::diag::{self, Severity, StreamErrorCode};
use crate::paths;
use crate::stat::StatBuf;
use crate::state;
use crate::stream::{flags, Stream, WrapperInfo, SEEK_CUR};
use crate::sys;
use crate::wrapper::{
    self, wrapper_warn, Located, MetadataOp, OpenError, Wrapper, DEFAULT_WRAPPER_NAME,
    REPORT_ERRORS, STREAM_ASSUME_REALPATH, STREAM_MUST_SEEK, STREAM_USE_URL, USE_PATH,
};

/// Joins logged opener errors into the caption message
/// (`php_stream_display_wrapper_name_errors`).
pub fn display_wrapper_errors(label: &str, caption: &str, err: &OpenError, code: StreamErrorCode) {
    if err.exception {
        return;
    }
    let msg: Vec<u8> = if label == DEFAULT_WRAPPER_NAME {
        b"no suitable wrapper could be found".to_vec()
    } else if !err.messages.is_empty() {
        let br: &[u8] = if state::settings().html_errors {
            b"<br />\n"
        } else {
            b"\n"
        };
        err.messages.join(br)
    } else if label == crate::plain::LABEL {
        sys::strerror(err.errno.unwrap_or(0)).into_bytes()
    } else {
        b"operation failed".to_vec()
    };
    diag::stream_error(
        Severity::Warning,
        label,
        code,
        true,
        diag::msg(&[caption.as_bytes(), b": ", &msg]),
    );
}

fn info_of(w: &dyn Wrapper) -> WrapperInfo {
    WrapperInfo {
        label: w.label(),
        is_url: w.is_url(),
        is_plain_files: w.is_plain_files(),
    }
}

/// The error raised for an empty path (`zend_value_error("Path must not be
/// empty")`). The VM must turn it into a `ValueError`.
pub const EMPTY_PATH_ERROR: &str = "Path must not be empty";

/// `php_stream_open_wrapper_ex()`. Returns `None` on failure after
/// emitting PHP's warnings (when `options` has `REPORT_ERRORS`).
/// `opened_path` receives the resolved path when known.
///
/// An empty `path` returns `None` without a warning; callers raise
/// [`EMPTY_PATH_ERROR`] as a `ValueError` (PHP checks this before).
#[allow(clippy::too_many_lines)] // a direct port of the C function
pub fn open_wrapper(
    path: &[u8],
    mode: &str,
    options: u32,
    opened_path: &mut Option<Vec<u8>>,
    context: Option<&ContextRef>,
) -> Option<Stream> {
    *opened_path = None;
    if path.is_empty() {
        return None;
    }
    let mut options = options;
    let mut path = path.to_vec();
    let mut resolved = None;
    if options & USE_PATH != 0 {
        let inc = state::settings().include_path;
        if let Some(r) = paths::resolve_path(&path, &inc) {
            path.clone_from(&r);
            resolved = Some(r);
            options |= STREAM_ASSUME_REALPATH;
            options &= !USE_PATH;
        }
    }
    let Some(Located {
        wrapper,
        path: path_to_open,
    }) = wrapper::locate(&path, options)
    else {
        wrapper_warn(
            DEFAULT_WRAPPER_NAME,
            options,
            StreamErrorCode::OpenFailed,
            "Failed to open stream: no suitable wrapper could be found",
        );
        return None;
    };
    if options & STREAM_USE_URL != 0 && !wrapper.is_url() {
        wrapper_warn(
            wrapper.label(),
            options,
            StreamErrorCode::ProtocolUnsupported,
            "This function may only be used against URLs",
        );
        return None;
    }
    if !wrapper.has_opener() {
        wrapper_warn(
            wrapper.label(),
            options,
            StreamErrorCode::NoOpener,
            "Failed to open stream: wrapper does not support stream open",
        );
        return None;
    }
    let label = wrapper.label();
    let mut stream = match wrapper.open(
        &path_to_open,
        mode,
        options & !REPORT_ERRORS,
        opened_path,
        context,
    ) {
        Ok(s) => s,
        Err(err) => {
            if options & REPORT_ERRORS != 0 {
                display_wrapper_errors(
                    label,
                    "Failed to open stream",
                    &err,
                    StreamErrorCode::OpenFailed,
                );
            }
            *opened_path = None;
            return None;
        }
    };
    stream.set_wrapper(info_of(&*wrapper));
    if opened_path.is_none() {
        if let Some(r) = resolved {
            *opened_path = Some(r);
        }
    }
    stream.orig_path = Some(path.clone());
    if stream.context.is_none() {
        stream.context = context.cloned();
    }
    if options & STREAM_MUST_SEEK != 0 && !stream.is_seekable() {
        // php_stream_make_seekable: copy into a temp stream.
        let mut temp = crate::memory::temp_create(
            crate::mode::TempMode::Default,
            crate::memory::MAX_MEM,
            None,
        );
        let (ok, _) = crate::stream::copy_to_stream(&mut stream, &mut temp, None);
        stream.close();
        if !ok {
            wrapper_warn(
                label,
                options,
                StreamErrorCode::SeekNotSupported,
                diag::msg(&[b"could not make seekable - ", &path]),
            );
            return None;
        }
        temp.rewind();
        temp.orig_path = Some(path);
        return Some(temp);
    }
    if stream.is_seekable() && mode.contains('a') && stream.position == 0 {
        if let Some(p) = stream.raw_seek(0, SEEK_CUR) {
            stream.position = p;
        }
    }
    Some(stream)
}

/// `php_stream_open_wrapper()` with `REPORT_ERRORS`, the common case
/// (`fopen()`).
pub fn fopen(
    path: &[u8],
    mode: &str,
    use_include_path: bool,
    context: Option<&ContextRef>,
) -> Option<Stream> {
    let mut opened = None;
    let opts = REPORT_ERRORS | if use_include_path { USE_PATH } else { 0 };
    open_wrapper(path, mode, opts, &mut opened, context)
}

/// `_php_stream_opendir()`.
pub fn opendir(path: &[u8], options: u32, context: Option<&ContextRef>) -> Option<Stream> {
    if path.is_empty() {
        return None;
    }
    let Some(Located {
        wrapper,
        path: path_to_open,
    }) = wrapper::locate(path, options)
    else {
        wrapper_warn(
            DEFAULT_WRAPPER_NAME,
            options,
            StreamErrorCode::OpenFailed,
            "Failed to open directory",
        );
        return None;
    };
    if !wrapper.has_dir_opener() {
        wrapper_warn(
            wrapper.label(),
            options,
            StreamErrorCode::NoOpener,
            "Failed to open directory: not implemented",
        );
        return None;
    }
    match wrapper.opendir(&path_to_open, options & !REPORT_ERRORS, context) {
        Ok(mut s) => {
            s.set_wrapper(info_of(&*wrapper));
            s.add_flags(flags::NO_BUFFER | flags::IS_DIR);
            if s.orig_path.is_none() {
                s.orig_path = Some(path.to_vec());
            }
            s.context = context.cloned();
            Some(s)
        }
        Err(err) => {
            if options & REPORT_ERRORS != 0 {
                display_wrapper_errors(
                    wrapper.label(),
                    "Failed to open directory",
                    &err,
                    StreamErrorCode::OpenFailed,
                );
            }
            None
        }
    }
}

/// `php_stream_stat_path_ex()`: `None` when no wrapper or failure.
pub fn stat_path(path: &[u8], flags: u32, context: Option<&ContextRef>) -> Option<StatBuf> {
    let loc = wrapper::locate(path, 0)?;
    loc.wrapper.url_stat(&loc.path, flags, context)?.ok()
}

/// `php_stream_mkdir()`.
pub fn mkdir(path: &[u8], mode: i64, options: u32, context: Option<&ContextRef>) -> bool {
    let Some(loc) = wrapper::locate(path, 0) else {
        return false;
    };
    loc.wrapper
        .mkdir(path, mode, options, context)
        .unwrap_or(false)
}

/// `php_stream_rmdir()`.
pub fn rmdir(path: &[u8], options: u32, context: Option<&ContextRef>) -> bool {
    let Some(loc) = wrapper::locate(path, 0) else {
        return false;
    };
    loc.wrapper.rmdir(path, options, context).unwrap_or(false)
}

/// Runs a metadata operation through the path's wrapper (touch, chmod,
/// chown, chgrp). Returns `None` when the wrapper has no metadata support.
pub fn metadata(path: &[u8], op: &MetadataOp, context: Option<&ContextRef>) -> Option<bool> {
    let loc = wrapper::locate(path, 0)?;
    loc.wrapper.metadata(path, op, context)
}

/// `php_stream_scandir()` sort order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScandirSort {
    /// `SCANDIR_SORT_ASCENDING`.
    Ascending,
    /// `SCANDIR_SORT_DESCENDING`.
    Descending,
    /// `SCANDIR_SORT_NONE`.
    None,
}

/// `php_stream_scandir()`: `None` when the directory cannot be opened.
pub fn scandir(
    dirname: &[u8],
    sort: ScandirSort,
    context: Option<&ContextRef>,
) -> Option<Vec<Vec<u8>>> {
    let mut s = opendir(dirname, REPORT_ERRORS, context)?;
    let mut names = Vec::new();
    while let Some(n) = s.readdir() {
        names.push(n);
    }
    s.close();
    match sort {
        ScandirSort::Ascending => names.sort(),
        ScandirSort::Descending => names.sort_by(|a, b| b.cmp(a)),
        ScandirSort::None => {}
    }
    Some(names)
}
