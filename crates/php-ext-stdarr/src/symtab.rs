//! The calling function's variables as php-stdarr's [`SymbolTable`]
//! (`zend_rebuild_symbol_table` for `extract()` / `compact()`).

use php_runtime::{RtResult, Value, ZStr};
use php_stdarr::symbols::{SymbolTable, VarSlot};
use php_vm::vm::{FrameKind, VarStore};
use php_vm::Vm;
use std::cell::RefCell;
use std::rc::Rc;

/// The variable table of the frame that called the running native
/// function.
#[derive(Debug)]
pub struct CallerSymbols {
    vars: Rc<RefCell<VarStore>>,
}

impl CallerSymbols {
    /// The caller's symbol table; `None` when the caller is not PHP code
    /// (a dynamic call through `call_user_func()` and friends).
    pub fn of_caller(vm: &Vm) -> Option<Self> {
        let n = vm.frames.len();
        let caller = vm.frames.get(n.checked_sub(2)?)?;
        if caller.kind == FrameKind::Builtin {
            return None;
        }
        Some(CallerSymbols {
            vars: caller.vars.clone(),
        })
    }

    /// Assignments made by `extract()` must honor typed-reference sources.
    pub fn checked(self, vm: &mut Vm) -> CheckedSymbols<'_> {
        CheckedSymbols { inner: self, vm }
    }
}

#[derive(Debug)]
pub struct CheckedSymbols<'a> {
    inner: CallerSymbols,
    vm: &'a mut Vm,
}

impl SymbolTable for CheckedSymbols<'_> {
    fn lookup(&mut self, name: &ZStr) -> VarSlot {
        self.inner.lookup(name)
    }

    fn assign(&mut self, name: &ZStr, value: Value) -> RtResult<()> {
        if let VarSlot::Defined(Value::Reference(r)) = self.inner.lookup(name) {
            self.vm
                .assign_ref_ex(&r, value, Some(false))
                .map(|_| ())
                .map_err(|t| self.vm.runtime_error_from(t))
        } else {
            self.inner.assign(name, value)
        }
    }

    fn set(&mut self, name: &ZStr, value: Value) {
        self.inner.set(name, value);
    }
}

impl SymbolTable for CallerSymbols {
    fn lookup(&mut self, name: &ZStr) -> VarSlot {
        let mut st = self.vars.borrow_mut();
        match st.find(name.as_bytes()) {
            None => VarSlot::Missing,
            Some(i) => match st.slots.get(i as usize).cloned().flatten() {
                None => VarSlot::Undefined,
                Some(v) => VarSlot::Defined(v),
            },
        }
    }

    fn assign(&mut self, name: &ZStr, value: Value) -> RtResult<()> {
        let mut st = self.vars.borrow_mut();
        let i = st.find_or_add(name.as_bytes()) as usize;
        match st.slots.get_mut(i) {
            Some(Some(Value::Reference(r))) => {
                let r = r.clone();
                drop(st);
                r.set(value);
            }
            Some(slot) => *slot = Some(value),
            None => {}
        }
        Ok(())
    }

    fn set(&mut self, name: &ZStr, value: Value) {
        let mut st = self.vars.borrow_mut();
        let i = st.find_or_add(name.as_bytes()) as usize;
        if let Some(slot) = st.slots.get_mut(i) {
            *slot = Some(value);
        }
    }
}
