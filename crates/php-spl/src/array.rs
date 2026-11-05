//! `ArrayObject`, `ArrayIterator` and `RecursiveArrayIterator`
//! (`spl_array.c`).
//!
//! The storage is either an owned array, the property table of a wrapped
//! object, the object's own property table (`IS_SELF`) or another array
//! object (`USE_OTHER`, e.g. the iterator returned by `getIterator()`).
//! The iteration position is a registered hash iterator in the storage
//! table, so it survives modifications like PHP's `ht_iter`.

use crate::args::{value_name, Args, IntoValid};
use crate::exceptions::{error, invalid_argument, out_of_bounds, type_error, unexpected_value};
use crate::host::{ObjectIterator, SplHost};
use crate::object::{state_of, SplObject};
use crate::serial;
use crate::util::{private_prop, this_obj, to_long, DimKind};
use php_runtime::convert::dval_to_lval_safe;
use php_runtime::numeric::handle_numeric_str;
use php_runtime::{
    ops, ArrayKey, ErrorLevel, HashTable, IterId, Obj, PhpRef, PropertyPurpose, RtResult,
    RuntimeError, Value, ZArray, ZStr,
};
use std::rc::Rc;

/// `ArrayObject::STD_PROP_LIST`.
pub const STD_PROP_LIST: i64 = 1;
/// `ArrayObject::ARRAY_AS_PROPS`.
pub const ARRAY_AS_PROPS: i64 = 2;
/// `RecursiveArrayIterator::CHILD_ARRAYS_ONLY`.
pub const CHILD_ARRAYS_ONLY: i64 = 4;
/// `SPL_ARRAY_IS_SELF`.
pub const IS_SELF: i64 = 0x0100_0000;
/// `SPL_ARRAY_USE_OTHER`.
pub const USE_OTHER: i64 = 0x0200_0000;
/// `SPL_ARRAY_INT_MASK`.
pub const INT_MASK: i64 = 0xFFFF_0000;
/// `SPL_ARRAY_CLONE_MASK`.
pub const CLONE_MASK: i64 = 0x0100_FFFF;

/// The internal state of an array object (`spl_array_object`).
#[derive(Debug, Clone)]
pub struct ArrayObject {
    /// The storage: an array, an object (wrapped object or, with
    /// `USE_OTHER`, another array object) or `null` (`IS_SELF`).
    pub array: Value,
    /// `ar_flags`.
    pub flags: i64,
    /// The registered iterator (`ht_iter`).
    pub ht_iter: Option<IterId>,
    /// `nApplyCount`: non-zero while a sort runs.
    pub apply_count: u32,
    /// `ce_get_iterator`.
    pub iterator_class: ZStr,
}

impl ArrayObject {
    /// A new state for the internal class `base`.
    #[must_use]
    pub fn new(base: &str) -> Self {
        let _ = base;
        ArrayObject {
            array: Value::empty_array(),
            flags: 0,
            ht_iter: None,
            apply_count: 0,
            iterator_class: ZStr::from("ArrayIterator"),
        }
    }
}

/// Where the hash table of an array object lives.
#[derive(Debug, Clone)]
enum Target {
    /// The array stored in this state.
    Own(Rc<SplObject>),
    /// An object's property table (wrapped object or `IS_SELF`).
    Props(Obj),
}

fn sorting_error() -> RuntimeError {
    error("Modification of ArrayObject during sorting is prohibited")
}

/// `spl_array_get_hash_table` resolution: follows `USE_OTHER` chains.
/// Also returns whether the storage is an object (`spl_array_is_object`).
fn target(host: &mut dyn SplHost, this: &Obj, st: &Rc<SplObject>) -> RtResult<(Target, bool)> {
    let mut obj = this.clone();
    let mut st = st.clone();
    for _ in 0..64 {
        let (flags, array) = st.array(|a| (a.flags, a.array.clone()))?;
        if flags & IS_SELF != 0 {
            return Ok((Target::Props(obj), true));
        }
        match array {
            Value::Object(o) if flags & USE_OTHER != 0 => {
                st = state_of(host, &o)?;
                obj = o;
            }
            Value::Object(o) => {
                // Reading the property table directly initializes a lazy
                // object (`spl_array_get_hash_table_ptr`).
                let o = host.init_lazy_object(&o)?;
                return Ok((Target::Props(o), true));
            }
            _ => return Ok((Target::Own(st), false)),
        }
    }
    Err(error("ArrayObject storage nesting is too deep"))
}

/// Runs `f` on the storage table (mutably; changes are written back).
fn with_ht<R>(
    host: &mut dyn SplHost,
    tgt: &Target,
    f: impl FnOnce(&mut HashTable) -> R,
) -> RtResult<R> {
    match tgt {
        Target::Own(st) => st.array(|a| {
            if !matches!(a.array, Value::Array(_)) {
                a.array = Value::empty_array();
            }
            match &mut a.array {
                Value::Array(z) => f(z.make_mut()),
                _ => unreachable!("set above"),
            }
        }),
        Target::Props(o) => {
            let mut z = host.std_properties(o);
            let r = f(z.make_mut());
            host.set_std_properties(o, z)?;
            Ok(r)
        }
    }
}

/// A copy of the storage table (cheap, shares the table).
fn table(host: &mut dyn SplHost, tgt: &Target) -> RtResult<ZArray> {
    match tgt {
        Target::Own(st) => st.array(|a| match &a.array {
            Value::Array(z) => z.clone(),
            _ => ZArray::new(),
        }),
        Target::Props(o) => Ok(host.std_properties(o)),
    }
}

/// `spl_array_skip_protected` on a position.
fn skip_protected(ht: &HashTable, mut pos: u32) -> u32 {
    loop {
        let p = ht.valid_pos(pos);
        match ht.at(p) {
            Some((ArrayKey::Str(s), _)) if s.as_bytes().first() == Some(&0) && !s.is_empty() => {
                pos = ht.pos_forward(p);
                if pos >= ht.used() {
                    return pos;
                }
            }
            _ => return p,
        }
    }
}

