//! Reading files and streams for `hash_file()`, `hash_hmac_file()`,
//! `hash_update_file()`, `hash_update_stream()`, `md5_file()` and
//! `sha1_file()`.
//!
//! Paths are opened with [`php_streams::open::open_wrapper`]
//! (`php_stream_open_wrapper_ex(path, "rb", REPORT_ERRORS, ...)`) inside
//! [`php_ext_streams::bridge::run`], so every registered wrapper works
//! (including user-space ones, whose callbacks need the VM) and the
//! streams layer's warnings come out as `func(): message`. Stream and
//! context resources are the ones `php-ext-streams` creates.

use php_ext_streams::{bridge, res};
use php_streams::context::ContextRef;
use php_streams::wrapper::REPORT_ERRORS;
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

/// The chunk size of the read loops (`char buf[1024]` in `hash.c`).
const CHUNK: usize = 1024;

/// Checks a path argument (`Z_PARAM_PATH`): no NUL bytes.
pub(crate) fn check_path(vm: &mut Vm, a: &Args, n: usize, path: &[u8]) -> VmResult<()> {
    if path.contains(&0) {
        return Err(a.value_error(vm, n, "must not contain any null bytes"));
    }
    Ok(())
}

/// `FG(default_context)`.
pub(crate) fn default_context() -> Option<ContextRef> {
    res::ctx_cell_of(&res::default_context()).map(|c| c.ctx.clone())
}

/// The `$stream_context` argument `n` (`r!`): `None` for `null`.
pub(crate) fn context_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Option<ContextRef>> {
    res::context_arg(vm, a, n, true)
}

/// How a [`read_path`] loop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReadEnd {
    /// `php_stream_read()` returned a negative count.
    pub(crate) error: bool,
    /// `php_stream_eof()` after the loop.
    pub(crate) eof: bool,
}

/// Opens `path` read-only and feeds its contents to `sink` in 1 KiB chunks.
///
/// Returns `Ok(None)` when the file could not be opened (the warning has
/// been emitted), otherwise `Ok(Some(ReadEnd))` describing how reading
/// stopped.
pub(crate) fn read_path(
    vm: &mut Vm,
    path: &[u8],
    context: Option<&ContextRef>,
    mut sink: impl FnMut(&[u8]),
) -> VmResult<Option<ReadEnd>> {
    if path.is_empty() {
        return Err(vm.throw("ValueError", php_streams::open::EMPTY_PATH_ERROR));
    }
    bridge::run(vm, || {
        let mut opened = None;
        let mut stream =
            php_streams::open::open_wrapper(path, "rb", REPORT_ERRORS, &mut opened, context)?;
        let mut buf = vec![0u8; CHUNK];
        let error = loop {
            match stream.read(&mut buf) {
                Ok(0) => break false,
                Ok(n) => sink(&buf[..n]),
                Err(_) => break true,
            }
        };
        let eof = stream.eof();
        stream.close();
        Some(ReadEnd { error, eof })
    })
}

/// `hash_update_stream()`'s loop: reads up to `length` bytes (negative:
/// until EOF) from the stream resource argument `n` in 1 KiB requests.
pub(crate) fn read_stream(vm: &mut Vm, a: &Args, n: usize, length: i64) -> VmResult<Vec<u8>> {
    let cell = res::stream_arg(vm, a, n)?;
    let data = res::with_stream(vm, &cell, |s| {
        let mut out = Vec::new();
        let mut buf = vec![0u8; CHUNK];
        let mut left = length;
        while left != 0 {
            let want = if left > 0 {
                usize::try_from(left).map_or(CHUNK, |l| l.min(CHUNK))
            } else {
                CHUNK
            };
            match s.read(&mut buf[..want]) {
                Ok(got) if got > 0 => {
                    out.extend_from_slice(&buf[..got]);
                    if left > 0 {
                        left -= i64::try_from(got).unwrap_or(left);
                    }
                }
                _ => break,
            }
        }
        out
    })?;
    Ok(data.unwrap_or_default())
}
