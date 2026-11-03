//! `SplFixedArray` (`spl_fixedarray.c`).

use crate::args::Args;
use crate::exceptions::{error, invalid_argument, out_of_bounds, type_error};
use crate::host::{ObjectIterator, SplHost};
use crate::object::{state_of, SplObject};
use crate::util::{this_obj, DimKind};
use php_runtime::convert::dval_to_lval_safe;
use php_runtime::numeric::handle_numeric_str;
use php_runtime::{ArrayKey, ErrorLevel, HashTable, Obj, PropertyPurpose, RtResult, Value, ZArray};
use std::rc::Rc;

/// The internal state of an `SplFixedArray` (`spl_fixedarray`).
#[derive(Debug, Clone, Default)]
pub struct FixedArray {
    /// The elements; the size is `elements.len()`.
    pub elements: Vec<Value>,
}

impl FixedArray {
    /// Creates an array of `size` nulls.
    #[must_use]
    pub fn with_size(size: usize) -> Self {
        FixedArray {
            elements: vec![Value::Null; size],
        }
    }

    /// `spl_fixedarray_resize`.
    pub fn resize(&mut self, size: usize) {
        self.elements.resize(size, Value::Null);
    }

    /// `toArray()`.
    #[must_use]
    pub fn to_array(&self) -> ZArray {
        ZArray::from_values(self.elements.iter().cloned())
    }
}

/// `spl_offset_convert_to_ulong`: converts an offset to an index. Negative
/// numbers map to huge indexes (always out of range).
pub fn offset_to_index(host: &mut dyn SplHost, offset: &Value) -> RtResult<u64> {
    #[allow(clippy::cast_sign_loss)]
    Ok(match offset {
        Value::Int(n) => *n as u64,
        Value::Str(s) => match handle_numeric_str(s.as_bytes()) {
            Some(n) => n as u64,
            None => return Err(illegal_offset(offset)),
        },
        Value::Float(d) => dval_to_lval_safe(*d, host)? as u64,
        Value::Bool(b) => u64::from(*b),
        Value::Reference(r) => return offset_to_index(host, &r.get()),
        Value::Resource(r) => {
            host.diagnostic(
                ErrorLevel::Warning,
                &format!(
                    "Resource ID#{} used as offset, casting to integer ({})",
                    r.handle(),
                    r.handle()
                ),
            )?;
            r.handle() as u64
        }
        _ => return Err(illegal_offset(offset)),
    })
}

fn illegal_offset(offset: &Value) -> php_runtime::RuntimeError {
    type_error(format!(
        "Cannot access offset of type {} on SplFixedArray",
        offset.type_name()
    ))
}

fn index_error() -> php_runtime::RuntimeError {
    out_of_bounds("Index invalid or out of range")
}

fn no_append() -> php_runtime::RuntimeError {
    error("[] operator not supported for SplFixedArray")
}

fn read_helper(st: &SplObject, host: &mut dyn SplHost, offset: Option<&Value>) -> RtResult<Value> {
    let offset = offset.ok_or_else(no_append)?;
    let index = offset_to_index(host, offset)?;
    st.fixed(|f| {
        usize::try_from(index)
            .ok()
            .and_then(|i| f.elements.get(i).cloned())
    })?
    .ok_or_else(index_error)
}

fn write_helper(
    st: &SplObject,
    host: &mut dyn SplHost,
    offset: Option<&Value>,
    value: Value,
) -> RtResult<()> {
    let offset = offset.ok_or_else(no_append)?;
    let index = offset_to_index(host, offset)?;
    let old = st.fixed(|f| {
        usize::try_from(index)
            .ok()
            .and_then(|i| f.elements.get_mut(i))
            .map(|slot| std::mem::replace(slot, value.deref()))
    })?;
    match old {
        Some(old) => {
            drop(old);
            Ok(())
        }
        None => Err(index_error()),
    }
}

fn unset_helper(st: &SplObject, host: &mut dyn SplHost, offset: &Value) -> RtResult<()> {
    write_helper(st, host, Some(offset), Value::Null)
}

