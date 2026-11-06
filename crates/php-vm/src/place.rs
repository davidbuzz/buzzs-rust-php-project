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
#[derive(Clone)]
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
    /// `place op= v` (`ZEND_ASSIGN_DIM_OP` / `ZEND_ASSIGN_OBJ_OP`): walks
    /// like [`WOp::FetchRw`] once, so offset diagnostics and object
    /// handlers run a single time. Object handlers at the last segment
    /// read, operate and write back themselves; a final slot inside a
    /// container yields its current value (`Step::Done`) and is then
    /// written by a silent second walk (see `Vm::finish_native`).
    AssignOp(BinaryOp, Value),
    MakeRef,
    AssignRef(Value),
    Unset,
    /// `place = $cv` where `$cv` is a reference to null, false or an array:
    /// `ZEND_ASSIGN_DIM` reads `OP_DATA` only after fetching the container
    /// for writing, so the value is read at the final slot (see
    /// [`Vm::place_assign_cv_ref`]). Boxed: rare, and `WOp` stays small.
    AssignDeref(Box<DeferredRead>),
}

/// The source of a [`WOp::AssignDeref`] and what the walk found.
pub(crate) struct DeferredRead {
    /// The reference whose value is assigned.
    src: PhpRef,
    hit: DeferredHit,
}

/// Where a [`WOp::AssignDeref`] walk left the assignment.
enum DeferredHit {
    /// Not reached the final slot, or the value was assigned there (the
    /// source was not a container on the walked path).
    None,
    /// The source is a container on the path (its borrow is held by the
    /// walk): a placeholder was stored in the final slot, which then
    /// receives a handle to the source's array (`$t = &$a; $a[] = $t;`).
    Slot,
    /// As `Slot`, but the final slot is this reference: the source's value
    /// is assigned through it once the walk has released its borrows.
    Ref(PhpRef),
}

impl WOp {
    /// Turns a [`WOp::AssignDeref`] into a plain assignment of the source's
    /// current value (an object handler continues the walk, or the walk is
    /// over), releasing the source handle without making it a GC root.
    pub(crate) fn resolve_deferred(self) -> WOp {
        match self {
            WOp::AssignDeref(d) => {
                let DeferredRead { src, hit } = *d;
                if let DeferredHit::Ref(r) = hit {
                    crate::gc::release_duplicate(Value::Reference(r));
                }
                let v = src.try_borrow().map_or(Value::Null, |b| b.clone());
                crate::gc::release_duplicate(Value::Reference(src));
                WOp::Assign(v)
            }
            other => other,
        }
    }
}

/// How a static property base is fetched for writing, for the check of
/// its set visibility (`zend_asymmetric_property_has_set_access`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StaticWrite {
    /// `ZEND_ASSIGN_STATIC_PROP`: "Cannot modify".
    Assign,
    /// Compound assignment, increment/decrement, reference binding or
    /// taking: "Cannot indirectly modify".
    Indirect,
    /// `FETCH_STATIC_PROP_W`/`_RW` of a container: an object passes.
    Fetch,
    /// `FETCH_STATIC_PROP_UNSET` of a container: an object or an
    /// uninitialized property passes.
    FetchUnset,
}

/// The operation a place is resolved for (see [`StaticWrite`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlaceOp {
    Other,
    Assign,
    Unset,
}

/// Deferred diagnostics: (level bits, message).
type Diags = Vec<(i64, String)>;

