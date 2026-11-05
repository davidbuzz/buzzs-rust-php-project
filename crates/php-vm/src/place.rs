//! Reading and writing places (`$a[..]->b[..]`): the equivalent of Zend's
//! `FETCH_DIM_*`, `FETCH_OBJ_*`, `ASSIGN_DIM`, `ASSIGN_OBJ`, `UNSET_*`,
//! `ISSET_ISEMPTY_*` handlers.
//!
//! A write walks the place in one go: inside one container (a variable, a
//! property slot, a static property or a reference) the walk works on
//! `&mut Value` and collects diagnostics; when it reaches an object it
//! releases all borrows and continues with the object handlers, which may
//! run user code (`__get`, `offsetGet`, ...).

use crate::exec::KeyCtx;
use crate::object;
use crate::opcode::{FetchMode, IncDecKind, Operand, PlaceBase, PlaceId, Seg};
use crate::vm::{Throw, VarStore, Vm, VmResult};
use php_runtime::{ops, ArrayKey, BinaryOp, Obj, PhpRef, Value, ZArray, ZStr};
use std::cell::RefCell;
use std::rc::Rc;

/// A resolved place base.
pub(crate) enum RBase {
    /// A variable slot.
    Var(Rc<RefCell<VarStore>>, usize),
    /// `$this`.
    This(Option<Obj>),
    /// A static property slot.
    Cell(Rc<RefCell<Value>>),
    /// A temporary value.
    Value(Value),
    /// `$GLOBALS`.
    Globals,
    /// An undefined variable-variable (read contexts).
    Undef(ZStr),
}

/// A resolved path segment.
#[derive(Clone, Debug)]
pub(crate) enum RSeg {
    /// `[key]`
    Dim(Value),
    /// `[]`
    Append,
    /// `->name`
    Prop(ZStr),
}

/// A write operation applied at the end of a path.
pub(crate) enum WOp {
    Assign(Value),
    /// Read the current value for a read-modify-write (creates missing
    /// elements with a warning).
    FetchRw,
    MakeRef,
    AssignRef(Value),
    Unset,
}

/// Deferred diagnostics: (level bits, message).
type Diags = Vec<(i64, String)>;

/// Errors raised inside a container walk (converted to exceptions later).
pub(crate) enum WalkErr {
    Error(String),
    TypeError(String),
}

/// Result of walking inside a container.
pub(crate) enum Step {
    /// Finished with a result value.
    Done(Value),
    /// Reached an object at segment `i` (or the value of a property).
    Obj(Obj, usize),
    /// A container error.
    Err(WalkErr),
    /// An assignment through a typed reference, performed once the walk
    /// has released its borrows (`zend_assign_to_typed_ref`).
    TypedRefAssign(PhpRef, Value),
    /// Need the value of a string offset write with a non-string value:
    /// (string slot handled) — not used.
    Nothing,
}

const E_WARNING: i64 = 2;
const E_DEPRECATED: i64 = 8192;

/// Converts an array key for a write/read context without the VM (pure);
/// diagnostics are deferred.
pub(crate) fn key_of(v: &Value, diags: &mut Diags, ctx: KeyCtx) -> Result<ArrayKey, WalkErr> {
    Ok(match v {
        Value::Int(i) => ArrayKey::Int(*i),
        Value::Str(s) => ArrayKey::from_zstr(s),
        Value::Null => {
            diags.push((
                E_DEPRECATED,
                "Using null as an array offset is deprecated, use an empty string instead".into(),
            ));
            ArrayKey::Str(ZStr::empty())
        }
        Value::Bool(b) => ArrayKey::Int(i64::from(*b)),
        Value::Float(d) => {
            let d = *d;
            if d.is_nan() || d.is_infinite() {
                diags.push((
                    E_WARNING,
                    format!(
                        "The float {} is not representable as an int, cast occurred",
                        php_runtime::float::format_double(d, -1, false)
                    ),
                ));
                ArrayKey::Int(0)
            } else {
                let l = php_runtime::convert::dval_to_lval_silent(d);
                #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
                if !crate::typecheck::fits_long(d) {
                    diags.push((
                        E_WARNING,
                        format!(
                            "The float {} is not representable as an int, cast occurred",
                            php_runtime::float::format_double(d, -1, false)
                        ),
                    ));
                } else if l as f64 != d {
                    diags.push((
                        E_DEPRECATED,
                        format!(
                            "Implicit conversion from float {} to int loses precision",
                            php_runtime::float::format_double(d, -1, false)
                        ),
                    ));
                }
                ArrayKey::Int(l)
            }
        }
        Value::Resource(r) => {
            diags.push((
                E_WARNING,
                format!(
                    "Resource ID#{} used as offset, casting to integer ({})",
                    r.handle(),
                    r.handle()
                ),
            ));
            ArrayKey::Int(r.handle())
        }
        Value::Reference(r) => return key_of(&r.get(), diags, ctx),
        Value::Array(_) | Value::Object(_) => {
            let tn = crate::typecheck::given_type(v);
            return Err(WalkErr::TypeError(match ctx {
                KeyCtx::Isset => format!("Cannot access offset of type {tn} in isset or empty"),
                KeyCtx::Unset => format!("Cannot unset offset of type {tn} on array"),
                // Array literals too (`zend_illegal_array_offset_access`);
                // "Illegal offset type" is only the compile-time error.
                _ => format!("Cannot access offset of type {tn} on array"),
            }));
        }
    })
}

fn undefined_key_msg(k: &ArrayKey) -> String {
    match k {
        ArrayKey::Int(i) => format!("Undefined array key {i}"),
        ArrayKey::Str(s) => format!("Undefined array key \"{}\"", s.to_string_lossy()),
    }
}

/// String offset conversion (`zend_check_string_offset`).
fn string_offset(dim: &Value, diags: &mut Diags, write: bool) -> Result<i64, WalkErr> {
    match dim {
        Value::Int(i) => Ok(*i),
        Value::Str(s) => {
            if let Some(n) = php_runtime::numeric::is_numeric_string_ex(s.as_bytes(), true) {
                if let php_runtime::numeric::Number::Int(i) = n.value {
                    if n.trailing_data {
                        diags.push((
                            E_WARNING,
                            format!("Illegal string offset \"{}\"", s.to_string_lossy()),
                        ));
                    }
                    return Ok(i);
                }
            }
            Err(WalkErr::TypeError(
                "Cannot access offset of type string on string".to_string(),
            ))
        }
        Value::Float(d) => {
            diags.push((E_WARNING, "String offset cast occurred".into()));
            Ok(php_runtime::convert::dval_to_lval_silent(*d))
        }
        Value::Null | Value::Bool(_) => {
            diags.push((E_WARNING, "String offset cast occurred".into()));
            Ok(match dim {
                Value::Bool(true) => 1,
                _ => 0,
            })
        }
        Value::Reference(r) => string_offset(&r.get(), diags, write),
        other => Err(WalkErr::TypeError(format!(
            "Cannot access offset of type {} on string",
            crate::typecheck::given_type(other)
        ))),
    }
}

/// Writes one byte of a string (`zend_assign_to_string_offset`).
fn assign_string_offset(
    s: &mut Value,
    dim: &Value,
    val: &Value,
    diags: &mut Diags,
) -> Result<Value, WalkErr> {
    let Value::Str(str) = s else {
        return Ok(Value::Null);
    };
    let offset = string_offset(dim, diags, true)?;
    let len = i64::try_from(str.len()).unwrap_or(i64::MAX);
    if offset < -len {
        diags.push((E_WARNING, format!("Illegal string offset {offset}")));
        return Ok(Value::Null);
    }
    let offset = if offset < 0 { offset + len } else { offset };
    let bytes: Vec<u8> = match val {
        Value::Str(v) => v.as_bytes().to_vec(),
        Value::Int(i) => i.to_string().into_bytes(),
        Value::Bool(true) => b"1".to_vec(),
        Value::Bool(false) | Value::Null => Vec::new(),
        Value::Float(f) => php_runtime::float::format_double(*f, 17, false).into_bytes(),
        _ => b"A".to_vec(),
    };
    if bytes.is_empty() {
        return Err(WalkErr::Error(
            "Cannot assign an empty string to a string offset".into(),
        ));
    }
    if bytes.len() != 1 {
        diags.push((
            E_WARNING,
            "Only the first byte will be assigned to the string offset".into(),
        ));
    }
    let c = bytes[0];
    let mut v = str.as_bytes().to_vec();
    let off = usize::try_from(offset).unwrap_or(0);
    if off >= v.len() {
        v.resize(off, b' ');
        v.push(c);
    } else {
        v[off] = c;
    }
    *str = ZStr::from(v);
    Ok(Value::Str(ZStr::from_byte(c)))
}