fn has_helper(
    st: &SplObject,
    host: &mut dyn SplHost,
    offset: &Value,
    check_empty: bool,
) -> RtResult<bool> {
    let index = offset_to_index(host, offset)?;
    let v = st.fixed(|f| {
        usize::try_from(index)
            .ok()
            .and_then(|i| f.elements.get(i).cloned())
    })?;
    match v {
        None => Ok(false),
        Some(v) if check_empty => v.to_bool(host),
        Some(v) => Ok(!v.is_null()),
    }
}

/// `SplFixedArray` methods. `method` is lowercase.
pub(crate) fn call(
    host: &mut dyn SplHost,
    method: &str,
    this: Option<&Obj>,
    args: &[Value],
) -> Option<RtResult<Value>> {
    Some(match method {
        "fromarray" => from_array(host, args),
        _ => {
            let this = match this_obj(this) {
                Ok(t) => t,
                Err(e) => return Some(Err(e)),
            };
            match method {
                "__construct" => construct(host, this, args),
                "__wakeup" => wakeup(host, this, args),
                "__serialize" => serialize(host, this, args),
                "__unserialize" => unserialize(host, this, args),
                "count" | "getsize" => simple(host, this, args, method),
                "toarray" | "jsonserialize" => simple(host, this, args, method),
                "setsize" => set_size(host, this, args),
                "offsetexists" => {
                    let f = "SplFixedArray::offsetExists";
                    (|| {
                        let a = Args::new(f, args, &["index"], 1, Some(1))?;
                        let st = state_of(host, this)?;
                        has_helper(&st, host, &a.raw(0), false).map(Value::Bool)
                    })()
                }
                "offsetget" => (|| {
                    let a = Args::new("SplFixedArray::offsetGet", args, &["index"], 1, Some(1))?;
                    let st = state_of(host, this)?;
                    read_helper(&st, host, Some(&a.raw(0)))
                })(),
                "offsetset" => (|| {
                    let a = Args::new(
                        "SplFixedArray::offsetSet",
                        args,
                        &["index", "value"],
                        2,
                        Some(2),
                    )?;
                    let st = state_of(host, this)?;
                    let off = a.raw(0);
                    write_helper(&st, host, Some(&off), a.raw(1)).map(|()| Value::Null)
                })(),
                "offsetunset" => (|| {
                    let a = Args::new("SplFixedArray::offsetUnset", args, &["index"], 1, Some(1))?;
                    let st = state_of(host, this)?;
                    unset_helper(&st, host, &a.raw(0)).map(|()| Value::Null)
                })(),
                "getiterator" => Args::none("SplFixedArray::getIterator", args)
                    .and_then(|()| crate::iter::internal_iterator_object(host, this)),
                _ => return None,
            }
        }
    })
}

fn construct(host: &mut dyn SplHost, this: &Obj, args: &[Value]) -> RtResult<Value> {
    let a = Args::new("SplFixedArray::__construct", args, &["size"], 0, Some(1))?;
    let size = a.opt_long(host, 0, 0)?;
    if size < 0 {
        return Err(a.value_error(0, "must be greater than or equal to 0"));
    }
    let st = state_of(host, this)?;
    st.fixed(|f| {
        if f.elements.is_empty() {
            f.resize(usize::try_from(size).unwrap_or(0));
        }
    })?;
    Ok(Value::Null)
}

fn simple(host: &mut dyn SplHost, this: &Obj, args: &[Value], method: &str) -> RtResult<Value> {
    let name = match method {
        "count" => "SplFixedArray::count",
        "getsize" => "SplFixedArray::getSize",
        "toarray" => "SplFixedArray::toArray",
        _ => "SplFixedArray::jsonSerialize",
    };
    Args::none(name, args)?;
    let st = state_of(host, this)?;
    st.fixed(|f| match method {
        "count" | "getsize" => Value::Int(i64::try_from(f.elements.len()).unwrap_or(i64::MAX)),
        _ => Value::Array(f.to_array()),
    })
}

