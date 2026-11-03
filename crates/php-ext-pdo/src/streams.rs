//! Stream resources for the values PDO and SQLite3 hand to PHP code
//! (`PDO::PARAM_LOB` columns, `openBlob()`) and read from it (LOB
//! parameters), shared with `php-ext-streams` so that `fread()`,
//! `fseek()`, `stream_get_contents()` & co. work on them.

use php_runtime::Value;
use php_streams::stream::Stream;
use php_vm::{Vm, VmResult};

/// Wraps a library stream in a new `stream` resource.
pub fn new_stream(stream: Stream) -> Value {
    php_ext_streams::res::new_stream(stream, None)
}

/// `php_stream_from_zval_no_verify()` + `php_stream_copy_to_mem()`: the
/// remaining contents of a stream resource (`None` when the value is not
/// an open stream).
pub fn read_stream(vm: &mut Vm, value: &Value) -> Option<Vec<u8>> {
    let Value::Resource(r) = value.deref() else {
        return None;
    };
    let cell = php_ext_streams::res::cell_of(&r)?;
    if cell.is_closed() {
        return None;
    }
    php_ext_streams::res::with_stream(vm, &cell, |s| s.copy_to_mem(None).unwrap_or_default())
        .ok()
        .flatten()
}

/// Runs a `php-streams` library call with the VM reachable from its
/// callbacks (user wrappers), emitting its queued warnings.
pub fn run<R>(vm: &mut Vm, f: impl FnOnce() -> R) -> VmResult<R> {
    php_ext_streams::bridge::run(vm, f)
}

/// Emits the warnings the streams library queued (`func(): message`).
pub fn flush_diagnostics(vm: &mut Vm) -> VmResult<()> {
    php_ext_streams::bridge::flush_diags(vm);
    Ok(())
}
