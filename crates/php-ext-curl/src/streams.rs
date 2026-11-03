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
        res::with_stream(vm, &cell, |s| s.flush())?;
    }
    Ok(())
}

/// What `curl_setopt()` needs to know about a stream argument.
pub(crate) struct StreamInfo {
    /// The open mode (`what->mode`).
    pub(crate) mode: String,
    /// The descriptor (when castable), for `CURLOPT_STDERR`.
    pub(crate) fd: Option<i32>,
}

/// `php_stream_cast(PHP_STREAM_AS_STDIO, REPORT_ERRORS)`: `None` when the
/// stream cannot be represented as a `FILE*` (the warning is emitted).
pub(crate) fn cast_stdio(vm: &mut Vm, r: &Resource) -> VmResult<Option<StreamInfo>> {
    let Some(cell) = res::cell_of(r) else {
        return Ok(None);
    };
    let out = res::with_stream(vm, &cell, |s| {
        let mode = s.mode.clone();
        s.cast(CastAs::Stdio, true).map(|fd| StreamInfo { mode, fd: Some(fd) })
    })?;
    Ok(out.flatten())
}