/// Runs `f` on the storage table and the iteration position
/// (`spl_array_get_pos_ptr`, creating the registered iterator on first use,
/// positioned at the first element); `f` returns the new position. All
/// position work happens inside one table access, so copy-on-write
/// separation cannot desynchronise positions and table layout.
fn with_pos<R>(
    host: &mut dyn SplHost,
    st: &Rc<SplObject>,
    tgt: &Target,
    is_obj: bool,
    f: impl FnOnce(&mut HashTable, u32) -> (u32, R),
) -> RtResult<R> {
    let id = st.array(|a| a.ht_iter)?;
    let (r, new_id) = with_ht(host, tgt, |ht| {
        let (id, pos) = if let Some(id) = id {
            (id, ht.iterator_pos(id))
        } else {
            let mut p = ht.valid_pos(0);
            if is_obj {
                p = skip_protected(ht, p);
            }
            (ht.iterator_add(p), p)
        };
        let (np, r) = f(ht, pos);
        ht.iterator_set_pos(id, np);
        (r, id)
    })?;
    if id.is_none() {
        st.array(|a| a.ht_iter = Some(new_id))?;
    }
    Ok(r)
}

/// The key under which `offset` is stored (`get_hash_key`); `Ok(None)`
/// for illegal offset types.
fn hash_key(host: &mut dyn SplHost, offset: &Value, is_obj: bool) -> RtResult<Option<ArrayKey>> {
    let h = match offset {
        Value::Null => {
            host.diagnostic(
                ErrorLevel::Deprecated,
                "Using null as an array offset is deprecated, use an empty string instead",
            )?;
            return Ok(Some(ArrayKey::Str(ZStr::empty())));
        }
        Value::Str(s) => match handle_numeric_str(s.as_bytes()) {
            Some(n) => n,
            None => return Ok(Some(ArrayKey::Str(s.clone()))),
        },
        Value::Resource(r) => {
            host.diagnostic(
                ErrorLevel::Warning,
                &format!(
                    "Resource ID#{} used as offset, casting to integer ({})",
                    r.handle(),
                    r.handle()
                ),
            )?;
            r.handle()
        }
        Value::Float(d) => dval_to_lval_safe(*d, host)?,
        Value::Bool(b) => i64::from(*b),
        Value::Int(n) => *n,
        Value::Reference(r) => return hash_key(host, &r.get(), is_obj),
        _ => return Ok(None),
    };
    Ok(Some(if is_obj {
        ArrayKey::Str(ZStr::from(h.to_string()))
    } else {
        ArrayKey::Int(h)
    }))
}

fn illegal_offset(obj: &Obj, offset: &Value, kind: DimKind) -> RuntimeError {
    match kind {
        DimKind::Is => type_error(format!(
            "Cannot access offset of type {} in isset or empty",
            offset.type_name()
        )),
        DimKind::Unset => type_error(format!(
            "Cannot unset offset of type {} on {}",
            offset.type_name(),
            obj.class_name()
        )),
        _ => type_error(format!(
            "Cannot access offset of type {} on {}",
            offset.type_name(),
            obj.class_name()
        )),
    }
}

fn undefined_key(host: &mut dyn SplHost, key: &ArrayKey) -> RtResult<()> {
    let msg = match key {
        ArrayKey::Int(n) => format!("Undefined array key {n}"),
        ArrayKey::Str(s) => format!("Undefined array key \"{}\"", s.to_string_lossy()),
    };
    host.diagnostic(ErrorLevel::Warning, &msg)
}

/// `spl_array_get_dimension_ptr` followed by the write-context reference
/// wrapping of `spl_array_read_dimension_ex`.
fn get_dimension(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &Rc<SplObject>,
    offset: Option<&Value>,
    kind: DimKind,
) -> RtResult<Value> {
    let Some(offset) = offset else {
        return Ok(Value::Null);
    };
    let write = matches!(kind, DimKind::Write | DimKind::ReadWrite);
    if write && st.array(|a| a.apply_count)? > 0 {
        return Err(sorting_error());
    }
    let (tgt, is_obj) = target(host, this, st)?;
    let Some(key) = hash_key(host, offset, is_obj)? else {
        let e = illegal_offset(this, offset, kind);
        if kind == DimKind::Unset {
            // `spl_array_get_dimension_ptr` hands BP_VAR_UNSET the
            // uninitialized zval with the exception pending, so the
            // engine's write-fetch notice still follows.
            host.diagnostic(
                ErrorLevel::Notice,
                &format!(
                    "Indirect modification of overloaded element of {} has no effect",
                    this.class_name()
                ),
            )?;
        }
        return Err(e);
    };
    let existing = table(host, &tgt)?.get(&key).cloned();
    match existing {
        Some(v) => {
            if matches!(kind, DimKind::Write | DimKind::ReadWrite | DimKind::Unset) {
                return with_ht(host, &tgt, |ht| {
                    ht.get_mut(&key)
                        .map_or(Value::Null, |slot| Value::Reference(slot.make_ref()))
                });
            }
            Ok(v.deref())
        }
        None => match kind {
            DimKind::Read => {
                undefined_key(host, &key)?;
                Ok(Value::Null)
            }
            DimKind::Unset | DimKind::Is => Ok(Value::Null),
            DimKind::ReadWrite | DimKind::Write => {
                if kind == DimKind::ReadWrite {
                    undefined_key(host, &key)?;
                }
                with_ht(host, &tgt, |ht| {
                    let slot = ht.lookup(key);
                    Value::Reference(slot.make_ref())
                })
            }
        },
    }
}

