//! Glue between the VM and the `php-streams` library.
//!
//! The library reports warnings through a thread-local diagnostics queue and
//! calls back into PHP (user-space wrappers and filters, `php://output`)
//! through traits that have no access to the VM. This module provides:
//!
//! * [`run`]: executes a library call with the VM reachable from those
//!   callbacks ([`with_vm`]), then emits the queued diagnostics as
//!   `func(): message` warnings and re-raises an exception thrown by a
//!   callback;
//! * synchronisation of the INI settings the library consults;
//! * conversion between VM values and the library's [`SValue`].

use std::cell::{Cell, RefCell};

use php_runtime::{ArrayKey, Value, ZArray, ZStr};
use php_streams::diag::{self, Severity};
use php_streams::state;
use php_streams::value::{Key as SKey, Value as SValue};
use php_vm::ext::{Throw, VmResult};
use php_vm::Vm;

thread_local! {
    /// The VM lent to library callbacks for the duration of [`enter`].
    static VM_PTR: Cell<*mut Vm> = const { Cell::new(std::ptr::null_mut()) };
    /// The first exception (or exit) raised inside a callback, re-raised
    /// when control returns to the native function.
    static PENDING: RefCell<Option<Throw>> = const { RefCell::new(None) };
}

/// Restores the previous VM pointer when a scope ends (also on unwind).
struct Restore(*mut Vm);

impl Drop for Restore {
    fn drop(&mut self) {
        VM_PTR.with(|p| p.set(self.0));
    }
}

/// Runs `f` with `vm` reachable through [`with_vm`].
///
/// The `&mut Vm` borrow is held for the whole call, so the VM is only ever
/// accessed through the pointer while the caller cannot touch it.
pub fn enter<R>(vm: &mut Vm, f: impl FnOnce() -> R) -> R {
    sync_settings(vm);
    let ptr: *mut Vm = vm;
    let prev = VM_PTR.with(|p| p.replace(ptr));
    let _restore = Restore(prev);
    f()
}

/// Calls `f` with the VM lent by the innermost [`enter`], or returns `None`
/// when no native function is running (e.g. a resource destroyed by the
/// VM's garbage handling).
pub fn with_vm<R>(f: impl FnOnce(&mut Vm) -> R) -> Option<R> {
    let ptr = VM_PTR.with(|p| p.replace(std::ptr::null_mut()));
    if ptr.is_null() {
        return None;
    }
    let _restore = Restore(ptr);
    // SAFETY: `ptr` was derived from the `&mut Vm` passed to `enter`, whose
    // frame is still active (the pointer is reset when it returns). That
    // borrow is not used while `enter`'s closure runs, and the slot is
    // nulled for the duration of `f`, so this is the only live mutable
    // access path to the VM; nested `enter` calls reborrow from it.
    let vm = unsafe { &mut *ptr };
    Some(f(vm))
}

/// Whether a VM is currently reachable.
pub fn has_vm() -> bool {
    VM_PTR.with(|p| !p.get().is_null())
}

/// Records a throw raised inside a callback (the first one wins).
pub fn set_pending(t: Throw) {
    PENDING.with(|p| {
        let mut p = p.borrow_mut();
        if p.is_none() {
            *p = Some(t);
        }
    });
}

/// Whether a callback threw.
pub fn has_pending() -> bool {
    PENDING.with(|p| p.borrow().is_some())
}

/// Takes the pending throw.
pub fn take_pending() -> Option<Throw> {
    PENDING.with(|p| p.borrow_mut().take())
}

/// Emits the queued library diagnostics through the VM.
pub fn flush_diags(vm: &mut Vm) {
    for d in diag::take() {
        // php_stream_display_wrapper_errors() stays silent when an
        // exception has already been thrown.
        if d.code == php_streams::diag::StreamErrorCode::OpenFailed && has_pending() {
            continue;
        }
        let text = d.text();
        let r = match d.severity {
            Severity::Warning => vm.php_warning(&text),
            Severity::Notice => vm.php_notice(&text),
            Severity::Deprecated => vm.php_deprecated(&text),
            Severity::Error => vm.raise(1, &text),
        };
        if let Err(t) = r {
            set_pending(t);
        }
    }
}

/// Runs a library call (see [`enter`]), emits its diagnostics and
/// propagates an exception raised by a user callback.
pub fn run<R>(vm: &mut Vm, f: impl FnOnce() -> R) -> VmResult<R> {
    let outer = take_pending();
    let r = enter(vm, f);
    flush_diags(vm);
    let inner = take_pending();
    if let Some(t) = outer {
        set_pending(t);
    }
    match inner {
        Some(t) => Err(t),
        None => Ok(r),
    }
}

