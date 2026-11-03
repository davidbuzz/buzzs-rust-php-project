//! Recovering the receiver of a static method called on an instance.
//!
//! PHP lets `XMLReader::open()`/`XML()` (declared `static`) be called as
//! `$reader->open(...)`: `xmlreader_get_method` swaps in non-static copies
//! so that the call operates on the object and returns a `bool`. The VM
//! drops `$this` for static methods, so the receiver is recovered from the
//! calling frame: the `InitMethodCall` instruction matching the `DoCall`
//! being executed names the object operand.

use php_runtime::{Obj, Value};
use php_vm::opcode::{Op, Operand};
use php_vm::vm::{Frame, FrameKind};
use php_vm::Vm;

/// The object `method` is being called on with `->`, if the call is an
/// instance call (`None` for `Class::method()` and for receivers that
/// cannot be recovered).
pub fn instance(vm: &Vm, method: &str) -> Option<Obj> {
    let f = vm
        .frames
        .iter()
        .rev()
        .find(|f| f.kind != FrameKind::Builtin)?;
    let ops = &f.code.ops;
    // The interpreter advances `ip` before executing an instruction.
    let mut i = f.ip.checked_sub(1)?;
    if !matches!(ops.get(i)?, Op::DoCall { .. }) {
        return None;
    }
    let mut depth = 0usize;
    loop {
        match &ops[i] {
            Op::DoCall { .. } | Op::CallableConvert { .. } => depth += 1,
            Op::InitMethodCall { obj, name } => {
                depth -= 1;
                if depth == 0 {
                    let named = match name {
                        Operand::Lit(n) => match f.code.literals.get(*n as usize) {
                            Some(Value::Str(s)) => {
                                s.as_bytes().eq_ignore_ascii_case(method.as_bytes())
                            }
                            _ => false,
                        },
                        _ => true,
                    };
                    if !named {
                        return None;
                    }
                    return operand_object(f, ops, i, *obj);
                }
            }
            Op::InitFCall { .. }
            | Op::InitDynCall { .. }
            | Op::InitStaticCall { .. }
            | Op::InitParentHook { .. }
            | Op::New { .. } => {
                depth -= 1;
                if depth == 0 {
                    return None;
                }
            }
            _ => {}
        }
        i = i.checked_sub(1)?;
    }
}

fn as_object(v: &Value) -> Option<Obj> {
    match v.deref() {
        Value::Object(o) => Some(o),
        _ => None,
    }
}

fn operand_object(f: &Frame, ops: &[Op], at: usize, obj: Operand) -> Option<Obj> {
    match obj {
        Operand::Cv(cv) => {
            let idx = f.cv_idx(cv);
            let vars = f.vars.borrow();
            vars.slots
                .get(idx)
                .and_then(|s| s.as_ref().and_then(as_object))
        }
        Operand::Unused => f.this.clone(),
        Operand::Tmp(t) | Operand::TmpKeep(t) => {
            // `$this->open()`: the temporary was filled by `FetchThis`
            // (and has been consumed since).
            let fetched_this = ops[..at]
                .iter()
                .rev()
                .take(4)
                .any(|op| matches!(op, Op::FetchThis { dst } if *dst == t));
            if fetched_this {
                return f.this.clone();
            }
            f.tmps.get(t as usize).and_then(as_object)
        }
        Operand::Lit(_) => None,
    }
}