fn set_size(host: &mut dyn SplHost, this: &Obj, args: &[Value]) -> RtResult<Value> {
    let a = Args::new("SplFixedArray::setSize", args, &["size"], 1, Some(1))?;
    let size = a.long(host, 0)?;
    if size < 0 {
        return Err(a.value_error(0, "must be greater than or equal to 0"));
    }
    let st = state_of(host, this)?;
    // Drop the removed elements after releasing the borrow (destructors).
    let removed = st.fixed(|f| {
        let n = usize::try_from(size).unwrap_or(usize::MAX);
        if n < f.elements.len() {
            f.elements.split_off(n)
        } else {
            f.resize(n);
            Vec::new()
        }
    })?;
    drop(removed);
    Ok(Value::Bool(true))
}

fn wakeup(host: &mut dyn SplHost, this: &Obj, args: &[Value]) -> RtResult<Value> {
    Args::none("SplFixedArray::__wakeup", args)?;
    let st = state_of(host, this)?;
    if st.fixed(|f| f.elements.is_empty())? {
        let props = host.std_properties(this);
        let elements: Vec<Value> = props.values().map(Value::deref).collect();
        st.fixed(|f| f.elements = elements)?;
        host.set_std_properties(this, ZArray::new())?;
    }
    Ok(Value::Null)
}

fn serialize(host: &mut dyn SplHost, this: &Obj, args: &[Value]) -> RtResult<Value> {
    Args::none("SplFixedArray::__serialize", args)?;
    let st = state_of(host, this)?;
    let props = host.std_properties(this);
    let elements = st.fixed(|f| f.elements.clone())?;
    let mut out = HashTable::with_capacity(elements.len() + props.len());
    for v in elements {
        let _ = out.push(v);
    }
    for (k, v) in props.iter() {
        if let ArrayKey::Str(s) = k {
            let _ = out.add(ArrayKey::Str(s.clone()), v.clone());
        }
    }
    Ok(Value::Array(ZArray::from_table(out)))
}

fn unserialize(host: &mut dyn SplHost, this: &Obj, args: &[Value]) -> RtResult<Value> {
    let a = Args::new("SplFixedArray::__unserialize", args, &["data"], 1, Some(1))?;
    let data = a.array(0)?;
    let st = state_of(host, this)?;
    if !st.fixed(|f| f.elements.is_empty())? {
        return Ok(Value::Null);
    }
    let mut elements = Vec::new();
    let mut members = Vec::new();
    for (k, v) in data.iter() {
        match k {
            ArrayKey::Int(_) => elements.push(v.deref()),
            ArrayKey::Str(s) => members.push((s.clone(), v.clone())),
        }
    }
    st.fixed(|f| f.elements = elements)?;
    for (k, v) in members {
        host.property_write(this, &k, v)?;
    }
    Ok(Value::Null)
}

fn from_array(host: &mut dyn SplHost, args: &[Value]) -> RtResult<Value> {
    let a = Args::new(
        "SplFixedArray::fromArray",
        args,
        &["array", "preserveKeys"],
        1,
        Some(2),
    )?;
    let data = a.array(0)?;
    let save_indexes = a.opt_bool(host, 1, true)?;
    let num = data.len();
    let fixed = if num > 0 && save_indexes {
        let size: usize = if data.is_packed() {
            data.used() as usize
        } else {
            let mut max_index: i64 = 0;
            for k in data.keys() {
                match k {
                    ArrayKey::Int(n) if *n >= 0 => max_index = max_index.max(*n),
                    _ => {
                        return Err(invalid_argument(
                            "array must contain only positive integer keys",
                        ))
                    }
                }
            }
            let Some(tmp) = max_index.checked_add(1) else {
                return Err(invalid_argument("integer overflow detected"));
            };
            usize::try_from(tmp).unwrap_or(usize::MAX)
        };
        let mut f = FixedArray::with_size(size);
        for (k, v) in data.iter() {
            if let ArrayKey::Int(n) = k {
                if let Some(slot) = usize::try_from(*n).ok().and_then(|i| f.elements.get_mut(i)) {
                    *slot = v.deref();
                }
            }
        }
        f
    } else {
        FixedArray {
            elements: data.values().map(Value::deref).collect(),
        }
    };
    let obj = host.create_object(&"SplFixedArray".into())?;
    let st = state_of(host, &obj)?;
    st.fixed(|f| *f = fixed)?;
    Ok(Value::Object(obj))
}

