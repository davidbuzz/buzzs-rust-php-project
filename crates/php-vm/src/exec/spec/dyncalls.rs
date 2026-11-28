// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Calls by a name known only at run time, started on a hit of the call
//! site's run-time cache: `$obj->$name()` (`ZEND_INIT_METHOD_CALL` with a
//! variable method name, ref: `Zend/zend_vm_def.h`).
//!
//! `Vm::op_init_method_call` fills the site's slot of the method cache
//! (`OpArray::method_cache`) for a name that is not a literal with the
//! name itself (`name_lit: None`); a later call of the same method for an
//! object of the same class from the same scope finds it there. That hit
//! is taken here on the running frame alone, the call pushed as the
//! generic op pushes it; anything else declines, having changed nothing,
//! and the generic op runs.

use crate::object;
use crate::opcode::Operand;
use crate::vm::{CvSlot, Frame, PendingCall};
use php_runtime::Value;
use std::rc::Rc;

/// `ZEND_INIT_METHOD_CALL` of `obj->name()` (`obj` a compiled variable, a
/// temporary or `$this`, [`Operand::Unused`]; `name` a compiled variable
/// or a temporary holding a string) at op `slot` of the running frame `f`,
/// on a hit of the site's method cache entry for the object's class, the
/// calling scope and that name (any case): the pending call pushed with
/// the cached method, its declaring scope and the object's class as the
/// called scope, `$this` unless the method is static; a temporary object
/// moves into the call (or is released after the push for a static
/// method), a temporary name is released (`FREE_OP2`). `false`, having
/// changed nothing, otherwise: a value that is not an object or a string
/// (the generic op's `Error`s), `__invoke` (a closure's own call), a
/// class with a native `get_method` handler, a variable of a frame with
/// an attached symbol table, a miss (the generic op resolves the method
/// and fills the entry).
#[inline(never)]
pub(in crate::exec) fn init_dyn_method_call_in(
    f: &mut Frame,
    obj: Operand,
    name: Operand,
    slot: usize,
) -> bool {
    let hit = {
        let o = match obj {
            Operand::Cv(cv) if !f.symtab_attached => {
                match f.slots.cvs().get(cv as usize).and_then(CvSlot::get) {
                    Some(Value::Object(o)) => o,
                    _ => return false,
                }
            }
            Operand::Tmp(t) => match f.slots.tmps().get(t as usize) {
                Some(Value::Object(o)) => o,
                _ => return false,
            },
            Operand::Unused => match f.this.as_ref() {
                Some(o) => o,
                None => return false,
            },
            _ => return false,
        };
        let n = match name {
            Operand::Cv(cv) if !f.symtab_attached => {
                match f.slots.cvs().get(cv as usize).and_then(CvSlot::get) {
                    Some(Value::Str(s)) => s,
                    _ => return false,
                }
            }
            Operand::Tmp(t) => match f.slots.tmps().get(t as usize) {
                Some(Value::Str(s)) => s,
                _ => return false,
            },
            _ => return false,
        };
        if n.eq_ignore_ascii_case(b"__invoke") {
            return false;
        }
        let cls = object::class_of(o);
        if cls.hooks.as_ref().is_some_and(|h| h.get_method.is_some()) {
            return false;
        }
        let cls_addr = Rc::as_ptr(cls) as usize;
        let scope_addr = f.scope.as_ref().map_or(0, |s| Rc::as_ptr(s) as usize);
        let Ok(cache) = f.code.method_cache.try_borrow() else {
            return false;
        };
        let Some(Some(e)) = cache.get(slot) else {
            return false;
        };
        if e.class_addr != cls_addr
            || e.scope_addr != scope_addr
            || e.name_lit.is_some()
            || !e.name.eq_ignore_ascii_case(n.as_bytes())
        {
            return false;
        }
        let Some(func) = e.func.get() else {
            return false;
        };
        let scope = match &e.method_scope {
            Some(w) => match w.upgrade() {
                Some(s) => Some(s),
                None => return false,
            },
            None => None,
        };
        (func, scope, e.is_static, Rc::clone(cls), o.clone())
    };
    let (func, scope, is_static, called, o) = hit;
    // The operands as the generic op reads them (`FREE_OP1` / `FREE_OP2`
    // of temporaries): the call holds its own handle of the object.
    for op in [obj, name] {
        if let Operand::Tmp(t) = op {
            if let Some(s) = f.slots.tmps_mut().get_mut(t as usize) {
                drop(std::mem::take(s));
            }
        }
    }
    f.calls.push(PendingCall {
        this: (!is_static).then_some(o),
        scope,
        called: Some(called),
        ..PendingCall::new(func)
    });
    true
}