/// `spl_array_read_dimension_ex`.
fn read_dimension_ex(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &Rc<SplObject>,
    check_inherited: bool,
    offset: Option<&Value>,
    kind: DimKind,
) -> RtResult<Value> {
    if check_inherited {
        let user_get = host.has_user_method(this, "offsetGet");
        let user_has = kind == DimKind::Is && host.has_user_method(this, "offsetExists");
        if user_get || user_has {
            if kind == DimKind::Is {
                let off = offset.cloned().unwrap_or_default();
                if !has_dimension_ex(host, this, st, true, &off, 0)? {
                    return Ok(Value::Null);
                }
            }
            if user_get {
                // `$o[]` in a write fetch passes an undefined offset.
                let r = match offset {
                    Some(off) => host.call_method(this, "offsetGet", std::slice::from_ref(off)),
                    None => host.call_method_undef(this, "offsetGet"),
                };
                if r.is_err() && !matches!(kind, DimKind::Read | DimKind::Is) {
                    // `rv` stays undefined: the engine gets the
                    // uninitialized zval and notices the write fetch.
                    host.diagnostic(
                        ErrorLevel::Notice,
                        &format!(
                            "Indirect modification of overloaded element of {} has no effect",
                            this.class_name()
                        ),
                    )?;
                }
                return r;
            }
        }
    }
    get_dimension(host, this, st, offset, kind)
}

/// `spl_array_write_dimension_ex`.
fn write_dimension_ex(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &Rc<SplObject>,
    check_inherited: bool,
    offset: Option<&Value>,
    value: Value,
) -> RtResult<()> {
    if check_inherited && host.has_user_method(this, "offsetSet") {
        let off = offset.cloned().unwrap_or_default();
        return host
            .call_method(this, "offsetSet", &[off, value])
            .map(|_| ());
    }
    if st.array(|a| a.apply_count)? > 0 {
        return Err(sorting_error());
    }
    let value = value.deref();
    let (tgt, is_obj) = target(host, this, st)?;
    let offset = offset.map(Value::deref);
    let Some(offset) = offset.filter(|o| !o.is_null()) else {
        let pushed = with_ht(host, &tgt, |ht| ht.push(value).is_ok())?;
        if !pushed {
            return Err(error(
                "Cannot add element to the array as the next element is already occupied",
            ));
        }
        return Ok(());
    };
    let Some(key) = hash_key(host, &offset, is_obj)? else {
        return Err(illegal_offset(this, &offset, DimKind::Write));
    };
    let garbage = with_ht(host, &tgt, |ht| match ht.get_mut(&key) {
        Some(Value::Reference(r)) => Some(r.set(value)),
        Some(slot) => Some(std::mem::replace(slot, value)),
        None => {
            match &key {
                ArrayKey::Str(s) => {
                    ht.insert_str_raw(s.clone(), value);
                }
                ArrayKey::Int(_) => {
                    ht.insert(key.clone(), value);
                }
            }
            None
        }
    })?;
    drop(garbage);
    Ok(())
}

/// `$ao[$k] = &$v`: `FETCH_DIM_W` makes the element a reference
/// (`spl_array_read_dimension_ex` in a write context) and `ASSIGN_REF`
/// binds it to `r`. Returns `false` when `offsetGet()` is overridden (the
/// fetch then yields a temporary and the engine reports the failure).
pub fn assign_ref_dimension(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &Rc<SplObject>,
    offset: &Value,
    r: &PhpRef,
) -> RtResult<bool> {
    if host.has_user_method(this, "offsetGet") {
        return Ok(false);
    }
    if st.array(|a| a.apply_count)? > 0 {
        return Err(sorting_error());
    }
    let (tgt, is_obj) = target(host, this, st)?;
    let offset = offset.deref();
    let Some(key) = hash_key(host, &offset, is_obj)? else {
        return Err(illegal_offset(this, &offset, DimKind::Write));
    };
    let value = Value::Reference(r.clone());
    let old = with_ht(host, &tgt, |ht| match key {
        ArrayKey::Str(s) => ht.insert_str_raw(s, value),
        ArrayKey::Int(_) => ht.insert(key, value),
    })?;
    drop(old);
    Ok(true)
}

/// `spl_array_unset_dimension_ex`.
fn unset_dimension_ex(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &Rc<SplObject>,
    check_inherited: bool,
    offset: &Value,
) -> RtResult<()> {
    if check_inherited && host.has_user_method(this, "offsetUnset") {
        return host
            .call_method(this, "offsetUnset", std::slice::from_ref(offset))
            .map(|_| ());
    }
    if st.array(|a| a.apply_count)? > 0 {
        return Err(sorting_error());
    }
    let (tgt, is_obj) = target(host, this, st)?;
    let Some(key) = hash_key(host, &offset.deref(), is_obj)? else {
        return Err(illegal_offset(this, &offset.deref(), DimKind::Unset));
    };
    let garbage = with_ht(host, &tgt, |ht| ht.remove(&key))?;
    drop(garbage);
    Ok(())
}

/// `spl_array_has_dimension_ex`; `check_empty` is 0 (isset), 1 (empty) or
/// 2 (`offsetExists()`: null values count as existing).
fn has_dimension_ex(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &Rc<SplObject>,
    check_inherited: bool,
    offset: &Value,
    check_empty: i32,
) -> RtResult<bool> {
    let mut value: Option<Value> = None;
    if check_inherited && host.has_user_method(this, "offsetExists") {
        let r = host.call_method(this, "offsetExists", std::slice::from_ref(offset))?;
        if !r.to_bool(host)? {
            return Ok(false);
        }
        if check_empty == 0 {
            return Ok(true);
        } else if host.has_user_method(this, "offsetGet") {
            value = Some(read_dimension_ex(
                host,
                this,
                st,
                true,
                Some(offset),
                DimKind::Read,
            )?);
        }
    }
    let value = if let Some(v) = value {
        v
    } else {
        let (tgt, is_obj) = target(host, this, st)?;
        let Some(key) = hash_key(host, &offset.deref(), is_obj)? else {
            return Err(illegal_offset(this, &offset.deref(), DimKind::Is));
        };
        let Some(tmp) = table(host, &tgt)?.get(&key).cloned() else {
            return Ok(false);
        };
        if check_empty == 2 {
            return Ok(true);
        }
        if check_empty != 0 && check_inherited && host.has_user_method(this, "offsetGet") {
            read_dimension_ex(host, this, st, true, Some(offset), DimKind::Read)?
        } else {
            tmp
        }
    };
    if check_empty != 0 {
        value.to_bool(host)
    } else {
        Ok(!value.is_null())
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `read_dimension` handler.
pub fn read_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    offset: Option<&Value>,
    kind: DimKind,
) -> RtResult<Value> {
    read_dimension_ex(host, obj, st, true, offset, kind)
}

/// `write_dimension` handler.
pub fn write_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    offset: Option<&Value>,
    value: Value,
) -> RtResult<()> {
    write_dimension_ex(host, obj, st, true, offset, value)
}

