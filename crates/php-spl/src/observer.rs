//! `SplObjectStorage` and `MultipleIterator` (`spl_observer.c`).

use crate::args::Args;
use crate::exceptions::{invalid_argument, out_of_bounds, runtime, type_error, unexpected_value};
use crate::handlers::{std_has_dimension, std_read_dimension};
use crate::host::SplHost;
use crate::object::{state_of, SplObject};
use crate::serial;
use crate::util::{private_prop, this_obj, to_long, DimKind};
use php_runtime::{ops, ArrayKey, HashTable, Obj, RtResult, RuntimeError, Value, ZArray, ZStr};
use std::cell::Cell;
use std::rc::Rc;

/// `MultipleIterator::MIT_NEED_ANY`.
pub const MIT_NEED_ANY: i64 = 0;
/// `MultipleIterator::MIT_NEED_ALL`.
pub const MIT_NEED_ALL: i64 = 1;
/// `MultipleIterator::MIT_KEYS_NUMERIC`.
pub const MIT_KEYS_NUMERIC: i64 = 0;
/// `MultipleIterator::MIT_KEYS_ASSOC`.
pub const MIT_KEYS_ASSOC: i64 = 2;

thread_local! {
    static GET_HASH_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Resets the `getHash()` nesting counter (request startup).
pub fn reset_get_hash_depth() {
    GET_HASH_DEPTH.with(|d| d.set(0));
}

fn mutating_within_get_hash() -> RtResult<()> {
    if GET_HASH_DEPTH.with(Cell::get) > 0 {
        return Err(RuntimeError::Error(
            "Modification of SplObjectStorage during getHash() is prohibited".to_string(),
        ));
    }
    Ok(())
}

/// The internal state (`spl_SplObjectStorage`). Each storage entry is a
/// two-element array `[object, info]` keyed by the object handle (or by the
/// string returned from an overridden `getHash()`).
#[derive(Debug, Clone)]
pub struct ObjectStorage {
    /// The entries.
    pub storage: HashTable,
    /// The iteration index (`key()`).
    pub index: i64,
    /// The iteration position.
    pub pos: u32,
    /// `MultipleIterator` flags.
    pub flags: i64,
}

impl ObjectStorage {
    /// A new storage for `SplObjectStorage` or `MultipleIterator`.
    #[must_use]
    pub fn new(base: &str) -> Self {
        let _ = base;
        ObjectStorage {
            storage: HashTable::new(),
            index: 0,
            pos: 0,
            flags: 0,
        }
    }

    fn reset(&mut self) {
        self.pos = self.storage.valid_pos(0);
        self.index = 0;
    }

    fn current(&self) -> Option<(Obj, Value)> {
        let p = self.storage.valid_pos(self.pos);
        self.storage.at(p).and_then(|(_, v)| entry(v))
    }

    /// The `(object, info)` pairs in order.
    #[must_use]
    pub fn entries(&self) -> Vec<(Obj, Value)> {
        self.storage.values().filter_map(entry).collect()
    }
}

fn entry(v: &Value) -> Option<(Obj, Value)> {
    let Value::Array(a) = v else { return None };
    let o = match a.get_int(0) {
        Some(Value::Object(o)) => o.clone(),
        _ => return None,
    };
    Some((o, a.get_int(1).cloned().unwrap_or_default()))
}

fn make_entry(obj: &Obj, inf: Value) -> Value {
    Value::Array(ZArray::from_values([Value::Object(obj.clone()), inf]))
}

/// `spl_object_hash` format.
#[must_use]
pub fn object_hash(obj: &Obj) -> ZStr {
    ZStr::from(format!("{:016x}0000000000000000", obj.handle()))
}

/// `spl_object_storage_get_hash`.
fn get_hash(host: &mut dyn SplHost, this: &Obj, obj: &Obj) -> RtResult<ArrayKey> {
    if !host.has_user_method(this, "getHash") {
        return Ok(ArrayKey::Int(i64::from(obj.handle())));
    }
    GET_HASH_DEPTH.with(|d| d.set(d.get() + 1));
    let r = host.call_method(this, "getHash", &[Value::Object(obj.clone())]);
    GET_HASH_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    match r?.deref() {
        Value::Str(s) => Ok(ArrayKey::Str(s)),
        other => Err(type_error(format!(
            "{}::getHash(): Return value must be of type string, {} returned",
            this.class_name(),
            crate::args::value_name(&other)
        ))),
    }
}

fn overridden(host: &mut dyn SplHost, this: &Obj, method: &str) -> bool {
    host.has_user_method(this, "getHash") || host.has_user_method(this, method)
}

/// `spl_object_storage_attach`.
pub fn attach(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &SplObject,
    obj: &Obj,
    inf: Option<Value>,
) -> RtResult<()> {
    mutating_within_get_hash()?;
    let key = if overridden(host, this, "offsetSet") {
        get_hash(host, this, obj)?
    } else {
        ArrayKey::Int(i64::from(obj.handle()))
    };
    let inf = inf.unwrap_or_default();
    let garbage = st.storage(|s| {
        if let Some(existing) = s.storage.get_mut(&key) {
            let old = existing.clone();
            if let Some((o, _)) = entry(&old) {
                *existing = make_entry(&o, inf);
            }
            Some(old)
        } else {
            s.storage.insert(key, make_entry(obj, inf));
            None
        }
    })?;
    drop(garbage);
    Ok(())
}

/// `spl_object_storage_detach`: whether an entry was removed.
pub fn detach(host: &mut dyn SplHost, this: &Obj, st: &SplObject, obj: &Obj) -> RtResult<bool> {
    mutating_within_get_hash()?;
    let key = if overridden(host, this, "offsetUnset") {
        get_hash(host, this, obj)?
    } else {
        ArrayKey::Int(i64::from(obj.handle()))
    };
    let removed = st.storage(|s| s.storage.remove(&key))?;
    Ok(removed.is_some())
}

/// `spl_object_storage_contains`.
pub fn contains(host: &mut dyn SplHost, this: &Obj, st: &SplObject, obj: &Obj) -> RtResult<bool> {
    let key = get_hash(host, this, obj)?;
    st.storage(|s| s.storage.contains_key(&key))
}

fn addall(host: &mut dyn SplHost, this: &Obj, st: &SplObject, other: &SplObject) -> RtResult<()> {
    let entries = other.storage(|s| s.entries())?;
    for (o, inf) in entries {
        attach(host, this, st, &o, Some(inf))?;
    }
    st.storage(|s| s.index = 0)?;
    Ok(())
}

/// Clones the storage of `old` into `st` (`spl_object_storage_new_ex` with
/// `orig`): entries are re-attached through the clone's hash function.
pub fn clone_into(
    host: &mut dyn SplHost,
    new_obj: &Obj,
    st: &SplObject,
    old: &SplObject,
) -> RtResult<()> {
    let flags = old.storage(|s| s.flags)?;
    st.storage(|s| s.flags = flags)?;
    addall(host, new_obj, st, old)
}

/// `SplObjectStorage` / `MultipleIterator` methods.
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
    if class == "MultipleIterator" {
        return multiple_call(host, method, this, args);
    }
    let func: &str = match method {
        "attach" => "SplObjectStorage::attach",
        "detach" => "SplObjectStorage::detach",
        "contains" => "SplObjectStorage::contains",
        "addall" => "SplObjectStorage::addAll",
        "removeall" => "SplObjectStorage::removeAll",
        "removeallexcept" => "SplObjectStorage::removeAllExcept",
        "getinfo" => "SplObjectStorage::getInfo",
        "setinfo" => "SplObjectStorage::setInfo",
        "count" => "SplObjectStorage::count",
        "rewind" => "SplObjectStorage::rewind",
        "valid" => "SplObjectStorage::valid",
        "key" => "SplObjectStorage::key",
        "current" => "SplObjectStorage::current",
        "next" => "SplObjectStorage::next",
        "seek" => "SplObjectStorage::seek",
        "unserialize" => "SplObjectStorage::unserialize",
        "serialize" => "SplObjectStorage::serialize",
        "offsetexists" => "SplObjectStorage::offsetExists",
        "offsetget" => "SplObjectStorage::offsetGet",
        "offsetset" => "SplObjectStorage::offsetSet",
        "offsetunset" => "SplObjectStorage::offsetUnset",
        "gethash" => "SplObjectStorage::getHash",
        "__serialize" => "SplObjectStorage::__serialize",
        "__unserialize" => "SplObjectStorage::__unserialize",
        "__debuginfo" => "SplObjectStorage::__debugInfo",
        _ => return None,
    };
    Some((|| -> RtResult<Value> {
        let st = state_of(host, this)?;
        match method {
            "attach" | "offsetset" => {
                let a = Args::new(func, args, &["object", "info"], 1, Some(2))?;
                let obj = a.object(0)?;
                let inf = a.has(1).then(|| a.raw(1));
                attach(host, this, &st, &obj, inf)?;
                Ok(Value::Null)
            }
            "detach" | "offsetunset" => {
                let a = Args::new(func, args, &["object"], 1, Some(1))?;
                let obj = a.object(0)?;
                detach(host, this, &st, &obj)?;
                st.storage(ObjectStorage::reset)?;
                Ok(Value::Null)
            }
            "contains" | "offsetexists" => {
                let a = Args::new(func, args, &["object"], 1, Some(1))?;
                let obj = a.object(0)?;
                contains(host, this, &st, &obj).map(Value::Bool)
            }
            "gethash" => {
                let a = Args::new(func, args, &["object"], 1, Some(1))?;
                Ok(Value::Str(object_hash(&a.object(0)?)))
            }
            "offsetget" => {
                let a = Args::new(func, args, &["object"], 1, Some(1))?;
                let obj = a.object(0)?;
                let key = get_hash(host, this, &obj)?;
                let e = st.storage(|s| s.storage.get(&key).and_then(entry))?;
                match e {
                    Some((_, inf)) => Ok(inf.deref()),
                    None => Err(unexpected_value("Object not found")),
                }
            }
            "addall" | "removeall" | "removeallexcept" => {
                let a = Args::new(func, args, &["storage"], 1, Some(1))?;
                let other_obj = a.object_of(host, 0, "SplObjectStorage")?;
                let other = state_of(host, &other_obj)?;
                match method {
                    "addall" => addall(host, this, &st, &other)?,
                    "removeall" => {
                        for (o, _) in other.storage(|s| s.entries())? {
                            detach(host, this, &st, &o)?;
                        }
                        st.storage(ObjectStorage::reset)?;
                    }
                    _ => {
                        for (o, _) in st.storage(|s| s.entries())? {
                            if !contains(host, &other_obj, &other, &o)? {
                                detach(host, this, &st, &o)?;
                            }
                        }
                        st.storage(ObjectStorage::reset)?;
                    }
                }
                st.storage(|s| Value::Int(to_long(s.storage.len())))
            }
            "getinfo" => {
                Args::none(func, args)?;
                st.storage(|s| s.current().map(|(_, i)| i).unwrap_or_default())
            }
            "setinfo" => {
                let a = Args::new(func, args, &["info"], 1, Some(1))?;
                let inf = a.raw(0);
                mutating_within_get_hash()?;
                let garbage = st.storage(|s| {
                    let p = s.storage.valid_pos(s.pos);
                    let slot = s.storage.at_mut(p)?;
                    let old = slot.clone();
                    let (o, _) = entry(&old)?;
                    *slot = make_entry(&o, inf);
                    Some(old)
                })?;
                drop(garbage);
                Ok(Value::Null)
            }
            "count" => {
                let a = Args::new(func, args, &["mode"], 0, Some(1))?;
                let _mode = a.opt_long(host, 0, 0)?;
                st.storage(|s| Value::Int(to_long(s.storage.len())))
            }
            "rewind" => {
                Args::none(func, args)?;
                st.storage(ObjectStorage::reset)?;
                Ok(Value::Null)
            }
            "valid" => {
                Args::none(func, args)?;
                st.storage(|s| Value::Bool(s.storage.valid_pos(s.pos) < s.storage.used()))
            }
            "key" => {
                Args::none(func, args)?;
                st.storage(|s| Value::Int(s.index))
            }
            "current" => {
                Args::none(func, args)?;
                match st.storage(|s| s.current())? {
                    Some((o, _)) => Ok(Value::Object(o)),
                    None => Err(runtime("Called current() on invalid iterator")),
                }
            }
            "next" => {
                Args::none(func, args)?;
                st.storage(|s| {
                    s.pos = s.storage.pos_forward(s.pos);
                    s.index += 1;
                })?;
                Ok(Value::Null)
            }
            "seek" => {
                let a = Args::new(func, args, &["offset"], 1, Some(1))?;
                let position = a.long(host, 0)?;
                st.storage(|s| seek(s, position))?
            }
            "serialize" => {
                Args::none(func, args)?;
                legacy_serialize(host, this, &st)
            }
            "unserialize" => {
                let a = Args::new(func, args, &["data"], 1, Some(1))?;
                let data = a.str(host, 0)?;
                legacy_unserialize(host, this, &st, data.as_bytes())?;
                Ok(Value::Null)
            }
            "__serialize" => {
                Args::none(func, args)?;
                let entries = st.storage(|s| s.entries())?;
                let mut flat = HashTable::with_capacity(entries.len() * 2);
                for (o, inf) in entries {
                    let _ = flat.push(Value::Object(o));
                    let _ = flat.push(inf);
                }
                let props = host.std_properties(this);
                Ok(Value::Array(ZArray::from_values([
                    Value::Array(ZArray::from_table(flat)),
                    Value::Array(ZArray::from_table(props.to_symtable())),
                ])))
            }
            "__unserialize" => {
                let a = Args::new(func, args, &["data"], 1, Some(1))?;
                let data = a.array(0)?;
                let storage = data.get_int(0).map(Value::deref);
                let members = data.get_int(1).map(Value::deref);
                let (Some(Value::Array(storage)), Some(Value::Array(members))) = (storage, members)
                else {
                    return Err(unexpected_value(
                        "Incomplete or ill-typed serialization data",
                    ));
                };
                if storage.len() % 2 != 0 {
                    return Err(unexpected_value("Odd number of elements"));
                }
                let mut key: Option<Value> = None;
                for v in storage.values() {
                    match key.take() {
                        Some(k) => {
                            let Value::Object(o) = k.deref() else {
                                return Err(unexpected_value("Non-object key"));
                            };
                            attach(host, this, &st, &o, Some(v.deref()))?;
                        }
                        None => key = Some(v.clone()),
                    }
                }
                host.properties_load(this, &members)?;
                Ok(Value::Null)
            }
            "__debuginfo" => {
                Args::none(func, args)?;
                debug_info(host, this, &st).map(Value::Array)
            }
            _ => unreachable!("filtered above"),
        }
    })())
}

