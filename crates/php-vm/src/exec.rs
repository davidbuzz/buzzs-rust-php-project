//! The interpreter loop (`execute_ex`) and simple instructions.

use crate::object;
use crate::opcode::{BinKind, CastKind, Op, Operand, UnKind};
use crate::vm::{FinState, Frame, RetTarget, Throw, Vm, VmResult};
use php_runtime::{ops, BinaryOp, ErrorLevel, Value, ZArray, ZStr};
use std::rc::Rc;

/// What an instruction asks the loop to do next.
pub(crate) enum Flow {
    /// Continue with the next instruction of the current frame.
    Next,
    /// The frame stack changed: reload the current frame.
    Reload,
    /// The base frame finished with this value.
    Done(Value),
}

impl Vm {
    /// The current frame.
    #[inline]
    pub(crate) fn frame(&self) -> &Frame {
        self.frames.last().expect("active frame")
    }

    /// The current frame (mutable).
    #[inline]
    pub(crate) fn frame_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("active frame")
    }

    /// Reads a literal of the current frame.
    #[inline]
    pub(crate) fn lit(&self, i: u32) -> Value {
        self.frame().code.literals[i as usize].clone()
    }

    /// Reads a literal as a string.
    pub(crate) fn lit_str(&self, i: u32) -> ZStr {
        match &self.frame().code.literals[i as usize] {
            Value::Str(s) => s.clone(),
            other => ZStr::from(format!("{other:?}")),
        }
    }

    /// Reads an operand (dereferenced), warning on undefined variables.
    pub(crate) fn read(&mut self, op: Operand) -> VmResult<Value> {
        match op {
            Operand::Tmp(t) => {
                let v = std::mem::take(&mut self.frame_mut().tmps[t as usize]);
                Ok(match v {
                    Value::Reference(r) => r.get(),
                    v => v,
                })
            }
            Operand::TmpKeep(t) => Ok(self.frame().tmps[t as usize].deref()),
            Operand::Lit(i) => Ok(self.lit(i)),
            Operand::Cv(cv) => self.read_cv(cv),
            Operand::Unused => Ok(Value::Null),
        }
    }

    /// Reads an operand without dereferencing temporaries holding
    /// references.
    pub(crate) fn read_raw(&mut self, op: Operand) -> VmResult<Value> {
        match op {
            Operand::Tmp(t) => Ok(std::mem::take(&mut self.frame_mut().tmps[t as usize])),
            Operand::TmpKeep(t) => Ok(self.frame().tmps[t as usize].clone()),
            other => self.read(other),
        }
    }

    /// Reads a compiled variable.
    pub(crate) fn read_cv(&mut self, cv: u32) -> VmResult<Value> {
        let f = self.frame();
        let idx = f.cv_idx(cv);
        let v = f
            .vars
            .borrow()
            .slots
            .get(idx)
            .and_then(|s| s.as_ref().map(Value::deref));
        match v {
            Some(v) => Ok(v),
            None => {
                let name = self.frame().code.cv_names[cv as usize].clone();
                self.warning(&crate::exec::undefined_var_msg(&name))?;
                Ok(Value::Null)
            }
        }
    }

    /// Reads a compiled variable without warning (`None` if undefined).
    pub(crate) fn cv_get(&self, cv: u32) -> Option<Value> {
        let f = self.frame();
        let idx = f.cv_idx(cv);
        f.vars.borrow().slots.get(idx).cloned().flatten()
    }

    /// Assigns to a compiled variable (through references).
    pub(crate) fn cv_assign(&mut self, cv: u32, v: Value) {
        let f = self.frame();
        let idx = f.cv_idx(cv);
        let mut vars = f.vars.borrow_mut();
        let slot = &mut vars.slots[idx];
        let old = match slot {
            Some(Value::Reference(r)) => Some(r.set(v)),
            _ => slot.replace(v),
        };
        drop(vars);
        if let Some(old) = &old {
            crate::gc::possible_root(old);
        }
        drop(old);
    }

    /// Binds a compiled variable to a reference.
    pub(crate) fn cv_bind(&mut self, cv: u32, v: Value) {
        let f = self.frame();
        let idx = f.cv_idx(cv);
        let old = f.vars.borrow_mut().slots[idx].replace(v);
        if let Some(old) = &old {
            crate::gc::possible_root(old);
        }
        drop(old);
    }

    /// Stores a temporary.
    #[inline]
    pub(crate) fn set_tmp(&mut self, t: u32, v: Value) {
        self.frame_mut().tmps[t as usize] = v;
    }

    /// Stores an optional result.
    #[inline]
    pub(crate) fn set_dst(&mut self, dst: Option<u32>, v: Value) {
        if let Some(t) = dst {
            self.set_tmp(t, v);
        }
    }

    /// Converts a runtime error into a throw (creating the exception).
    pub(crate) fn rt_throw(&mut self, e: php_runtime::RuntimeError) -> Throw {
        use php_runtime::RuntimeError as E;
        if let Some(t) = self.pending_throw.take() {
            return t;
        }
        match e {
            E::Thrown(Value::Object(o)) => Throw::Exception(o),
            E::Thrown(_) => Throw::Fatal,
            E::Fatal(m) => {
                let _ = self.fatal(&m);
                Throw::Fatal
            }
            E::Error(m) => self.make_throw("Error", &m),
            E::TypeError(m) => self.make_throw("TypeError", &m),
            E::ValueError(m) => self.make_throw("ValueError", &m),
            E::ArithmeticError(m) => self.make_throw("ArithmeticError", &m),
            E::DivisionByZeroError(m) => self.make_throw("DivisionByZeroError", &m),
            E::ArgumentCountError(m) => self.make_throw("ArgumentCountError", &m),
            E::Custom { class, message } => {
                let c = class.to_string_lossy();
                self.make_throw(&c, &message)
            }
        }
    }

    /// Runs the frames from `base` until the base frame returns.
    pub fn execute(&mut self, base: usize) -> VmResult<Value> {
        self.depth += 1;
        if self.depth > 20000 {
            self.depth -= 1;
            let _ = self.fatal("Maximum function nesting level reached, aborting!");
            return Err(Throw::Fatal);
        }
        let r = self.execute_inner(base);
        self.depth -= 1;
        r
    }

    fn execute_inner(&mut self, base: usize) -> VmResult<Value> {
        loop {
            match self.run(base) {
                Ok(v) => return Ok(v),
                Err(Throw::Exception(e)) => {
                    if let Err(t) = self.handle_exception(e, base) {
                        return Err(t);
                    }
                }
                Err(t) => {
                    self.frames.truncate(base);
                    return Err(t);
                }
            }
        }
    }

    /// Finds a handler for `exc` in the frames above `base`. Returns
    /// `Ok(())` when execution can continue, or the throw that escapes.
    pub(crate) fn handle_exception(&mut self, exc: php_runtime::Obj, base: usize) -> VmResult<()> {
        loop {
            let fidx = self.frames.len() - 1;
            let f = self.frames.last_mut().expect("frame");
            if !f.silence_saved.is_empty() {
                let saved = f.silence_saved[0];
                f.silence_saved.clear();
                self.error_reporting = saved;
            }
            let f = self.frames.last_mut().expect("frame");
            let throw_ip = f.ip.saturating_sub(1) as u32;
            let code = f.code.clone();
            for entry in &code.try_table {
                let in_try = throw_ip >= entry.try_start && throw_ip < entry.try_end;
                if in_try {
                    if let Some(cs) = entry.catch_start {
                        f.pending_exc = Some(exc);
                        f.ip = cs as usize;
                        f.calls.clear();
                        return Ok(());
                    }
                }
                if let Some((fin, fs, fe)) = entry.finally {
                    if throw_ip >= entry.try_start && throw_ip < entry.catch_end {
                        f.fins[fin as usize] = FinState::Exception(exc);
                        f.ip = fs as usize;
                        f.calls.clear();
                        return Ok(());
                    }
                    if throw_ip >= fs && throw_ip < fe {
                        let prev = std::mem::take(&mut f.fins[fin as usize]);
                        if let FinState::Exception(p) = prev {
                            crate::exceptions::chain_previous(&exc, &p);
                        }
                    }
                }
            }
            // Unwind this frame.
            let popped = self.frames.pop();
            if let Some(p) = &popped {
                if let Some(g) = &p.generator {
                    crate::generator::mark_finished(g);
                }
            }
            drop(popped);
            if fidx <= base {
                return Err(Throw::Exception(exc));
            }
        }
    }

    /// The interpreter loop proper.
    #[allow(clippy::too_many_lines)]
    fn run(&mut self, base: usize) -> VmResult<Value> {
        'frames: loop {
            let code = self.frame().code.clone();
            loop {
                let ip = {
                    let f = self.frame_mut();
                    let ip = f.ip;
                    f.ip += 1;
                    ip
                };
                let flow = match &code.ops[ip] {
                    Op::Nop => Flow::Next,
                    Op::Move { src, dst } => {
                        let v = self.read(*src)?;
                        self.set_tmp(*dst, v);
                        Flow::Next
                    }
                    Op::Bin { op, a, b, dst } => {
                        let x = self.read(*a)?;
                        let y = self.read(*b)?;
                        let r = self.binary(*op, &x, &y)?;
                        drop((x, y));
                        self.set_tmp(*dst, r);
                        Flow::Next
                    }
                    Op::Unary { op, a, dst } => {
                        let x = self.read(*a)?;
                        let r =
                            match op {
                                UnKind::Not => Value::Bool(!self.to_bool(&x)?),
                                UnKind::Bool => Value::Bool(self.to_bool(&x)?),
                                UnKind::BitNot => {
                                    ops::bit_not(&x, self).map_err(|e| self.rt_throw(e))?
                                }
                                UnKind::Neg => ops::mul(&x, &Value::Int(-1), self)
                                    .map_err(|e| self.rt_throw(e))?,
                                UnKind::Plus => ops::mul(&x, &Value::Int(1), self)
                                    .map_err(|e| self.rt_throw(e))?,
                            };
                        self.set_tmp(*dst, r);
                        Flow::Next
                    }
                    Op::Cast { kind, a, dst } => {
                        let x = self.read(*a)?;
                        let r = self.cast(*kind, x)?;
                        self.set_tmp(*dst, r);
                        Flow::Next
                    }
                    Op::Rope { parts, dst } => {
                        let mut out: Vec<u8> = Vec::new();
                        for p in parts.iter() {
                            let v = self.read(*p)?;
                            match &v {
                                Value::Str(s) => out.extend_from_slice(s.as_bytes()),
                                _ => {
                                    let s = self.to_zstr(&v)?;
                                    out.extend_from_slice(s.as_bytes());
                                }
                            }
                        }
                        self.set_tmp(*dst, Value::Str(ZStr::from(out)));
                        Flow::Next
                    }
                    Op::InitArray { dst } => {
                        self.set_tmp(*dst, Value::Array(ZArray::new()));
                        Flow::Next
                    }
                    Op::ArrayAdd { arr, key, val } => {
                        let v = self.read(*val)?;
                        let k = match key {
                            Some(k) => Some(self.read(*k)?),
                            None => None,
                        };
                        self.array_literal_add(*arr, k, v)?;
                        Flow::Next
                    }
                    Op::ArrayAddRef { arr, key, place } => {
                        let k = match key {
                            Some(k) => Some(self.read(*k)?),
                            None => None,
                        };
                        let r = self.place_make_ref(*place)?;
                        self.array_literal_add(*arr, k, Value::Reference(r))?;
                        Flow::Next
                    }
                    Op::ArrayUnpack { arr, src } => {
                        let v = self.read(*src)?;
                        self.array_unpack(*arr, v)?;
                        Flow::Next
                    }
                    Op::Jmp(t) => {
                        self.frame_mut().ip = *t as usize;
                        Flow::Next
                    }
                    Op::JmpZ(a, t) => {
                        let v = self.read(*a)?;
                        if !self.to_bool(&v)? {
                            self.frame_mut().ip = *t as usize;
                        }
                        Flow::Next
                    }
                    Op::JmpNz(a, t) => {
                        let v = self.read(*a)?;
                        if self.to_bool(&v)? {
                            self.frame_mut().ip = *t as usize;
                        }
                        Flow::Next
                    }
                    Op::JmpZEx { a, dst, target } => {
                        let v = self.read(*a)?;
                        let b = self.to_bool(&v)?;
                        if !b {
                            self.set_tmp(*dst, Value::Bool(false));
                            self.frame_mut().ip = *target as usize;
                        }
                        Flow::Next
                    }
                    Op::JmpNzEx { a, dst, target } => {
                        let v = self.read(*a)?;
                        let b = self.to_bool(&v)?;
                        if b {
                            self.set_tmp(*dst, Value::Bool(true));
                            self.frame_mut().ip = *target as usize;
                        }
                        Flow::Next
                    }
                    Op::JmpSet { a, dst, target } => {
                        let v = self.read(*a)?;
                        if self.to_bool(&v)? {
                            self.set_tmp(*dst, v);
                            self.frame_mut().ip = *target as usize;
                        }
                        Flow::Next
                    }
                    Op::Coalesce { a, dst, target } => {
                        let v = match a {
                            Operand::Cv(cv) => {
                                self.cv_get(*cv).map(|v| v.deref()).unwrap_or_default()
                            }
                            o => self.read(*o)?,
                        };
                        if !v.is_null() {
                            self.set_tmp(*dst, v);
                            self.frame_mut().ip = *target as usize;
                        }
                        Flow::Next
                    }
                    Op::JmpNull { a, dst, target } => {
                        let v = match a {
                            Operand::Cv(cv) => {
                                let v = self.cv_get(*cv).map(|v| v.deref());
                                match v {
                                    Some(v) => v,
                                    None => self.read(*a)?,
                                }
                            }
                            o => self.read(*o)?,
                        };
                        if v.is_null() {
                            if let Operand::TmpKeep(t) = a {
                                self.set_tmp(*t, Value::Null);
                            }
                            self.set_tmp(*dst, Value::Null);
                            self.frame_mut().ip = *target as usize;
                        }
                        Flow::Next
                    }
                    Op::AssignCv { cv, val, dst } => {
                        let v = self.read(*val)?;
                        let v = self.copy_on_assign(v);
                        if let Some(d) = dst {
                            self.set_tmp(*d, v.clone());
                        }
                        self.cv_assign(*cv, v);
                        Flow::Next
                    }
                    Op::Assign { place, val, dst } => {
                        let v = self.read(*val)?;
                        let v = self.copy_on_assign(v);
                        let r = self.place_assign(*place, v)?;
                        self.set_dst(*dst, r);
                        Flow::Next
                    }
                    Op::AssignOp {
                        place,
                        op,
                        val,
                        dst,
                    } => {
                        let v = self.read(*val)?;
                        let r = self.place_assign_op(*place, *op, v)?;
                        self.set_dst(*dst, r);
                        Flow::Next
                    }
                    Op::IncDec { place, kind, dst } => {
                        let r = self.place_incdec(*place, *kind)?;
                        self.set_dst(*dst, r);
                        Flow::Next
                    }
                    Op::AssignRef { place, src, dst } => {
                        let v = self.read_raw(*src)?;
                        let r = self.place_assign_ref(*place, v)?;
                        self.set_dst(*dst, r);
                        Flow::Next
                    }
                    Op::MakeRef { place, dst } => {
                        let r = self.place_make_ref(*place)?;
                        self.set_tmp(*dst, Value::Reference(r));
                        Flow::Next
                    }
                    Op::Fetch { place, mode, dst } => {
                        let v = self.place_fetch(*place, *mode)?;
                        self.set_tmp(*dst, v);
                        Flow::Next
                    }
                    Op::Isset { place, dst } => {
                        let v = self.place_isset(*place, false)?;
                        self.set_tmp(*dst, Value::Bool(v));
                        Flow::Next
                    }
                    Op::Empty { place, dst } => {
                        let v = self.place_isset(*place, true)?;
                        self.set_tmp(*dst, Value::Bool(v));
                        Flow::Next
                    }
                    Op::Unset { place } => {
                        self.place_unset(*place)?;
                        Flow::Next
                    }
                    Op::Global { cv, name } => {
                        let n = self.lit_str(*name);
                        let r = self.global_ref(n.as_bytes());
                        self.cv_bind(*cv, Value::Reference(r));
                        Flow::Next
                    }
                    Op::BindStatic { cv, idx, skip } => {
                        if let Some(r) = self.static_slot(*idx) {
                            self.cv_bind(*cv, Value::Reference(r));
                            self.frame_mut().ip = *skip as usize;
                        }
                        Flow::Next
                    }
                    Op::InitStatic { cv, idx, val } => {
                        let v = self.read(*val)?;
                        let r = self.static_init(*idx, v);
                        self.cv_bind(*cv, Value::Reference(r));
                        Flow::Next
                    }
                    Op::FetchThis { dst } => {
                        let t = self.frame().this.clone();
                        match t {
                            Some(o) => self.set_tmp(*dst, Value::Object(o)),
                            None => {
                                return Err(self
                                    .make_throw("Error", "Using $this when not in object context"))
                            }
                        }
                        Flow::Next
                    }
                    Op::InitFCall {
                        name,
                        fallback,
                        display,
                    } => {
                        self.op_init_fcall(*name, *fallback, *display)?;
                        Flow::Next
                    }
                    Op::InitDynCall { callee } => {
                        let c = self.read(*callee)?;
                        self.op_init_dyn_call(c)?;
                        Flow::Next
                    }
                    Op::InitMethodCall { obj, name } => {
                        let o = self.read(*obj)?;
                        let n = self.read(*name)?;
                        self.op_init_method_call(o, n)?;
                        Flow::Next
                    }
                    Op::InitStaticCall { class, name } => {
                        let n = self.read(*name)?;
                        self.op_init_static_call(class, n)?;
                        Flow::Next
                    }
                    Op::InitParentHook { prop, set } => {
                        let p = self.lit_str(*prop);
                        self.op_init_parent_hook(p, *set)?;
                        Flow::Next
                    }
                    Op::New { class, dst, skip } => {
                        let has_ctor = self.op_new(class, *dst)?;
                        if !has_ctor {
                            self.frame_mut().ip = *skip as usize;
                        }
                        Flow::Next
                    }
                    Op::Send { src } => {
                        self.op_send(src, None)?;
                        Flow::Next
                    }
                    Op::SendNamed { name, src } => {
                        let n = self.lit_str(*name);
                        self.op_send(src, Some(n))?;
                        Flow::Next
                    }
                    Op::SendUnpack { src } => {
                        let v = self.read(*src)?;
                        self.op_send_unpack(v)?;
                        Flow::Next
                    }
                    Op::DoCall { dst } => {
                        if self.op_do_call(*dst)? {
                            Flow::Reload
                        } else {
                            Flow::Next
                        }
                    }
                    Op::SendPlaceholder { name } => {
                        let n = name.map(|n| self.lit_str(n));
                        self.op_send_placeholder(n)?;
                        Flow::Next
                    }
                    Op::CallablePartial {
                        dst,
                        variadic,
                        const_args,
                        name,
                    } => {
                        let call = self.frame_mut().calls.pop().expect("pending call");
                        let n = self.lit_str(*name);
                        let c = self.make_partial(call, *variadic, *const_args, n)?;
                        self.set_tmp(*dst, c);
                        Flow::Next
                    }
                    Op::CallableConvert { dst } => {
                        let call = self.frame_mut().calls.pop().expect("pending call");
                        let c = self.make_closure_from_call(call)?;
                        self.set_tmp(*dst, c);
                        Flow::Next
                    }
                    Op::Recv { n, cv } => {
                        self.op_recv(*n, *cv)?;
                        Flow::Next
                    }
                    Op::RecvInit { n, cv, skip } => {
                        if self.op_recv_init(*n, *cv)? {
                            self.frame_mut().ip = *skip as usize;
                        }
                        Flow::Next
                    }
                    Op::RecvDefault { n, cv, val } => {
                        let v = self.read(*val)?;
                        self.op_recv_default(*n, *cv, v)?;
                        Flow::Next
                    }
                    Op::RecvVariadic { n, cv } => {
                        self.op_recv_variadic(*n, *cv)?;
                        Flow::Next
                    }
                    Op::VerifyReturn { val } => {
                        let _ = val;
                        Flow::Next
                    }
                    Op::Return { val } => {
                        let v = match val {
                            Operand::Unused => None,
                            o => Some(self.read(*o)?),
                        };
                        self.op_return(v, base)?
                    }
                    Op::ReturnByRefValue { val } => {
                        let unused = *val == Operand::Unused;
                        let raw = if unused { Value::Null } else { self.read_raw(*val)? };
                        let v = match raw {
                            Value::Reference(r) => Value::Reference(r),
                            other => {
                                self.notice("Only variable references should be returned by reference")?;
                                let checked = self.verify_return((!unused).then_some(other))?;
                                Value::Reference(php_runtime::PhpRef::new(checked))
                            }
                        };
                        self.op_return_raw(Some(v), base)?
                    }
                    Op::ReturnRef { place } => {
                        let by_ref = self
                            .frame()
                            .ufunc
                            .as_ref()
                            .is_some_and(|u| u.flags & crate::func::flags::RETURN_REF != 0);
                        let v = if by_ref {
                            Value::Reference(self.place_make_ref(*place)?)
                        } else {
                            self.place_fetch(*place, crate::opcode::FetchMode::Read)?
                        };
                        self.op_return_raw(Some(v), base)?
                    }
                    Op::GeneratorCreate => self.op_generator_create(base)?,
                    Op::Yield { key, val, dst } => {
                        let k = match key {
                            Some(k) => Some(self.read(*k)?),
                            None => None,
                        };
                        let v = match val {
                            Some(v) => Some(self.read_raw(*v)?),
                            None => None,
                        };
                        self.op_yield(k, v, *dst, base)?
                    }
                    Op::YieldRef { key, place, dst } => {
                        let k = match key {
                            Some(k) => Some(self.read(*k)?),
                            None => None,
                        };
                        let r = self.place_make_ref(*place)?;
                        self.op_yield(k, Some(Value::Reference(r)), *dst, base)?
                    }
                    Op::YieldFrom { src, dst } => {
                        let v = self.read_raw(*src)?;
                        self.op_yield_from(v, *dst, base)?
                    }
                    Op::DeclareFunction(idx) => {
                        let f = code.decls.functions.borrow()[*idx as usize].clone();
                        self.declare_function(&f)?;
                        Flow::Next
                    }
                    Op::DeclareClass(idx) => {
                        let d = code.decls.classes.borrow()[*idx as usize].clone();
                        self.declare_class(&d, false)?;
                        Flow::Next
                    }
                    Op::DeclareAnonClass { idx, dst } => {
                        let d = code.decls.classes.borrow()[*idx as usize].clone();
                        let c = self.declare_class(&d, true)?;
                        self.set_tmp(*dst, Value::Str(c.name.clone()));
                        Flow::Next
                    }
                    Op::MakeClosure { idx, dst } => {
                        let f = code.decls.functions.borrow()[*idx as usize].clone();
                        let c = self.make_closure(&f)?;
                        self.set_tmp(*dst, c);
                        Flow::Next
                    }
                    Op::DeclareConst { name, val, attrs } => {
                        let n = self.lit_str(*name);
                        let v = self.read(*val)?;
                        if self.declare_constant(n.clone(), v)? && *attrs != u32::MAX {
                            if let Some(a) =
                                code.decls.const_attributes.borrow().get(*attrs as usize)
                            {
                                self.const_attributes.insert(n, a.clone());
                            }
                        }
                        Flow::Next
                    }
                    Op::FetchConst {
                        name,
                        fallback,
                        dst,
                    } => {
                        let v = self.fetch_constant(*name, *fallback)?;
                        self.set_tmp(*dst, v);
                        Flow::Next
                    }
                    Op::FetchClassConst { class, name, dst } => {
                        let n = self.read(*name)?;
                        let v = self.op_fetch_class_const(class, n)?;
                        self.set_tmp(*dst, v);
                        Flow::Next
                    }
                    Op::FetchClassName { class, dst } => {
                        let v = self.op_fetch_class_name(class)?;
                        self.set_tmp(*dst, v);
                        Flow::Next
                    }
                    Op::InstanceOf { a, class, dst } => {
                        let v = self.read(*a)?;
                        let r = self.op_instanceof(&v, class)?;
                        self.set_tmp(*dst, Value::Bool(r));
                        Flow::Next
                    }
                    Op::Clone { a, dst } => {
                        let v = self.read(*a)?;
                        let r = self.clone_value(v)?;
                        self.set_tmp(*dst, r);
                        Flow::Next
                    }
                    Op::Echo(a) => {
                        let v = self.read(*a)?;
                        match &v {
                            Value::Str(s) => self.echo(s.as_bytes()),
                            _ => {
                                let s = self.to_zstr(&v)?;
                                self.echo(s.as_bytes());
                            }
                        }
                        Flow::Next
                    }
                    Op::Exit(a) => {
                        let v = self.read(*a)?;
                        self.op_exit(v)?;
                        return Err(Throw::Exit);
                    }
                    Op::Free(a) => {
                        let v = self.read_raw(*a)?;
                        drop(v);
                        Flow::Next
                    }
                    Op::Throw(a) => {
                        let v = self.read(*a)?;
                        return Err(self.op_throw(v));
                    }
                    Op::Catch { classes, cv, next } => {
                        let exc = self.frame().pending_exc.clone();
                        let Some(exc) = exc else {
                            self.frame_mut().ip = *next as usize;
                            continue;
                        };
                        let cls = object::class_of(&exc).clone();
                        let mut matched = false;
                        for c in classes.iter() {
                            let name = self.lit_str(*c);
                            let lc = name.to_ascii_lowercase();
                            if cls.instance_of_name(lc.as_bytes()) {
                                matched = true;
                                break;
                            }
                        }
                        if matched {
                            self.frame_mut().pending_exc = None;
                            if let Some(cv) = cv {
                                self.cv_assign(*cv, Value::Object(exc));
                            }
                        } else {
                            self.frame_mut().ip = *next as usize;
                        }
                        Flow::Next
                    }
                    Op::Rethrow => {
                        let exc = self.frame_mut().pending_exc.take();
                        match exc {
                            Some(e) => return Err(Throw::Exception(e)),
                            None => Flow::Next,
                        }
                    }
                    Op::FastCall { fin, target } => {
                        let f = self.frame_mut();
                        f.fins[*fin as usize] = FinState::Return(f.ip);
                        f.ip = *target as usize;
                        Flow::Next
                    }
                    Op::FastRet { fin } => {
                        let st = std::mem::take(&mut self.frame_mut().fins[*fin as usize]);
                        match st {
                            FinState::Return(ip) => {
                                self.frame_mut().ip = ip;
                                Flow::Next
                            }
                            FinState::Exception(e) => return Err(Throw::Exception(e)),
                            FinState::None => Flow::Next,
                        }
                    }
                    Op::DiscardFinally { fin } => {
                        self.frame_mut().fins[*fin as usize] = FinState::None;
                        Flow::Next
                    }
                    Op::BeginSilence { dst } => {
                        let old = self.error_reporting;
                        self.frame_mut().silence_saved.push(old);
                        self.set_tmp(*dst, Value::Int(old));
                        // E_ERROR | E_CORE_ERROR | E_COMPILE_ERROR | E_USER_ERROR | E_RECOVERABLE_ERROR | E_PARSE
                        self.error_reporting = old & 4437;
                        Flow::Next
                    }
                    Op::EndSilence { saved } => {
                        let v = self.read(*saved)?;
                        self.frame_mut().silence_saved.pop();
                        if let Value::Int(old) = v {
                            // Keep changes made inside the silenced expression
                            // only if they are not the silence mask.
                            if self.error_reporting == (old & 4437) {
                                self.error_reporting = old;
                            }
                        }
                        Flow::Next
                    }
                    Op::Include { kind, a, dst } => {
                        let v = self.read(*a)?;
                        if self.op_include(*kind, v, *dst)? {
                            Flow::Reload
                        } else {
                            Flow::Next
                        }
                    }
                    Op::FeReset {
                        subject,
                        place,
                        kind,
                        iter,
                        end,
                    } => {
                        let has = self.op_fe_reset(*subject, *place, *kind, *iter)?;
                        if !has {
                            self.frame_mut().ip = *end as usize;
                        }
                        Flow::Next
                    }
                    Op::FeFetch {
                        iter,
                        end,
                        val,
                        key,
                    } => {
                        let has = self.op_fe_fetch(*iter, *val, *key)?;
                        if !has {
                            self.frame_mut().ip = *end as usize;
                        }
                        Flow::Next
                    }
                    Op::FeFree { iter } => {
                        self.op_fe_free(*iter);
                        Flow::Next
                    }
                    Op::MatchError { subject } => {
                        let v = self.read(*subject)?;
                        return Err(self.match_error(&v));
                    }
                    Op::Ticks(n) => {
                        self.op_ticks(*n)?;
                        Flow::Next
                    }
                    Op::Diag { level, msg } => {
                        let m = self.lit_str(*msg).to_string_lossy();
                        self.error(i64::from(*level), &m)?;
                        Flow::Next
                    }
                    Op::AssertCheck { dst, skip } => {
                        let a = self.ini.get_long("zend.assertions");
                        if a != 1 {
                            self.set_tmp(*dst, Value::Bool(true));
                            self.frame_mut().ip = *skip as usize;
                        }
                        Flow::Next
                    }
                };
                if object::has_pending_destructors() {
                    self.run_pending_destructors()?;
                }
                // Root buffer reached the threshold (`gc_possible_root_when_full`).
                if crate::gc::wants_run() {
                    self.gc_safe_point()?;
                }
                // Delayed memory_limit violation (`zend_mm_delayed_error`).
                if php_memlimit::is_pending() {
                    self.raise_pending_oom()?;
                }
                self.check_interrupt()?;
                match flow {
                    Flow::Next => {}
                    Flow::Reload => continue 'frames,
                    Flow::Done(v) => return Ok(v),
                }
            }
        }
    }

    /// Applies a binary operator.
    pub(crate) fn binary(&mut self, op: BinKind, a: &Value, b: &Value) -> VmResult<Value> {
        let r = match op {
            BinKind::Arith(o) => match (o, a, b) {
                (BinaryOp::Add, Value::Int(x), Value::Int(y)) => match x.checked_add(*y) {
                    Some(r) => Ok(Value::Int(r)),
                    None => ops::add(a, b, self),
                },
                (BinaryOp::Sub, Value::Int(x), Value::Int(y)) => match x.checked_sub(*y) {
                    Some(r) => Ok(Value::Int(r)),
                    None => ops::sub(a, b, self),
                },
                (BinaryOp::Concat, Value::Str(x), Value::Str(y)) => {
                    Ok(Value::Str(ZStr::concat(x.as_bytes(), y.as_bytes())))
                }
                _ => ops::binary_op(o, a, b, self),
            },
            BinKind::Identical => ops::is_identical(a, b).map(Value::Bool),
            BinKind::NotIdentical => ops::is_identical(a, b).map(|r| Value::Bool(!r)),
            BinKind::Equal => match (a, b) {
                (Value::Int(x), Value::Int(y)) => Ok(Value::Bool(x == y)),
                _ => ops::is_equal(a, b, self).map(Value::Bool),
            },
            BinKind::NotEqual => ops::is_not_equal(a, b, self).map(Value::Bool),
            BinKind::Less => match (a, b) {
                (Value::Int(x), Value::Int(y)) => Ok(Value::Bool(x < y)),
                _ => ops::is_smaller(a, b, self).map(Value::Bool),
            },
            BinKind::LessEqual => match (a, b) {
                (Value::Int(x), Value::Int(y)) => Ok(Value::Bool(x <= y)),
                _ => ops::is_smaller_or_equal(a, b, self).map(Value::Bool),
            },
            BinKind::Spaceship => ops::spaceship(a, b, self),
        };
        r.map_err(|e| self.rt_throw(e))
    }

    /// `zend_is_true`.
    pub(crate) fn to_bool(&mut self, v: &Value) -> VmResult<bool> {
        match v {
            Value::Bool(b) => Ok(*b),
            Value::Int(i) => Ok(*i != 0),
            Value::Null => Ok(false),
            _ => v.to_bool(self).map_err(|e| self.rt_throw(e)),
        }
    }

    /// `zval_get_string`.
    pub(crate) fn to_zstr(&mut self, v: &Value) -> VmResult<ZStr> {
        match v {
            Value::Str(s) => Ok(s.clone()),
            _ => v.to_zstr(self).map_err(|e| self.rt_throw(e)),
        }
    }

    /// `zval_get_long`.
    pub(crate) fn to_long(&mut self, v: &Value) -> VmResult<i64> {
        match v {
            Value::Int(i) => Ok(*i),
            _ => v.to_long(self).map_err(|e| self.rt_throw(e)),
        }
    }

    /// `zval_get_double`.
    pub(crate) fn to_double(&mut self, v: &Value) -> VmResult<f64> {
        match v {
            Value::Float(f) => Ok(*f),
            _ => v.to_double(self).map_err(|e| self.rt_throw(e)),
        }
    }

    /// Value copy semantics for assignment: references are unwrapped.
    #[inline]
    pub(crate) fn copy_on_assign(&self, v: Value) -> Value {
        match v {
            Value::Reference(r) => r.get(),
            v => v,
        }
    }

    fn cast(&mut self, kind: CastKind, x: Value) -> VmResult<Value> {
        Ok(match kind {
            CastKind::Int => Value::Int(self.to_long(&x)?),
            CastKind::Float => Value::Float(self.to_double(&x)?),
            CastKind::String => Value::Str(self.to_zstr(&x)?),
            CastKind::Bool => Value::Bool(self.to_bool(&x)?),
            CastKind::Null => Value::Null,
            CastKind::Array => match x {
                Value::Array(_) => x,
                Value::Object(o) => Value::Array(self.object_to_array(&o)?),
                Value::Null => Value::Array(ZArray::new()),
                other => Value::Array(ZArray::from_values([other])),
            },
            CastKind::Object => match x {
                Value::Object(_) => x,
                Value::Array(a) => self.array_to_object(&a)?,
                Value::Null => self.create_std_object_vm()?,
                other => {
                    let o = self.create_std_object_vm()?;
                    if let Value::Object(obj) = &o {
                        self.write_dynamic_prop(obj, ZStr::from("scalar"), other);
                    }
                    o
                }
            },
        })
    }

    fn array_literal_add(&mut self, arr: u32, key: Option<Value>, v: Value) -> VmResult<()> {
        let mut a = std::mem::take(&mut self.frame_mut().tmps[arr as usize]);
        let res = (|| -> VmResult<()> {
            let Value::Array(za) = &mut a else {
                return Ok(());
            };
            match key {
                None => {
                    if za.make_mut().push(v).is_err() {
                        return Err(self.make_throw(
                            "Error",
                            "Cannot add element to the array as the next element is already occupied",
                        ));
                    }
                }
                Some(k) => {
                    let key = self.array_key(&k, KeyCtx::Literal)?;
                    if let Some(key) = key {
                        za.make_mut().insert(key, v);
                    }
                }
            }
            Ok(())
        })();
        self.frame_mut().tmps[arr as usize] = a;
        res
    }

    fn array_unpack(&mut self, arr: u32, src: Value) -> VmResult<()> {
        match src {
            Value::Array(s) => {
                let mut a = std::mem::take(&mut self.frame_mut().tmps[arr as usize]);
                if let Value::Array(za) = &mut a {
                    let t = za.make_mut();
                    for (k, v) in s.iter() {
                        let v = v.deref();
                        match k {
                            php_runtime::ArrayKey::Int(_) => {
                                if t.push(v).is_err() {
                                    self.frame_mut().tmps[arr as usize] = a;
                                    return Err(self.make_throw(
                                        "Error",
                                        "Cannot add element to the array as the next element is already occupied",
                                    ));
                                }
                            }
                            php_runtime::ArrayKey::Str(_) => {
                                t.insert(k.clone(), v);
                            }
                        }
                    }
                }
                self.frame_mut().tmps[arr as usize] = a;
                Ok(())
            }
            Value::Object(o) if self.instance_of(&o, b"traversable") => {
                let items = self.iterate_to_vec(&Value::Object(o))?;
                let mut a = std::mem::take(&mut self.frame_mut().tmps[arr as usize]);
                if let Value::Array(za) = &mut a {
                    let t = za.make_mut();
                    for (k, v) in items {
                        match k {
                            Value::Int(_) => {
                                let _ = t.push(v);
                            }
                            Value::Str(s) => {
                                t.insert(php_runtime::ArrayKey::from_zstr(&s), v);
                            }
                            other => {
                                self.frame_mut().tmps[arr as usize] = a;
                                return Err(self.make_throw(
                                    "Error",
                                    &format!(
                                        "Keys must be of type int|string during array unpacking"
                                    ),
                                ))
                                .map(|()| drop(other));
                            }
                        }
                    }
                }
                self.frame_mut().tmps[arr as usize] = a;
                Ok(())
            }
            other => Err(self.make_throw(
                "Error",
                &format!(
                    "Only arrays and Traversables can be unpacked, {} given",
                    crate::typecheck::given_type(&other)
                ),
            )),
        }
    }

    fn op_throw(&mut self, v: Value) -> Throw {
        match v {
            Value::Object(o) if self.instance_of(&o, b"throwable") => Throw::Exception(o),
            _ => self.make_throw("Error", "Can only throw objects"),
        }
    }

    fn op_exit(&mut self, v: Value) -> VmResult<()> {
        match v {
            Value::Str(s) => {
                self.echo(s.as_bytes());
            }
            Value::Int(i) => {
                #[allow(clippy::cast_possible_truncation)]
                let st = i as i32;
                self.exit_status = st;
            }
            Value::Null => {}
            Value::Bool(b) => {
                self.exit_status = i32::from(b);
            }
            Value::Float(f) => {
                #[allow(clippy::cast_possible_truncation)]
                let st = f as i32;
                self.exit_status = st;
            }
            other => {
                return Err(self.make_throw(
                    "TypeError",
                    &format!(
                        "exit(): Argument #1 ($status) must be of type string|int, {} given",
                        crate::typecheck::given_type(&other)
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Runs the destructors of objects released since the last check.
    pub(crate) fn run_pending_destructors(&mut self) -> VmResult<()> {
        loop {
            let pending = object::take_pending_destructors();
            if pending.is_empty() {
                return Ok(());
            }
            // `zend_objects_destroy_object`: a destructor runs with the
            // pending exception set aside; a new exception gets the pending
            // one as its innermost `previous`.
            let mut pending_err: Option<Throw> = None;
            for o in pending {
                if let Err(e) = self.call_destructor(&o) {
                    pending_err = Some(match (pending_err.take(), e) {
                        (Some(Throw::Exception(old)), Throw::Exception(new)) => {
                            crate::exceptions::chain_previous(&new, &old);
                            Throw::Exception(new)
                        }
                        (Some(old), Throw::Exception(_)) => old,
                        (_, e) => e,
                    });
                }
            }
            if let Some(e) = pending_err {
                return Err(e);
            }
        }
    }

    /// Calls `__destruct` on an object.
    pub(crate) fn call_destructor(&mut self, o: &php_runtime::Obj) -> VmResult<()> {
        let cls = object::class_of(o).clone();
        let Some(m) = cls.magic.destruct.clone() else {
            // Native destructor hook without a `__destruct` method.
            if let Some(f) = cls.hooks.as_ref().and_then(|h| h.destruct) {
                return f(self, o);
            }
            return Ok(());
        };
        if m.visibility() != crate::func::flags::PUBLIC && !self.in_shutdown {
            // Calling a private/protected destructor from the wrong scope.
            let scope = self.frames.last().and_then(|f| f.scope.clone());
            let allowed = scope.is_some_and(|s| self.can_access_method(&m, Some(&s)));
            if !allowed {
                let vis = if m.visibility() == crate::func::flags::PRIVATE {
                    "private"
                } else {
                    "protected"
                };
                let scope_name = scope_display(self.frames.last().and_then(|f| f.scope.as_ref()));
                self.warning(&format!(
                    "Call to {vis} {}::__destruct() from {scope_name} during shutdown ignored",
                    cls.name
                ))?;
                return Ok(());
            }
        }
        self.call_method(o, &m, Vec::new())?;
        // A native destructor hook also runs after a user `__destruct`
        // (the internal free path of the native parent).
        if let Some(f) = cls.hooks.as_ref().and_then(|h| h.destruct) {
            f(self, o)?;
        }
        Ok(())
    }

    /// The type of `ZEND_RETURN`: finishes the current frame.
    pub(crate) fn op_return(&mut self, v: Option<Value>, base: usize) -> VmResult<Flow> {
        let v = self.verify_return(v)?;
        self.op_return_raw(Some(v), base)
    }

    pub(crate) fn op_return_raw(&mut self, v: Option<Value>, base: usize) -> VmResult<Flow> {
        let v = v.unwrap_or_default();
        let fidx = self.frames.len() - 1;
        let frame = self.frames.pop().expect("frame");
        if let Some(g) = &frame.generator {
            crate::generator::finish_with(g, v);
            drop(frame);
            if fidx <= base {
                return Ok(Flow::Done(Value::Null));
            }
            return Ok(Flow::Reload);
        }
        let ret = frame.ret;
        let by_ref = frame
            .ufunc
            .as_ref()
            .is_some_and(|u| u.flags & crate::func::flags::RETURN_REF != 0);
        crate::gc::frame_released(&frame);
        drop(frame);
        let v = if by_ref { v } else { self.copy_on_assign(v) };
        if fidx <= base {
            return Ok(Flow::Done(v));
        }
        if let RetTarget::Tmp(t) = ret {
            self.set_tmp(t, v);
        }
        Ok(Flow::Reload)
    }

    /// Creates an exception of an internal class and returns the throw.
    pub(crate) fn make_throw(&mut self, class: &str, msg: &str) -> Throw {
        match self.create_exception(class, msg, 0) {
            Ok(o) => Throw::Exception(o),
            Err(t) => t,
        }
    }

    /// Reports an `E_WARNING`.
    pub(crate) fn warning(&mut self, msg: &str) -> VmResult<()> {
        self.error(ErrorLevel::Warning.bits(), msg)
    }

    /// Reports an `E_NOTICE`.
    pub(crate) fn notice(&mut self, msg: &str) -> VmResult<()> {
        self.error(ErrorLevel::Notice.bits(), msg)
    }

    /// Reports an `E_DEPRECATED`.
    pub(crate) fn deprecated(&mut self, msg: &str) -> VmResult<()> {
        self.error(ErrorLevel::Deprecated.bits(), msg)
    }

    /// The current class scope.
    pub(crate) fn scope(&self) -> Option<Rc<crate::class::Class>> {
        self.frames
            .iter()
            .rev()
            .find(|f| f.kind != crate::vm::FrameKind::Builtin)
            .and_then(|f| f.scope.clone())
    }
}

/// How a key is used (affects error messages).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyCtx {
    /// Array literal.
    Literal,
    /// Read access.
    Read,
    /// Write access.
    Write,
    /// isset/empty.
    Isset,
    /// unset.
    Unset,
}

/// "scope X" / "global scope" for messages.
pub(crate) fn scope_display(scope: Option<&Rc<crate::class::Class>>) -> String {
    match scope {
        Some(c) => format!("scope {}", object::display_name(&c.name)),
        None => "global scope".to_string(),
    }
}

/// "Undefined variable $x" — or "Undefined global variable $x" for the
/// auto-globals, which php-src fetches with `ZEND_FETCH_GLOBAL_LOCK`.
pub(crate) fn undefined_var_msg(name: &impl std::fmt::Display) -> String {
    let n = name.to_string();
    let global = matches!(
        n.as_str(),
        "_GET" | "_POST" | "_COOKIE" | "_SERVER" | "_ENV" | "_REQUEST" | "_FILES" | "_SESSION"
    );
    format!(
        "Undefined {}variable ${n}",
        if global { "global " } else { "" }
    )
}

impl Vm {
    /// Raises a recorded `memory_limit` violation as PHP's fatal error.
    pub(crate) fn raise_pending_oom(&mut self) -> VmResult<()> {
        if let Some(over) = php_memlimit::take() {
            self.error(crate::errors::level::ERROR, &over.to_string())?;
        }
        Ok(())
    }
}