/// `has_dimension` handler.
pub fn has_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    offset: &Value,
    check_empty: bool,
) -> RtResult<bool> {
    has_dimension_ex(host, obj, st, true, offset, i32::from(check_empty))
}

/// `unset_dimension` handler.
pub fn unset_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    offset: &Value,
) -> RtResult<()> {
    unset_dimension_ex(host, obj, st, true, offset)
}

fn count_helper(host: &mut dyn SplHost, this: &Obj, st: &Rc<SplObject>) -> RtResult<i64> {
    let (tgt, is_obj) = target(host, this, st)?;
    let t = table(host, &tgt)?;
    if is_obj {
        Ok(to_long(
            t.keys()
                .filter(|k| !matches!(k, ArrayKey::Str(s) if s.as_bytes().first() == Some(&0)))
                .count(),
        ))
    } else {
        Ok(to_long(t.len()))
    }
}

/// `count_elements` handler.
pub fn count_elements(host: &mut dyn SplHost, obj: &Obj, st: &Rc<SplObject>) -> RtResult<i64> {
    if host.has_user_method(obj, "count") {
        let r = host.call_method(obj, "count", &[])?;
        return r.to_long(host);
    }
    count_helper(host, obj, st)
}

/// `get_properties_for` handler: the storage for `(array)` casts,
/// `var_export()` and `json_encode()` (unless `STD_PROP_LIST`); `None`
/// means the standard property table.
pub fn properties_for(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    purpose: PropertyPurpose,
) -> RtResult<Option<ZArray>> {
    if st.array(|a| a.flags)? & STD_PROP_LIST != 0 {
        return Ok(None);
    }
    match purpose {
        PropertyPurpose::ArrayCast | PropertyPurpose::VarExport | PropertyPurpose::Json => {
            let (tgt, _) = target(host, obj, st)?;
            table(host, &tgt).map(Some)
        }
        _ => Ok(None),
    }
}

/// `spl_array_get_debug_info` (also `__debugInfo()`).
pub fn debug_info(host: &mut dyn SplHost, obj: &Obj, st: &Rc<SplObject>) -> RtResult<ZArray> {
    let props = host.std_properties(obj);
    let (flags, storage) = st.array(|a| (a.flags, a.array.clone()))?;
    if flags & IS_SELF != 0 {
        return Ok(props);
    }
    let mut out: HashTable = (*props).clone();
    let base = if host.instance_of(obj, "ArrayIterator") {
        "ArrayIterator"
    } else {
        "ArrayObject"
    };
    out.insert(ArrayKey::Str(private_prop(base, "storage")), storage);
    Ok(ZArray::from_table(out))
}

fn as_props(host: &mut dyn SplHost, obj: &Obj, st: &Rc<SplObject>, name: &ZStr) -> RtResult<bool> {
    Ok(st.array(|a| a.flags)? & ARRAY_AS_PROPS != 0 && !host.std_has_property(obj, name))
}

/// `read_property` handler (`ARRAY_AS_PROPS`); `None` = standard lookup.
pub fn read_property(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    name: &ZStr,
    kind: DimKind,
) -> RtResult<Option<Value>> {
    if !as_props(host, obj, st, name)? {
        return Ok(None);
    }
    let member = Value::Str(name.clone());
    if kind == DimKind::Write && host.has_user_method(obj, "offsetGet") {
        // get_property_ptr_ptr returns NULL: falls back to read_property.
        return read_dimension(host, obj, st, Some(&member), DimKind::Read).map(Some);
    }
    read_dimension(host, obj, st, Some(&member), kind).map(Some)
}

/// `write_property` handler; returns whether it was handled.
pub fn write_property(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    name: &ZStr,
    value: Value,
) -> RtResult<bool> {
    if !as_props(host, obj, st, name)? {
        return Ok(false);
    }
    write_dimension(host, obj, st, Some(&Value::Str(name.clone())), value)?;
    Ok(true)
}

/// `has_property` handler (`check`: 0 isset, 1 empty, 2 exists).
pub fn has_property(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    name: &ZStr,
    check: i32,
) -> RtResult<Option<bool>> {
    if !as_props(host, obj, st, name)? {
        return Ok(None);
    }
    has_dimension_ex(host, obj, st, true, &Value::Str(name.clone()), check).map(Some)
}

/// `unset_property` handler; returns whether it was handled.
pub fn unset_property(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    name: &ZStr,
) -> RtResult<bool> {
    if !as_props(host, obj, st, name)? {
        return Ok(false);
    }
    unset_dimension(host, obj, st, &Value::Str(name.clone()))?;
    Ok(true)
}

/// `compare` handler (`spl_array_compare_objects`).
pub fn compare(host: &mut dyn SplHost, a: &Obj, b: &Obj) -> RtResult<i32> {
    let (Some(sa), Some(sb)) = (host.spl_state(a), host.spl_state(b)) else {
        return Ok(1);
    };
    if sa.array(|_| ()).is_err() || sb.array(|_| ()).is_err() {
        return Ok(1);
    }
    let (ta, _) = target(host, a, &sa)?;
    let (tb, _) = target(host, b, &sb)?;
    let ha = table(host, &ta)?;
    let hb = table(host, &tb)?;
    let mut result = ops::compare_tables(&ha, &hb, host)?;
    let both_self = matches!((&ta, &tb), (Target::Props(x), Target::Props(y)) if Obj::ptr_eq(x, a) && Obj::ptr_eq(y, b));
    if result == 0 && !both_self {
        if a.class_name() != b.class_name() {
            return Ok(1);
        }
        let pa = host.std_properties(a);
        let pb = host.std_properties(b);
        result = ops::compare_tables(&pa, &pb, host)?;
    }
    Ok(result)
}

