// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The calling function's variables as php-stdarr's [`SymbolTable`]
//! (`zend_rebuild_symbol_table` for `extract()` / `compact()`).

use php_runtime::{RtResult, Value, ZStr};
use php_stdarr::symbols::{SymbolTable, VarSlot};
use php_vm::vm::{CvSlot, Frame, FrameKind, VarStore};
use php_vm::Vm;
use std::cell::RefCell;
use std::rc::Rc;

/// The variable table of the frame that called the running native
/// function.
///
/// php-src's rebuilt symbol table (`zend_rebuild_symbol_table`) points at
/// the frame's compiled-variable slots (`IS_INDIRECT`): building it costs
/// one entry a variable once, and a write through it lands in the slot.
/// This VM's tables hold copies, so attaching one copies every variable of
/// the frame in, and every write has to be copied back out. A function
/// whose table is not attached is therefore read and written slot by slot
/// (`direct`): a name is looked up among the function's compiled
/// variables, and only a name the function does not know (a variable
/// `extract()` creates) attaches the real table, from then on.
#[derive(Debug)]
pub struct CallerSymbols {
    vars: Rc<RefCell<VarStore>>,
    /// The caller's index in the frame stack.
    frame: usize,
    /// The caller is a function whose table is not attached: its compiled
    /// variables are its slots, and `vars` holds only the variables it
    /// does not compile (none, usually).
    direct: bool,
}

/// The index of compiled variable `name` of the frame.
fn cv_of(f: &Frame, name: &[u8]) -> Option<usize> {
    f.code.cv_names.iter().position(|n| n.as_bytes() == name)
}

impl CallerSymbols {
    /// The caller's symbol table; `None` when the caller is not PHP code
    /// (a dynamic call through `call_user_func()` and friends).
    /// A function whose table is not attached is accessed slot by slot;
    /// any other caller has its compiled variables copied into its
    /// (attached) symbol table first. After writing through it, call
    /// [`CallerSymbols::finish`].
    pub fn of_caller(vm: &mut Vm) -> Option<Self> {
        let idx = vm.frames.len().checked_sub(2)?;
        let f = vm.frames.get(idx)?;
        if f.kind == FrameKind::Builtin {
            return None;
        }
        if f.kind == FrameKind::Function && !f.symtab_attached {
            return Some(CallerSymbols {
                vars: f.vars.clone(),
                frame: idx,
                direct: true,
            });
        }
        let vars = vm.rebuild_symbol_table(idx)?;
        Some(CallerSymbols {
            vars,
            frame: idx,
            direct: false,
        })
    }

    /// Makes the writes made through this table visible to the caller's
    /// compiled variables (nothing to do for writes made to the slots).
    ///
    /// A handle made while the caller was accessed slot by slot does not
    /// know whether that access ended since (a name the function does not
    /// compile attaches its table, in the handle the writes went through):
    /// the frame says so, and the writes made to its table from then on
    /// are synced like any other caller's.
    pub fn finish(&self, vm: &mut Vm) {
        if self.direct {
            let attached = vm
                .frames
                .get(self.frame)
                .filter(|f| f.symtab_attached)
                .map(|f| f.vars.clone());
            if let Some(vars) = attached {
                vm.sync_symbol_table_writes(&vars);
            }
            return;
        }
        vm.sync_symbol_table_writes(&self.vars);
    }

    /// A second handle on the same table, to [`CallerSymbols::finish`]
    /// after [`CallerSymbols::checked`] consumed this one.
    #[must_use]
    pub fn handle(&self) -> Self {
        CallerSymbols {
            vars: self.vars.clone(),
            frame: self.frame,
            direct: self.direct,
        }
    }