// ---------------------------------------------------------------------------
// Object handlers
// ---------------------------------------------------------------------------

/// `read_dimension` handler.
pub fn read_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    offset: Option<&Value>,
    kind: DimKind,
) -> RtResult<Value> {
    if kind == DimKind::Is {
        let present = match offset {
            Some(o) => has_dimension(host, obj, st, o, false)?,
            None => false,
        };
        if !present {
            return Ok(Value::Null);
        }
    }
    if host.has_user_method(obj, "offsetGet") {
        let off = offset.cloned().unwrap_or_default();
        return host.call_method(obj, "offsetGet", &[off]);
    }
    read_helper(st, host, offset)
}

/// `write_dimension` handler.
pub fn write_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    offset: Option<&Value>,
    value: Value,
) -> RtResult<()> {
    if host.has_user_method(obj, "offsetSet") {
        let off = offset.cloned().unwrap_or_default();
        return host
            .call_method(obj, "offsetSet", &[off, value])
            .map(|_| ());
    }
    write_helper(st, host, offset, value)
}

/// `unset_dimension` handler.
pub fn unset_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    offset: &Value,
) -> RtResult<()> {
    if host.has_user_method(obj, "offsetUnset") {
        return host
            .call_method(obj, "offsetUnset", &[offset.clone()])
            .map(|_| ());
    }
    unset_helper(st, host, offset)
}

/// `has_dimension` handler.
pub fn has_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    offset: &Value,
    check_empty: bool,
) -> RtResult<bool> {
    if host.has_user_method(obj, "offsetExists") {
        let r = host.call_method(obj, "offsetExists", &[offset.clone()])?;
        return r.to_bool(host);
    }
    has_helper(st, host, offset, check_empty)
}

/// `count_elements` handler.
pub fn count_elements(host: &mut dyn SplHost, obj: &Obj, st: &Rc<SplObject>) -> RtResult<i64> {
    if host.has_user_method(obj, "count") {
        let r = host.call_method(obj, "count", &[])?;
        return r.to_long(host);
    }
    st.fixed(|f| i64::try_from(f.elements.len()).unwrap_or(i64::MAX))
}

/// `get_properties_for` handler: the elements followed by the properties.
pub fn properties_for(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    purpose: PropertyPurpose,
) -> RtResult<ZArray> {
    let _ = purpose;
    let props = host.std_properties(obj);
    let elements = st.fixed(|f| f.elements.clone())?;
    let mut ht = HashTable::with_capacity(elements.len());
    for v in elements {
        let _ = ht.push(v);
    }
    for (k, v) in props.iter() {
        match k {
            ArrayKey::Str(_) => {
                let _ = ht.add(k.clone(), v.clone());
            }
            ArrayKey::Int(_) => {
                ht.insert(k.clone(), v.clone());
            }
        }
    }
    Ok(ZArray::from_table(ht))
}

/// The `SplFixedArray` iterator (`spl_fixedarray_it`).
#[derive(Debug)]
pub struct FixedArrayIter {
    obj: Obj,
    st: Rc<SplObject>,
    current: i64,
}

impl FixedArrayIter {
    /// Creates the iterator.
    #[must_use]
    pub fn new(obj: Obj, st: Rc<SplObject>) -> Self {
        FixedArrayIter {
            obj,
            st,
            current: 0,
        }
    }
}

impl ObjectIterator for FixedArrayIter {
    fn rewind(&mut self, _host: &mut dyn SplHost) -> RtResult<()> {
        self.current = 0;
        Ok(())
    }
    fn valid(&mut self, _host: &mut dyn SplHost) -> RtResult<bool> {
        let n = self.st.fixed(|f| f.elements.len())?;
        Ok(self.current >= 0 && usize::try_from(self.current).is_ok_and(|c| c < n))
    }
    fn current(&mut self, host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        let _ = &self.obj;
        read_helper(&self.st, host, Some(&Value::Int(self.current))).map(Some)
    }
    fn key(&mut self, _host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        Ok(Some(Value::Int(self.current)))
    }
    fn next(&mut self, _host: &mut dyn SplHost) -> RtResult<()> {
        self.current += 1;
        Ok(())
    }
}