fn seek(s: &mut ObjectStorage, position: i64) -> RtResult<Value> {
    if position < 0 || position >= to_long(s.storage.len()) {
        return Err(out_of_bounds(format!(
            "Seek position {position} is out of range"
        )));
    }
    if position == 0 {
        s.reset();
    } else if position > s.index {
        while position > s.index {
            s.pos = s.storage.pos_forward(s.pos);
            s.index += 1;
        }
    } else if position < s.index {
        if s.index - position > position {
            s.reset();
            while position > s.index {
                s.pos = s.storage.pos_forward(s.pos);
                s.index += 1;
            }
        } else {
            while position < s.index {
                s.pos = s.storage.pos_backward(s.pos);
                s.index -= 1;
            }
        }
    }
    Ok(Value::Null)
}

/// `spl_object_storage_debug_info` (also `MultipleIterator::__debugInfo`).
pub fn debug_info(host: &mut dyn SplHost, this: &Obj, st: &SplObject) -> RtResult<ZArray> {
    let props = host.std_properties(this);
    let mut out: HashTable = (*props).clone();
    let entries = st.storage(|s| s.entries())?;
    let mut storage = HashTable::new();
    for (o, inf) in entries {
        let mut t = HashTable::new();
        t.insert(ArrayKey::from("obj"), Value::Object(o));
        t.insert(ArrayKey::from("inf"), inf);
        let _ = storage.push(Value::Array(ZArray::from_table(t)));
    }
    out.insert(
        ArrayKey::Str(private_prop("SplObjectStorage", "storage")),
        Value::Array(ZArray::from_table(storage)),
    );
    Ok(ZArray::from_table(out))
}

