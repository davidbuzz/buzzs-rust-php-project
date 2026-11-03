//! The internal pointer of objects (`reset($obj)`, `next($obj)`, ...).
//!
//! PHP keeps `nInternalPointer` in the object's property table, so moving
//! it persists between calls. The VM materializes property tables on
//! demand, so the position is remembered here per object, as the key of
//! the current property (or "past the end"), and re-applied to a fresh
//! `(array)`-style property table on every call.

use crate::bridge::rt;
use php_runtime::{ArrayKey, HashTable, Obj, PhpObject, PropertyPurpose, RuntimeContext, Value};
use php_vm::ext::VmResult;
use php_vm::Vm;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// The remembered pointer of one object.
#[derive(Debug, Clone)]
enum Cursor {
    /// At the property with this key.
    At(ArrayKey),
    /// Past the end (or before the start): no current element.
    Invalid,
}

thread_local! {
    static CURSORS: RefCell<Vec<(Weak<dyn PhpObject>, Cursor)>> = const { RefCell::new(Vec::new()) };
}

fn load(obj: &Obj) -> Option<Cursor> {
    CURSORS.with(|c| {
        c.borrow()
            .iter()
            .find(|(w, _)| w.upgrade().is_some_and(|rc| Rc::ptr_eq(&rc, obj.rc())))
            .map(|(_, cur)| cur.clone())
    })
}

fn store(obj: &Obj, cursor: Cursor) {
    CURSORS.with(|c| {
        let mut c = c.borrow_mut();
        c.retain(|(w, _)| w.strong_count() > 0);
        match c
            .iter_mut()
            .find(|(w, _)| w.upgrade().is_some_and(|rc| Rc::ptr_eq(&rc, obj.rc())))
        {
            Some(entry) => entry.1 = cursor,
            None => c.push((Rc::downgrade(obj.rc()), cursor)),
        }
    });
}

/// An internal-pointer operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `current()` / `pos()`
    Current,
    /// `key()`
    Key,
    /// `reset()`
    Reset,
    /// `end()`
    End,
    /// `next()`
    Next,
    /// `prev()`
    Prev,
}

/// Runs `op` on an object's property table (after the "Calling X() on an
/// object is deprecated" deprecation) and remembers the new position.
pub fn run(vm: &mut Vm, func: &str, obj: &Obj, op: Op) -> VmResult<Value> {
    vm.php_deprecated(&format!("Calling {func}() on an object is deprecated"))?;
    let r = vm.object_properties(obj, PropertyPurpose::ArrayCast);
    let table = rt(vm, r)?.unwrap_or_default();
    let mut ht: HashTable = (*table).clone();
    match load(obj) {
        None => ht.reset(),
        Some(Cursor::At(k)) => match ht.position_of(&k) {
            Some(p) => ht.set_internal_pointer(p),
            None => ht.set_internal_pointer(u32::MAX),
        },
        Some(Cursor::Invalid) => ht.set_internal_pointer(u32::MAX),
    }
    match op {
        Op::Current | Op::Key => {}
        Op::Reset => ht.reset(),
        Op::End => ht.end(),
        Op::Next => ht.move_next(),
        Op::Prev => ht.move_prev(),
    }
    let cursor = ht
        .current_key()
        .map_or(Cursor::Invalid, |k| Cursor::At(k.clone()));
    store(obj, cursor);
    Ok(match op {
        Op::Key => ht.current_key().map_or(Value::Null, ArrayKey::to_value),
        _ => ht.current().map_or(Value::Bool(false), Value::deref),
    })
}
