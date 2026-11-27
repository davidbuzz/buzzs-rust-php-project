// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Access to the stream resources used by `CURLOPT_FILE`, `INFILE`,
//! `WRITEHEADER` and `STDERR` (php-ext-streams payloads).

use php_ext_streams::res;
use php_runtime::Resource;
use php_streams::stream::CastAs;
use php_vm::ext::VmResult;
use php_vm::Vm;

/// Whether `r` is a stream resource that is still open.
pub(crate) fn is_open(r: &Resource) -> bool {
    !r.is_closed() && res::cell_of(r).is_some_and(|c| !c.is_closed())
}

/// Writes `data` to the stream; returns the bytes written (0 on failure).
pub(crate) fn write(vm: &mut Vm, r: &Resource, data: &[u8]) -> VmResult<usize> {
    let Some(cell) = res::cell_of(r).filter(|_| !r.is_closed()) else {
        return Ok(0);
    };
    Ok(res::with_stream(vm, &cell, |s| s.write(data).unwrap_or(0))?.unwrap_or(0))
}

/// Reads up to `buf.len()` bytes from the stream.
pub(crate) fn read(vm: &mut Vm, r: &Resource, buf: &mut [u8]) -> VmResult<usize> {
    let Some(cell) = res::cell_of(r).filter(|_| !r.is_closed()) else {
        return Ok(0);
    };
    Ok(res::with_stream(vm, &cell, |s| s.read(buf).unwrap_or(0))?.unwrap_or(0))
}

/// Flushes the stream.
pub(crate) fn flush(vm: &mut Vm, r: &Resource) -> VmResult<()> {
    if let Some(cell) = res::cell_of(r).filter(|_| !r.is_closed()) {
        res::with_stream(vm, &cell, php_streams::stream::Stream::flush)?;
    }
    Ok(())
}

/// What `curl_setopt()` needs to know about a stream argument.
pub(crate) struct StreamInfo {
    /// The open mode (`what->mode`).
    pub(crate) mode: String,
    /// The descriptor of a stream that has one, for `CURLOPT_STDERR`.
    pub(crate) fd: Option<i32>,
}

/// `php_stream_cast(PHP_STREAM_AS_STDIO, REPORT_ERRORS)`. With
/// `fopencookie()` every stream can be a `FILE*` (a plain file is its own,
/// any other one gets a `FILE` whose functions call the stream), so this
/// only fails for a stream that is busy. `want_fd` asks for the descriptor
/// libcurl's verbose log is written to; without it (a stream this
/// extension reads and writes itself) the cast is only the buffer
/// synchronisation `_php_stream_cast()` starts with.
pub(crate) fn cast_stdio(vm: &mut Vm, r: &Resource, want_fd: bool) -> VmResult<Option<StreamInfo>> {
    let Some(cell) = res::cell_of(r) else {
        return Ok(None);
    };
    res::with_stream(vm, &cell, |s| {
        let mode = s.mode.clone();
        // The descriptor of a plain file's own `FILE`.
        let mut fd = s.cast(CastAs::Stdio, true);
        if !want_fd {
            fd = None;
        } else if fd.is_none() && s.ops_label() == "TEMP" && !s.is_filtered() {
            // A `php://temp` stream moves to its temporary file: unlike a
            // `FILE` that calls the stream, a descriptor also takes what
            // libcurl prints while a handle is freed.
            fd = s.cast(CastAs::Fd, false);
            if fd.is_none() {
                fd = s.cast(CastAs::Fd, true);
            }
        }
        StreamInfo { mode, fd }
    })
}