fn legacy_serialize(host: &mut dyn SplHost, this: &Obj, st: &SplObject) -> RtResult<Value> {
    let entries = st.storage(|s| s.entries())?;
    let mut all = vec![Value::Int(to_long(entries.len()))];
    for (o, inf) in entries {
        all.push(Value::Object(o));
        all.push(inf);
    }
    all.push(Value::Array(host.std_properties(this)));
    let pieces = serial::serialize_seq(host, &all)?;
    let n = pieces.len();
    let mut buf = b"x:".to_vec();
    for (i, p) in pieces.into_iter().enumerate() {
        if i == n - 1 {
            buf.extend_from_slice(b"m:");
            buf.extend(p);
        } else if i == 0 {
            buf.extend(p);
        } else if i % 2 == 1 {
            buf.extend(p);
            buf.push(b',');
        } else {
            buf.extend(p);
            buf.push(b';');
        }
    }
    Ok(Value::Str(buf.into()))
}

fn legacy_unserialize(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &SplObject,
    buf: &[u8],
) -> RtResult<()> {
    if buf.is_empty() {
        return Ok(());
    }
    let err = |p: usize| unexpected_value(format!("Error at offset {p} of {} bytes", buf.len()));
    let mut p = 0usize;
    if buf.first() != Some(&b'x') {
        return Err(err(0));
    }
    if buf.get(1) != Some(&b':') {
        return Err(err(1));
    }
    p = p.max(2);
    let (count, np) = serial::unserialize_at(host, buf, p).map_err(err)?;
    let Value::Int(mut count) = count else {
        return Err(err(np));
    };
    p = np - 1; // back to the ';'
    if count < 0 {
        return Err(err(p));
    }
    while count > 0 {
        count -= 1;
        if buf.get(p) != Some(&b';') {
            return Err(err(p));
        }
        p += 1;
        if !matches!(buf.get(p), Some(b'O' | b'C' | b'r')) {
            return Err(err(p));
        }
        let (entry_v, np) = serial::unserialize_at(host, buf, p).map_err(err)?;
        p = np;
        let mut inf = None;
        if buf.get(p) == Some(&b',') {
            p += 1;
            let (v, np) = serial::unserialize_at(host, buf, p).map_err(err)?;
            p = np;
            inf = Some(v);
        }
        let Value::Object(o) = entry_v else {
            return Err(err(p));
        };
        attach(host, this, st, &o, inf)?;
    }
    if buf.get(p) != Some(&b';') {
        return Err(err(p));
    }
    p += 1;
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

// ---------------------------------------------------------------------------
// MultipleIterator
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn multiple_call(
    host: &mut dyn SplHost,
    method: &str,
    this: &Obj,
    args: &[Value],
) -> Option<RtResult<Value>> {
    let func: &str = match method {
        "__construct" => "MultipleIterator::__construct",
        "getflags" => "MultipleIterator::getFlags",
        "setflags" => "MultipleIterator::setFlags",
        "attachiterator" => "MultipleIterator::attachIterator",
        "detachiterator" => "MultipleIterator::detachIterator",
        "containsiterator" => "MultipleIterator::containsIterator",
        "countiterators" => "MultipleIterator::countIterators",
        "rewind" => "MultipleIterator::rewind",
        "valid" => "MultipleIterator::valid",
        "key" => "MultipleIterator::key",
        "current" => "MultipleIterator::current",
        "next" => "MultipleIterator::next",
        "__debuginfo" => "MultipleIterator::__debugInfo",
        _ => return None,
    };
    Some((|| -> RtResult<Value> {
        let st = state_of(host, this)?;
        match method {
            "__construct" => {
                let a = Args::new(func, args, &["flags"], 0, Some(1))?;
                let flags = a.opt_long(host, 0, MIT_NEED_ALL | MIT_KEYS_NUMERIC)?;
                st.storage(|s| s.flags = flags)?;
                Ok(Value::Null)
            }
            "getflags" => {
                Args::none(func, args)?;
                st.storage(|s| Value::Int(s.flags))
            }
            "setflags" => {
                let a = Args::new(func, args, &["flags"], 1, Some(1))?;
                let flags = a.long(host, 0)?;
                st.storage(|s| s.flags = flags)?;
                Ok(Value::Null)
            }
            "attachiterator" => {
                let a = Args::new(func, args, &["iterator", "info"], 1, Some(2))?;
                let it = a.object_of(host, 0, "Iterator")?;
                let info = a.raw(1);
                let info = match info {
                    Value::Null => None,
                    Value::Str(_) | Value::Int(_) => Some(info),
                    Value::Float(_) | Value::Bool(_) => {
                        match crate::args::parse_long_weak(host, &info)? {
                            crate::args::LongParse::Ok(n) => Some(Value::Int(n)),
                            _ => Some(Value::Str(info.to_zstr(host)?)),
                        }
                    }
                    _ => return Err(a.type_error(1, "string|int|null")),
                };
                if let Some(info) = &info {
                    let entries = st.storage(|s| {
                        s.reset();
                        s.entries()
                    })?;
                    for (_, inf) in &entries {
                        if ops::is_identical(info, inf)? {
                            return Err(invalid_argument("Key duplication error"));
                        }
                    }
                    st.storage(|s| s.pos = s.storage.used())?;
                }
                attach(host, this, &st, &it, info)?;
                Ok(Value::Null)
            }
            "detachiterator" => {
                let a = Args::new(func, args, &["iterator"], 1, Some(1))?;
                let it = a.object_of(host, 0, "Iterator")?;
                detach(host, this, &st, &it)?;
                st.storage(ObjectStorage::reset)?;
                Ok(Value::Null)
            }
            "containsiterator" => {
                let a = Args::new(func, args, &["iterator"], 1, Some(1))?;
                let it = a.object_of(host, 0, "Iterator")?;
                contains(host, this, &st, &it).map(Value::Bool)
            }
            "countiterators" => {
                Args::none(func, args)?;
                st.storage(|s| Value::Int(to_long(s.storage.len())))
            }
            "rewind" | "next" => {
                Args::none(func, args)?;
                let entries = st.storage(|s| {
                    s.pos = s.storage.used();
                    s.entries()
                })?;
                for (it, _) in entries {
                    host.call_method(&it, method, &[])?;
                }
                Ok(Value::Null)
            }
            "valid" => {
                Args::none(func, args)?;
                let (entries, flags) = st.storage(|s| {
                    s.pos = s.storage.used();
                    (s.entries(), s.flags)
                })?;
                if entries.is_empty() {
                    return Ok(Value::Bool(false));
                }
                let expect = flags & MIT_NEED_ALL != 0;
                for (it, _) in entries {
                    let r = host.call_method(&it, "valid", &[])?;
                    let valid = matches!(r.deref(), Value::Bool(true));
                    if expect != valid {
                        return Ok(Value::Bool(!expect));
                    }
                }
                Ok(Value::Bool(expect))
            }
            "key" | "current" => {
                Args::none(func, args)?;
                get_all(host, &st, method == "current")
            }
            "__debuginfo" => {
                Args::none(func, args)?;
                debug_info(host, this, &st).map(Value::Array)
            }
            _ => unreachable!("filtered above"),
        }
    })())
}

fn get_all(host: &mut dyn SplHost, st: &SplObject, current: bool) -> RtResult<Value> {
    let name = if current { "current" } else { "key" };
    let (entries, flags) = st.storage(|s| {
        s.pos = s.storage.used();
        (s.entries(), s.flags)
    })?;
    if entries.is_empty() {
        return Err(runtime(format!("Called {name}() on an invalid iterator")));
    }
    let mut out = HashTable::with_capacity(entries.len());
    for (it, inf) in entries {
        let r = host.call_method(&it, "valid", &[])?;
        let valid = matches!(r.deref(), Value::Bool(true));
        let retval = if valid {
            host.call_method(&it, name, &[])?
        } else if flags & MIT_NEED_ALL != 0 {
            return Err(runtime(format!(
                "Called {name}() with non valid sub iterator"
            )));
        } else {
            Value::Null
        };
        if flags & MIT_KEYS_ASSOC != 0 {
            match inf.deref() {
                Value::Int(n) => {
                    out.insert(ArrayKey::Int(n), retval);
                }
                Value::Str(s) => {
                    out.insert(ArrayKey::from_zstr(&s), retval);
                }
                _ => {
                    return Err(invalid_argument("Sub-Iterator is associated with NULL"));
                }
            }
        } else {
            let _ = out.push(retval);
        }
    }
    Ok(Value::Array(ZArray::from_table(out)))
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
    let o = match offset.map(Value::deref) {
        Some(Value::Object(o))
            if !host.has_user_method(obj, "getHash")
                && !host.has_user_method(obj, "offsetGet")
                && !host.has_user_method(obj, "offsetExists") =>
        {
            o
        }
        _ => return std_read_dimension(host, obj, offset, kind),
    };
    let e = st.storage(|s| s.storage.get_int(i64::from(o.handle())).and_then(entry))?;
    match e {
        Some((_, inf)) => Ok(inf.deref()),
        None if kind == DimKind::Is => Ok(Value::Null),
        None => Err(unexpected_value("Object not found")),
    }
}

/// `write_dimension` handler (`MultipleIterator` additionally requires
/// `Iterator` objects).
pub fn write_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    offset: Option<&Value>,
    value: Value,
) -> RtResult<()> {
    mutating_within_get_hash()?;
    let o = match offset.map(Value::deref) {
        Some(Value::Object(o)) if !overridden(host, obj, "offsetSet") => o,
        _ => {
            let off = offset.cloned().unwrap_or_default();
            return host
                .call_method(obj, "offsetSet", &[off, value])
                .map(|_| ());
        }
    };
    if st.base() == "MultipleIterator" && !host.instance_of(&o, "Iterator") {
        return Err(type_error(
            "Can only attach objects that implement the Iterator interface",
        ));
    }
    let key = ArrayKey::Int(i64::from(o.handle()));
    let garbage = st.storage(|s| s.storage.insert(key, make_entry(&o, value)))?;
    drop(garbage);
    Ok(())
}