/// The walk inside one container.
#[allow(clippy::too_many_lines)]
pub(crate) fn descend(
    v: &mut Value,
    segs: &[RSeg],
    i: usize,
    wop: &mut WOp,
    diags: &mut Diags,
    undef: bool,
) -> Step {
    if i == segs.len() {
        return apply_final(v, wop);
    }
    if let Value::Reference(r) = v {
        let r = r.clone();
        if r.try_borrow().is_none() {
            return Step::Err(WalkErr::Error(
                "Cannot modify a value while it is being iterated".into(),
            ));
        }
        if !matches!(wop, WOp::Unset) && !matches!(segs[i], RSeg::Prop(_)) {
            if let Some(msg) = crate::typed_refs::walk_auto_init_error(&r) {
                return Step::Err(WalkErr::TypeError(msg));
            }
        }
        let mut b = r.borrow_mut();
        return descend(&mut b, segs, i, wop, diags, false);
    }
    let last = i + 1 == segs.len();
    let is_unset = matches!(wop, WOp::Unset);
    match &segs[i] {
        RSeg::Prop(name) => match v {
            Value::Object(o) => Step::Obj(o.clone(), i),
            other => {
                if is_unset {
                    return Step::Done(Value::Null);
                }
                let tn = other.value_name();
                let tn = if undef { "null".to_string() } else { tn };
                if last && matches!(wop, WOp::Assign(_)) {
                    Step::Err(WalkErr::Error(format!(
                        "Attempt to assign property \"{}\" on {tn}",
                        name.to_string_lossy()
                    )))
                } else {
                    Step::Err(WalkErr::Error(format!(
                        "Attempt to modify property \"{}\" on {tn}",
                        name.to_string_lossy()
                    )))
                }
            }
        },
        seg @ (RSeg::Dim(_) | RSeg::Append) => {
            match v {
                Value::Array(_) => {}
                Value::Null => {
                    if is_unset {
                        return Step::Done(Value::Null);
                    }
                    *v = Value::Array(ZArray::new());
                }
                Value::Bool(false) => {
                    if is_unset {
                        return Step::Done(Value::Null);
                    }
                    diags.push((
                        E_DEPRECATED,
                        "Automatic conversion of false to array is deprecated".into(),
                    ));
                    *v = Value::Array(ZArray::new());
                }
                Value::Object(o) => return Step::Obj(o.clone(), i),
                Value::Str(s) => {
                    if is_unset {
                        if last {
                            return Step::Err(WalkErr::Error("Cannot unset string offsets".into()));
                        }
                        return Step::Err(WalkErr::Error("Cannot unset string offsets".into()));
                    }
                    if !last {
                        return Step::Err(WalkErr::Error(
                            "Cannot use string offset as an array".into(),
                        ));
                    }
                    let RSeg::Dim(dim) = seg else {
                        return Step::Err(WalkErr::Error(
                            "[] operator not supported for strings".into(),
                        ));
                    };
                    return match wop {
                        WOp::Assign(val) => {
                            if s.is_empty() && false {
                                return Step::Nothing;
                            }
                            let val = val.clone();
                            match assign_string_offset(v, dim, &val, diags) {
                                Ok(r) => Step::Done(r),
                                Err(e) => Step::Err(e),
                            }
                        }
                        // `zend_check_string_offset` runs first: its
                        // warnings (which an error handler may turn into
                        // an exception) and offset type errors precede
                        // `zend_wrong_string_offset_error`.
                        WOp::FetchRw => match string_offset(dim, diags, true) {
                            Ok(_) => Step::Err(WalkErr::Error(
                                "Cannot use assign-op operators with string offsets".into(),
                            )),
                            Err(e) => Step::Err(e),
                        },
                        WOp::MakeRef | WOp::AssignRef(_) => match string_offset(dim, diags, true) {
                            Ok(_) => Step::Err(WalkErr::Error(
                                "Cannot create references to/from string offsets".into(),
                            )),
                            Err(e) => Step::Err(e),
                        },
                        WOp::Unset => {
                            Step::Err(WalkErr::Error("Cannot unset string offsets".into()))
                        }
                    };
                }
                other => {
                    if is_unset {
                        return Step::Err(WalkErr::Error(
                            "Cannot unset offset in a non-array variable".into(),
                        ));
                    }
                    let _ = other;
                    return Step::Err(WalkErr::Error(
                        "Cannot use a scalar value as an array".into(),
                    ));
                }
            }
            let Value::Array(arr) = v else {
                return Step::Done(Value::Null);
            };
            match seg {
                RSeg::Append => {
                    let t = arr.make_mut();
                    if last {
                        match wop {
                            WOp::Assign(val) => {
                                let val = std::mem::take(val);
                                if t.push(val.clone()).is_err() {
                                    return Step::Err(WalkErr::Error(
                                        "Cannot add element to the array as the next element is already occupied".into(),
                                    ));
                                }
                                Step::Done(val)
                            }
                            WOp::AssignRef(r) => {
                                let r = std::mem::take(r);
                                if t.push(r.clone()).is_err() {
                                    return Step::Err(WalkErr::Error(
                                        "Cannot add element to the array as the next element is already occupied".into(),
                                    ));
                                }
                                Step::Done(r)
                            }
                            _ => match t.push_mut(Value::Null) {
                                Ok(slot) => apply_final(slot, wop),
                                Err(_) => {
                                    Step::Err(WalkErr::Error(
                                        "Cannot add element to the array as the next element is already occupied".into(),
                                    ))
                                }
                            },
                        }
                    } else {
                        match t.push_mut(Value::Null) {
                            Ok(slot) => descend(slot, segs, i + 1, wop, diags, true),
                            Err(_) => {
                                Step::Err(WalkErr::Error(
                                        "Cannot add element to the array as the next element is already occupied".into(),
                                    ))
                            }
                        }
                    }
                }
                RSeg::Dim(k) => {
                    let ctx = if is_unset {
                        KeyCtx::Unset
                    } else {
                        KeyCtx::Write
                    };
                    // ZEND_UNSET_DIM maps a null offset to "" without the
                    // null-offset deprecation; intermediate unset fetches
                    // (FETCH_DIM_UNSET) still raise it.
                    let key = if is_unset && last && matches!(k, Value::Null) {
                        ArrayKey::Str(ZStr::empty())
                    } else {
                        match key_of(k, diags, ctx) {
                            Ok(k) => k,
                            Err(e) => return Step::Err(e),
                        }
                    };
                    if is_unset {
                        if last {
                            if arr.contains_key(&key) {
                                let old = arr.make_mut().remove(&key);
                                drop(old);
                            }
                            return Step::Done(Value::Null);
                        }
                        if !arr.contains_key(&key) {
                            return Step::Done(Value::Null);
                        }
                        let t = arr.make_mut();
                        let Some(slot) = t.get_mut(&key) else {
                            return Step::Done(Value::Null);
                        };
                        return descend(slot, segs, i + 1, wop, diags, false);
                    }
                    let t = arr.make_mut();
                    let rw = matches!(wop, WOp::FetchRw);
                    let (slot, inserted) = t.lookup_ex(key.clone());
                    let existed = !inserted;
                    if !existed && rw {
                        diags.push((E_WARNING, undefined_key_msg(&key)));
                    }
                    if last {
                        match wop {
                            WOp::AssignRef(r) => {
                                let r = std::mem::take(r);
                                *slot = r.clone();
                                Step::Done(r)
                            }
                            _ => apply_final(slot, wop),
                        }
                    } else {
                        descend(slot, segs, i + 1, wop, diags, !existed)
                    }
                }
                RSeg::Prop(_) => Step::Done(Value::Null),
            }
        }
    }
}

