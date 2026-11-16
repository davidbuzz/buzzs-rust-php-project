// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Reading image files and writing encoded images through the stream
//! layer (`_php_image_create_from()`, `create_output_context()` and
//! `create_stream_context()` of gd.c).

use std::rc::Rc;

use php_ext_streams::{bridge, res};
use php_runtime::Value;
use php_streams::stream::Stream;
use php_streams::wrapper::REPORT_ERRORS;
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

/// What `_php_image_create_from()` gets out of the opened stream.
pub(crate) enum Opened {
    /// `php_stream_open_wrapper()` failed (it already warned).
    Failed,
    /// A plain file (`PHP_STREAM_IS_STDIO`): its contents.
    File(Vec<u8>),
    /// Any other stream: its contents (`php_stream_copy_to_mem()`).
    Other(Vec<u8>),
}

/// `php_stream_open_wrapper()`'s "Path cannot be empty" `ValueError`.
pub(crate) fn check_not_empty(vm: &mut Vm, path: &[u8]) -> VmResult<()> {
    if path.is_empty() {
        return Err(vm.throw("ValueError", php_streams::open::EMPTY_PATH_ERROR));
    }
    Ok(())
}

/// Opens `path` read-only and reads it completely.
pub(crate) fn read_path(vm: &mut Vm, path: &[u8]) -> VmResult<Opened> {
    check_not_empty(vm, path)?;
    let out = bridge::run(vm, || {
        let mut opened = None;
        let mut s = php_streams::open::open_wrapper(path, "rb", REPORT_ERRORS, &mut opened, None)?;
        let stdio = s.ops_label() == "STDIO";
        let data = s.copy_to_mem(None).unwrap_or_default();
        s.close();
        Some(if stdio {
            Opened::File(data)
        } else {
            Opened::Other(data)
        })
    })?;
    Ok(out.unwrap_or(Opened::Failed))
}

/// Where an encoded image goes.
pub(crate) enum Sink {
    /// `php_write()` (no `$file`, or `null`).
    Output,
    /// A stream resource passed by the caller (left open).
    Borrowed(Rc<res::StreamCell>),
    /// A stream opened from a path (closed when done).
    Owned(Box<Stream>),
}

/// `create_output_context(to_zval, arg_num)` for argument `n` (0-based):
/// `Ok(None)` when PHP returns `false` (the stream could not be opened).
pub(crate) fn output_sink(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Option<Sink>> {
    let v = a.at(n);
    match &v {
        Value::Null => Ok(Some(Sink::Output)),
        Value::Resource(r) => {
            let cell = res::stream_of_resource(vm, a, r)?;
            Ok(Some(Sink::Borrowed(cell)))
        }
        Value::Str(s) => {
            if s.as_bytes().contains(&0) {
                return Err(a.type_error(vm, n + 1, "must not contain null bytes"));
            }
            open_for_writing(vm, s.as_bytes())
        }
        other => Err(a.type_error(
            vm,
            n + 1,
            &format!(
                "must be a file name or a stream resource, {} given",
                other.value_name()
            ),
        )),
    }
}

/// `php_stream_open_wrapper(path, "wb", REPORT_ERRORS|IGNORE_PATH)`.
pub(crate) fn open_for_writing(vm: &mut Vm, path: &[u8]) -> VmResult<Option<Sink>> {
    check_not_empty(vm, path)?;
    let s = bridge::run(vm, || {
        let mut opened = None;
        php_streams::open::open_wrapper(path, "wb", REPORT_ERRORS, &mut opened, None)
    })?;
    Ok(s.map(|s| Sink::Owned(Box::new(s))))
}

impl Sink {
    /// Writes `data` and releases the sink (`ctx->gd_free(ctx)`).
    pub(crate) fn finish(self, vm: &mut Vm, data: &[u8]) -> VmResult<()> {
        match self {
            Sink::Output => {
                if !data.is_empty() {
                    vm.output(data);
                }
                Ok(())
            }
            Sink::Borrowed(cell) => {
                if !data.is_empty() {
                    res::with_stream(vm, &cell, |s| {
                        let _ = s.write(data);
                    })?;
                }
                Ok(())
            }
            Sink::Owned(mut s) => bridge::run(vm, move || {
                if !data.is_empty() {
                    let _ = s.write(data);
                }
                s.close();
            }),
        }
    }

    /// Releases the sink without writing (an argument error after the
    /// context was created).
    pub(crate) fn discard(self, vm: &mut Vm) -> VmResult<()> {
        self.finish(vm, &[])
    }
}