/// `has_dimension` handler.
pub fn has_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    offset: &Value,
    check_empty: bool,
) -> RtResult<bool> {
    let o = match offset.deref() {
        Value::Object(o)
            if !host.has_user_method(obj, "getHash")
                && !host.has_user_method(obj, "offsetGet")
                && !host.has_user_method(obj, "offsetExists") =>
        {
            o
        }
        _ => return std_has_dimension(host, obj, offset, check_empty),
    };
    let e = st.storage(|s| s.storage.get_int(i64::from(o.handle())).and_then(entry))?;
    match e {
        None => Ok(false),
        Some((_, inf)) if check_empty => inf.to_bool(host),
        Some(_) => Ok(true),
    }
}

/// `unset_dimension` handler.
pub fn unset_dimension(
    host: &mut dyn SplHost,
    obj: &Obj,
    st: &Rc<SplObject>,
    offset: &Value,
) -> RtResult<()> {
    mutating_within_get_hash()?;
    match offset.deref() {
        Value::Object(o) if !overridden(host, obj, "offsetUnset") => {
            let removed =
                st.storage(|s| s.storage.remove(&ArrayKey::Int(i64::from(o.handle()))))?;
            drop(removed);
            Ok(())
        }
        _ => host
            .call_method(obj, "offsetUnset", std::slice::from_ref(offset))
            .map(|_| ()),
    }
}

/// `compare` handler (`spl_object_storage_compare_objects`): only two
/// plain `SplObjectStorage` instances are comparable; their entries are
/// compared by key and info.
pub fn compare(host: &mut dyn SplHost, a: &Obj, b: &Obj) -> RtResult<i32> {
    if !a.class_name().eq_ignore_ascii_case(b"SplObjectStorage")
        || !b.class_name().eq_ignore_ascii_case(b"SplObjectStorage")
    {
        return Ok(1);
    }
    let sa = state_of(host, a)?;
    let sb = state_of(host, b)?;
    let ta = sa.storage(|s| s.storage.clone())?;
    let tb = sb.storage(|s| s.storage.clone())?;
    if ta.len() != tb.len() {
        return Ok(if ta.len() > tb.len() { 1 } else { -1 });
    }
    for (k, v1) in &ta {
        let Some(v2) = tb.get(k) else {
            return Ok(1);
        };
        let i1 = entry(v1).map(|e| e.1).unwrap_or_default();
        let i2 = entry(v2).map(|e| e.1).unwrap_or_default();
        let r = ops::compare(&i1, &i2, host)?;
        if r != 0 {
            return Ok(r);
        }
    }
    Ok(0)
}
