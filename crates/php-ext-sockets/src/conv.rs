//! Conversions between VM values and the `php_sockets::value` model.

use php_runtime::{ArrayKey, PropertyPurpose, RuntimeContext, Value, ZArray, ZStr};
use php_sockets::{Key, OutValue, Value as SValue};
use php_streams::stream::CastAs;
use php_vm::ext::VmResult;
use php_vm::{object, Vm};

use crate::objects::{self, SockObj};

fn key_in(k: &ArrayKey) -> Key {
    match k {
        ArrayKey::Int(i) => Key::Int(*i),
        ArrayKey::Str(s) => Key::Str(s.as_bytes().to_vec()),
    }
}

fn key_out(k: Key) -> ArrayKey {
    match k {
        Key::Int(i) => ArrayKey::Int(i),
        Key::Str(s) => ArrayKey::Str(ZStr::from(s.as_slice())),
    }
}

fn table_in(vm: &mut Vm, t: &ZArray) -> VmResult<Vec<(Key, SValue)>> {
    let mut out = Vec::with_capacity(t.len());
    for (k, v) in t.iter() {
        out.push((key_in(k), to_svalue(vm, &v.deref())?));
    }
    Ok(out)
}

/// A VM value as the library sees it (references dereferenced, `Socket`
/// objects by descriptor, streams cast to descriptors, other objects with
/// their `(array)` properties and `__toString()` result).
pub(crate) fn to_svalue(vm: &mut Vm, v: &Value) -> VmResult<SValue> {
    Ok(match v {
        Value::Null => SValue::Null,
        Value::Bool(b) => SValue::Bool(*b),
        Value::Int(i) => SValue::Int(*i),
        Value::Float(f) => SValue::Float(*f),
        Value::Str(s) => SValue::Str(s.as_bytes().to_vec()),
        Value::Array(a) => SValue::Array(table_in(vm, a)?),
        Value::Reference(r) => to_svalue(vm, &r.get())?,
        Value::Resource(r) => match php_ext_streams::res::cell_of(r) {
            Some(cell) => {
                let fd = cell.get().and_then(|mut s| {
                    s.cast(CastAs::FdForSelect, false)
                        .or_else(|| s.cast(CastAs::Fd, false))
                });
                SValue::Stream { fd }
            }
            None => SValue::Resource,
        },
        Value::Object(o) => {
            if let Some(fd) = object::internal::<SockObj, _>(o, |s| s.sock.fd()) {
                return Ok(SValue::Socket { fd });
            }
            let cls = object::class_of(o).clone();
            let props = match vm.object_properties(o, PropertyPurpose::ArrayCast) {
                Ok(Some(t)) => table_in(vm, &t)?,
                _ => Vec::new(),
            };
            let string = if cls.magic.to_string.is_some() {
                Some(vm.value_to_string(v)?.as_bytes().to_vec())
            } else {
                None
            };
            SValue::Object {
                class: cls.name.to_string_lossy(),
                props,
                string,
            }
        }
    })
}

/// A library result as a VM value (new `Socket` objects and stream
/// resources are created here).
pub(crate) fn from_out(vm: &mut Vm, v: OutValue) -> VmResult<Value> {
    Ok(match v {
        OutValue::Null => Value::Null,
        OutValue::False => Value::Bool(false),
        OutValue::Int(i) => Value::Int(i),
        OutValue::Str(s) => Value::Str(ZStr::from(s.as_slice())),
        OutValue::Array(items) => {
            let mut arr = ZArray::with_capacity(items.len());
            for (k, v) in items {
                let v = from_out(vm, v)?;
                arr.make_mut().insert(key_out(k), v);
            }
            Value::Array(arr)
        }
        OutValue::Socket(s) => objects::new_socket(vm, s)?,
        OutValue::StreamFd(fd) => {
            let file = php_streams::sys::file_from_fd(fd);
            let stream = php_streams::plain::stream_from_file(file, "rw", false);
            php_ext_streams::res::new_stream(stream, None)
        }
    })
}
