//! Stream resources for the values PDO and SQLite3 hand to PHP code
//! (`PDO::PARAM_LOB` columns, `openBlob()`) and read from it (LOB
//! parameters).
//!
//! The VM's stream resources are owned by the `php-ext-streams` binding,
//! which does not publish its resource type yet; until it does, streams
//! created here are wrapped in a local payload type ([`StreamRes`]) that
//! [`read_stream`] understands.

use php_runtime::{Resource, Value};
use php_streams::stream::Stream;
use php_vm::{Vm, VmResult};
use std::cell::RefCell;

/// The payload of a stream resource created by this crate.
#[derive(Debug)]
pub struct StreamRes(pub RefCell<Option<Stream>>);

/// Wraps a library stream in a new `stream` resource.
pub fn new_stream(stream: Stream) -> Value {
    Value::Resource(Resource::new(
        "stream",
        Some(Box::new(StreamRes(RefCell::new(Some(stream))))),
    ))
}

/// `php_stream_from_zval_no_verify()` + `php_stream_copy_to_mem()`: the
/// remaining contents of a stream resource (`None` when the value is not a
/// stream this crate can read).
pub fn read_stream(vm: &mut Vm, value: &Value) -> Option<Vec<u8>> {
    let Value::Resource(r) = value.deref() else {
        return None;
    };
    let data = r.data();
    let res = data.as_ref()?.downcast_ref::<StreamRes>()?;
    let mut slot = res.0.try_borrow_mut().ok()?;
    let s = slot.as_mut()?;
    let out = s.copy_to_mem(None).unwrap_or_default();
    drop(slot);
    let _ = flush_diagnostics(vm);
    Some(out)
}

/// Emits the warnings the streams library queued (`func(): message`).
pub fn flush_diagnostics(vm: &mut Vm) -> VmResult<()> {
    use php_streams::diag::{take, Severity};
    let mut first = Ok(());
    for d in take() {
        let text = d.text();
        let r = match d.severity {
            Severity::Warning => vm.php_warning(&text),
            Severity::Notice => vm.php_notice(&text),
            Severity::Deprecated => vm.php_deprecated(&text),
            Severity::Error => vm.raise(1, &text),
        };
        if first.is_ok() {
            first = r;
        }
    }
    first
}
