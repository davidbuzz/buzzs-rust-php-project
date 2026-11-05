//! Per-request state (`ZT_G(...)` globals reset in `RINIT`/`RSHUTDOWN`).

use php_runtime::{Obj, PhpObject, Value, ZArray};
use php_vm::Vm;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

/// One entry of the global weak map (`ZT_G(global_weakmap)`).
struct WeakEntry {
    key: Weak<dyn PhpObject>,
    /// The key the dump uses (the object address, like
    /// `zend_object_to_weakref_key`).
    id: i64,
    value: Value,
}

#[derive(Default)]
struct State {
    weakmap: Vec<WeakEntry>,
    /// Values kept alive by `zend_leak_variable()`.
    leaked: Vec<Value>,
    /// Temporary files of uploads parsed by `zend_test_handle_post_data()`
    /// (`SG(rfc1867_uploaded_files)`).
    uploaded: Vec<PathBuf>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// `PHP_RINIT(zend_test)`.
pub(crate) fn request_startup(_vm: &mut Vm) {
    STATE.with(|s| *s.borrow_mut() = State::default());
}

/// `PHP_RSHUTDOWN(zend_test)`: drops the weak map values and, as the
/// SAPI deactivation does, deletes the uploads' temporary files.
pub(crate) fn request_shutdown(_vm: &mut Vm) {
    let old = STATE.with(|s| std::mem::take(&mut *s.borrow_mut()));
    php_cli_server::multipart::cleanup(&old.uploaded);
    drop(old);
}

/// Records the temporary files of the uploads just parsed.
pub(crate) fn set_uploaded(files: Vec<PathBuf>) {
    STATE.with(|s| s.borrow_mut().uploaded = files);
}

/// Takes the recorded upload files (to delete them).
pub(crate) fn take_uploaded() -> Vec<PathBuf> {
    STATE.with(|s| std::mem::take(&mut s.borrow_mut().uploaded))
}

fn weak_id(o: &Obj) -> i64 {
    i64::try_from(o.addr() >> 3).unwrap_or_default()
}

/// Drops entries whose object is gone (`zend_weakrefs_notify`).
fn prune(st: &mut State) {
    st.weakmap.retain(|e| e.key.strong_count() > 0);
}

/// `zend_weakrefs_hash_add`: `false` if the object is already a key.
pub(crate) fn weakmap_attach(o: &Obj, v: Value) -> bool {
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        prune(&mut st);
        let id = weak_id(o);
        if st.weakmap.iter().any(|e| e.id == id) {
            return false;
        }
        st.weakmap.push(WeakEntry {
            key: Rc::downgrade(o.rc()),
            id,
            value: v,
        });
        true
    })
}

/// `zend_weakrefs_hash_del`.
pub(crate) fn weakmap_remove(o: &Obj) -> bool {
    let removed = STATE.with(|s| {
        let mut st = s.borrow_mut();
        prune(&mut st);
        let id = weak_id(o);
        let pos = st.weakmap.iter().position(|e| e.id == id)?;
        Some(st.weakmap.remove(pos))
    });
    removed.is_some()
}

/// A copy of the weak map keyed by object id.
pub(crate) fn weakmap_dump() -> ZArray {
    let entries: Vec<(i64, Value)> = STATE.with(|s| {
        let mut st = s.borrow_mut();
        prune(&mut st);
        st.weakmap.iter().map(|e| (e.id, e.value.clone())).collect()
    });
    ZArray::from_pairs(entries)
}

/// `zend_leak_variable()`: keeps an extra reference until the end of the
/// request.
pub(crate) fn leak(v: Value) {
    STATE.with(|s| s.borrow_mut().leaked.push(v));
}
