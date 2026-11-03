//! `ArrayObject`, `ArrayIterator` and `RecursiveArrayIterator` (`spl_array.c`).

/// The internal state of an array object.
#[derive(Debug, Clone, Default)]
pub struct ArrayObject {}

impl ArrayObject {
    /// A new state for the internal class `base`.
    #[must_use]
    pub fn new(base: &str) -> Self {
        let _ = base;
        ArrayObject {}
    }
}

use crate::host::{ObjectIterator, SplHost};
use crate::object::SplObject;
use crate::util::DimKind;
use php_runtime::{Obj, PropertyPurpose, RtResult, Value, ZArray};
use std::rc::Rc;

pub(crate) fn call(
    _host: &mut dyn SplHost,
    _class: &str,
    _method: &str,
    _this: Option<&Obj>,
    _args: &[Value],
) -> Option<RtResult<Value>> {
    None
}

/// `read_dimension` handler.
pub fn read_dimension(
    _host: &mut dyn SplHost,
    _obj: &Obj,
    _st: &Rc<SplObject>,
    _offset: Option<&Value>,
    _kind: DimKind,
) -> RtResult<Value> {
    Ok(Value::Null)
}

/// `write_dimension` handler.
pub fn write_dimension(
    _host: &mut dyn SplHost,
    _obj: &Obj,
    _st: &Rc<SplObject>,
    _offset: Option<&Value>,
    _value: Value,
) -> RtResult<()> {
    Ok(())
}

/// `has_dimension` handler.
pub fn has_dimension(
    _host: &mut dyn SplHost,
    _obj: &Obj,
    _st: &Rc<SplObject>,
    _offset: &Value,
    _check_empty: bool,
) -> RtResult<bool> {
    Ok(false)
}

/// `unset_dimension` handler.
pub fn unset_dimension(
    _host: &mut dyn SplHost,
    _obj: &Obj,
    _st: &Rc<SplObject>,
    _offset: &Value,
) -> RtResult<()> {
    Ok(())
}

/// `count_elements` handler.
pub fn count_elements(_host: &mut dyn SplHost, _obj: &Obj, _st: &Rc<SplObject>) -> RtResult<i64> {
    Ok(0)
}

/// `get_properties_for` handler.
pub fn properties_for(
    _host: &mut dyn SplHost,
    _obj: &Obj,
    _st: &Rc<SplObject>,
    _purpose: PropertyPurpose,
) -> RtResult<Option<ZArray>> {
    Ok(None)
}

/// `get_debug_info` handler.
pub fn debug_info(_host: &mut dyn SplHost, _obj: &Obj, _st: &Rc<SplObject>) -> RtResult<ZArray> {
    Ok(ZArray::new())
}

/// `compare` handler.
pub fn compare(_host: &mut dyn SplHost, _a: &Obj, _b: &Obj) -> RtResult<i32> {
    Ok(1)
}

/// Clone support.
pub fn clone_into(
    _host: &mut dyn SplHost,
    _old: &Obj,
    _new_obj: &Obj,
    _fresh: &SplObject,
    _st: &SplObject,
) -> RtResult<()> {
    Ok(())
}

/// Native iterator.
pub fn native_iterator(
    _host: &mut dyn SplHost,
    _obj: &Obj,
    _st: &Rc<SplObject>,
    _by_ref: bool,
) -> RtResult<Option<Box<dyn ObjectIterator>>> {
    Ok(None)
}