/// Clone support (`spl_array_object_new_ex` with `clone_orig`).
pub fn clone_into(
    host: &mut dyn SplHost,
    old: &Obj,
    new_obj: &Obj,
    fresh: &SplObject,
    st: &SplObject,
) -> RtResult<()> {
    let _ = new_obj;
    let (flags, iterator_class) = st.array(|a| (a.flags, a.iterator_class.clone()))?;
    let mut new_flags = flags & CLONE_MASK;
    let array = if flags & IS_SELF != 0 {
        Value::Null
    } else if host.instance_of(old, "ArrayObject") {
        let old_rc = state_of(host, old)?;
        let (tgt, _) = target(host, old, &old_rc)?;
        let t = table(host, &tgt)?;
        Value::Array(ZArray::from_table((*t).clone()))
    } else {
        new_flags |= USE_OTHER;
        Value::Object(old.clone())
    };
    fresh.array(|a| {
        a.flags = new_flags;
        a.array = array;
        a.iterator_class = iterator_class;
    })
}

/// The `ArrayIterator` `foreach` iterator (`spl_array_it_funcs`).
pub fn native_iterator(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    by_ref: bool,
) -> RtResult<Option<Box<dyn ObjectIterator>>> {
    if !host.instance_of(obj, "ArrayIterator") {
        return Ok(None);
    }
    Ok(Some(Box::new(ArrayIter {
        obj: obj.clone(),
        st: st.clone(),
        by_ref,
    })))
}

/// `foreach` over an `ArrayIterator`, sharing the object's position.
#[derive(Debug)]
pub struct ArrayIter {
    obj: Obj,
    st: Rc<SplObject>,
    by_ref: bool,
}

impl ObjectIterator for ArrayIter {
    fn rewind(&mut self, host: &mut dyn SplHost) -> RtResult<()> {
        rewind(host, &self.obj, &self.st)
    }
    fn valid(&mut self, host: &mut dyn SplHost) -> RtResult<bool> {
        current_value(host, &self.obj, &self.st).map(|v| v.is_some())
    }
    fn current(&mut self, host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        let (tgt, is_obj) = target(host, &self.obj, &self.st)?;
        let by_ref = self.by_ref;
        with_pos(host, &self.st, &tgt, is_obj, |ht, pos| {
            let p = ht.valid_pos(pos);
            let v = if by_ref {
                ht.at_mut(p).map(|slot| Value::Reference(slot.make_ref()))
            } else {
                ht.at(p).map(|(_, v)| v.clone())
            };
            (pos, v)
        })
    }
    fn key(&mut self, host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        current_key(host, &self.obj, &self.st).map(Some)
    }
    fn next(&mut self, host: &mut dyn SplHost) -> RtResult<()> {
        next(host, &self.obj, &self.st).map(|_| ())
    }
}

fn rewind(host: &mut dyn SplHost, this: &Obj, st: &Rc<SplObject>) -> RtResult<()> {
    let (tgt, is_obj) = target(host, this, st)?;
    with_pos(host, st, &tgt, is_obj, |ht, _| {
        let p = ht.valid_pos(0);
        (if is_obj { skip_protected(ht, p) } else { p }, ())
    })
}

/// `spl_array_next_ex`: whether there are more elements afterwards.
fn next(host: &mut dyn SplHost, this: &Obj, st: &Rc<SplObject>) -> RtResult<bool> {
    let (tgt, is_obj) = target(host, this, st)?;
    with_pos(host, st, &tgt, is_obj, |ht, pos| {
        let mut np = ht.pos_forward(pos);
        if is_obj {
            np = skip_protected(ht, np);
        }
        let more = ht.at(ht.valid_pos(np)).is_some();
        (np, more)
    })
}

fn current_key(host: &mut dyn SplHost, this: &Obj, st: &Rc<SplObject>) -> RtResult<Value> {
    let (tgt, is_obj) = target(host, this, st)?;
    with_pos(host, st, &tgt, is_obj, |ht, pos| {
        (
            pos,
            ht.at(ht.valid_pos(pos))
                .map_or(Value::Null, |(k, _)| k.to_value()),
        )
    })
}