    /// Assignments made by `extract()` must honor typed-reference sources.
    pub fn checked(self, vm: &mut Vm) -> CheckedSymbols<'_> {
        CheckedSymbols { inner: self, vm }
    }

    /// The caller's frame while it can still be accessed slot by slot:
    /// the same frame (its table) and still no attached table.
    fn direct_frame<'v>(&self, vm: &'v mut Vm) -> Option<&'v mut Frame> {
        if !self.direct {
            return None;
        }
        vm.frames
            .get_mut(self.frame)
            .filter(|f| !f.symtab_attached && Rc::ptr_eq(&f.vars, &self.vars))
    }

    /// Leaves slot-by-slot access: the caller's table is attached and
    /// filled from its compiled variables (`zend_rebuild_symbol_table`),
    /// and the following accesses go through it.
    #[cold]
    fn attach(&mut self, vm: &mut Vm) {
        self.direct = false;
        if let Some(vars) = vm.rebuild_symbol_table(self.frame) {
            self.vars = vars;
        }
    }

    /// [`SymbolTable::lookup`] with access to the caller's slots.
    fn lookup_in(&mut self, vm: &mut Vm, name: &ZStr) -> VarSlot {
        if self.direct {
            match self.direct_frame(vm) {
                Some(f) => {
                    if let Some(i) = cv_of(f, name.as_bytes()) {
                        return match f.cvs.get(i).and_then(CvSlot::get) {
                            Some(v) => VarSlot::Defined(v.clone()),
                            None => VarSlot::Undefined,
                        };
                    }
                    // Not a compiled variable: `vars` holds it, if anything.
                }
                None => self.attach(vm),
            }
        }
        self.lookup(name)
    }

    /// Writes `value` to variable `name` of the caller: through the
    /// reference the variable holds when `assign` (`$name = $value`),
    /// replacing what the variable holds otherwise (an `EXTR_REFS`
    /// binding, a new variable). The old value is released as
    /// `zval_ptr_dtor` releases it (a possible cycle root; its destructor
    /// when this was its last handle), at once, as in php-src.
    fn write_in(&mut self, vm: &mut Vm, name: &ZStr, value: Value, assign: bool) {
        if self.direct {
            let Some(f) = self.direct_frame(vm) else {
                self.attach(vm);
                return self.write_table(name, value, assign);
            };
            let Some(slot) = cv_of(f, name.as_bytes()).and_then(|i| f.cvs.get_mut(i)) else {
                self.attach(vm);
                return self.write_table(name, value, assign);
            };
            let held = match slot.get() {
                Some(Value::Reference(r)) if assign => Some(r.clone()),
                _ => None,
            };
            let old = match held {
                Some(r) => Some(r.set(value)),
                None => slot.set(Some(value)),
            };
            if let Some(old) = &old {
                php_vm::gc::possible_root(old);
            }
            drop(old);
            return;
        }
        self.write_table(name, value, assign);
    }

    /// [`CallerSymbols::write_in`] on the attached table.
    fn write_table(&mut self, name: &ZStr, value: Value, assign: bool) {
        if assign {
            // Infallible here: typed references were handled by the caller.
            let _ = SymbolTable::assign(self, name, value);
        } else {
            SymbolTable::set(self, name, value);
        }
    }

    /// `compact()`'s view of a caller accessed slot by slot: the variables
    /// the arguments name (strings, nested arrays of them), read once up
    /// front, so that `compact()` can report through the VM while it reads
    /// them. Nothing between the reads can change a variable of the caller
    /// that is not a reference (a warning's error handler runs in a frame
    /// of its own), and a reference is read when the result is built.
    /// `None` when the caller is not accessed slot by slot.
    pub fn snapshot(&mut self, vm: &mut Vm, args: &[Value]) -> Option<Snapshot> {
        if !self.direct {
            return None;
        }
        let mut names = Vec::new();
        let mut seen = Vec::new();
        for a in args {
            collect_names(a, &mut names, &mut seen);
        }
        let mut vars = Vec::with_capacity(names.len());
        for n in names {
            let slot = self.lookup_in(vm, &n);
            if !self.direct {
                // The frame changed under us: the table then.
                return None;
            }
            vars.push((n, slot));
        }
        Some(Snapshot { vars })
    }
}

/// The names `compact()` looks up for argument `v`: a string, or the
/// strings of an array, recursively; an array already being walked (a
/// recursive one, which `compact()` reports) is not entered again.
fn collect_names(v: &Value, names: &mut Vec<ZStr>, seen: &mut Vec<usize>) {
    match v {
        Value::Str(s) => {
            if !names.iter().any(|n: &ZStr| n.as_bytes() == s.as_bytes()) {
                names.push(s.clone());
            }
        }
        Value::Reference(r) => {
            let inner = r.get();
            collect_names(&inner, names, seen);
        }
        Value::Array(a) => {
            let addr = a.addr();
            if seen.contains(&addr) {
                return;
            }
            seen.push(addr);
            for e in a.values() {
                collect_names(e, names, seen);
            }
            seen.pop();
        }
        _ => {}
    }
}

/// The variables [`CallerSymbols::snapshot`] read for `compact()`.
#[derive(Debug)]
pub struct Snapshot {
    vars: Vec<(ZStr, VarSlot)>,
}

impl SymbolTable for Snapshot {
    fn lookup(&mut self, name: &ZStr) -> VarSlot {
        self.vars
            .iter()
            .find(|(n, _)| n.as_bytes() == name.as_bytes())
            .map_or(VarSlot::Missing, |(_, v)| v.clone())
    }

    // `compact()` only reads.
    fn assign(&mut self, _name: &ZStr, _value: Value) -> RtResult<()> {
        Ok(())
    }

    fn set(&mut self, _name: &ZStr, _value: Value) {}
}

#[derive(Debug)]
pub struct CheckedSymbols<'a> {
    inner: CallerSymbols,
    vm: &'a mut Vm,
}

impl SymbolTable for CheckedSymbols<'_> {
    fn lookup(&mut self, name: &ZStr) -> VarSlot {
        self.inner.lookup_in(self.vm, name)
    }

    fn assign(&mut self, name: &ZStr, value: Value) -> RtResult<()> {
        if let VarSlot::Defined(Value::Reference(r)) = self.inner.lookup_in(self.vm, name) {
            self.vm
                .assign_ref_ex(&r, value, Some(false))
                .map(|_| ())
                .map_err(|t| self.vm.runtime_error_from(t))
        } else {
            self.inner.write_in(self.vm, name, value, true);
            Ok(())
        }
    }

    fn set(&mut self, name: &ZStr, value: Value) {
        self.inner.write_in(self.vm, name, value, false);
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