thread_local! {
    /// Assignments through references that were borrowed by the walk that
    /// made them (see [`apply_final`]).
    static DEFERRED_REF_ASSIGNS: std::cell::RefCell<Vec<(PhpRef, Value)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

thread_local! {
    /// The `ArrayAccess` object and offset a read-modify-write walk ended
    /// on (`FETCH_DIM_RW` on an object yields a temporary): set by
    /// `Vm::obj_walk`, taken by `Vm::fetch_rw`.
    static OVERLOADED_RW: std::cell::Cell<Option<(Obj, Value)>> =
        const { std::cell::Cell::new(None) };
}

/// Performs deferred reference assignments; called once a write walk has
/// dropped its borrows.
pub(crate) fn flush_deferred_ref_assigns() {
    let pending = DEFERRED_REF_ASSIGNS.with(|d| std::mem::take(&mut *d.borrow_mut()));
    for (r, v) in pending {
        let old = r.set(v);
        drop(old);
    }
}

pub(crate) fn apply_final(v: &mut Value, wop: &mut WOp) -> Step {
    match wop {
        WOp::Assign(val) => {
            let val = std::mem::take(val);
            match v {
                Value::Reference(r) if crate::typed_refs::has_type_sources(r) => {
                    return Step::TypedRefAssign(r.clone(), val);
                }
                Value::Reference(r) => {
                    // The reference may be the container being walked
                    // (`$a['k'] = null` where `$a['k']` is `&$a`): its value
                    // is replaced once the walk has released it.
                    if r.try_borrow_mut().is_none() {
                        DEFERRED_REF_ASSIGNS
                            .with(|d| d.borrow_mut().push((r.clone(), val.clone())));
                        return Step::Done(val);
                    }
                    let old = r.set(val.clone());
                    drop(old);
                }
                _ => {
                    let old = std::mem::replace(v, val.clone());
                    drop(old);
                }
            }
            Step::Done(val)
        }
        WOp::FetchRw => Step::Done(v.deref()),
        WOp::MakeRef => Step::Done(Value::Reference(v.make_ref())),
        WOp::AssignRef(r) => {
            let r = std::mem::take(r);
            *v = r.clone();
            Step::Done(r)
        }
        WOp::Unset => {
            *v = Value::Null;
            Step::Done(Value::Null)
        }
    }
}

impl Vm {
    /// Resolves a place's base and segments.
    #[inline]
    pub(crate) fn resolve_place(
        &mut self,
        pid: PlaceId,
        write: bool,
    ) -> VmResult<(RBase, Vec<RSeg>)> {
        self.resolve_place_ex(pid, write, false)
    }

    /// [`Vm::resolve_place`]; `quiet` is the `BP_VAR_IS` fetch of
    /// `isset()`/`empty()`, where an undeclared or inaccessible static
    /// property base resolves to an undefined base without an error.
    fn resolve_place_ex(
        &mut self,
        pid: PlaceId,
        write: bool,
        quiet: bool,
    ) -> VmResult<(RBase, Vec<RSeg>)> {
        let code = self.frame().code.clone();
        let p = &code.places[pid as usize];
        let base = match &p.base {
            PlaceBase::Cv(cv) => {
                let f = self.frame();
                RBase::Var(f.vars.clone(), f.cv_idx(*cv))
            }
            PlaceBase::VarVar(op) => {
                let n = self.read(*op)?;
                let n = self.to_zstr(&n)?;
                if n.as_bytes() == b"this" {
                    RBase::This(self.frame().this.clone())
                } else {
                    let vars = self.frame().vars.clone();
                    let idx = if write {
                        Some(vars.borrow_mut().find_or_add(n.as_bytes()))
                    } else {
                        vars.borrow_mut().find(n.as_bytes())
                    };
                    match idx {
                        Some(i) => RBase::Var(vars, i as usize),
                        None => RBase::Undef(n),
                    }
                }
            }
            PlaceBase::Global(op) => {
                let n = self.read(*op)?;
                let n = self.to_zstr(&n)?;
                let g = self.globals.clone();
                let idx = if write || is_superglobal_name(n.as_bytes()) {
                    Some(g.borrow_mut().find_or_add(n.as_bytes()))
                } else {
                    g.borrow_mut().find(n.as_bytes())
                };
                match idx {
                    Some(i) => RBase::Var(g, i as usize),
                    None => RBase::Undef(n),
                }
            }
            PlaceBase::GlobalsArray => RBase::Globals,
            PlaceBase::This => RBase::This(self.frame().this.clone()),
            PlaceBase::StaticProp { class, name } => {
                let n = self.read(*name)?;
                let n = self.to_zstr(&n)?;
                let cls = self.resolve_class_spec(class)?;
                if quiet {
                    match self.static_prop_cell_is(&cls, &n)? {
                        Some(cell) => RBase::Cell(cell),
                        None => RBase::Undef(n),
                    }
                } else {
                    RBase::Cell(self.static_prop_cell(&cls, &n)?)
                }
            }
            PlaceBase::Value(op) => {
                let v = self.read_raw(*op)?;
                RBase::Value(v)
            }
        };
        let mut segs = Vec::with_capacity(p.segs.len());
        for s in &p.segs {
            segs.push(match s {
                Seg::Dim(op) => RSeg::Dim(self.read_dim_operand(*op)?),
                Seg::Append => RSeg::Append,
                Seg::Prop(op) => {
                    let v = self.read(*op)?;
                    RSeg::Prop(self.to_zstr(&v)?)
                }
            });
        }
        Ok((base, segs))
    }

    fn read_dim_operand(&mut self, op: Operand) -> VmResult<Value> {
        self.read(op)
    }

    /// Emits deferred diagnostics.
    pub(crate) fn flush_diags(&mut self, diags: Diags) -> VmResult<()> {
        for (level, msg) in diags {
            self.error(level, &msg)?;
        }
        Ok(())
    }

    pub(crate) fn walk_err(&mut self, e: WalkErr) -> Throw {
        match e {
            WalkErr::Error(m) => self.make_throw("Error", &m),
            WalkErr::TypeError(m) => self.make_throw("TypeError", &m),
        }
    }

    /// Runs a write operation on a place.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn write_place(&mut self, base: RBase, segs: &[RSeg], wop: WOp) -> VmResult<Value> {
        let r = self.write_place_inner(base, segs, wop);
        flush_deferred_ref_assigns();
        r
    }

    fn write_place_inner(&mut self, base: RBase, segs: &[RSeg], mut wop: WOp) -> VmResult<Value> {
        let mut diags = Vec::new();
        let step = match base {
            RBase::Var(store, idx) => {
                if segs.is_empty() {
                    return self.write_var_root(&store, idx, wop);
                }
                let rw = matches!(wop, WOp::FetchRw);
                let mut st = store.borrow_mut();
                let undef = st.slots[idx].is_none();
                if undef {
                    if matches!(wop, WOp::Unset) {
                        return Ok(Value::Null);
                    }
                    if rw {
                        let name = st.names[idx].clone();
                        drop(st);
                        self.warning(&crate::exec::undefined_var_msg(&name))?;
                        st = store.borrow_mut();
                    }
                    st.slots[idx] = Some(Value::Null);
                }
                let slot = st.slots[idx].as_mut().expect("slot defined");
                let s = descend(slot, segs, 0, &mut wop, &mut diags, undef);
                drop(st);
                s
            }
            RBase::This(o) => match o {
                Some(o) if segs.is_empty() => {
                    return Ok(match wop {
                        WOp::MakeRef => Value::Reference(PhpRef::new(Value::Object(o))),
                        _ => Value::Object(o),
                    });
                }
                Some(o) => Step::Obj(o, 0),
                None => {
                    return Err(self.make_throw("Error", "Using $this when not in object context"))
                }
            },
            RBase::Cell(cell) => {
                if let Some(v) = self.typed_static_write(&cell, segs, &mut wop)? {
                    return Ok(v);
                }
                if segs.is_empty() {
                    let mut b = cell.borrow_mut();
                    let s = apply_final(&mut b, &mut wop);
                    drop(b);
                    return self.finish_step(s, segs, wop);
                }
                let mut b = cell.borrow_mut();
                let s = descend(&mut b, segs, 0, &mut wop, &mut diags, false);
                drop(b);
                s
            }
            RBase::Value(mut v) => {
                if segs.is_empty() {
                    return Ok(match wop {
                        WOp::MakeRef => match v {
                            Value::Reference(r) => Value::Reference(r),
                            other => Value::Reference(PhpRef::new(other)),
                        },
                        WOp::FetchRw => v.deref(),
                        _ => Value::Null,
                    });
                }
                descend(&mut v, segs, 0, &mut wop, &mut diags, false)
            }
            RBase::Globals => {
                return Err(self.make_throw(
                    "Error",
                    "$GLOBALS can only be modified using the $GLOBALS[$name] = $value syntax",
                ));
            }
            RBase::Undef(_) => return Ok(Value::Null),
        };
        self.flush_diags(diags)?;
        self.finish_step(step, segs, wop)
    }

    fn finish_step(&mut self, step: Step, segs: &[RSeg], wop: WOp) -> VmResult<Value> {
        match step {
            Step::Done(v) => Ok(v),
            Step::Nothing => Ok(Value::Null),
            Step::Err(e) => Err(self.walk_err(e)),
            Step::TypedRefAssign(r, v) => self.assign_ref(&r, v),
            Step::Obj(o, i) => self.obj_walk(o, segs, i, wop),
        }
    }

    fn write_var_root(
        &mut self,
        store: &Rc<RefCell<VarStore>>,
        idx: usize,
        wop: WOp,
    ) -> VmResult<Value> {
        match wop {
            WOp::Assign(v) => {
                let mut st = store.borrow_mut();
                if let Some(Value::Reference(r)) = &st.slots[idx] {
                    if crate::typed_refs::has_type_sources(r) {
                        let r = r.clone();
                        drop(st);
                        return self.assign_ref(&r, v);
                    }
                }
                let old = match &mut st.slots[idx] {
                    Some(Value::Reference(r)) => Some(r.set(v.clone())),
                    slot => slot.replace(v.clone()),
                };
                drop(st);
                drop(old);
                Ok(v)
            }
            WOp::FetchRw => {
                let cur = store.borrow().slots[idx].as_ref().map(Value::deref);
                match cur {
                    Some(v) => Ok(v),
                    None => {
                        let name = store.borrow().names[idx].clone();
                        self.warning(&crate::exec::undefined_var_msg(&name))?;
                        Ok(Value::Null)
                    }
                }
            }
            WOp::MakeRef => {
                let mut st = store.borrow_mut();
                let slot = st.slots[idx].get_or_insert(Value::Null);
                Ok(Value::Reference(slot.make_ref()))
            }
            WOp::AssignRef(r) => {
                let old = store.borrow_mut().slots[idx].replace(r.clone());
                if let Some(old) = &old {
                    crate::gc::possible_root(old);
                }
                drop(old);
                Ok(r)
            }
            WOp::Unset => {
                let old = store.borrow_mut().slots[idx].take();
                if let Some(old) = &old {
                    crate::gc::possible_root(old);
                }
                drop(old);
                Ok(Value::Null)
            }
        }
    }

    /// Continues a write walk at an object.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn obj_walk(
        &mut self,
        obj: Obj,
        segs: &[RSeg],
        i: usize,
        wop: WOp,
    ) -> VmResult<Value> {
        let last = i + 1 == segs.len();
        match &segs[i] {
            RSeg::Prop(name) => {
                if last {
                    return match wop {
                        WOp::Assign(v) => self.write_property(&obj, name, v),
                        WOp::FetchRw => match self.native_read_property(
                            &obj,
                            name,
                            crate::ext::PropCtx::ReadWrite,
                        )? {
                            Some(v) => Ok(v.deref()),
                            None => self.fetch_property_rw(&obj, name),
                        },
                        WOp::MakeRef => self.property_ref(&obj, name).map(Value::Reference),
                        WOp::AssignRef(r) => self.assign_property_ref(&obj, name, r),
                        WOp::Unset => {
                            self.unset_property(&obj, name)?;
                            Ok(Value::Null)
                        }
                    };
                }
                // Intermediate: native classes provide the container through
                // read_property (`get_property_ptr_ptr` returning NULL).
                let ctx = match wop {
                    WOp::Unset => crate::ext::PropCtx::Unset,
                    WOp::FetchRw => crate::ext::PropCtx::ReadWrite,
                    _ => crate::ext::PropCtx::Write,
                };
                // Objects and references continue the walk; other values
                // fall back to the standard property storage.
                match self.native_read_property(&obj, name, ctx)? {
                    Some(Value::Object(o2)) => return self.obj_walk(o2, segs, i + 1, wop),
                    Some(Value::Reference(r)) => {
                        let mut diags = Vec::new();
                        let mut w = wop;
                        let s = {
                            let mut b = r.borrow_mut();
                            descend(&mut b, segs, i + 1, &mut w, &mut diags, false)
                        };
                        self.flush_diags(diags)?;
                        return self.finish_step(s, segs, w);
                    }
                    _ => {}
                }
                if matches!(wop, WOp::Unset) {
                    let v = self.read_property_for_unset(&obj, name)?;
                    return match v {
                        Some(Value::Object(o2)) => self.obj_walk(o2, segs, i + 1, wop),
                        Some(Value::Reference(r)) => {
                            let mut diags = Vec::new();
                            let mut w = wop;
                            let s = {
                                let mut b = r.borrow_mut();
                                descend(&mut b, segs, i + 1, &mut w, &mut diags, false)
                            };
                            self.flush_diags(diags)?;
                            self.finish_step(s, segs, w)
                        }
                        _ => Ok(Value::Null),
                    };
                }
                self.typed_prop_dim_write_check(&obj, name, &segs[i + 1], &wop)?;
                match self.property_container(&obj, name, matches!(wop, WOp::FetchRw))? {
                    PropContainer::Declared(idx) => {
                        let obj = self.lazy_object_for(&obj, Some(name))?;
                        let mut diags = Vec::new();
                        let mut w = wop;
                        let s = {
                            let d = object::data(&obj);
                            let mut slots = d.slots.borrow_mut();
                            let slot = slots[idx].get_or_insert(Value::Null);
                            descend(slot, segs, i + 1, &mut w, &mut diags, false)
                        };
                        self.flush_diags(diags)?;
                        self.finish_step(s, segs, w)
                    }
                    PropContainer::Dynamic(key) => {
                        let obj = self.lazy_object_for(&obj, Some(name))?;
                        let mut diags = Vec::new();
                        let mut w = wop;
                        let s = {
                            let d = object::data(&obj);
                            let mut dynp = d.dynamic.borrow_mut();
                            let t = dynp.get_or_insert_with(php_runtime::HashTable::new);
                            let slot = t.lookup(key);
                            descend(slot, segs, i + 1, &mut w, &mut diags, false)
                        };
                        self.flush_diags(diags)?;
                        self.finish_step(s, segs, w)
                    }
                    PropContainer::Value(v) => match v {
                        Value::Object(o2) => self.obj_walk(o2, segs, i + 1, wop),
                        mut other => {
                            let mut diags = Vec::new();
                            let mut w = wop;
                            let s = descend(&mut other, segs, i + 1, &mut w, &mut diags, false);
                            self.flush_diags(diags)?;
                            self.finish_step(s, segs, w)
                        }
                    },
                }
            }
            seg @ (RSeg::Dim(_) | RSeg::Append) => {
                let key = match seg {
                    RSeg::Dim(k) => k.clone(),
                    _ => Value::Null,
                };
                if let Some(h) = crate::ext::hooks_of(&obj) {
                    let keyopt = match seg {
                        RSeg::Dim(k) => Some(k),
                        _ => None,
                    };
                    if last {
                        match &wop {
                            WOp::Assign(v) => {
                                if let Some(f) = h.write_dimension {
                                    if f(self, &obj, keyopt, v)? {
                                        return Ok(v.clone());
                                    }
                                }
                            }
                            WOp::AssignRef(r) => {
                                if let Some(f) = h.write_dimension {
                                    if f(self, &obj, keyopt, r)? {
                                        return Ok(r.clone());
                                    }
                                }
                            }
                            WOp::Unset => {
                                if let (Some(f), Some(k)) = (h.unset_dimension, keyopt) {
                                    if f(self, &obj, k)? {
                                        return Ok(Value::Null);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    // `$o[][..] = v` reads the appended dimension (key null).
                    let read_key = if keyopt.is_none() && !last {
                        Some(&key)
                    } else {
                        keyopt
                    };
                    if let (Some(f), Some(k)) = (h.read_dimension, read_key) {
                        if !last || matches!(wop, WOp::FetchRw | WOp::MakeRef) {
                            self.dim_ctx = if keyopt.is_none() {
                                crate::ext::DimensionCtx::Append
                            } else {
                                match (&wop, last) {
                                    (WOp::FetchRw, _) => crate::ext::DimensionCtx::ReadWrite,
                                    (WOp::Unset, false) => crate::ext::DimensionCtx::Unset,
                                    _ => crate::ext::DimensionCtx::Write,
                                }
                            };
                            let r = f(self, &obj, k);
                            self.dim_ctx = crate::ext::DimensionCtx::Read;
                            if let Some(v) = r? {
                                if last {
                                    return Ok(match wop {
                                        WOp::MakeRef => Value::Reference(match v {
                                            Value::Reference(r) => r,
                                            o => PhpRef::new(o),
                                        }),
                                        _ => v.deref(),
                                    });
                                }
                                return match v {
                                    Value::Object(o2) => self.obj_walk(o2, segs, i + 1, wop),
                                    Value::Reference(rf) => {
                                        let mut diags = Vec::new();
                                        let mut w = wop;
                                        let s = {
                                            let mut b = rf.borrow_mut();
                                            descend(&mut b, segs, i + 1, &mut w, &mut diags, false)
                                        };
                                        self.flush_diags(diags)?;
                                        self.finish_step(s, segs, w)
                                    }
                                    mut other => {
                                        let mut diags = Vec::new();
                                        let mut w = wop;
                                        let s = descend(
                                            &mut other,
                                            segs,
                                            i + 1,
                                            &mut w,
                                            &mut diags,
                                            false,
                                        );
                                        self.flush_diags(diags)?;
                                        self.finish_step(s, segs, w)
                                    }
                                };
                            }
                        }
                    }
                }
                if !self.instance_of(&obj, b"arrayaccess") {
                    return Err(self.make_throw(
                        "Error",
                        &format!(
                            "Cannot use object of type {} as array",
                            object::display_name(&object::class_of(&obj).name)
                        ),
                    ));
                }
                if last {
                    let assign_ref = matches!(wop, WOp::AssignRef(_));
                    return match wop {
                        WOp::Assign(v) => {
                            self.call_method_name(&obj, "offsetSet", vec![key, v.clone()])?;
                            Ok(v)
                        }
                        // `zend_binary_assign_op_obj_dim`: `$o[] op= v` reads
                        // `offsetGet(null)` (bug69955); the write-back then
                        // calls `offsetSet(null, ...)`.
                        WOp::FetchRw => {
                            let v =
                                self.call_method_name_raw(&obj, "offsetGet", vec![key.clone()])?;
                            OVERLOADED_RW.with(|c| c.set(Some((obj, key))));
                            Ok(v)
                        }
                        WOp::Unset => {
                            self.call_method_name(&obj, "offsetUnset", vec![key])?;
                            Ok(Value::Null)
                        }
                        WOp::MakeRef | WOp::AssignRef(_) => {
                            let r = self.call_method_name_raw(&obj, "offsetGet", vec![key])?;
                            let r = match r {
                                Value::Reference(r) => r,
                                other => {
                                    // `zend_fetch_dimension_address` (W): an
                                    // object result is still usable.
                                    if !matches!(other, Value::Object(_)) {
                                        self.notice(&format!(
                                            "Indirect modification of overloaded element of {} has no effect",
                                            object::display_name(&object::class_of(&obj).name)
                                        ))?;
                                    }
                                    PhpRef::new(other)
                                }
                            };
                            if assign_ref {
                                // `ZEND_ASSIGN_REF`: the fetched dimension
                                // is not an INDIRECT slot.
                                return Err(self.make_throw(
                                    "Error",
                                    "Cannot assign by reference to an array dimension of an object",
                                ));
                            }
                            Ok(Value::Reference(r))
                        }
                    };
                }
                let r = self.call_method_name_raw(&obj, "offsetGet", vec![key])?;
                match r {
                    Value::Object(o2) => self.obj_walk(o2, segs, i + 1, wop),
                    Value::Reference(rf) => {
                        let inner = rf.get();
                        if let Value::Object(o2) = inner {
                            return self.obj_walk(o2, segs, i + 1, wop);
                        }
                        let mut diags = Vec::new();
                        let mut w = wop;
                        let s = {
                            let mut b = rf.borrow_mut();
                            descend(&mut b, segs, i + 1, &mut w, &mut diags, false)
                        };
                        self.flush_diags(diags)?;
                        self.finish_step(s, segs, w)
                    }
                    mut other => {
                        if !matches!(wop, WOp::FetchRw) {
                            self.notice(&format!(
                                "Indirect modification of overloaded element of {} has no effect",
                                object::display_name(&object::class_of(&obj).name)
                            ))?;
                        }
                        let mut diags = Vec::new();
                        let mut w = wop;
                        let s = descend(&mut other, segs, i + 1, &mut w, &mut diags, false);
                        self.flush_diags(diags)?;
                        self.finish_step(s, segs, w)
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Public place operations
    // ------------------------------------------------------------------

    /// `place = v`
    pub(crate) fn place_assign(&mut self, pid: PlaceId, v: Value) -> VmResult<Value> {
        let (base, segs) = self.resolve_place(pid, true)?;
        self.write_place(base, &segs, WOp::Assign(v))
    }

    /// The `BP_VAR_RW` read of a compound assignment's property
    /// (`zend_std_get_property_ptr_ptr`, `Zend/zend_object_handlers.c`):
    /// an error handler run by the "Undefined property" warning may define
    /// the property, and the operation then works on that value
    /// (`zend_assign_op_overloaded_property`).
    fn fetch_property_rw(&mut self, obj: &Obj, name: &ZStr) -> VmResult<Value> {
        let v = self.read_property(obj, name, FetchMode::Read)?;
        if !matches!(v, Value::Null) || self.error_handlers.is_empty() {
            return Ok(v);
        }
        Ok(stored_property(obj, name).unwrap_or(v))
    }

    /// `place = $cv` where `$cv` is undefined. `ZEND_ASSIGN_DIM`
    /// (`Zend/zend_vm_def.h`) fetches the dimension before reading
    /// `OP_DATA`, so the offsets' "Undefined variable" warnings come first
    /// (an error handler that throws on them never sees the value's
    /// warning); `ZEND_ASSIGN_OBJ` reads `OP_DATA` before the property name.
    #[cold]
    pub(crate) fn place_assign_undef_cv(&mut self, pid: PlaceId, cv: u32) -> VmResult<Value> {
        let last_is_prop = matches!(
            self.frame().code.places[pid as usize].segs.last(),
            Some(Seg::Prop(_))
        );
        if last_is_prop {
            let v = self.read_cv(cv)?;
            return self.place_assign(pid, v);
        }
        let (base, segs) = self.resolve_place(pid, true)?;
        let v = self.read_cv(cv)?;
        self.write_place(base, &segs, WOp::Assign(v))
    }

    /// `place op= v`
    pub(crate) fn place_assign_op(
        &mut self,
        pid: PlaceId,
        op: BinaryOp,
        v: Value,
    ) -> VmResult<Value> {
        let (base, segs) = self.resolve_place(pid, true)?;
        // Fast path: plain variable holding a string being appended to.
        let base2 = match &base {
            RBase::Var(s, i) => RBase::Var(s.clone(), *i),
            RBase::This(o) => RBase::This(o.clone()),
            RBase::Cell(c) => RBase::Cell(c.clone()),
            RBase::Value(v) => RBase::Value(v.clone()),
            RBase::Globals => RBase::Globals,
            RBase::Undef(n) => RBase::Undef(n.clone()),
        };
        if segs.is_empty() {
            if let RBase::Var(store, idx) = &base {
                if op == BinaryOp::Concat {
                    if let Value::Str(add) = &v {
                        let mut st = store.borrow_mut();
                        if let Some(Value::Str(s)) = &mut st.slots[*idx] {
                            s.append(add.as_bytes());
                            return Ok(Value::Str(s.clone()));
                        }
                    }
                }
            }
        }
        let (cur, overloaded) = self.fetch_rw(base, &segs)?;
        if let Some((obj, key)) = overloaded {
            // `zend_binary_assign_op_obj_dim`: offsetGet, op, offsetSet.
            let nv = ops::binary_op(op, &cur.deref(), &v, self).map_err(|e| self.rt_throw(e))?;
            drop(cur);
            self.call_method_name(&obj, "offsetSet", vec![key, nv.clone()])?;
            return Ok(nv);
        }
        let nv = ops::binary_op(op, &cur, &v, self).map_err(|e| self.rt_throw(e))?;
        drop(cur);
        self.write_place(base2, &segs, WOp::Assign(nv))
    }

    /// The read half of a read-modify-write: the current value and, when
    /// the walk ended on an `ArrayAccess` offset, that object and offset.
    fn fetch_rw(&mut self, base: RBase, segs: &[RSeg]) -> VmResult<(Value, Option<(Obj, Value)>)> {
        if !matches!(segs.last(), Some(RSeg::Dim(_) | RSeg::Append)) {
            return Ok((self.write_place(base, segs, WOp::FetchRw)?, None));
        }
        // Save and restore around the walk: user code it runs (offsetGet,
        // destructors) may perform read-modify-writes of its own.
        let saved = OVERLOADED_RW.with(std::cell::Cell::take);
        let r = self.write_place(base, segs, WOp::FetchRw);
        let mine = OVERLOADED_RW.with(|c| c.replace(saved));
        Ok((r?, mine))
    }

    /// `++place` etc.
    pub(crate) fn place_incdec(&mut self, pid: PlaceId, kind: IncDecKind) -> VmResult<Value> {
        // Fast path: `$i++` on a compiled variable holding an int (no
        // overflow), before resolving the place generically.
        {
            let f = self.frame();
            let p = &f.code.places[pid as usize];
            if let (PlaceBase::Cv(cv), true) = (&p.base, p.segs.is_empty()) {
                let idx = f.cv_idx(*cv);
                let mut st = f.vars.borrow_mut();
                if let Some(Some(Value::Int(i))) = st.slots.get_mut(idx) {
                    let old = *i;
                    let new = if kind.is_inc() {
                        old.checked_add(1)
                    } else {
                        old.checked_sub(1)
                    };
                    if let Some(n) = new {
                        *i = n;
                        return Ok(Value::Int(if kind.is_post() { old } else { n }));
                    }
                }
            }
        }
        let (base, segs) = self.resolve_place(pid, true)?;
        let base2 = match &base {
            RBase::Var(s, i) => RBase::Var(s.clone(), *i),
            RBase::This(o) => RBase::This(o.clone()),
            RBase::Cell(c) => RBase::Cell(c.clone()),
            RBase::Value(v) => RBase::Value(v.clone()),
            RBase::Globals => RBase::Globals,
            RBase::Undef(n) => RBase::Undef(n.clone()),
        };
        // Fast path for integer variables.
        if segs.is_empty() {
            if let RBase::Var(store, idx) = &base {
                let mut st = store.borrow_mut();
                if let Some(slot) = &mut st.slots[*idx] {
                    let target = match slot {
                        Value::Reference(_) => None,
                        s => Some(s),
                    };
                    if let Some(Value::Int(i)) = target {
                        let old = *i;
                        let new = if kind.is_inc() {
                            old.checked_add(1)
                        } else {
                            old.checked_sub(1)
                        };
                        if let Some(n) = new {
                            *i = n;
                            return Ok(Value::Int(if kind.is_post() { old } else { n }));
                        }
                    }
                }
            }
        }
        let (cur, overloaded) = self.fetch_rw(base, &segs)?;
        // `FETCH_DIM_RW` on an object: a non-reference result is a
        // temporary, so the increment is not written back.
        let temporary = match &overloaded {
            Some((obj, _)) if !matches!(cur, Value::Reference(_)) => {
                if !matches!(cur, Value::Object(_)) {
                    self.notice(&format!(
                        "Indirect modification of overloaded element of {} has no effect",
                        object::display_name(&object::class_of(obj).name)
                    ))?;
                }
                true
            }
            _ => false,
        };
        // Objects: property inc/dec of undefined, string offsets checked.
        let mut nv = cur.deref();
        let r = if kind.is_inc() {
            ops::increment(&mut nv, self)
        } else {
            ops::decrement(&mut nv, self)
        };
        if let Err(e) = r {
            let err = self.rt_throw(e);
            return Err(self.incdec_write_back(base2, &segs, nv, err));
        }
        let old = cur.deref();
        if temporary {
            return Ok(if kind.is_post() { old } else { nv });
        }
        let guard = crate::typed_refs::IncDecGuard::new(kind.is_inc(), &old, &nv);
        let res = if let Value::Reference(r) = &cur {
            self.assign_ref(r, nv)
        } else {
            self.write_place(base2, &segs, WOp::Assign(nv))
        };
        drop(guard);
        let res = res?;
        Ok(if kind.is_post() { old } else { res })
    }

    /// `zend_{pre,post}_incdec_overloaded_property`: a property that is
    /// not modified in place (readonly, hooked, magic, asymmetric
    /// visibility) is written back even when the increment failed, so the
    /// write's own error ("Cannot modify readonly property ...") becomes
    /// the exception, with the `TypeError` as its previous one. Writing an
    /// ordinary property back is unobservable.
    #[cold]
    fn incdec_write_back(&mut self, base: RBase, segs: &[RSeg], old: Value, err: Throw) -> Throw {
        if !matches!(segs.last(), Some(RSeg::Prop(_))) {
            return err;
        }
        match self.write_place(base, segs, WOp::Assign(old)) {
            Ok(_) => err,
            Err(Throw::Exception(new)) => {
                if let Throw::Exception(prev) = &err {
                    crate::exceptions::chain_previous(&new, prev);
                }
                Throw::Exception(new)
            }
            Err(t) => t,
        }
    }

    /// `place =& v`
    pub(crate) fn place_assign_ref(&mut self, pid: PlaceId, v: Value) -> VmResult<Value> {
        let v = match v {
            Value::Reference(r) => Value::Reference(r),
            other => {
                self.notice("Only variables should be assigned by reference")?;
                return self.place_assign(pid, other);
            }
        };
        let (base, segs) = self.resolve_place(pid, true)?;
        self.write_place(base, &segs, WOp::AssignRef(v))
    }

    /// Makes a place a reference.
    pub(crate) fn place_make_ref(&mut self, pid: PlaceId) -> VmResult<PhpRef> {
        let (base, segs) = self.resolve_place(pid, true)?;
        match self.write_place(base, &segs, WOp::MakeRef)? {
            Value::Reference(r) => Ok(r),
            other => Ok(PhpRef::new(other)),
        }
    }

    /// `unset(place)`
    pub(crate) fn place_unset(&mut self, pid: PlaceId) -> VmResult<()> {
        let code = self.frame().code.clone();
        let p = &code.places[pid as usize];
        if let (PlaceBase::StaticProp { class, name }, true) = (&p.base, p.segs.is_empty()) {
            return Err(self.unset_static_prop(class, *name));
        }
        let (base, segs) = self.resolve_place(pid, false)?;
        if let RBase::Undef(_) = &base {
            return Ok(());
        }
        if segs.is_empty() {
            if let RBase::Cell(_) = base {
                return Err(self.make_throw("Error", "Attempt to unset static property"));
            }
            if let RBase::This(_) = base {
                return Ok(());
            }
        }
        self.write_place(base, &segs, WOp::Unset).map(|_| ())
    }

    /// `ZEND_UNSET_STATIC_PROP`: the class is fetched, then the name, and
    /// `zend_std_unset_static_property` always throws.
    #[cold]
    fn unset_static_prop(&mut self, class: &crate::opcode::ClassSpec, name: Operand) -> Throw {
        let cls = match self.resolve_class_spec(class) {
            Ok(c) => c,
            Err(t) => return t,
        };
        let n = match self.read(name).and_then(|v| self.to_zstr(&v)) {
            Ok(n) => n,
            Err(t) => return t,
        };
        self.make_throw(
            "Error",
            &format!(
                "Attempt to unset static property {}::${}",
                object::display_name(&cls.name),
                n.to_string_lossy()
            ),
        )
    }

    /// Reads a place.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn place_fetch(&mut self, pid: PlaceId, mode: FetchMode) -> VmResult<Value> {
        // Fast path: reading a plain compiled variable (`f($x)`).
        if mode == FetchMode::Read {
            let p = &self.frame().code.places[pid as usize];
            if let (PlaceBase::Cv(cv), true) = (&p.base, p.segs.is_empty()) {
                let cv = *cv;
                return self.read_cv(cv);
            }
        }
        let (base, segs) = self.resolve_place_ex(pid, false, mode == FetchMode::Is)?;
        let mut cur = match base {
            RBase::Var(store, idx) => {
                let v = store.borrow().slots[idx].as_ref().map(Value::deref);
                match v {
                    Some(v) => v,
                    None => {
                        if mode != FetchMode::Is {
                            let name = store.borrow().names[idx].clone();
                            self.warning(&crate::exec::undefined_var_msg(&name))?;
                        }
                        Value::Null
                    }
                }
            }
            RBase::This(o) => match o {
                Some(o) => Value::Object(o),
                None => {
                    if mode == FetchMode::Is {
                        Value::Null
                    } else {
                        return Err(
                            self.make_throw("Error", "Using $this when not in object context")
                        );
                    }
                }
            },
            RBase::Cell(c) => {
                self.typed_static_read_check(&c, mode)?;
                c.borrow().deref()
            }
            RBase::Value(v) => v.deref(),
            RBase::Undef(name) => {
                if mode != FetchMode::Is {
                    self.warning(&crate::exec::undefined_var_msg(&name))?;
                }
                Value::Null
            }
            RBase::Globals => Value::Array(ZArray::from_table(self.globals.borrow().to_array())),
        };
        for seg in segs.iter() {
            cur = match seg {
                RSeg::Dim(k) => self.fetch_dim(cur, k, mode)?,
                RSeg::Append => {
                    return Err(self.make_throw("Error", "Cannot use [] for reading"));
                }
                RSeg::Prop(name) => match cur {
                    Value::Object(o) => self.read_property(&o, name, mode)?,
                    other => {
                        if mode == FetchMode::Read {
                            self.warning(&format!(
                                "Attempt to read property \"{}\" on {}",
                                name.to_string_lossy(),
                                other.value_name()
                            ))?;
                        }
                        Value::Null
                    }
                },
            };
            if mode == FetchMode::Is && cur.is_null() {
                return Ok(Value::Null);
            }
        }
        Ok(cur)
    }

    /// One dimension read (`zend_fetch_dimension_address_read`).
    pub(crate) fn fetch_dim(&mut self, cur: Value, k: &Value, mode: FetchMode) -> VmResult<Value> {
        match cur {
            Value::Array(a) => {
                let mut diags = Vec::new();
                let ctx = if mode == FetchMode::Is {
                    KeyCtx::Isset
                } else {
                    KeyCtx::Read
                };
                let key = match key_of(k, &mut diags, ctx) {
                    Ok(k) => k,
                    Err(e) => {
                        self.flush_diags(diags)?;
                        if mode == FetchMode::Is {
                            if let WalkErr::TypeError(_) = e {
                                // isset() with illegal offset type throws too.
                            }
                        }
                        return Err(self.walk_err(e));
                    }
                };
                self.flush_diags(diags)?;
                match a.get(&key) {
                    Some(v) => Ok(v.deref()),
                    None => {
                        if mode != FetchMode::Is {
                            self.warning(&undefined_key_msg(&key))?;
                        }
                        Ok(Value::Null)
                    }
                }
            }
            Value::Str(s) => {
                if mode == FetchMode::List {
                    // zend_fetch_dimension_address_read with is_list: a
                    // string is not destructured.
                    self.warning("Cannot use string as array")?;
                    return Ok(Value::Null);
                }
                let mut diags = Vec::new();
                let off = match k {
                    Value::Str(ks) if mode == FetchMode::Is => {
                        match php_runtime::numeric::is_numeric_string_ex(ks.as_bytes(), true) {
                            Some(n) if !n.trailing_data => match n.value {
                                php_runtime::numeric::Number::Int(i) => i,
                                php_runtime::numeric::Number::Float(_) => return Ok(Value::Null),
                            },
                            _ => return Ok(Value::Null),
                        }
                    }
                    _ => {
                        if mode == FetchMode::Is {
                            match k {
                                Value::Int(i) => *i,
                                Value::Float(d) => php_runtime::convert::dval_to_lval_silent(*d),
                                Value::Bool(b) => i64::from(*b),
                                Value::Null => 0,
                                _ => return Ok(Value::Null),
                            }
                        } else {
                            match string_offset(k, &mut diags, false) {
                                Ok(o) => o,
                                Err(e) => {
                                    self.flush_diags(diags)?;
                                    return Err(self.walk_err(e));
                                }
                            }
                        }
                    }
                };
                self.flush_diags(diags)?;
                let len = i64::try_from(s.len()).unwrap_or(i64::MAX);
                let real = if off < 0 { off + len } else { off };
                if real < 0 || real >= len {
                    if mode == FetchMode::Is {
                        return Ok(Value::Null);
                    }
                    self.warning(&format!("Uninitialized string offset {off}"))?;
                    return Ok(Value::Str(ZStr::empty()));
                }
                Ok(Value::Str(ZStr::from_byte(
                    s.as_bytes()[usize::try_from(real).unwrap_or(0)],
                )))
            }
            Value::Object(o) => {
                if let Some(h) = crate::ext::hooks_of(&o) {
                    if mode == FetchMode::Is {
                        if let Some(f) = h.has_dimension {
                            if let Some(false) = f(self, &o, k, false)? {
                                return Ok(Value::Null);
                            }
                        }
                    }
                    if let Some(f) = h.read_dimension {
                        self.dim_ctx = if mode == FetchMode::Is {
                            crate::ext::DimensionCtx::Is
                        } else {
                            crate::ext::DimensionCtx::Read
                        };
                        let r = f(self, &o, k);
                        self.dim_ctx = crate::ext::DimensionCtx::Read;
                        if let Some(v) = r? {
                            return Ok(v.deref());
                        }
                    }
                }
                if !self.instance_of(&o, b"arrayaccess") {
                    return Err(self.make_throw(
                        "Error",
                        &format!(
                            "Cannot use object of type {} as array",
                            object::display_name(&object::class_of(&o).name)
                        ),
                    ));
                }
                if mode == FetchMode::Is {
                    let e = self.call_method_name(&o, "offsetExists", vec![k.clone()])?;
                    if !self.to_bool(&e)? {
                        return Ok(Value::Null);
                    }
                }
                self.call_method_name(&o, "offsetGet", vec![k.clone()])
            }
            Value::Null => {
                if mode == FetchMode::Read {
                    self.warning("Trying to access array offset on null")?;
                }
                Ok(Value::Null)
            }
            other => {
                match mode {
                    FetchMode::Read => {
                        self.warning(&format!(
                            "Trying to access array offset on {}",
                            other.value_name()
                        ))?;
                    }
                    FetchMode::List => {
                        self.warning(&format!("Cannot use {} as array", other.type_name()))?;
                    }
                    FetchMode::Is => {}
                }
                Ok(Value::Null)
            }
        }
    }

    /// `isset(place)` / `empty(place)` (returns the isset or empty result).
    #[allow(clippy::too_many_lines)]
    pub(crate) fn place_isset(&mut self, pid: PlaceId, empty: bool) -> VmResult<bool> {
        let (base, segs) = self.resolve_place_ex(pid, false, true)?;
        let mut cur = match base {
            RBase::Var(store, idx) => store.borrow().slots[idx].as_ref().map(Value::deref),
            RBase::This(o) => o.map(Value::Object),
            RBase::Cell(c) => {
                (!crate::typed_refs::static_cell_uninit(&c)).then(|| c.borrow().deref())
            }
            RBase::Value(v) => Some(v.deref()),
            RBase::Undef(_) => None,
            RBase::Globals => Some(Value::Array(ZArray::from_table(
                self.globals.borrow().to_array(),
            ))),
        };
        let n = segs.len();
        if n == 0 {
            return Ok(match cur {
                None => empty,
                Some(v) => {
                    if empty {
                        !self.to_bool(&v)?
                    } else {
                        !v.is_null()
                    }
                }
            });
        }
        for (i, seg) in segs.iter().enumerate() {
            let Some(c) = cur.take() else {
                return Ok(empty);
            };
            if c.is_null() {
                return Ok(empty);
            }
            let last = i + 1 == n;
            if !last {
                let v = match seg {
                    RSeg::Dim(k) => self.fetch_dim(c, k, FetchMode::Is)?,
                    RSeg::Prop(name) => match c {
                        Value::Object(o) => self.read_property(&o, name, FetchMode::Is)?,
                        _ => Value::Null,
                    },
                    RSeg::Append => {
                        return Err(self.make_throw("Error", "Cannot use [] for reading"))
                    }
                };
                cur = Some(v);
                continue;
            }
            // Last segment.
            return match seg {
                RSeg::Dim(k) => match c {
                    Value::Array(a) => {
                        // The lookup snapshot is Zend's temporary GC addref,
                        // not an owner that survives a diagnostic callback.
                        let guarded = matches!(k, Value::Float(_) | Value::Null);
                        let had_owner = guarded && a.refcount() > 1;
                        let mut diags = Vec::new();
                        let key = match key_of(k, &mut diags, KeyCtx::Isset) {
                            Ok(k) => k,
                            Err(e) => return Err(self.walk_err(e)),
                        };
                        // isset() does not emit the null-offset deprecation twice.
                        self.flush_diags(diags)?;
                        if had_owner && a.refcount() == 1 {
                            return Ok(empty);
                        }
                        match a.get(&key) {
                            None => Ok(empty),
                            Some(v) => {
                                let v = v.deref();
                                if empty {
                                    Ok(!self.to_bool(&v)?)
                                } else {
                                    Ok(!v.is_null())
                                }
                            }
                        }
                    }
                    Value::Str(s) => {
                        let off = match k {
                            Value::Int(i) => Some(*i),
                            Value::Str(ks) => match php_runtime::numeric::is_numeric_string_ex(
                                ks.as_bytes(),
                                false,
                            ) {
                                Some(n) => match n.value {
                                    php_runtime::numeric::Number::Int(i) => Some(i),
                                    php_runtime::numeric::Number::Float(_) => None,
                                },
                                None => None,
                            },
                            Value::Float(d) => Some(php_runtime::convert::dval_to_lval_silent(*d)),
                            Value::Bool(b) => Some(i64::from(*b)),
                            Value::Null => Some(0),
                            _ => None,
                        };
                        let len = i64::try_from(s.len()).unwrap_or(0);
                        match off {
                            Some(o) => {
                                let real = if o < 0 { o + len } else { o };
                                if real < 0 || real >= len {
                                    Ok(empty)
                                } else if empty {
                                    Ok(s.as_bytes()[usize::try_from(real).unwrap_or(0)] == b'0')
                                } else {
                                    Ok(true)
                                }
                            }
                            None => Ok(empty),
                        }
                    }
                    Value::Object(o) => {
                        if let Some(f) = crate::ext::hooks_of(&o).and_then(|h| h.has_dimension) {
                            if let Some(r) = f(self, &o, k, empty)? {
                                return Ok(if empty { !r } else { r });
                            }
                        }
                        if !self.instance_of(&o, b"arrayaccess") {
                            return Err(self.make_throw(
                                "Error",
                                &format!(
                                    "Cannot use object of type {} as array",
                                    object::display_name(&object::class_of(&o).name)
                                ),
                            ));
                        }
                        let e = self.call_method_name(&o, "offsetExists", vec![k.clone()])?;
                        let exists = self.to_bool(&e)?;
                        if !empty {
                            return Ok(exists);
                        }
                        if !exists {
                            return Ok(true);
                        }
                        let v = self.call_method_name(&o, "offsetGet", vec![k.clone()])?;
                        Ok(!self.to_bool(&v)?)
                    }
                    _ => Ok(empty),
                },
                RSeg::Prop(name) => match c {
                    Value::Object(o) => self.has_property(&o, name, empty),
                    _ => Ok(empty),
                },
                RSeg::Append => Err(self.make_throw("Error", "Cannot use [] for reading")),
            };
        }
        Ok(empty)
    }

    /// A reference to a global variable (`global $x`).
    pub(crate) fn global_ref(&mut self, name: &[u8]) -> PhpRef {
        let g = self.globals.clone();
        let mut st = g.borrow_mut();
        let idx = st.find_or_add(name) as usize;
        let slot = st.slots[idx].get_or_insert(Value::Null);
        slot.make_ref()
    }
}

fn is_superglobal_name(n: &[u8]) -> bool {
    matches!(
        n,
        b"_GET"
            | b"_POST"
            | b"_COOKIE"
            | b"_SERVER"
            | b"_ENV"
            | b"_REQUEST"
            | b"_FILES"
            | b"_SESSION"
    )
}

/// Where a property's value lives for nested writes.
pub(crate) enum PropContainer {
    /// A declared property slot.
    Declared(usize),
    /// A dynamic property.
    Dynamic(ArrayKey),
    /// A value returned by `__get` (modifications are lost).
    Value(Value),
}

/// The value stored for `$obj->name` (a declared slot without hooks, or a
/// dynamic property), bypassing magic; `None` when unset or when `__get()`
/// or a lazy initializer would be involved.
fn stored_property(obj: &Obj, name: &ZStr) -> Option<Value> {
    let d = object::data(obj);
    if d.class.magic.get.is_some() || d.lazy.borrow().is_some() {
        return None;
    }
    match d.class.prop_map.get(name) {
        Some(pi) if pi.hooks.is_none() => d
            .slots
            .borrow()
            .get(pi.slot as usize)
            .and_then(|s| s.as_ref().map(Value::deref)),
        Some(_) => None,
        None => d
            .dynamic
            .borrow()
            .as_ref()
            .and_then(|t| t.get(&ArrayKey::Str(name.clone())).map(Value::deref)),
    }
}