fn current_value(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &Rc<SplObject>,
) -> RtResult<Option<Value>> {
    let (tgt, is_obj) = target(host, this, st)?;
    with_pos(host, st, &tgt, is_obj, |ht, pos| {
        (pos, ht.at(ht.valid_pos(pos)).map(|(_, v)| v.deref()))
    })
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

/// `spl_array_set_array`.
fn set_array(
    host: &mut dyn SplHost,
    func: &str,
    this: &Obj,
    st: &Rc<SplObject>,
    array: &Value,
    mut ar_flags: i64,
    just_array: bool,
) -> RtResult<()> {
    if st.array(|a| a.apply_count)? > 0 {
        return Err(sorting_error());
    }
    let new_array = match array.deref() {
        Value::Array(a) => Value::Array(a),
        Value::Object(o) => {
            let which = if host.instance_of(this, "ArrayIterator") {
                "ArrayIterator"
            } else {
                "ArrayObject"
            };
            host.diagnostic(
                ErrorLevel::Deprecated,
                &format!(
                    "{func}(): Using an object as a backing array for {which} is deprecated, as it allows violating class constraints and invariants"
                ),
            )?;
            let other_is_array = host.spl_state(&o).is_some_and(|s| s.array(|_| ()).is_ok());
            if other_is_array {
                if just_array {
                    let other = state_of(host, &o)?;
                    ar_flags = other.array(|a| a.flags)? & !INT_MASK;
                }
                if Obj::ptr_eq(&o, this) {
                    ar_flags |= IS_SELF;
                    Value::Null
                } else {
                    ar_flags |= USE_OTHER;
                    Value::Object(o)
                }
            } else {
                let overloaded = host.spl_state(&o).is_some_and(|s| s.fixed(|_| ()).is_ok())
                    || host.is_overloaded_object(&o);
                if overloaded {
                    return Err(invalid_argument(format!(
                        "Overloaded object of type {} is not compatible with {}",
                        o.class_name(),
                        this.class_name()
                    )));
                }
                if php_runtime::RuntimeContext::is_enum(host, &o) {
                    return Err(invalid_argument(format!(
                        "Enums are not compatible with {}",
                        this.class_name()
                    )));
                }
                Value::Object(o)
            }
        }
        _ => return Ok(()),
    };
    let garbage = st.array(|a| {
        let old = std::mem::replace(&mut a.array, new_array);
        a.flags &= !IS_SELF & !USE_OTHER;
        a.flags |= ar_flags;
        a.ht_iter = None;
        old
    })?;
    drop(garbage);
    Ok(())
}

fn array_or_object_arg(a: &Args<'_>, i: usize) -> RtResult<Value> {
    match a.raw(i) {
        v @ (Value::Array(_) | Value::Object(_)) => Ok(v),
        _ => Err(a.type_error(i, "array")),
    }
}

#[allow(clippy::many_single_char_names)]
fn class_arg(host: &mut dyn SplHost, a: &Args<'_>, i: usize) -> RtResult<ZStr> {
    let v = a.raw(i);
    let name = match &v {
        Value::Str(s) => s.clone(),
        Value::Object(o) => match host.cast_object(o, php_runtime::CastTarget::String)? {
            Some(Value::Str(s)) => s,
            _ => return Err(a.type_error(i, "a valid class name").into_valid()),
        },
        _ => return Err(a.type_error(i, "a valid class name").into_valid()),
    };
    match host.class_is_a(&name, "ArrayIterator") {
        Some(true) => {
            let n = name.as_bytes();
            let n = n.strip_prefix(b"\\").unwrap_or(n);
            Ok(ZStr::from(n))
        }
        _ => Err(type_error(format!(
            "{} must be a class name derived from ArrayIterator, \"{}\" given",
            a.arg_prefix(i),
            name.to_string_lossy()
        ))),
    }
}

/// Methods of `ArrayObject`, `ArrayIterator` and `RecursiveArrayIterator`.
#[allow(clippy::too_many_lines)]
pub(crate) fn call(
    host: &mut dyn SplHost,
    class: &str,
    method: &str,
    this: Option<&Obj>,
    args: &[Value],
) -> Option<RtResult<Value>> {
    let this = match this_obj(this) {
        Ok(t) => t,
        Err(e) => return Some(Err(e)),
    };
    let name = crate::classes::find_method(class, method)
        .filter(|(decl, _)| *decl == class)
        .map(|(_, m)| m.name)?;
    let func = format!("{class}::{name}");
    let func = func.as_str();
    Some((|| -> RtResult<Value> {
        let st = state_of(host, this)?;
        match method {
            "__construct" => {
                if args.is_empty() {
                    return Ok(Value::Null);
                }
                let max = if class == "ArrayObject" { 3 } else { 2 };
                let a = Args::new(
                    func,
                    args,
                    &["array", "flags", "iteratorClass"],
                    0,
                    Some(max),
                )?;
                let array = array_or_object_arg(&a, 0)?;
                let flags = a.opt_long(host, 1, 0)?;
                if a.len() > 2 {
                    let c = class_arg(host, &a, 2)?;
                    st.array(|s| s.iterator_class = c)?;
                }
                set_array(
                    host,
                    func,
                    this,
                    &st,
                    &array,
                    flags & !INT_MASK,
                    a.len() == 1,
                )?;
                Ok(Value::Null)
            }
            "offsetexists" => {
                let a = Args::new(func, args, &["key"], 1, Some(1))?;
                has_dimension_ex(host, this, &st, false, &a.raw(0), 2).map(Value::Bool)
            }
            "offsetget" => {
                let a = Args::new(func, args, &["key"], 1, Some(1))?;
                let k = a.raw(0);
                read_dimension_ex(host, this, &st, false, Some(&k), DimKind::Read)
                    .map(|v| v.deref())
            }
            "offsetset" => {
                let a = Args::new(func, args, &["key", "value"], 2, Some(2))?;
                let k = a.raw(0);
                write_dimension_ex(host, this, &st, false, Some(&k), a.raw(1))?;
                Ok(Value::Null)
            }
            "offsetunset" => {
                let a = Args::new(func, args, &["key"], 1, Some(1))?;
                unset_dimension_ex(host, this, &st, false, &a.raw(0))?;
                Ok(Value::Null)
            }
            "append" => {
                let a = Args::new(func, args, &["value"], 1, Some(1))?;
                let (_, is_obj) = target(host, this, &st)?;
                if is_obj {
                    return Err(error(format!(
                        "Cannot append properties to objects, use {}::offsetSet() instead",
                        this.class_name()
                    )));
                }
                write_dimension(host, this, &st, None, a.raw(0))?;
                Ok(Value::Null)
            }
            "getarraycopy" => {
                Args::none(func, args)?;
                let (tgt, _) = target(host, this, &st)?;
                let t = table(host, &tgt)?;
                Ok(Value::Array(ZArray::from_table((*t).clone())))
            }
            "count" => {
                Args::none(func, args)?;
                count_helper(host, this, &st).map(Value::Int)
            }
            "getflags" => {
                Args::none(func, args)?;
                st.array(|s| Value::Int(s.flags & !INT_MASK))
            }
            "setflags" => {
                let a = Args::new(func, args, &["flags"], 1, Some(1))?;
                let f = a.long(host, 0)?;
                st.array(|s| s.flags = (s.flags & INT_MASK) | (f & !INT_MASK))?;
                Ok(Value::Null)
            }
            "asort" | "ksort" => {
                let a = Args::new(func, args, &["flags"], 0, Some(1))?;
                let flags = a.opt_long(host, 0, 0)?;
                sort_method(host, this, &st, method, Some(Value::Int(flags)))
            }
            "uasort" | "uksort" => {
                let a = Args::new(func, args, &["callback"], 1, Some(1))?;
                sort_method(host, this, &st, method, Some(a.raw(0)))
            }
            "natsort" | "natcasesort" => {
                Args::none(func, args)?;
                sort_method(host, this, &st, method, None)
            }
            "serialize" => {
                Args::none(func, args)?;
                let (flags, array) = st.array(|s| (s.flags, s.array.clone()))?;
                let mut all = vec![Value::Int(flags & CLONE_MASK)];
                if flags & IS_SELF == 0 {
                    all.push(array);
                }
                all.push(Value::Array(host.std_properties(this)));
                let pieces = serial::serialize_seq(host, &all)?;
                let n = pieces.len();
                let mut buf = b"x:".to_vec();
                for (i, p) in pieces.into_iter().enumerate() {
                    if i == n - 1 {
                        buf.extend_from_slice(b"m:");
                        buf.extend(p);
                    } else {
                        buf.extend(p);
                        if i > 0 {
                            buf.push(b';');
                        }
                    }
                }
                Ok(Value::Str(buf.into()))
            }
            "unserialize" => {
                let a = Args::new(func, args, &["data"], 1, Some(1))?;
                let data = a.str(host, 0)?;
                legacy_unserialize(host, func, this, &st, data.as_bytes())?;
                Ok(Value::Null)
            }
            "__serialize" => {
                Args::none(func, args)?;
                let (flags, array, ic) =
                    st.array(|s| (s.flags, s.array.clone(), s.iterator_class.clone()))?;
                let storage = if flags & IS_SELF != 0 {
                    Value::Null
                } else {
                    array
                };
                let props = host.std_properties(this);
                let ic = if ic.eq_ignore_ascii_case(b"ArrayIterator") {
                    Value::Null
                } else {
                    Value::Str(ic)
                };
                Ok(Value::Array(ZArray::from_values([
                    Value::Int(flags & CLONE_MASK),
                    storage,
                    Value::Array(ZArray::from_table(props.to_symtable())),
                    ic,
                ])))
            }
            "__unserialize" => {
                let a = Args::new(func, args, &["data"], 1, Some(1))?;
                let data = a.array(0)?;
                unserialize(host, func, this, &st, &data)?;
                Ok(Value::Null)
            }
            "__debuginfo" => {
                Args::none(func, args)?;
                debug_info(host, this, &st).map(Value::Array)
            }
            "getiterator" => {
                Args::none(func, args)?;
                let (flags, ic) = st.array(|s| (s.flags, s.iterator_class.clone()))?;
                let it = host.create_object(&ic)?;
                let ist = state_of(host, &it)?;
                let this_val = Value::Object(this.clone());
                ist.array(|s| {
                    s.flags = (flags & CLONE_MASK) | USE_OTHER;
                    s.array = this_val;
                    s.iterator_class = ic;
                })?;
                Ok(Value::Object(it))
            }
            "exchangearray" => {
                let a = Args::new(func, args, &["array"], 1, Some(1))?;
                let array = array_or_object_arg(&a, 0)?;
                if st.array(|s| s.apply_count)? > 0 {
                    return Err(sorting_error());
                }
                let (tgt, _) = target(host, this, &st)?;
                let old = table(host, &tgt)?;
                let copy = Value::Array(ZArray::from_table((*old).clone()));
                set_array(host, func, this, &st, &array, 0, true)?;
                Ok(copy)
            }
            "setiteratorclass" => {
                let a = Args::new(func, args, &["iteratorClass"], 1, Some(1))?;
                let c = class_arg(host, &a, 0)?;
                st.array(|s| s.iterator_class = c)?;
                Ok(Value::Null)
            }
            "getiteratorclass" => {
                Args::none(func, args)?;
                st.array(|s| Value::Str(s.iterator_class.clone()))
            }
            "rewind" => {
                Args::none(func, args)?;
                rewind(host, this, &st)?;
                Ok(Value::Null)
            }
            "seek" => {
                let a = Args::new(func, args, &["offset"], 1, Some(1))?;
                let opos = a.long(host, 0)?;
                if opos >= 0 {
                    rewind(host, this, &st)?;
                    let mut ok = true;
                    let mut position = opos;
                    while position > 0 {
                        position -= 1;
                        ok = next(host, this, &st)?;
                        if !ok {
                            break;
                        }
                    }
                    if ok && current_value(host, this, &st)?.is_some() {
                        return Ok(Value::Null);
                    }
                }
                Err(out_of_bounds(format!(
                    "Seek position {opos} is out of range"
                )))
            }
            "current" => {
                Args::none(func, args)?;
                current_value(host, this, &st).map(Option::unwrap_or_default)
            }
            "key" => {
                Args::none(func, args)?;
                current_key(host, this, &st)
            }
            "next" => {
                Args::none(func, args)?;
                next(host, this, &st)?;
                Ok(Value::Null)
            }
            "valid" => {
                Args::none(func, args)?;
                current_value(host, this, &st).map(|v| Value::Bool(v.is_some()))
            }
            "haschildren" => {
                Args::none(func, args)?;
                let flags = st.array(|s| s.flags)?;
                Ok(Value::Bool(match current_value(host, this, &st)? {
                    Some(Value::Array(_)) => true,
                    Some(Value::Object(_)) => flags & CHILD_ARRAYS_ONLY == 0,
                    _ => false,
                }))
            }
            "getchildren" => {
                Args::none(func, args)?;
                let flags = st.array(|s| s.flags)?;
                let Some(entry) = current_value(host, this, &st)? else {
                    return Ok(Value::Null);
                };
                if let Value::Object(o) = &entry {
                    if flags & CHILD_ARRAYS_ONLY != 0 {
                        return Ok(Value::Null);
                    }
                    let cls = this.class_name();
                    if host.instance_of(o, &cls.to_string_lossy()) {
                        return Ok(entry);
                    }
                }
                let o = host.instantiate(&this.class_name(), &[entry, Value::Int(flags)])?;
                Ok(Value::Object(o))
            }
            _ => Err(RuntimeError::Error(format!(
                "Call to undefined method {func}()"
            ))),
        }
    })())
}

/// `spl_array_method`: runs one of the global sort functions on the
/// storage while modifications are prohibited.
fn sort_method(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &Rc<SplObject>,
    func: &str,
    extra: Option<Value>,
) -> RtResult<Value> {
    let (tgt, _) = target(host, this, st)?;
    let t = table(host, &tgt)?;
    let r = PhpRef::new(Value::Array(t));
    st.array(|s| s.apply_count += 1)?;
    let res = (|| -> RtResult<()> {
        match func {
            "asort" | "ksort" => {
                let flags = match extra {
                    Some(Value::Int(n)) => n,
                    _ => 0,
                };
                if func == "asort" {
                    php_stdarr::sort::asort(host, &r, flags)
                } else {
                    php_stdarr::sort::ksort(host, &r, flags)
                }
            }
            "natsort" => php_stdarr::sort::natsort(host, &r),
            "natcasesort" => php_stdarr::sort::natcasesort(host, &r),
            _ => {
                let cb = extra.unwrap_or_default();
                if let Err(reason) = host.check_callable(&cb) {
                    return Err(type_error(format!(
                        "{func}(): Argument #2 ($callback) must be a valid callback, {reason}"
                    )));
                }
                user_sort(host, &r, &cb, func == "uksort")
            }
        }
    })();
    st.array(|s| s.apply_count -= 1)?;
    if let Value::Array(sorted) = r.get() {
        with_ht(host, &tgt, |ht| {
            let mut new_table = (*sorted).clone();
            new_table.take_iterators_from(ht);
            *ht = new_table;
        })?;
    }
    res?;
    Ok(Value::Bool(true))
}

fn user_sort<'h>(
    host: &mut (dyn SplHost + 'h),
    r: &PhpRef,
    cb: &Value,
    by_key: bool,
) -> RtResult<()> {
    let mut call = |h: &mut (dyn SplHost + 'h), a: &[Value]| h.call_callable(cb, a);
    if by_key {
        php_stdarr::sort::uksort::<dyn SplHost + 'h>(host, r, &mut call)
    } else {
        php_stdarr::sort::uasort::<dyn SplHost + 'h>(host, r, &mut call)
    }
}

fn unserialize(
    host: &mut dyn SplHost,
    func: &str,
    this: &Obj,
    st: &Rc<SplObject>,
    data: &ZArray,
) -> RtResult<()> {
    let flags = data.get_int(0).map(Value::deref);
    let storage = data.get_int(1).map(Value::deref);
    let members = data.get_int(2).map(Value::deref);
    let iterator_class = data.get_int(3).map(Value::deref);
    let (Some(Value::Int(flags)), Some(storage), Some(Value::Array(members))) =
        (flags, storage, members)
    else {
        return Err(unexpected_value(
            "Incomplete or ill-typed serialization data",
        ));
    };
    if !matches!(iterator_class, None | Some(Value::Null | Value::Str(_))) {
        return Err(unexpected_value(
            "Incomplete or ill-typed serialization data",
        ));
    }
    st.array(|s| {
        s.flags &= !CLONE_MASK;
        s.flags |= flags & CLONE_MASK;
    })?;
    if flags & IS_SELF != 0 {
        st.array(|s| s.array = Value::Null)?;
    } else {
        if !matches!(storage, Value::Array(_) | Value::Object(_)) {
            return Err(invalid_argument(
                "Passed variable is not an array or object",
            ));
        }
        set_array(host, func, this, st, &storage, 0, true)?;
    }
    host.properties_load(this, &members)?;
    if let Some(Value::Str(c)) = iterator_class {
        match host.class_is_a(&c, "ArrayIterator") {
            None => {
                return Err(unexpected_value(format!(
                    "Cannot deserialize ArrayObject with iterator class '{}'; no such class exists",
                    c.to_string_lossy()
                )))
            }
            Some(false) => {
                return Err(unexpected_value(format!(
                    "Cannot deserialize ArrayObject with iterator class '{}'; this class is not derived from ArrayIterator",
                    c.to_string_lossy()
                )))
            }
            Some(true) => st.array(|s| s.iterator_class = c)?,
        }
    }
    Ok(())
}

fn legacy_unserialize(
    host: &mut dyn SplHost,
    func: &str,
    this: &Obj,
    st: &Rc<SplObject>,
    buf: &[u8],
) -> RtResult<()> {
    if buf.is_empty() {
        return Ok(());
    }
    if st.array(|s| s.apply_count)? > 0 {
        return Err(sorting_error());
    }
    let err = |p: usize| unexpected_value(format!("Error at offset {p} of {} bytes", buf.len()));
    if buf.first() != Some(&b'x') {
        return Err(err(0));
    }
    if buf.get(1) != Some(&b':') {
        return Err(err(1));
    }
    let mut p = 2usize;
    let (zflags, np) = serial::unserialize_at(host, buf, p).map_err(err)?;
    let Value::Int(flags) = zflags else {
        return Err(err(np));
    };
    p = np - 1;
    if buf.get(p) != Some(&b';') {
        return Err(err(p));
    }
    p += 1;
    if flags & IS_SELF != 0 {
        st.array(|s| {
            s.flags &= !CLONE_MASK;
            s.flags |= flags & CLONE_MASK;
            s.array = Value::Null;
        })?;
    } else {
        if !matches!(buf.get(p), Some(b'a' | b'O' | b'C' | b'r')) {
            return Err(err(p));
        }
        let (array, np) = serial::unserialize_at(host, buf, p).map_err(err)?;
        if !matches!(array, Value::Array(_) | Value::Object(_)) {
            return Err(err(np));
        }
        p = np;
        st.array(|s| {
            s.flags &= !CLONE_MASK;
            s.flags |= flags & CLONE_MASK;
        })?;
        if let Value::Array(_) = array {
            st.array(|s| {
                s.array = array;
                s.ht_iter = None;
            })?;
        } else {
            set_array(host, func, this, st, &array, 0, true)?;
        }
        if buf.get(p) != Some(&b';') {
            return Err(err(p));
        }
        p += 1;
    }
    if buf.get(p) != Some(&b'm') {
        return Err(err(p));
    }
    p += 1;
    if buf.get(p) != Some(&b':') {
        return Err(err(p));
    }
    p += 1;
    let (members, np) = serial::unserialize_at(host, buf, p).map_err(err)?;
    let Value::Array(members) = members else {
        return Err(err(np));
    };
    host.properties_load(this, &members)?;
    Ok(())
}

/// The value name used by `TypeError` messages (re-exported helper).
#[must_use]
pub fn given(v: &Value) -> String {
    value_name(v)
}