/// Errors raised inside a container walk (converted to exceptions later).
pub(crate) enum WalkErr {
    Error(String),
    TypeError(String),
    /// A string offset is assigned an object: its string conversion
    /// (`zval_try_get_string_func`, maybe `__toString()`) needs the VM, so
    /// the walk stops after the offset checks and the caller converts the
    /// value and writes its first byte with a silent second walk.
    StringOffsetObject,
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

/// String offset conversion (`zend_check_string_offset`). `unset` is the
/// `BP_VAR_UNSET` fetch type: no leading-numeric warning, and illegal
/// offsets report "Cannot unset string offsets".
fn string_offset(dim: &Value, diags: &mut Diags, unset: bool) -> Result<i64, WalkErr> {
    match dim {
        Value::Int(i) => Ok(*i),
        Value::Str(s) => {
            if let Some(n) = php_runtime::numeric::is_numeric_string_ex(s.as_bytes(), true) {
                if let php_runtime::numeric::Number::Int(i) = n.value {
                    if n.trailing_data && !unset {
                        diags.push((
                            E_WARNING,
                            format!("Illegal string offset \"{}\"", s.to_string_lossy()),
                        ));
                    }
                    return Ok(i);
                }
            }
            if unset {
                return Err(WalkErr::Error("Cannot unset string offsets".into()));
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
        Value::Reference(r) => string_offset(&r.get(), diags, unset),
        _ if unset => Err(WalkErr::Error("Cannot unset string offsets".into())),
        other => Err(WalkErr::TypeError(format!(
            "Cannot access offset of type {} on string",
            crate::typecheck::given_type(other)
        ))),
    }
}

/// String offset conversion of a quiet read (`zend_fetch_dimension_address_read`
/// with `BP_VAR_IS`): casts are silent and a non-numeric string gives
/// `None`, but a leading-numeric string still warns and an illegal offset
/// type still throws.
fn string_offset_is(dim: &Value, diags: &mut Diags) -> Result<Option<i64>, WalkErr> {
    use php_runtime::numeric::Number;
    match dim {
        Value::Int(i) => Ok(Some(*i)),
        Value::Str(s) => match php_runtime::numeric::is_numeric_string_ex(s.as_bytes(), true) {
            Some(n) => match n.value {
                Number::Int(i) => {
                    if n.trailing_data {
                        diags.push((
                            E_WARNING,
                            format!("Illegal string offset \"{}\"", s.to_string_lossy()),
                        ));
                    }
                    Ok(Some(i))
                }
                Number::Float(_) => Ok(None),
            },
            None => Ok(None),
        },
        Value::Float(d) => Ok(Some(php_runtime::convert::dval_to_lval_silent(*d))),
        Value::Bool(b) => Ok(Some(i64::from(*b))),
        Value::Null => Ok(Some(0)),
        Value::Reference(r) => string_offset_is(&r.get(), diags),
        other => Err(WalkErr::TypeError(format!(
            "Cannot access offset of type {} on string",
            crate::typecheck::given_type(other)
        ))),
    }
}

/// A string container in a write walk. Only a final plain assignment
/// writes (`zend_assign_to_string_offset`); every other context checks the
/// offset first (`zend_check_string_offset`, with its warnings) and then
/// throws the context's error: `zend_use_new_element_for_string`,
/// `ZEND_UNSET_DIM`'s "Cannot unset string offsets" and
/// `zend_wrong_string_offset_error`.
#[cold]
fn string_container_step(
    v: &mut Value,
    segs: &[RSeg],
    i: usize,
    wop: &mut WOp,
    diags: &mut Diags,
) -> Step {
    let last = i + 1 == segs.len();
    let unset = matches!(wop, WOp::Unset);
    if unset && last {
        return Step::Err(WalkErr::Error("Cannot unset string offsets".into()));
    }
    let RSeg::Dim(dim) = &segs[i] else {
        return Step::Err(WalkErr::Error(
            "[] operator not supported for strings".into(),
        ));
    };
    if last {
        if let WOp::AssignDeref(d) = wop {
            // The source is null, false or an array (one holding this
            // string when the walk holds its borrow): converting any array
            // to a string offset value behaves alike.
            let val = d
                .src
                .try_borrow()
                .map_or_else(|| Value::Array(ZArray::new()), |b| b.clone());
            return string_container_step(v, segs, i, &mut WOp::Assign(val), diags);
        }
        if let WOp::Assign(val) = wop {
            if dim.is_null() {
                if let Some(cv) = take_pending_undef_dim(segs) {
                    // `zend_assign_to_string_offset` separates the string
                    // and holds it across `zend_check_string_offset`'s
                    // warnings: the VM raises them (`Vm::undef_dim_diags`).
                    if let Value::Str(s) = v {
                        if s.is_interned() || s.refcount() != 1 {
                            *s = ZStr::new(s.as_bytes().to_vec());
                        }
                        let hit = UndefDimHit::Str(s.clone());
                        UNDEF_DIM_HIT.with(|c| c.set(Some((cv, hit))));
                    }
                    return Step::Done(Value::Null);
                }
            }
            let val = val.clone();
            return match assign_string_offset(v, dim, &val, diags) {
                Ok(r) => Step::Done(r),
                Err(e) => Step::Err(e),
            };
        }
    }
    if let Err(e) = string_offset(dim, diags, unset) {
        return Step::Err(e);
    }
    let msg = if last {
        match wop {
            WOp::FetchRw => "Cannot increment/decrement string offsets",
            WOp::MakeRef | WOp::AssignRef(_) => "Cannot create references to/from string offsets",
            _ => "Cannot use assign-op operators with string offsets",
        }
    } else if matches!(segs[i + 1], RSeg::Prop(_)) {
        // FETCH_DIM_W/RW/UNSET feeding a property or dimension fetch.
        "Cannot use string offset as an object"
    } else {
        "Cannot use string offset as an array"
    };
    Step::Err(WalkErr::Error(msg.into()))
}

/// The string form of a value written to a string offset; its first byte
/// is stored (`zend_assign_to_string_offset`).
fn string_offset_value_bytes(val: &Value) -> Vec<u8> {
    match val {
        Value::Str(v) => v.as_bytes().to_vec(),
        Value::Int(i) => i.to_string().into_bytes(),
        Value::Bool(true) => b"1".to_vec(),
        Value::Bool(false) | Value::Null => Vec::new(),
        Value::Float(f) => php_runtime::float::format_double(*f, 17, false).into_bytes(),
        Value::Array(_) => b"Array".to_vec(),
        Value::Resource(r) => format!("Resource id #{}", r.handle()).into_bytes(),
        // Objects are converted by `__toString()` on the retry path
        // (`assign_string_offset` reports `StringOffsetObject`).
        Value::Object(_) | Value::Reference(_) => b"A".to_vec(),
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
    let offset = string_offset(dim, diags, false)?;
    let len = i64::try_from(str.len()).unwrap_or(i64::MAX);
    if offset < -len {
        diags.push((E_WARNING, format!("Illegal string offset {offset}")));
        return Ok(Value::Null);
    }
    let offset = if offset < 0 { offset + len } else { offset };
    let bytes: Vec<u8> = match val.deref() {
        Value::Str(v) => v.as_bytes().to_vec(),
        Value::Int(i) => i.to_string().into_bytes(),
        Value::Bool(true) => b"1".to_vec(),
        Value::Bool(false) | Value::Null => Vec::new(),
        Value::Float(f) => php_runtime::float::format_double(f, 17, false).into_bytes(),
        // `zval_try_get_string_func`.
        Value::Array(_) => {
            diags.push((E_WARNING, "Array to string conversion".into()));
            b"Array".to_vec()
        }
        Value::Resource(r) => format!("Resource id #{}", r.handle()).into_bytes(),
        Value::Object(_) | Value::Reference(_) => return Err(WalkErr::StringOffsetObject),
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
        // Walked through the container's own handle: a temporary clone
        // released afterwards would be buffered as a GC possible root
        // (php-src never touches the count here).
        let r: &PhpRef = r;
        if r.try_borrow().is_none() {
            return Step::Err(WalkErr::Error(
                "Cannot modify a value while it is being iterated".into(),
            ));
        }
        if !matches!(wop, WOp::Unset) && !matches!(segs[i], RSeg::Prop(_)) {
            if let Some(msg) = crate::typed_refs::walk_auto_init_error(r) {
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
                // `zend_throw_non_object_error`: ASSIGN_OBJ and
                // ASSIGN_OBJ_OP "assign", PRE/POST_INC/DEC_OBJ
                // "increment/decrement", FETCH_OBJ_W/RW "modify".
                if last
                    && matches!(
                        wop,
                        WOp::Assign(_) | WOp::AssignOp(..) | WOp::AssignDeref(_)
                    )
                {
                    Step::Err(WalkErr::Error(format!(
                        "Attempt to assign property \"{}\" on {tn}",
                        name.to_string_lossy()
                    )))
                } else if last && matches!(wop, WOp::FetchRw) {
                    // A final FetchRw walk is the read of `++`/`--`
                    // (compound assignments walk with `WOp::AssignOp`).
                    Step::Err(WalkErr::Error(format!(
                        "Attempt to increment/decrement property \"{}\" on {tn}",
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
            // Whether this container was `false` just converted to an
            // array (`zend_false_to_array_deprecated`).
            let mut from_false = false;
            match v {
                Value::Array(_) => {}
                Value::Null => {
                    if is_unset {
                        return Step::Done(Value::Null);
                    }
                    *v = Value::Array(ZArray::new());
                }
                Value::Bool(false) => {
                    diags.push((
                        E_DEPRECATED,
                        "Automatic conversion of false to array is deprecated".into(),
                    ));
                    if is_unset {
                        // UNSET_DIM / FETCH_DIM_UNSET leave `false` alone.
                        return Step::Done(Value::Null);
                    }
                    *v = Value::Array(ZArray::new());
                    from_false = true;
                }
                Value::Object(o) => return Step::Obj(o.clone(), i),
                Value::Str(_) => return string_container_step(v, segs, i, wop, diags),
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
                            _ => {
                                // The key `push_mut` uses (`nNextFreeElement`).
                                let slot_key = t.next_index();
                                match t.push_mut(Value::Null) {
                                    Ok(slot) => {
                                        if matches!(wop, WOp::AssignOp(..)) {
                                            record_append(i, slot_key);
                                        }
                                        apply_final(slot, wop)
                                    }
                                    Err(_) => Step::Err(WalkErr::Error(
                                        "Cannot add element to the array as the next element is already occupied".into(),
                                    )),
                                }
                            }
                        }
                    } else {
                        let slot_key = t.next_index();
                        match t.push_mut(Value::Null) {
                            Ok(slot) => {
                                if matches!(wop, WOp::AssignOp(..)) {
                                    record_append(i, slot_key);
                                }
                                descend(slot, segs, i + 1, wop, diags, true)
                            }
                            Err(_) => {
                                Step::Err(WalkErr::Error(
                                        "Cannot add element to the array as the next element is already occupied".into(),
                                    ))
                            }
                        }
                    }
                }
                RSeg::Dim(k) => {
                    if last && k.is_null() && matches!(wop, WOp::Assign(_)) {
                        if let Some(cv) = take_pending_undef_dim(segs) {
                            // `slow_index_convert_w` holds the (separated)
                            // array across the "Undefined variable" warning
                            // and the null-offset deprecation.
                            arr.make_mut();
                            let hit = UndefDimHit::Array(arr.clone(), from_false);
                            UNDEF_DIM_HIT.with(|c| c.set(Some((cv, hit))));
                            return Step::Done(Value::Null);
                        }
                    }
                    if last
                        && matches!(
                            k,
                            Value::Float(_)
                                | Value::Null
                                | Value::Resource(_)
                                | Value::Reference(_)
                        )
                        && matches!(wop, WOp::Assign(_))
                        && take_pending_convert_dim(segs)
                    {
                        // `slow_index_convert_w` holds the (separated)
                        // array across the conversion's diagnostic; the
                        // write is redone once it has been raised.
                        if let Err(e) = key_of(k, diags, KeyCtx::Write) {
                            return Step::Err(e);
                        }
                        arr.make_mut();
                        CONVERT_DIM_HIT.with(|c| c.set(Some(arr.clone())));
                        return Step::Done(Value::Null);
                    }
                    // Only the final ZEND_UNSET_DIM reports "Cannot unset
                    // offset"; FETCH_DIM_UNSET converts like a write fetch.
                    let ctx = if is_unset && last {
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
                    let rw = matches!(wop, WOp::FetchRw | WOp::AssignOp(..));
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
                            _ => {
                                if rw && existed && diags.is_empty() {
                                    if let Some(step) = concat_in_place(slot, segs, wop) {
                                        return step;
                                    }
                                }
                                let step = apply_final(slot, wop);
                                if !existed && rw {
                                    watch_rw_container(arr);
                                }
                                step
                            }
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
    /// The elements `[]` segments appended during the first walk of a
    /// compound assignment, as (segment index, key). `ZEND_ASSIGN_DIM_OP`
    /// with no offset inserts one null element and operates on it in place
    /// (`zend_hash_next_index_insert(ht, &EG(uninitialized_zval))`); the
    /// silent write-back walk targets those elements instead of appending
    /// again (bug32833: `$a[] .= 'x'`).
    static APPENDED: std::cell::RefCell<Vec<(usize, i64)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// See `APPENDED`.
#[cold]
#[inline(never)]
fn record_append(seg: usize, key: i64) {
    APPENDED.with(|a| a.borrow_mut().push((seg, key)));
}

/// The segments for the write-back walk of a compound assignment whose
/// first walk over `segs` appended elements: each recorded `[]` becomes
/// the offset of the element it appended. `None` when nothing was
/// appended (the walk is repeated over `segs` as is).
fn take_appended_segs(segs: &[RSeg]) -> Option<Vec<RSeg>> {
    if !segs.iter().any(|s| matches!(s, RSeg::Append)) {
        return None;
    }
    let rec = APPENDED.with(|a| std::mem::take(&mut *a.borrow_mut()));
    if rec.is_empty() {
        return None;
    }
    let mut out = segs.to_vec();
    for (i, k) in rec {
        if let Some(seg @ RSeg::Append) = out.get_mut(i) {
            *seg = RSeg::Dim(Value::Int(k));
        }
    }
    Some(out)
}

thread_local! {
    /// The `ArrayAccess` object and offset a read-modify-write walk ended
    /// on (`FETCH_DIM_RW` on an object yields a temporary): set by
    /// `Vm::obj_walk`, taken by `Vm::fetch_rw`.
    static OVERLOADED_RW: std::cell::Cell<Option<(Obj, Value)>> =
        const { std::cell::Cell::new(None) };
    /// The array whose missing key a read-modify-write walk added, held
    /// across the "Undefined array key" warning like
    /// `zend_undefined_index_write`'s `GC_ADDREF`: an error handler that
    /// writes to or frees the array leaves this the only owner.
    static RW_UNDEF_WATCH: std::cell::Cell<Option<ZArray>> = const { std::cell::Cell::new(None) };
    /// Set when a `WOp::FetchRw` walk was abandoned that way (read by
    /// `Vm::fetch_rw`).
    static RW_SKIPPED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The undefined compiled variable (and frame depth) a
    /// `place = $undef` walk through a property still has to warn about:
    /// set by `Vm::place_assign_undef_cv`.
    static PENDING_UNDEF_VALUE: std::cell::Cell<Option<(u32, usize)>> =
        const { std::cell::Cell::new(None) };
    /// The frame depth of a `$o->p =& f()` walk whose value is not a
    /// reference: set by `Vm::place_assign_ref_nonvar_prop`, taken by
    /// `Vm::assign_property_ref` once the walk reaches the property.
    static PENDING_NONVAR_REF: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
    /// The final offset of a `place = v` walk that is an undefined
    /// compiled variable, keyed by the walk's segments (their address):
    /// set by `Vm::assign_undef_dim`. `ZEND_ASSIGN_DIM` reads the offset
    /// only once it has the container, so the "Undefined variable" warning
    /// follows the earlier fetches' diagnostics, is not raised for a
    /// scalar container, and comes from inside `zend_check_string_offset`
    /// for a string.
    static PENDING_UNDEF_DIM: std::cell::Cell<Option<(*const RSeg, u32)>> =
        const { std::cell::Cell::new(None) };
    /// The container a walk stopped at for that offset, with the variable
    /// (see [`UndefDimHit`]).
    static UNDEF_DIM_HIT: std::cell::Cell<Option<(u32, UndefDimHit)>> =
        const { std::cell::Cell::new(None) };
    /// The final offset of a `place = v` walk whose conversion raises a
    /// diagnostic (a fractional or out-of-range float, null or a resource)
    /// while an error handler is set, keyed by the walk's segments (their
    /// address): set by `Vm::assign_converted_dim`.
    static PENDING_CONVERT_DIM: std::cell::Cell<Option<*const RSeg>> =
        const { std::cell::Cell::new(None) };
    /// The array such a walk stopped at (`slow_index_convert_w`'s
    /// `GC_ADDREF(ht)`).
    static CONVERT_DIM_HIT: std::cell::Cell<Option<ZArray>> =
        const { std::cell::Cell::new(None) };
}

/// Takes the pending converted final offset of the walk over `segs`.
fn take_pending_convert_dim(segs: &[RSeg]) -> bool {
    PENDING_CONVERT_DIM.with(|c| match c.get() {
        Some(ptr) if std::ptr::eq(ptr, segs.as_ptr()) => {
            c.set(None);
            true
        }
        _ => false,
    })
}

/// Whether converting the final offset `seg` of an assignment raises a
/// diagnostic (`slow_index_convert_w`).
#[cold]
#[inline(never)]
fn final_dim_converts_noisily(seg: Option<&RSeg>) -> bool {
    let Some(RSeg::Dim(k)) = seg else {
        return false;
    };
    if !matches!(
        k,
        Value::Float(_) | Value::Null | Value::Resource(_) | Value::Reference(_)
    ) {
        return false;
    }
    let mut diags = Vec::new();
    key_of(k, &mut diags, KeyCtx::Write).is_ok() && !diags.is_empty()
}

/// Whether the property reference assignment at frame depth `depth` is the
/// `ZEND_RETURNS_FUNCTION` case of [`Vm::place_assign_ref`] (a call result
/// that is not a reference); clears the mark.
pub(crate) fn take_nonvar_ref(depth: usize) -> bool {
    PENDING_NONVAR_REF.with(|c| {
        if c.get() == Some(depth) {
            c.set(None);
            true
        } else {
            false
        }
    })
}

/// The container a `place = v` walk reached at an undefined-variable final
/// offset; the walk stops there without writing, the VM raises the offset
/// diagnostics, and the assignment is redone silently unless the error
/// handler destroyed (or, for an array, shared) the container.
enum UndefDimHit {
    /// An array: `slow_index_convert_w`'s `GC_ADDREF(ht)`. The flag is set
    /// when the walk made it from `false`: `ZEND_ASSIGN_DIM` holds the new
    /// array across the "Automatic conversion of false to array" deprecation
    /// (already raised with the walk's diagnostics) and abandons the write,
    /// before reading the offset, when the error handler freed it.
    Array(ZArray, bool),
    /// A string: `zend_assign_to_string_offset`'s `GC_ADDREF(s)`.
    Str(ZStr),
}

/// The in-place append behind [`Vm::concat_assign_cv`]: `None` when place
/// `pid` is not a plain compiled variable holding a string, or `rhs` is
/// neither a string nor an int; otherwise the string was extended (a
/// shared one is copied first, as `zend_string_extend` requires a
/// refcount of 1) and the result is returned: the new string when
/// `want_result`, else `Null`.
#[inline]
fn concat_cv_in_place(
    f: &crate::vm::Frame,
    pid: PlaceId,
    rhs: &Value,
    want_result: bool,
) -> Option<Value> {
    let p = &f.code.places[pid as usize];
    let (PlaceBase::Cv(cv), true) = (&p.base, p.segs.is_empty()) else {
        return None;
    };
    let idx = f.cv_idx(*cv);
    let mut st = f.vars.borrow_mut();
    let Some(Some(Value::Str(s))) = st.slots.get_mut(idx) else {
        return None;
    };
    match rhs {
        Value::Str(add) => s.append(add.as_bytes()),
        Value::Int(i) => s.append_long(*i),
        _ => return None,
    }
    Some(if want_result {
        Value::Str(s.clone())
    } else {
        Value::Null
    })
}

/// Takes the pending undefined final offset of the walk over `segs`.
fn take_pending_undef_dim(segs: &[RSeg]) -> Option<u32> {
    PENDING_UNDEF_DIM.with(|c| match c.get() {
        Some((ptr, cv)) if std::ptr::eq(ptr, segs.as_ptr()) => {
            c.set(None);
            Some(cv)
        }
        _ => None,
    })
}

/// See `RW_UNDEF_WATCH`.
#[cold]
fn watch_rw_container(arr: &ZArray) {
    RW_UNDEF_WATCH.with(|w| w.set(Some(arr.clone())));
}

/// After a read-modify-write walk's diagnostics: whether the error handler
/// wrote to or freed the array whose missing key the walk added
/// (`zend_undefined_index_write` / `zend_undefined_offset_write` then
/// return NULL and the operation is skipped; bug70662, bug78598).
fn rw_container_changed(wop: &WOp) -> bool {
    if !matches!(wop, WOp::FetchRw | WOp::AssignOp(..)) {
        return false;
    }
    let changed = RW_UNDEF_WATCH
        .with(std::cell::Cell::take)
        .is_some_and(|a| a.refcount() == 1);
    if changed && matches!(wop, WOp::FetchRw) {
        RW_SKIPPED.with(|c| c.set(true));
    }
    changed
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
        WOp::FetchRw | WOp::AssignOp(..) => Step::Done(v.deref()),
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
        WOp::AssignDeref(d) => {
            if let Some(cur) = d.src.try_borrow() {
                // Not a container of this walk: read it now, after the
                // containers were fetched for writing.
                let val = cur.clone();
                drop(cur);
                return apply_final(v, &mut WOp::Assign(val));
            }
            match v {
                Value::Reference(r) => d.hit = DeferredHit::Ref(r.clone()),
                _ => {
                    let old = std::mem::take(v);
                    drop(old);
                    d.hit = DeferredHit::Slot;
                }
            }
            Step::Done(Value::Null)
        }
    }
}

/// `.=` on a slot holding a string, in place. php-src's `ZEND_ASSIGN_DIM_OP`
/// and `ZEND_ASSIGN_STATIC_PROP_OP` operate on the fetched slot
/// (`zend_binary_op(var_ptr, var_ptr, value)`: `concat_function` with
/// `result == op1` extends the string with `zend_string_extend` when
/// nothing else holds it; a shared or interned string is copied once and
/// grows in place from then on), where the generic walk yields a copy of
/// the value, concatenates into a new string of the full length and writes
/// that back with a second, silent walk. `None` leaves the operation to
/// that walk: a right-hand side that is neither a string nor an int (its
/// conversion may warn or call `__toString()`), a slot not holding a
/// string directly (a reference, another type), or a `[]` segment in
/// `segs` (its appended element is recorded for the write-back walk).
/// Callers use it only while no diagnostic of the walk is pending: an
/// error handler raised afterwards could otherwise observe or abort an
/// operation that has already happened. The operation becomes
/// [`WOp::FetchRw`], whose finished step is the result, so the callers'
/// write-back of a compound assignment is skipped.
fn concat_in_place(slot: &mut Value, segs: &[RSeg], wop: &mut WOp) -> Option<Step> {
    let WOp::AssignOp(BinaryOp::Concat, rhs) = wop else {
        return None;
    };
    if segs.iter().any(|seg| matches!(seg, RSeg::Append)) {
        return None;
    }
    let Value::Str(s) = slot else {
        return None;
    };
    match rhs {
        Value::Str(add) => s.append(add.as_bytes()),
        Value::Int(i) => s.append(php_runtime::convert::long_to_zstr(*i).as_bytes()),
        _ => return None,
    }
    let result = Value::Str(s.clone());
    *wop = WOp::FetchRw;
    Some(Step::Done(result))
}

/// The index of the segment whose container the walk over `segs` (from
/// `v`, the container of segment `i`) reached last through `src`, or
/// `None` when the path does not run through it. Used after a
/// [`WOp::AssignDeref`] walk, whose containers are unshared arrays.
fn last_entry(v: &Value, segs: &[RSeg], i: usize, src: &PhpRef) -> Option<usize> {
    if i >= segs.len() {
        return None;
    }
    match v {
        Value::Reference(r) => {
            let deeper = r
                .try_borrow()
                .and_then(|inner| last_entry(&inner, segs, i, src));
            deeper.or_else(|| PhpRef::ptr_eq(r, src).then_some(i))
        }
        Value::Array(a) => {
            // A `[]` segment appended the table's last element.
            let key = match &segs[i] {
                RSeg::Append => a.last().map(|(k, _)| k.clone())?,
                RSeg::Dim(k) => key_of(k, &mut Vec::new(), KeyCtx::Write).ok()?,
                RSeg::Prop(_) => return None,
            };
            last_entry(a.get(&key)?, segs, i + 1, src)
        }
        _ => None,
    }
}

/// Completes a [`DeferredHit::Slot`]: stores a handle to `src`'s array in
/// the final slot, which `segs` (the path from that array, the walk's
/// last entry into `src`) leads to: `$t = &$a; $a[] = $t;` makes `$a`
/// contain itself, like php-src. Returns the assigned value.
fn store_self_reference(segs: &[RSeg], src: &PhpRef) -> Option<Value> {
    let mut path = Vec::with_capacity(segs.len());
    for seg in segs {
        path.push(match seg {
            RSeg::Append => None,
            RSeg::Dim(k) => Some(key_of(k, &mut Vec::new(), KeyCtx::Write).ok()?),
            RSeg::Prop(_) => return None,
        });
    }
    let mut b = src.try_borrow_mut()?;
    let stored = match &mut *b {
        Value::Array(a) => {
            let stored = a.store_self_at(&path);
            if stored.is_some() {
                // The collector tracks arrays that contain themselves, so
                // the cycle is freed like php-src's array roots.
                crate::gc::self_array_created(a);
            }
            stored
        }
        _ => None,
    };
    drop(b);
    stored
}

/// Completes a [`WOp::AssignDeref`] walk over `segs` from variable slot
/// `idx` of `store` that returned `done`, once its borrows are released.
fn finish_deferred(
    done: Value,
    d: &mut DeferredRead,
    store: &Rc<RefCell<VarStore>>,
    idx: usize,
    segs: &[RSeg],
) -> Step {
    match std::mem::replace(&mut d.hit, DeferredHit::None) {
        DeferredHit::None => Step::Done(done),
        DeferredHit::Ref(r) => {
            // The final slot is a reference: assign the source's value
            // through it.
            let v = d.src.try_borrow().map_or(Value::Null, |b| b.clone());
            if crate::typed_refs::has_type_sources(&r) {
                Step::TypedRefAssign(r, v)
            } else {
                let old = r.set(v.clone());
                drop(old);
                crate::gc::release_duplicate(Value::Reference(r));
                Step::Done(v)
            }
        }
        DeferredHit::Slot => {
            let entry = {
                let st = store.borrow();
                st.slots
                    .get(idx)
                    .and_then(Option::as_ref)
                    .and_then(|b| last_entry(b, segs, 0, &d.src))
            };
            let v = entry.and_then(|e| store_self_reference(&segs[e..], &d.src));
            Step::Done(v.unwrap_or(Value::Null))
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
        self.resolve_place_ex(pid, write, false, None, PlaceOp::Other)
    }

    /// [`Vm::resolve_place`]; `quiet` is the `BP_VAR_IS` fetch of
    /// `isset()`/`empty()`, where an undeclared or inaccessible static
    /// property base resolves to an undefined base without an error.
    ///
    /// With `undef_last_dim`, a final offset that is an undefined compiled
    /// variable resolves to null without the warning and the variable is
    /// stored there (see `PENDING_UNDEF_DIM`).
    ///
    /// `op` tells a static property base how it is written (see
    /// [`StaticWrite`]).
    fn resolve_place_ex(
        &mut self,
        pid: PlaceId,
        write: bool,
        quiet: bool,
        mut undef_last_dim: Option<&mut Option<u32>>,
        op: PlaceOp,
    ) -> VmResult<(RBase, Vec<RSeg>)> {
        let code = self.frame().code.clone();
        let p = &code.places[pid as usize];
        let sw = match (write, p.segs.is_empty(), op) {
            (true, true, PlaceOp::Assign) => Some(StaticWrite::Assign),
            (true, true, _) => Some(StaticWrite::Indirect),
            (true, false, _) => Some(StaticWrite::Fetch),
            (false, false, PlaceOp::Unset) => Some(StaticWrite::FetchUnset),
            (false, _, _) => None,
        };
        let base = self.resolve_base(&p.base, write, quiet, sw)?;
        let n = p.segs.len();
        let mut segs = Vec::with_capacity(n);
        for (j, s) in p.segs.iter().enumerate() {
            segs.push(match s {
                Seg::Dim(Operand::Cv(cv)) if j + 1 == n && undef_last_dim.is_some() => {
                    match self.read_cv_defined(*cv) {
                        Some(v) => RSeg::Dim(v),
                        None => {
                            if let Some(slot) = undef_last_dim.as_deref_mut() {
                                *slot = Some(*cv);
                            }
                            RSeg::Dim(Value::Null)
                        }
                    }
                }
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

    /// Resolves a place's base: the variable, `$this`, a static property
    /// cell or a temporary (reading the name operands of `$$n`/`A::$n`).
    /// `quiet` is `BP_VAR_IS`: an undeclared or inaccessible static
    /// property base resolves to an undefined base without an error.
    /// `sw` is how a static property base is written, for its set
    /// visibility check.
    fn resolve_base(
        &mut self,
        base: &PlaceBase,
        write: bool,
        quiet: bool,
        sw: Option<StaticWrite>,
    ) -> VmResult<RBase> {
        Ok(match base {
            PlaceBase::Cv(cv) => {
                let f = self.frame();
                RBase::Var(f.vars.clone(), f.cv_idx(*cv))
            }
            PlaceBase::VarVar(op) => {
                let n = self.read(*op)?;
                let n = self.to_zstr(&n)?;
                if n.as_bytes() == b"this" {
                    // `zend_fetch_var_address_helper`: `$this` is never in
                    // the symbol table, so `zend_fetch_this_var` runs: a
                    // write fetch throws, a read outside an object is an
                    // undefined variable.
                    if write {
                        return Err(self.make_throw("Error", "Cannot re-assign $this"));
                    }
                    match self.frame().this.clone() {
                        Some(o) => RBase::This(Some(o)),
                        None => RBase::Undef(n),
                    }
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
                // The op's run-time cache slot (`CACHED_PTR`, see
                // `rtcache`): the class and name are only looked up on a
                // miss.
                if let Some(cell) = self.static_prop_hit(class, *name) {
                    return Ok(RBase::Cell(cell));
                }
                let n = self.read(*name)?;
                let n = self.to_zstr(&n)?;
                let cls = self.resolve_class_spec(class)?;
                let r = if quiet {
                    match self.static_prop_cell_is(&cls, &n)? {
                        Some(cell) => RBase::Cell(cell),
                        None => RBase::Undef(n),
                    }
                } else if let Some(sw) = sw {
                    let (cell, fl) = self.static_prop_cell_flags(&cls, &n)?;
                    if fl & crate::func::flags::SET_MASK != 0
                        && self.check_static_set_access(&cls, &n, &cell, sw)?
                    {
                        RBase::Undef(n)
                    } else {
                        RBase::Cell(cell)
                    }
                } else {
                    RBase::Cell(self.static_prop_cell(&cls, &n)?)
                };
                if matches!(r, RBase::Cell(_)) {
                    self.static_prop_fill(class, *name);
                }
                r
            }
            PlaceBase::Value(op) => {
                let v = self.read_raw(*op)?;
                RBase::Value(v)
            }
        })
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
            // Only reached when no caller could convert the value.
            WalkErr::StringOffsetObject => {
                self.make_throw("Error", "Cannot assign an object to a string offset")
            }
        }
    }

    /// The rest of `zend_assign_to_string_offset` for an object value, once
    /// the walk has checked the offset ([`WalkErr::StringOffsetObject`]):
    /// converts it (`__toString()` may throw, leaving the string as it
    /// was), warns when more than one byte results, and writes the first
    /// byte by walking the place again (`rewalk`) with the converted value.
    #[cold]
    #[inline(never)]
    fn assign_string_offset_object(
        &mut self,
        wop: &WOp,
        rewalk: impl FnOnce(&mut Self, WOp) -> VmResult<Value>,
    ) -> VmResult<Value> {
        let WOp::Assign(val) = wop else {
            return Err(self.walk_err(WalkErr::StringOffsetObject));
        };
        let s = self.to_zstr(&val.deref())?;
        let first = match s.as_bytes() {
            [] => ZStr::empty(),
            [c] => ZStr::from_byte(*c),
            [c, ..] => {
                let c = *c;
                self.warning("Only the first byte will be assigned to the string offset")?;
                ZStr::from_byte(c)
            }
        };
        rewalk(self, WOp::Assign(Value::Str(first)))
    }

    /// Runs a write operation on a place.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn write_place(&mut self, base: RBase, segs: &[RSeg], wop: WOp) -> VmResult<Value> {
        let r = self.write_place_inner(base, segs, wop, false);
        flush_deferred_ref_assigns();
        r
    }

    /// `quiet` drops the walk's deferred diagnostics: the write-back walk
    /// of a compound assignment repeats offset conversions the first walk
    /// already reported.
    fn write_place_inner(
        &mut self,
        base: RBase,
        segs: &[RSeg],
        mut wop: WOp,
        quiet: bool,
    ) -> VmResult<Value> {
        let again = matches!(
            wop,
            WOp::AssignOp(..) | WOp::FetchRw | WOp::Assign(Value::Object(_) | Value::Reference(_))
        )
        .then(|| base.clone());
        let mut diags = Vec::new();
        let step = match base {
            RBase::Var(store, idx) => {
                if segs.is_empty() {
                    return self.write_var_root(&store, idx, wop);
                }
                let rw = matches!(wop, WOp::FetchRw | WOp::AssignOp(..));
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
                    if matches!(segs[0], RSeg::Prop(_)) {
                        // `ASSIGN_OBJ`/`FETCH_OBJ_W` on an undefined CV
                        // throw without defining it (`zend_throw_non_object_error`).
                        drop(st);
                        let mut null = Value::Null;
                        let s = descend(&mut null, segs, 0, &mut wop, &mut diags, true);
                        if !quiet {
                            self.flush_diags(diags)?;
                        }
                        return self.finish_step(s, segs, wop);
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
                    // `ZEND_ASSIGN_STATIC_PROP_OP` with `.=` on a string
                    // extends it in place (`typed_static_write` has let a
                    // typed property holding a string through).
                    let s = match concat_in_place(&mut b, segs, &mut wop) {
                        Some(s) => s,
                        None => apply_final(&mut b, &mut wop),
                    };
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
                        WOp::FetchRw | WOp::AssignOp(..) => v.deref(),
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
        let appended = if matches!(wop, WOp::AssignOp(..)) {
            take_appended_segs(segs)
        } else {
            None
        };
        let flushed = if quiet {
            Ok(())
        } else {
            self.flush_diags(diags)
        };
        if let Err(t) = flushed {
            return Err(match again {
                Some(b) => self.undo_rw_insert(b, segs, t),
                None => {
                    rw_container_changed(&wop);
                    t
                }
            });
        }
        let changed = rw_container_changed(&wop);
        if changed {
            return Ok(Value::Null);
        }
        match (&step, &wop, again) {
            (Step::Done(cur), WOp::AssignOp(op, rhs), Some(again)) => {
                // The final slot lies inside the base's container: write the
                // result back with a second, silent walk.
                let nv = self.assign_op_value(*op, cur, rhs)?;
                let segs = appended.as_deref().unwrap_or(segs);
                return self.write_place_inner(again, segs, WOp::Assign(nv), true);
            }
            (Step::Err(WalkErr::StringOffsetObject), _, Some(again)) => {
                return self.assign_string_offset_object(&wop, |vm, w| {
                    vm.write_place_inner(again, segs, w, true)
                });
            }
            _ => {}
        }
        self.finish_step(step, segs, wop)
    }

    /// `zend_undefined_offset_write` / `zend_undefined_index_write`: when
    /// the "Undefined array key" warning throws (an error handler), the
    /// missing key is not added. The walk added it before reporting, so a
    /// silent unset walk removes it again, unless the handler already
    /// replaced the array (then only the orphaned copy holds it).
    #[cold]
    #[inline(never)]
    fn undo_rw_insert(&mut self, base: RBase, segs: &[RSeg], err: Throw) -> Throw {
        let unchanged = RW_UNDEF_WATCH
            .with(std::cell::Cell::take)
            .is_some_and(|a| a.refcount() > 1);
        if unchanged {
            let _ = self.write_place_inner(base, segs, WOp::Unset, true);
        }
        err
    }

    /// `zend_fetch_var_address_helper` with `BP_VAR_RW` on an undefined
    /// `$GLOBALS['name']` (`ZEND_FETCH_GLOBAL`): "Undefined global
    /// variable", then the variable is defined as null unless that threw.
    #[cold]
    #[inline(never)]
    fn rw_undefined_global(&mut self, pid: PlaceId, base: &RBase) -> VmResult<()> {
        let RBase::Var(store, idx) = base else {
            return Ok(());
        };
        let name = {
            let st = store.borrow();
            if st.slots.get(*idx).is_none_or(Option::is_some) {
                return Ok(());
            }
            st.names[*idx].clone()
        };
        let msg = self.undefined_place_var_msg(pid, &name);
        self.warning(&msg)?;
        if let Some(slot) = store.borrow_mut().slots.get_mut(*idx) {
            if slot.is_none() {
                *slot = Some(Value::Null);
            }
        }
        Ok(())
    }

    /// The binary operation of a compound assignment.
    fn assign_op_value(&mut self, op: BinaryOp, cur: &Value, rhs: &Value) -> VmResult<Value> {
        ops::binary_op(op, cur, rhs, self).map_err(|e| self.rt_throw(e))
    }

    /// Finishes a walk step taken inside a container reached through an
    /// object (a property slot, a reference or a handler's result). For
    /// [`WOp::AssignOp`] a final slot's current value comes back as
    /// `Step::Done`: the result is computed with all borrows released and
    /// written by walking the same container again (`walk`) silently.
    fn finish_native(
        &mut self,
        step: Step,
        segs: &[RSeg],
        wop: WOp,
        appended: Option<Vec<RSeg>>,
        mut walk: impl FnMut(&[RSeg], &mut WOp, &mut Diags) -> Step,
    ) -> VmResult<Value> {
        if let (Step::Done(cur), WOp::AssignOp(op, rhs)) = (&step, &wop) {
            let nv = self.assign_op_value(*op, cur, rhs)?;
            let mut w = WOp::Assign(nv);
            let segs = appended.as_deref().unwrap_or(segs);
            let s = walk(segs, &mut w, &mut Vec::new());
            return self.finish_step(s, segs, w);
        }
        if matches!(step, Step::Err(WalkErr::StringOffsetObject)) {
            return self.assign_string_offset_object(&wop, |vm, mut w| {
                let s = walk(segs, &mut w, &mut Vec::new());
                vm.finish_step(s, segs, w)
            });
        }
        self.finish_step(step, segs, wop)
    }

    /// Walks inside one container (`walk` borrows it for the duration of
    /// a call), reports the walk's diagnostics and finishes the step.
    fn descend_with(
        &mut self,
        segs: &[RSeg],
        wop: WOp,
        mut walk: impl FnMut(&[RSeg], &mut WOp, &mut Diags) -> Step,
    ) -> VmResult<Value> {
        let mut diags = Vec::new();
        let mut w = wop;
        // A hit pending in an outer walk (whose diagnostics are being
        // reported) is not this walk's.
        let outer_hit = UNDEF_DIM_HIT.with(std::cell::Cell::take);
        let s = walk(segs, &mut w, &mut diags);
        let appended = if matches!(w, WOp::AssignOp(..)) {
            take_appended_segs(segs)
        } else {
            None
        };
        let hit = UNDEF_DIM_HIT.with(|c| c.replace(outer_hit));
        let flushed = self.flush_diags(diags);
        let changed = rw_container_changed(&w);
        flushed?;
        if changed {
            return Ok(Value::Null);
        }
        if let Some((cv, hit)) = hit {
            return self.descend_undef_dim(cv, hit, segs, w, walk);
        }
        self.finish_native(s, segs, w, appended, walk)
    }

    /// [`Vm::assign_undef_dim`] for a walk stopped inside a container
    /// reached through an object: the container is walked again rather
    /// than the place, so object handlers run once.
    #[cold]
    #[inline(never)]
    fn descend_undef_dim(
        &mut self,
        cv: u32,
        hit: UndefDimHit,
        segs: &[RSeg],
        wop: WOp,
        mut walk: impl FnMut(&[RSeg], &mut WOp, &mut Diags) -> Step,
    ) -> VmResult<Value> {
        let WOp::Assign(v) = &wop else {
            return Ok(Value::Null);
        };
        if !self.undef_dim_diags(cv, hit, v)? {
            return Ok(Value::Null);
        }
        let mut w = wop;
        let s = walk(segs, &mut w, &mut Vec::new());
        self.finish_step(s, segs, w)
    }

    /// Continues a write walk at segment `i` inside a reference.
    fn descend_ref(&mut self, r: &PhpRef, segs: &[RSeg], i: usize, wop: WOp) -> VmResult<Value> {
        self.descend_with(segs, wop, |sg, w, d| {
            let mut b = r.borrow_mut();
            descend(&mut b, sg, i, w, d, false)
        })
    }

    /// Continues a write walk at segment `i` inside a temporary (a
    /// handler's non-reference result: the modification is lost).
    fn descend_temp(&mut self, mut v: Value, segs: &[RSeg], i: usize, wop: WOp) -> VmResult<Value> {
        self.descend_with(segs, wop, |sg, w, d| descend(&mut v, sg, i, w, d, false))
    }

    fn finish_step(&mut self, step: Step, segs: &[RSeg], wop: WOp) -> VmResult<Value> {
        match step {
            Step::Done(v) => Ok(v),
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
            WOp::FetchRw | WOp::AssignOp(..) => {
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
            w @ WOp::AssignDeref(_) => self.write_var_root(store, idx, w.resolve_deferred()),
        }
    }

    /// `ZEND_ASSIGN_OBJ_OP` with `.=` on an initialized plain public
    /// property slot holding a string ([`Vm::plain_property_slot`]: the
    /// slot `get_property_ptr_ptr` hands out, with no hook, magic method,
    /// native handler, lazy state or type involved), for the places the
    /// property cache does not cover (`$this->o->s .= 'x'`,
    /// `$list[$i]->name .= 'x'`): the string is extended in place like
    /// [`concat_in_place`] does. `None` leaves the operation to the generic
    /// read, operate and write-back path.
    fn plain_prop_concat(o: &Obj, name: &ZStr, rhs: &Value) -> Option<Value> {
        let tmp;
        let add: &[u8] = match rhs {
            Value::Str(s) => s.as_bytes(),
            Value::Int(i) => {
                tmp = php_runtime::convert::long_to_zstr(*i);
                tmp.as_bytes()
            }
            _ => return None,
        };
        let idx = Self::plain_property_slot(o, name, true)?;
        let data = object::data(o);
        let mut slots = data.slots.borrow_mut();
        let r = match slots.get_mut(idx) {
            Some(Some(Value::Str(s))) => {
                s.append(add);
                Some(Value::Str(s.clone()))
            }
            _ => None,
        };
        drop(slots);
        r
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
                        WOp::Assign(v) => self.write_property_walk(&obj, name, v),
                        WOp::FetchRw => match self.native_read_property(
                            &obj,
                            name,
                            crate::ext::PropCtx::ReadWrite,
                        )? {
                            Some(v) => Ok(v.deref()),
                            None => self.fetch_property_rw(&obj, name),
                        },
                        WOp::AssignOp(op, rhs) => {
                            if op == BinaryOp::Concat {
                                if let Some(r) = Self::plain_prop_concat(&obj, name, &rhs) {
                                    return Ok(r);
                                }
                            }
                            // `zend_assign_op_overloaded_property` and the
                            // `get_property_ptr_ptr` path alike: read, operate,
                            // write back through the property handlers.
                            let cur = match self.native_read_property(
                                &obj,
                                name,
                                crate::ext::PropCtx::ReadWrite,
                            )? {
                                Some(v) => v.deref(),
                                None => self.fetch_property_rw(&obj, name)?,
                            };
                            let nv = self.assign_op_value(op, &cur, &rhs)?;
                            drop(cur);
                            self.write_property(&obj, name, nv)
                        }
                        WOp::MakeRef => self.property_ref(&obj, name).map(Value::Reference),
                        WOp::AssignRef(r) => self.assign_property_ref(&obj, name, r),
                        WOp::Unset => {
                            self.unset_property(&obj, name)?;
                            Ok(Value::Null)
                        }
                        w @ WOp::AssignDeref(_) => {
                            self.obj_walk(obj, segs, i, w.resolve_deferred())
                        }
                    };
                }
                // Intermediate: native classes provide the container through
                // read_property (`get_property_ptr_ptr` returning NULL).
                let ctx = match wop {
                    WOp::Unset => crate::ext::PropCtx::Unset,
                    WOp::FetchRw | WOp::AssignOp(..) => crate::ext::PropCtx::ReadWrite,
                    _ => crate::ext::PropCtx::Write,
                };
                // Objects and references continue the walk; other values
                // fall back to the standard property storage.
                match self.native_read_property(&obj, name, ctx)? {
                    Some(Value::Object(o2)) => return self.obj_walk(o2, segs, i + 1, wop),
                    Some(Value::Reference(r)) => return self.descend_ref(&r, segs, i + 1, wop),
                    // A native property without usable storage (virtual or
                    // write-restricted): as in `zend_fetch_property_address`
                    // with `get_property_ptr_ptr` returning NULL, the walk goes
                    // on with `read_property`'s temporary (DOM's
                    // `$node->previousSibling->nodeValue = ''` on a null
                    // sibling: "Attempt to assign property on null").
                    Some(other)
                        if !matches!(wop, WOp::Unset)
                            && Self::native_prop_without_ptr(&obj, name) =>
                    {
                        return self.descend_temp(other, segs, i + 1, wop);
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
                let rw = matches!(wop, WOp::FetchRw | WOp::AssignOp(..));
                match self.property_container(&obj, name, rw, 1)? {
                    PropContainer::Declared(idx) => {
                        let obj = self.lazy_object_for(&obj, Some(name))?;
                        let walk = |sg: &[RSeg], w: &mut WOp, d: &mut Diags| {
                            let data = object::data(&obj);
                            let mut slots = data.slots.borrow_mut();
                            let slot = slots[idx].get_or_insert(Value::Null);
                            descend(slot, sg, i + 1, w, d, false)
                        };
                        self.descend_with(segs, wop, walk)
                    }
                    PropContainer::Dynamic(key) => {
                        let obj = self.lazy_object_for(&obj, Some(name))?;
                        let walk = |sg: &[RSeg], w: &mut WOp, d: &mut Diags| {
                            let data = object::data(&obj);
                            let mut dynp = data.dynamic.borrow_mut();
                            let t = dynp.get_or_insert_with(Box::default);
                            let slot = t.lookup(key.clone());
                            descend(slot, sg, i + 1, w, d, false)
                        };
                        self.descend_with(segs, wop, walk)
                    }
                    PropContainer::Value(v) => match v {
                        Value::Object(o2) => self.obj_walk(o2, segs, i + 1, wop),
                        other => self.descend_temp(other, segs, i + 1, wop),
                    },
                }
            }
            seg @ (RSeg::Dim(_) | RSeg::Append) => {
                if last && matches!(seg, RSeg::Dim(Value::Null)) && matches!(wop, WOp::Assign(_)) {
                    self.take_pending_undef_dim_warn(segs)?;
                }
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
                                    self.take_pending_undef_value()?;
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
                            WOp::AssignOp(op, rhs) => {
                                // `zend_binary_assign_op_obj_dim`: read_dimension
                                // with BP_VAR_R (the hook context outside hook
                                // calls), operate, then write_dimension.
                                if let (Some(f), Some(k)) = (h.read_dimension, keyopt) {
                                    if let Some(cur) = f(self, &obj, k)? {
                                        let nv = self.assign_op_value(*op, &cur.deref(), rhs)?;
                                        drop(cur);
                                        return self.obj_walk(
                                            obj.clone(),
                                            segs,
                                            i,
                                            WOp::Assign(nv),
                                        );
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    // `$o[][..] = v` and `&$o[]` read the appended dimension
                    // (no offset).
                    let read_key = if keyopt.is_none() && (!last || matches!(wop, WOp::MakeRef)) {
                        Some(&key)
                    } else {
                        keyopt
                    };
                    if let (Some(f), Some(k)) = (h.read_dimension, read_key) {
                        if !last || matches!(wop, WOp::FetchRw | WOp::MakeRef) {
                            let rw = matches!(wop, WOp::FetchRw | WOp::AssignOp(..));
                            self.dim_ctx = match (keyopt.is_none(), rw, &wop, last) {
                                (true, true, ..) => crate::ext::DimensionCtx::AppendRw,
                                (true, false, ..) => crate::ext::DimensionCtx::Append,
                                (false, true, ..) => crate::ext::DimensionCtx::ReadWrite,
                                (false, false, WOp::Unset, false) => {
                                    crate::ext::DimensionCtx::Unset
                                }
                                _ => crate::ext::DimensionCtx::Write,
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
                                    Value::Reference(rf) => self.descend_ref(&rf, segs, i + 1, wop),
                                    other => self.descend_temp(other, segs, i + 1, wop),
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
                            self.take_pending_undef_value()?;
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
                        WOp::AssignOp(op, rhs) => {
                            // `zend_binary_assign_op_obj_dim` over
                            // `zend_std_read_dimension`/`write_dimension`
                            // (`$o[] op= v` passes a null offset to both).
                            let cur =
                                self.call_method_name_raw(&obj, "offsetGet", vec![key.clone()])?;
                            let nv = self.assign_op_value(op, &cur.deref(), &rhs)?;
                            drop(cur);
                            self.call_method_name(&obj, "offsetSet", vec![key, nv.clone()])?;
                            Ok(nv)
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
                                            object::class_of(&obj).name
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
                        w @ WOp::AssignDeref(_) => {
                            self.obj_walk(obj, segs, i, w.resolve_deferred())
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
                        self.descend_ref(&rf, segs, i + 1, wop)
                    }
                    other => {
                        // An increment walks twice (read, then write): the
                        // write walk reports.
                        if !matches!(wop, WOp::FetchRw) {
                            self.notice(&format!(
                                "Indirect modification of overloaded element of {} has no effect",
                                object::class_of(&obj).name
                            ))?;
                        }
                        self.descend_temp(other, segs, i + 1, wop)
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Public place operations
    // ------------------------------------------------------------------

    /// `place = $cv` where `$cv` is a reference to null, false or an array
    /// ([`crate::exec::CvOpData::Deferred`]). `ZEND_ASSIGN_DIM`
    /// (`Zend/zend_vm_def.h`) fetches the container for writing (creating
    /// and separating arrays) before it reads `OP_DATA`, so when the
    /// reference is a container on the path, the array ends up holding
    /// itself rather than a copy: `$t = &$a; $a[] = $t;` (gh16318,
    /// mb_check_encoding_array). The value is therefore read at the final
    /// slot ([`WOp::AssignDeref`]). Places on a static property or through
    /// a property, and the special offset cases of [`Vm::place_assign`],
    /// read it first, as before.
    #[cold]
    #[inline(never)]
    pub(crate) fn place_assign_cv_ref(&mut self, pid: PlaceId, cv: u32) -> VmResult<Value> {
        // `$a[] = $a` (`zend_is_assign_to_self`): php-src copies the value
        // into a temporary before fetching the container.
        let to_self = matches!(
            self.frame().code.places[pid as usize].base,
            PlaceBase::Cv(b) if b == cv
        );
        let src = match self.cv_get(cv) {
            Some(Value::Reference(r)) if !to_self => r,
            Some(v) => {
                let d = v.deref();
                crate::gc::release_duplicate(v);
                return self.place_assign(pid, d);
            }
            None => return self.place_assign_undef_cv(pid, cv),
        };
        let mut undef_dim = None;
        let resolved =
            self.resolve_place_ex(pid, true, false, Some(&mut undef_dim), PlaceOp::Assign);
        let (base, segs) = match resolved {
            Ok(r) => r,
            Err(e) => {
                crate::gc::release_duplicate(Value::Reference(src));
                return Err(e);
            }
        };
        let noisy = !self.error_handlers.is_empty() && final_dim_converts_noisily(segs.last());
        let target = match &base {
            RBase::Var(store, idx)
                if undef_dim.is_none()
                    && !noisy
                    && !segs.is_empty()
                    && !segs.iter().any(|s| matches!(s, RSeg::Prop(_)))
                    && store.borrow().slots.get(*idx).is_some_and(Option::is_some) =>
            {
                Some((store.clone(), *idx))
            }
            _ => None,
        };
        let Some((store, idx)) = target else {
            let v = src.get();
            crate::gc::release_duplicate(Value::Reference(src));
            if let Some(cv) = undef_dim {
                return self.assign_undef_dim(base, &segs, v, cv);
            }
            if noisy {
                return self.assign_converted_dim(base, &segs, v);
            }
            return self.write_place(base, &segs, WOp::Assign(v));
        };
        let mut wop = WOp::AssignDeref(Box::new(DeferredRead {
            src,
            hit: DeferredHit::None,
        }));
        let mut diags = Vec::new();
        let mut st = store.borrow_mut();
        let step = match st.slots.get_mut(idx).and_then(Option::as_mut) {
            Some(slot) => descend(slot, &segs, 0, &mut wop, &mut diags, false),
            None => Step::Done(Value::Null),
        };
        drop(st);
        let step = match (step, &mut wop) {
            (Step::Done(v), WOp::AssignDeref(d)) => finish_deferred(v, d, &store, idx, &segs),
            (s, _) => s,
        };
        let flushed = self.flush_diags(diags);
        let wop = wop.resolve_deferred();
        let r = match flushed {
            Ok(()) => self.finish_step(step, &segs, wop),
            Err(e) => Err(e),
        };
        flush_deferred_ref_assigns();
        r
    }

    /// `place = v`
    pub(crate) fn place_assign(&mut self, pid: PlaceId, v: Value) -> VmResult<Value> {
        let (single, head) = {
            let p = &self.frame().code.places[pid as usize];
            (propcache::cacheable(p), propcache::cacheable_head(p))
        };
        // Fast path: ASSIGN_OBJ to a cached property.
        let v = if single {
            match self.prop_cache_assign(pid, v) {
                Ok(r) => return Ok(r),
                Err(v) => v,
            }
        } else {
            v
        };
        let mut undef_dim = None;
        let (base, segs) =
            self.resolve_place_ex(pid, true, false, Some(&mut undef_dim), PlaceOp::Assign)?;
        if let Some(cv) = undef_dim {
            return self.assign_undef_dim(base, &segs, v, cv);
        }
        if !self.error_handlers.is_empty() && final_dim_converts_noisily(segs.last()) {
            return self.assign_converted_dim(base, &segs, v);
        }
        // Fast path: `$this->a[$k] = v` & co. enter a cached declared slot
        // (FETCH_OBJ_W with a runtime cache slot).
        if head && !single {
            if let Some((obj, idx)) = self.prop_cache_container(pid) {
                drop(base);
                return self.write_cached_container(&obj, idx, &segs, WOp::Assign(v));
            }
        }
        self.write_place(base, &segs, WOp::Assign(v))
    }

    /// The cached declared slot a write walk of place `pid` (resolved to
    /// `segs`) may start from: `Some` when its first segment is a cached
    /// property and more segments follow (see
    /// [`Vm::write_cached_container`]).
    fn cached_container_of(&mut self, pid: PlaceId, segs: &[RSeg]) -> Option<(Obj, usize)> {
        if segs.len() > 1 && propcache::cacheable_head(&self.frame().code.places[pid as usize]) {
            self.prop_cache_container(pid)
        } else {
            None
        }
    }

    /// [`Vm::write_place`] of place `pid`, starting from a cached first
    /// property when the cache covers it.
    fn write_place_cached(
        &mut self,
        pid: PlaceId,
        base: RBase,
        segs: &[RSeg],
        wop: WOp,
    ) -> VmResult<Value> {
        if let Some((obj, idx)) = self.cached_container_of(pid, segs) {
            drop(base);
            return self.write_cached_container(&obj, idx, segs, wop);
        }
        self.write_place(base, segs, wop)
    }

    /// The write walk of a resolved place whose first segment is a cached
    /// declared property, from slot `idx` of `obj` on: what
    /// [`Vm::write_place`] does for an object base reaching `obj_walk`'s
    /// `PropContainer::Declared(idx)`, once `Vm::prop_cache_container`
    /// stood in for the property's `FETCH_OBJ_W`.
    fn write_cached_container(
        &mut self,
        obj: &Obj,
        idx: usize,
        segs: &[RSeg],
        wop: WOp,
    ) -> VmResult<Value> {
        // `write_place_inner`'s check after the base step.
        if rw_container_changed(&wop) {
            flush_deferred_ref_assigns();
            return Ok(Value::Null);
        }
        let walk = |sg: &[RSeg], w: &mut WOp, d: &mut Diags| {
            let data = object::data(obj);
            let mut slots = data.slots.borrow_mut();
            let slot = slots[idx].get_or_insert(Value::Null);
            descend(slot, sg, 1, w, d, false)
        };
        let r = self.descend_with(segs, wop, walk);
        flush_deferred_ref_assigns();
        r
    }

    /// `place[k] = v` where converting `k` raises a diagnostic and an error
    /// handler is set: `slow_index_convert_w` (`Zend/zend_execute.c`)
    /// holds the array across the diagnostic and skips the write (the
    /// assignment yields null) when the handler captured or destroyed it
    /// (`array_offset_002`). The walk stops at the final array without
    /// writing (see `PENDING_CONVERT_DIM`); once its diagnostics have been
    /// raised the assignment is redone silently.
    #[cold]
    #[inline(never)]
    fn assign_converted_dim(&mut self, base: RBase, segs: &[RSeg], v: Value) -> VmResult<Value> {
        let again = base.clone();
        let saved = PENDING_CONVERT_DIM.with(|c| c.replace(Some(segs.as_ptr())));
        let saved_hit = CONVERT_DIM_HIT.with(std::cell::Cell::take);
        let r = self.write_place(base, segs, WOp::Assign(v.clone()));
        PENDING_CONVERT_DIM.with(|c| c.set(saved));
        let hit = CONVERT_DIM_HIT.with(|c| c.replace(saved_hit));
        let r = r?;
        let Some(held) = hit else {
            return Ok(r);
        };
        // The variable's slot and `held`.
        if held.refcount() != 2 {
            return Ok(Value::Null);
        }
        drop(held);
        let r = self.write_place_inner(again, segs, WOp::Assign(v), true);
        flush_deferred_ref_assigns();
        r
    }

    /// `place[$undef] = v` (`ZEND_ASSIGN_DIM` with an undefined CV offset):
    /// the walk stops at the final container without writing (see
    /// `PENDING_UNDEF_DIM`); the offset diagnostics are raised with the
    /// container held, and the assignment is then redone silently unless
    /// the error handler destroyed it.
    #[cold]
    #[inline(never)]
    fn assign_undef_dim(
        &mut self,
        base: RBase,
        segs: &[RSeg],
        v: Value,
        cv: u32,
    ) -> VmResult<Value> {
        let again = base.clone();
        let saved = PENDING_UNDEF_DIM.with(|c| c.replace(Some((segs.as_ptr(), cv))));
        let saved_hit = UNDEF_DIM_HIT.with(std::cell::Cell::take);
        let r = self.write_place(base, segs, WOp::Assign(v.clone()));
        let unconsumed = PENDING_UNDEF_DIM.with(|c| c.replace(saved)).is_some();
        let hit = UNDEF_DIM_HIT.with(|c| c.replace(saved_hit));
        let r = r?;
        let Some((cv, hit)) = hit else {
            if unconsumed {
                // A container this walk does not stop at: warn late
                // rather than never.
                self.undef_dim_warning(cv)?;
            }
            return Ok(r);
        };
        if !self.undef_dim_diags(cv, hit, &v)? {
            return Ok(Value::Null);
        }
        let r = self.write_place_inner(again, segs, WOp::Assign(v), true);
        flush_deferred_ref_assigns();
        r
    }

    /// The "Undefined variable" warning of a final offset.
    fn undef_dim_warning(&mut self, cv: u32) -> VmResult<()> {
        let name = self.frame().code.cv_names[cv as usize].clone();
        self.warning(&crate::exec::undefined_var_msg(&name))
    }

    /// Raises the diagnostics of an undefined-variable final offset at the
    /// container the walk stopped at: `slow_index_convert_w` (array) or
    /// `zend_assign_to_string_offset` (string). `false` when the error
    /// handler destroyed the container (or shared the array): the write is
    /// abandoned and the assignment yields null.
    #[cold]
    fn undef_dim_diags(&mut self, cv: u32, hit: UndefDimHit, v: &Value) -> VmResult<bool> {
        if let UndefDimHit::Array(held, true) = &hit {
            // `ZEND_ASSIGN_DIM`: `GC_DELREF(ht) == 0` after the false
            // deprecation -> `assign_dim_error`; the offset is never read.
            if held.refcount() == 1 {
                return Ok(false);
            }
        }
        self.undef_dim_warning(cv)?;
        match hit {
            UndefDimHit::Array(held, _) => {
                if held.refcount() != 2 {
                    return Ok(false);
                }
                self.deprecated(
                    "Using null as an array offset is deprecated, use an empty string instead",
                )?;
                Ok(held.refcount() == 2)
            }
            UndefDimHit::Str(held) => {
                self.warning("String offset cast occurred")?;
                if held.refcount() == 1 {
                    return Ok(false);
                }
                if string_offset_value_bytes(v).len() > 1 {
                    self.warning("Only the first byte will be assigned to the string offset")?;
                    if held.refcount() == 1 {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }

    /// Raises the pending undefined final offset's warning of the walk
    /// over `segs` before an object's `write_dimension` (`ZEND_ASSIGN_DIM`
    /// reads the offset before calling it).
    fn take_pending_undef_dim_warn(&mut self, segs: &[RSeg]) -> VmResult<()> {
        match take_pending_undef_dim(segs) {
            Some(cv) => self.undef_dim_warning(cv),
            None => Ok(()),
        }
    }

    /// The `BP_VAR_RW` read of a compound assignment's property
    /// (`zend_std_get_property_ptr_ptr`, `Zend/zend_object_handlers.c`):
    /// an error handler run by the "Undefined property" warning may define
    /// the property, and the operation then works on that value
    /// (`zend_assign_op_overloaded_property`).
    ///
    /// `obj` is an [`Vm::obj_walk`] temporary.
    fn fetch_property_rw(&mut self, obj: &Obj, name: &ZStr) -> VmResult<Value> {
        if let Some(v) = self.rw_create_dynamic(obj, name, 1)? {
            return Ok(v);
        }
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
        let (base, segs) = self.resolve_place_ex(pid, true, false, None, PlaceOp::Assign)?;
        if !segs.iter().any(|s| matches!(s, RSeg::Prop(_))) {
            let v = self.read_cv(cv)?;
            return self.write_place(base, &segs, WOp::Assign(v));
        }
        // Through a property, the fetches (`FETCH_OBJ_W`'s "Indirect
        // modification" notice, `__get`, `offsetGet`) precede the OP_DATA
        // read: the value's warning is raised just before the final store
        // (`Vm::take_pending_undef_value`) or after the walk.
        let depth = self.frames.len();
        let saved = PENDING_UNDEF_VALUE.with(|c| c.replace(Some((cv, depth))));
        let r = self.write_place(base, &segs, WOp::Assign(Value::Null));
        let mine = PENDING_UNDEF_VALUE.with(|c| c.replace(saved));
        let r = r?;
        if let Some((cv, _)) = mine {
            self.read_cv(cv)?;
        }
        Ok(r)
    }

    /// Raises the deferred "Undefined variable" warning of
    /// [`Vm::place_assign_undef_cv`] before an object's final dimension
    /// write (`offsetSet`, a native `write_dimension`), as `ZEND_ASSIGN_DIM`
    /// reads `OP_DATA` before `zend_assign_to_object_dim`.
    fn take_pending_undef_value(&mut self) -> VmResult<()> {
        let depth = self.frames.len();
        let pending = PENDING_UNDEF_VALUE.with(|c| match c.get() {
            Some((cv, d)) if d == depth => {
                c.set(None);
                Some(cv)
            }
            _ => None,
        });
        if let Some(cv) = pending {
            self.read_cv(cv)?;
        }
        Ok(())
    }

    /// `place op= v`
    pub(crate) fn place_assign_op(
        &mut self,
        pid: PlaceId,
        op: BinaryOp,
        v: Value,
    ) -> VmResult<Value> {
        let (single, head) = {
            let p = &self.frame().code.places[pid as usize];
            (propcache::cacheable(p), propcache::cacheable_head(p))
        };
        // Fast paths: ASSIGN_OBJ_OP on a cached property: `.=` on a string
        // appends in place, another operator on a number operates in place.
        if single {
            if op == BinaryOp::Concat {
                if let Some(r) = self.prop_cache_concat(pid, &v) {
                    return Ok(r);
                }
            }
            if let Some(r) = self.prop_cache_assign_op(pid, op, &v)? {
                return Ok(r);
            }
        }
        if let Some(cv) = self.rw_undef_cv_container(pid) {
            self.rw_undef_container_first(cv)?;
        }
        let (base, segs) = self.resolve_place(pid, true)?;
        if matches!(
            self.frame().code.places[pid as usize].base,
            PlaceBase::Global(_)
        ) {
            self.rw_undefined_global(pid, &base)?;
        }
        if !segs.is_empty() {
            // `$this->a[$k] op= v` & co.: enter a cached declared slot.
            if head && !single {
                if let Some((obj, idx)) = self.prop_cache_container(pid) {
                    drop(base);
                    return self.write_cached_container(&obj, idx, &segs, WOp::AssignOp(op, v));
                }
            }
            // One walk: offsets convert and object handlers run once.
            return self.write_place(base, &segs, WOp::AssignOp(op, v));
        }
        // Fast path: a plain variable holding an int or float
        // (`ZEND_ASSIGN_OP` on a CV): no destructor, reference or type
        // constraint can be involved, so the slot is updated in place.
        if let RBase::Var(store, idx) = &base {
            let cur = match store.borrow().slots.get(*idx) {
                Some(Some(n @ (Value::Int(_) | Value::Float(_)))) => Some(n.clone()),
                _ => None,
            };
            if let Some(cur) = cur {
                let nv = ops::binary_op(op, &cur, &v, self).map_err(|e| self.rt_throw(e))?;
                if let Some(slot) = store.borrow_mut().slots.get_mut(*idx) {
                    *slot = Some(nv.clone());
                }
                return Ok(nv);
            }
        }
        // Fast path: plain variable holding a string being appended to.
        if let RBase::Var(store, idx) = &base {
            if op == BinaryOp::Concat {
                if let Value::Str(add) = &v {
                    let mut st = store.borrow_mut();
                    if let Some(Value::Str(s)) = &mut st.slots[*idx] {
                        if !ZStr::ptr_eq(s, add) {
                            s.append(add.as_bytes());
                            return Ok(Value::Str(s.clone()));
                        }
                        // `$s .= $s`: php-src extends the one string in
                        // place (`concat_function`, result == op1 == op2),
                        // so the operand's handle goes first and the memory
                        // limit sees only the growth.
                        drop(st);
                        drop(v);
                        let mut st = store.borrow_mut();
                        let Some(Value::Str(s)) = &mut st.slots[*idx] else {
                            unreachable!("the slot held the string a moment ago")
                        };
                        s.append_self();
                        return Ok(Value::Str(s.clone()));
                    }
                }
            }
        }
        let base2 = base.clone();
        let cur = self.write_place(base, &segs, WOp::FetchRw)?;
        let nv = ops::binary_op(op, &cur, &v, self).map_err(|e| self.rt_throw(e))?;
        drop(cur);
        self.write_place(base2, &segs, WOp::Assign(nv))
    }

    /// The undefined compiled variable a read-modify-write of `pid` fetches
    /// as a dimension container (`$a[$k] op= v`, `$a[$k]++`).
    #[inline]
    fn rw_undef_cv_container(&self, pid: PlaceId) -> Option<u32> {
        let f = self.frame();
        let p = &f.code.places[pid as usize];
        let PlaceBase::Cv(cv) = p.base else {
            return None;
        };
        if !matches!(p.segs.first(), Some(Seg::Dim(_) | Seg::Append)) {
            return None;
        }
        let undef = matches!(f.vars.borrow().slots.get(f.cv_idx(cv)), Some(None));
        undef.then_some(cv)
    }

    /// `ZEND_ASSIGN_DIM_OP` / `ZEND_FETCH_DIM_RW` (`Zend/zend_vm_def.h`)
    /// fetch the container (`BP_VAR_RW`: "Undefined variable", then null)
    /// before reading the offset, so an undefined container warns before
    /// an undefined offset variable does.
    #[cold]
    #[inline(never)]
    fn rw_undef_container_first(&mut self, cv: u32) -> VmResult<()> {
        self.read_cv(cv)?;
        let f = self.frame();
        let idx = f.cv_idx(cv);
        let vars = f.vars.clone();
        if let Some(slot) = vars.borrow_mut().slots.get_mut(idx) {
            if slot.is_none() {
                *slot = Some(Value::Null);
            }
        }
        Ok(())
    }

    /// The read half of a read-modify-write: the current value and, when
    /// the walk ended on an `ArrayAccess` offset, that object and offset.
    /// `None` when the walk was abandoned because an error handler changed
    /// the array (see `rw_container_changed`).
    #[allow(clippy::type_complexity)] // (value, overloaded ArrayAccess target)
    fn fetch_rw(
        &mut self,
        pid: PlaceId,
        base: RBase,
        segs: &[RSeg],
    ) -> VmResult<Option<(Value, Option<(Obj, Value)>)>> {
        if !matches!(segs.last(), Some(RSeg::Dim(_) | RSeg::Append)) {
            return Ok(Some((
                self.write_place_cached(pid, base, segs, WOp::FetchRw)?,
                None,
            )));
        }
        // Save and restore around the walk: user code it runs (offsetGet,
        // destructors) may perform read-modify-writes of its own.
        let saved = OVERLOADED_RW.with(std::cell::Cell::take);
        let saved_skip = RW_SKIPPED.with(|c| c.replace(false));
        let r = self.write_place_cached(pid, base, segs, WOp::FetchRw);
        let mine = OVERLOADED_RW.with(|c| c.replace(saved));
        let skipped = RW_SKIPPED.with(|c| c.replace(saved_skip));
        let r = r?;
        Ok((!skipped).then_some((r, mine)))
    }

    /// `$cv .= <string or int>` (`ZEND_ASSIGN_OP` on a CV, whose
    /// `concat_function` call has `result == op1`): a compiled variable
    /// holding a string (not a reference) is appended to in place, which
    /// extends the buffer when nothing else holds it (`zend_string_extend`),
    /// without resolving the place generically. Writes the result to `dst`
    /// and returns `true`; `false` leaves the operation to
    /// [`Vm::place_assign_op`], whose string fast path does the same after
    /// the generic resolution (integers it converts to a temporary string
    /// first).
    #[inline]
    pub(crate) fn concat_assign_cv(&mut self, pid: PlaceId, v: &Value, dst: Option<u32>) -> bool {
        let Some(result) = concat_cv_in_place(self.frame(), pid, v, dst.is_some()) else {
            return false;
        };
        if let Some(t) = dst {
            self.set_tmp(t, result);
        }
        true
    }

    /// [`Vm::concat_assign_cv`] for a literal right operand (`$s .= "x"`,
    /// `ZEND_ASSIGN_OP` CV, CONST), appended straight from the literal
    /// table without reading (cloning) the operand. `false` when `val` is
    /// not a literal or the fast path does not apply; the caller then
    /// reads the operand as usual.
    #[inline]
    pub(crate) fn concat_assign_cv_lit(
        &mut self,
        pid: PlaceId,
        val: Operand,
        dst: Option<u32>,
    ) -> bool {
        let Operand::Lit(l) = val else {
            return false;
        };
        let f = self.frame();
        let Some(result) = concat_cv_in_place(f, pid, &f.code.literals[l as usize], dst.is_some())
        else {
            return false;
        };
        if let Some(t) = dst {
            self.set_tmp(t, result);
        }
        true
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
        // Fast path: PRE/POST_INC/DEC_OBJ on a cached declared slot.
        if propcache::cacheable(&self.frame().code.places[pid as usize]) {
            if let Some(r) = self.prop_cache_incdec(pid, kind) {
                return Ok(r);
            }
        }
        if let Some(cv) = self.rw_undef_cv_container(pid) {
            self.rw_undef_container_first(cv)?;
        }
        let (base, segs) = self.resolve_place(pid, true)?;
        if matches!(
            self.frame().code.places[pid as usize].base,
            PlaceBase::Global(_)
        ) {
            self.rw_undefined_global(pid, &base)?;
        }
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
        let Some((cur, overloaded)) = self.fetch_rw(pid, base, &segs)? else {
            // FETCH_DIM_RW yielded a null temporary: ++/-- act on it alone.
            let mut nv = Value::Null;
            if kind.is_inc() {
                ops::increment(&mut nv, self).map_err(|e| self.rt_throw(e))?;
            } else {
                ops::decrement(&mut nv, self).map_err(|e| self.rt_throw(e))?;
            }
            return Ok(if kind.is_post() { Value::Null } else { nv });
        };
        // `FETCH_DIM_RW` on an object: a non-reference result is a
        // temporary, so the increment is not written back.
        let temporary = match &overloaded {
            Some((obj, _)) if !matches!(cur, Value::Reference(_)) => {
                if !matches!(cur, Value::Object(_)) {
                    self.notice(&format!(
                        "Indirect modification of overloaded element of {} has no effect",
                        object::class_of(obj).name
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
            // A silent write-back walk: `FETCH_DIM_RW` converted the
            // offsets (and reported them) once already. (From a cached
            // property slot there is no base step whose diagnostics the
            // silent walk would drop.)
            if let Some((obj, idx)) = self.cached_container_of(pid, &segs) {
                drop(base2);
                self.write_cached_container(&obj, idx, &segs, WOp::Assign(nv))
            } else {
                let r = self.write_place_inner(base2, &segs, WOp::Assign(nv), true);
                flush_deferred_ref_assigns();
                r
            }
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
                let segs = &self.frame().code.places[pid as usize].segs;
                let to_prop = matches!(segs.last(), Some(Seg::Prop(_)));
                let to_dim = !segs.is_empty() && !segs.iter().any(|s| matches!(s, Seg::Prop(_)));
                if to_prop {
                    return self.place_assign_ref_nonvar_prop(pid, other);
                }
                if to_dim {
                    return self.assign_non_ref_to_dim(pid, other);
                }
                self.notice("Only variables should be assigned by reference")?;
                return self.place_assign(pid, other);
            }
        };
        let (base, segs) = self.resolve_place(pid, true)?;
        self.write_place(base, &segs, WOp::AssignRef(v))
    }

    /// `$a[...] =& f()` where `f()` did not return a reference:
    /// `FETCH_DIM_W` runs first, so its errors ("Cannot add element to the
    /// array as the next element is already occupied") take precedence;
    /// then `zend_wrong_assign_to_variable_reference()` raises the notice
    /// and assigns the value to the fetched element.
    #[cold]
    #[inline(never)]
    fn assign_non_ref_to_dim(&mut self, pid: PlaceId, v: Value) -> VmResult<Value> {
        let r = self.place_make_ref(pid)?;
        self.notice("Only variables should be assigned by reference")?;
        self.assign_ref(&r, v)
    }

    /// `$o->p =& f()` where `f()` returned a value: `ZEND_ASSIGN_OBJ_REF`
    /// (`zend_assign_to_property_reference`, `Zend/zend_execute.c`) fetches
    /// the property for writing before it looks at the value, so `__get()`
    /// runs first and a value it hands out throws "Cannot assign by
    /// reference to overloaded object" instead of the notice (bug69732).
    /// The walk carries the value as a fresh reference and marks itself
    /// for [`Vm::assign_property_ref`] (see [`take_nonvar_ref`]).
    #[cold]
    #[inline(never)]
    fn place_assign_ref_nonvar_prop(&mut self, pid: PlaceId, v: Value) -> VmResult<Value> {
        let (base, segs) = self.resolve_place(pid, true)?;
        let depth = self.frames.len();
        let saved = PENDING_NONVAR_REF.with(|c| c.replace(Some(depth)));
        let r = self.write_place(
            base,
            &segs,
            WOp::AssignRef(Value::Reference(PhpRef::new(v))),
        );
        PENDING_NONVAR_REF.with(|c| c.set(saved));
        r
    }

    /// Makes a place a reference.
    pub(crate) fn place_make_ref(&mut self, pid: PlaceId) -> VmResult<PhpRef> {
        // Fast path: `&$this->p` on a cached declared slot.
        if propcache::cacheable(&self.frame().code.places[pid as usize]) {
            if let Some(rf) = self.prop_cache_make_ref(pid) {
                return Ok(rf);
            }
        }
        let (base, segs) = self.resolve_place(pid, true)?;
        // `&$this->a[$k]` & co. enter a cached declared slot.
        match self.write_place_cached(pid, base, &segs, WOp::MakeRef)? {
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
        let (base, segs) = self.resolve_place_ex(pid, false, false, None, PlaceOp::Unset)?;
        if let RBase::Undef(_) = &base {
            return Ok(());
        }
        if let RBase::Var(store, idx) = &base {
            // UNSET_DIM, FETCH_DIM_UNSET and FETCH_OBJ_UNSET on an
            // undefined CV warn (ZVAL_UNDEFINED_OP1); a lone UNSET_OBJ
            // does not.
            let warns = !segs.is_empty() && !matches!(segs.as_slice(), [RSeg::Prop(_)]);
            if warns
                && store.borrow().slots[*idx].is_none()
                && matches!(
                    self.frame().code.places[pid as usize].base,
                    PlaceBase::Cv(_)
                )
            {
                let name = store.borrow().names[*idx].clone();
                return self.warning(&crate::exec::undefined_var_msg(&name));
            }
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

    /// The read warning for an undefined variable base: `$GLOBALS[...]`
    /// fetches carry `ZEND_FETCH_GLOBAL` ("Undefined global variable",
    /// `zend_fetch_var_address_helper`).
    #[cold]
    fn undefined_place_var_msg(&self, pid: PlaceId, name: &ZStr) -> String {
        if matches!(
            self.frame().code.places[pid as usize].base,
            PlaceBase::Global(_)
        ) {
            format!("Undefined global variable ${name}")
        } else {
            crate::exec::undefined_var_msg(name)
        }
    }

    /// Reads a place.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn place_fetch(&mut self, pid: PlaceId, mode: FetchMode) -> VmResult<Value> {
        let head = {
            let p = &self.frame().code.places[pid as usize];
            // Fast path: reading a plain compiled variable (`f($x)`).
            if mode == FetchMode::Read {
                if let (PlaceBase::Cv(cv), true) = (&p.base, p.segs.is_empty()) {
                    let cv = *cv;
                    return self.read_cv(cv);
                }
                // FETCH_DIM_FUNC_ARG sent by value: `$a[]` fails before its
                // container or any later offset is read
                // (`zend_use_undef_in_read_context`).
                if matches!(p.segs.first(), Some(Seg::Append)) {
                    return Err(self.make_throw("Error", "Cannot use [] for reading"));
                }
            }
            // Fast path: `$cv->name` / `$this->name` on a cached property
            // (FETCH_OBJ_R/IS with a runtime cache slot); a longer chain
            // (`$this->a[$i]`, `$this->a->b`) goes on from that value.
            if mode != FetchMode::List && propcache::cacheable_head(p) {
                let single = p.segs.len() == 1;
                match self.prop_cache_read(pid) {
                    Some(value) if single => return Ok(value),
                    found => found,
                }
            } else {
                None
            }
        };
        let code = self.frame().code.clone();
        let place = &code.places[pid as usize];
        let segs = &place.segs;
        let (mut cur, rest) = if let Some(v) = head {
            (v, &segs[1..])
        } else {
            let base = self.resolve_base(&place.base, false, mode == FetchMode::Is, None)?;
            (self.fetch_base_value(pid, base, segs, mode)?, &segs[..])
        };
        // Each offset/name operand is read when its FETCH_DIM_*/FETCH_OBJ_*
        // runs (after the container's warnings); a quiet chain that reached
        // null still reads, and reports, the remaining operands.
        let mut settled = mode == FetchMode::Is && rest.len() < segs.len() && cur.is_null();
        for seg in rest {
            match seg {
                Seg::Dim(op) => {
                    let k = self.read_dim_operand(*op)?;
                    if !settled {
                        cur = self.fetch_dim(cur, &k, mode)?;
                    }
                }
                Seg::Append => {
                    return Err(self.make_throw("Error", "Cannot use [] for reading"));
                }
                Seg::Prop(op) => {
                    let v = self.read(*op)?;
                    if !settled {
                        cur = match cur {
                            Value::Object(o) => {
                                let name = self.to_zstr(&v)?;
                                self.read_property(&o, &name, mode)?
                            }
                            other => {
                                if mode == FetchMode::Read {
                                    let name = self.to_zstr(&v)?;
                                    self.warning(&format!(
                                        "Attempt to read property \"{}\" on {}",
                                        name.to_string_lossy(),
                                        other.value_name()
                                    ))?;
                                }
                                Value::Null
                            }
                        };
                    }
                }
            }
            if mode == FetchMode::Is && cur.is_null() {
                settled = true;
            }
        }
        Ok(cur)
    }

    /// The value of a read place's resolved base (`place_fetch` without a
    /// cached head), with the undefined-variable warnings of a read.
    fn fetch_base_value(
        &mut self,
        pid: PlaceId,
        base: RBase,
        segs: &[Seg],
        mode: FetchMode,
    ) -> VmResult<Value> {
        Ok(match base {
            RBase::Var(store, idx) => {
                let v = store.borrow().slots[idx].as_ref().map(Value::deref);
                match v {
                    Some(v) => v,
                    None => {
                        if mode != FetchMode::Is {
                            let name = store.borrow().names[idx].clone();
                            let msg = self.undefined_place_var_msg(pid, &name);
                            self.warning(&msg)?;
                        }
                        Value::Null
                    }
                }
            }
            RBase::This(o) => match o {
                Some(o) => Value::Object(o),
                None => {
                    // FETCH_OBJ_IS / FETCH_DIM_IS on `$this` still need it.
                    if mode == FetchMode::Is && segs.is_empty() {
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
                    let msg = self.undefined_place_var_msg(pid, &name);
                    self.warning(&msg)?;
                }
                Value::Null
            }
            RBase::Globals => Value::Array(ZArray::from_table(self.globals.borrow().to_array())),
        })
    }

    /// One dimension read (`zend_fetch_dimension_address_read`).
    pub(crate) fn fetch_dim(&mut self, cur: Value, k: &Value, mode: FetchMode) -> VmResult<Value> {
        match cur {
            Value::Array(a) => {
                let mut diags = Vec::new();
                // FETCH_DIM_IS (`??`, the containers of a nested isset())
                // converts like a read: only the final ISSET_ISEMPTY_DIM_OBJ
                // says "in isset or empty".
                let key = match key_of(k, &mut diags, KeyCtx::Read) {
                    Ok(k) => k,
                    Err(e) => {
                        self.flush_diags(diags)?;
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
                let off = if mode == FetchMode::Is {
                    string_offset_is(k, &mut diags)
                } else {
                    string_offset(k, &mut diags, false).map(Some)
                };
                self.flush_diags(diags)?;
                let off = match off {
                    Ok(Some(o)) => o,
                    Ok(None) => return Ok(Value::Null),
                    Err(e) => return Err(self.walk_err(e)),
                };
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
                    if mode == FetchMode::Is && !h.read_dimension_is {
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
        let (single, head) = {
            let p = &self.frame().code.places[pid as usize];
            (propcache::cacheable(p), propcache::cacheable_head(p))
        };
        // Fast path: ISSET_ISEMPTY_PROP_OBJ on a cached property.
        if single {
            if let Some(r) = self.prop_cache_isset(pid, empty)? {
                return Ok(r);
            }
        }
        // `isset($this->a[$k])` & co.: the container's FETCH_OBJ_IS from
        // the cache.
        let head = if head && !single {
            self.prop_cache_read(pid)
        } else {
            None
        };
        let code = self.frame().code.clone();
        let place = &code.places[pid as usize];
        let segs = &place.segs;
        let (mut cur, start) = if let Some(v) = head {
            (Some(v), 1)
        } else {
            let base = self.resolve_base(&place.base, false, true, None)?;
            let cur = match base {
                RBase::Var(store, idx) => store.borrow().slots[idx].as_ref().map(Value::deref),
                // ZEND_ISSET_ISEMPTY_PROP_OBJ / FETCH_*_IS on an unset `$this`
                // (zend_this_not_in_object_context_helper).
                RBase::This(None) if !segs.is_empty() => {
                    return Err(self.make_throw("Error", "Using $this when not in object context"));
                }
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
            (cur, 0)
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
        // Each operand is read when its opcode runs, also once the result
        // is known (an undefined offset variable still warns); property
        // names convert only when an object is reached.
        let mut settled = false;
        for (i, seg) in segs.iter().enumerate().skip(start) {
            let raw = match seg {
                Seg::Dim(op) => self.read_dim_operand(*op)?,
                Seg::Prop(op) => self.read(*op)?,
                Seg::Append => Value::Null,
            };
            if settled {
                continue;
            }
            let c = match cur.take() {
                Some(c) if !c.is_null() => c,
                _ => {
                    settled = true;
                    continue;
                }
            };
            let last = i + 1 == n;
            if !last {
                let v = match seg {
                    Seg::Dim(_) => self.fetch_dim(c, &raw, FetchMode::Is)?,
                    Seg::Prop(_) => match c {
                        Value::Object(o) => {
                            let name = self.to_zstr(&raw)?;
                            self.read_property(&o, &name, FetchMode::Is)?
                        }
                        _ => Value::Null,
                    },
                    Seg::Append => {
                        return Err(self.make_throw("Error", "Cannot use [] for reading"))
                    }
                };
                cur = Some(v);
                continue;
            }
            // Last segment.
            let k = &raw;
            return match seg {
                Seg::Dim(_) => match c {
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
                            Value::Float(d) => Some(self.isset_float_string_offset(*d)?),
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
                Seg::Prop(_) => match c {
                    Value::Object(o) => {
                        let name = self.to_zstr(k)?;
                        self.has_property(&o, &name, empty)
                    }
                    _ => Ok(empty),
                },
                Seg::Append => Err(self.make_throw("Error", "Cannot use [] for reading")),
            };
        }
        Ok(empty)
    }

    /// `zval_get_long_ex(offset, /* is_strict */ true)` on a float offset
    /// of a string in `isset()/empty()` (`zend_isset_dim_slow`): the
    /// not-representable warning, then the precision-loss deprecation.
    #[cold]
    fn isset_float_string_offset(&mut self, d: f64) -> VmResult<i64> {
        let l = php_runtime::convert::dval_to_lval_silent(d);
        let shown = php_runtime::float::format_double(d, -1, false);
        if !d.is_finite() || !crate::typecheck::fits_long(d) {
            self.warning(&format!(
                "The float {shown} is not representable as an int, cast occurred"
            ))?;
        }
        if !php_runtime::convert::is_long_compatible(d, l) {
            self.error(
                E_DEPRECATED,
                &format!("Implicit conversion from float {shown} to int loses precision"),
            )?;
        }
        Ok(l)
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

mod propcache;

#[cfg(test)]
mod tests;