/// Copies the INI settings consulted by the streams layer.
fn sync_settings(vm: &Vm) {
    let get = |n: &str| vm.ini_string(n).unwrap_or_default();
    let include_path = get("include_path");
    let open_basedir = get("open_basedir");
    let sys_temp_dir = get("sys_temp_dir");
    let upload_tmp_dir = get("upload_tmp_dir");
    let allow_url_fopen = vm.ini_bool("allow_url_fopen");
    let allow_url_include = vm.ini_bool("allow_url_include");
    let auto_detect_line_endings = vm.ini_bool("auto_detect_line_endings");
    let default_socket_timeout = vm.ini_long("default_socket_timeout");
    let html_errors = vm.ini_bool("html_errors");
    state::update_settings(|s| {
        s.include_path = include_path;
        s.open_basedir = open_basedir;
        s.sys_temp_dir = sys_temp_dir;
        s.upload_tmp_dir = upload_tmp_dir;
        s.allow_url_fopen = allow_url_fopen;
        s.allow_url_include = allow_url_include;
        s.auto_detect_line_endings = auto_detect_line_endings;
        s.default_socket_timeout = default_socket_timeout;
        s.html_errors = html_errors;
        s.sapi_is_cli = true;
    });
}

/// Installs the library hooks (output, executing file name).
pub fn install_hooks() {
    state::set_hooks(state::Hooks {
        output: Some(Box::new(|data: &[u8]| {
            let data = data.to_vec();
            if with_vm(|vm| vm.output(&data)).is_none() {
                use std::io::Write;
                let _ = std::io::stdout().write_all(&data);
            }
        })),
        executed_filename: Some(Box::new(|| {
            with_vm(|vm| vm.current_location().0.as_bytes().to_vec())
        })),
        request_body: None,
    });
}

// ---------------------------------------------------------------------------
// Value conversion
// ---------------------------------------------------------------------------

/// Converts a VM value for the library.
pub fn to_lib(v: &Value) -> SValue {
    match v {
        Value::Null => SValue::Null,
        Value::Bool(b) => SValue::Bool(*b),
        Value::Int(i) => SValue::Int(*i),
        Value::Float(f) => SValue::Float(*f),
        Value::Str(s) => SValue::Str(s.as_bytes().to_vec()),
        Value::Array(a) => SValue::Array(
            a.iter()
                .map(|(k, v)| {
                    let k = match k {
                        ArrayKey::Int(i) => SKey::Int(*i),
                        ArrayKey::Str(s) => SKey::Str(s.as_bytes().to_vec()),
                    };
                    (k, to_lib(v))
                })
                .collect(),
        ),
        Value::Object(o) => SValue::Object {
            class: o.class_name().to_string_lossy(),
            string_value: None,
        },
        Value::Resource(r) => match crate::res::stream_id_of(r) {
            Some(id) => SValue::Stream(id),
            None => SValue::Resource(r.handle()),
        },
        Value::Reference(r) => to_lib(&r.get()),
    }
}

/// Converts a library value for the VM.
pub fn from_lib(v: &SValue) -> Value {
    match v {
        SValue::Null | SValue::Object { .. } => Value::Null,
        SValue::Bool(b) => Value::Bool(*b),
        SValue::Int(i) => Value::Int(*i),
        SValue::Float(f) => Value::Float(*f),
        SValue::Str(s) => Value::Str(ZStr::from(s.as_slice())),
        SValue::Array(items) => {
            let mut a = ZArray::new();
            for (k, v) in items {
                match k {
                    SKey::Int(i) => a.make_mut().insert(*i, from_lib(v)),
                    SKey::Str(s) => a.make_mut().insert(ArrayKey::Str(ZStr::from(s.as_slice())), from_lib(v)),
                };
            }
            Value::Array(a)
        }
        SValue::Stream(id) => crate::res::stream_value_by_id(*id).unwrap_or(Value::Null),
        SValue::Resource(_) => Value::Null,
    }
}

/// A PHP string value from bytes.
pub fn bytes(b: impl AsRef<[u8]>) -> Value {
    Value::Str(ZStr::from(b.as_ref()))
}

/// The type name PHP prints in "must be of type X, Y given" messages.
pub fn given_type(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Int(_) => "int".into(),
        Value::Float(_) => "float".into(),
        Value::Str(_) => "string".into(),
        Value::Array(_) => "array".into(),
        Value::Object(o) => o.class_name().to_string_lossy(),
        Value::Resource(_) => "resource".into(),
        Value::Reference(r) => given_type(&r.get()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_roundtrip() {
        let mut a = ZArray::new();
        a.make_mut().insert("k", Value::Int(3));
        a.make_mut().insert(5, bytes("x"));
        let v = Value::Array(a);
        let back = from_lib(&to_lib(&v));
        let arr = back.as_array().expect("array");
        assert_eq!(arr.get_str(b"k"), Some(&Value::Int(3)));
        assert_eq!(arr.get_int(5).and_then(Value::as_str).map(ZStr::as_bytes), Some(&b"x"[..]));
    }

    #[test]
    fn no_vm_outside_enter() {
        assert!(!has_vm());
        assert!(with_vm(|_| ()).is_none());
    }

    #[test]
    fn type_names() {
        assert_eq!(given_type(&Value::Null), "null");
        assert_eq!(given_type(&Value::Bool(false)), "false");
        assert_eq!(given_type(&bytes("a")), "string");
    }
}
