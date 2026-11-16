// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Argument helpers shared by the procedural functions and their method
//! aliases (`zend_parse_method_parameters` + `ERROR_ARG_POS`).

use php_runtime::{ArrayKey, Obj, Value, ZArray, ZStr};
use php_vm::ext::Args;
use php_vm::{Vm, VmResult};

/// The receiver (`$this` for methods, the first argument for functions)
/// and the index of the first remaining argument.
pub fn recv(vm: &mut Vm, a: &Args) -> VmResult<(Obj, usize)> {
    if let Some(t) = &a.this {
        return Ok((t.clone(), 0));
    }
    match a.at(0) {
        Value::Object(o) => Ok((o, 1)),
        _ => Err(vm.throw("Error", "mysqli: missing object argument")),
    }
}

/// `ERROR_ARG_POS(n)`: the 1-based argument number as seen by the caller.
#[must_use]
pub fn pos(a: &Args, n: usize) -> usize {
    if a.this.is_some() {
        n - 1
    } else {
        n
    }
}

/// Whether the call is a method call (`hasThis()`).
#[must_use]
pub fn is_method(a: &Args) -> bool {
    a.this.is_some()
}

/// Optional string argument (`s!`): `None` when missing or null.
#[must_use]
pub fn opt_str(a: &Args, n: usize) -> Option<ZStr> {
    match a.at(n) {
        Value::Str(s) => Some(s),
        _ => None,
    }
}

/// Optional int argument (`l!`): `None` when missing or null.
#[must_use]
pub fn opt_int(a: &Args, n: usize) -> Option<i64> {
    match a.at(n) {
        Value::Int(i) => Some(i),
        _ => None,
    }
}

/// `MYSQLI_RETURN_LONG_INT`: values beyond `ZEND_LONG_MAX` become strings.
#[must_use]
pub fn long_or_string(v: u64) -> Value {
    match i64::try_from(v) {
        Ok(i) if i < i64::MAX => Value::Int(i),
        _ => Value::Str(ZStr::from(v.to_string())),
    }
}

/// `affected_rows` style values: `u64::MAX` is -1.
#[must_use]
pub fn affected(v: u64) -> Value {
    if v == u64::MAX {
        Value::Int(-1)
    } else {
        long_or_string(v)
    }
}

/// A string value from bytes.
#[must_use]
pub fn bstr(b: &[u8]) -> Value {
    Value::Str(ZStr::from(b))
}

/// Builds `[['errno' => .., 'sqlstate' => .., 'error' => ..], ...]`.
#[must_use]
pub fn error_list(list: &[php_mysqlnd::ErrorEntry]) -> Value {
    let rows = list.iter().map(|e| {
        Value::Array(ZArray::from_pairs([
            (ArrayKey::from("errno"), Value::Int(i64::from(e.errno))),
            (
                ArrayKey::from("sqlstate"),
                Value::Str(ZStr::from(e.sqlstate.as_str())),
            ),
            (ArrayKey::from("error"), bstr(&e.error)),
        ]))
    });
    Value::Array(ZArray::from_values(rows))
}

/// Creates a `stdClass` object with the given properties.
pub fn std_object(vm: &mut Vm, props: Vec<(&str, Value)>) -> VmResult<Value> {
    let o = crate::objects::new_object(vm, "stdClass")?;
    for (k, v) in props {
        vm.set_property_raw(&o, k, v);
    }
    Ok(Value::Object(o))
}

/// `zend_zval_value_name` for messages.
#[must_use]
pub fn type_name(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Int(_) => "int".into(),
        Value::Float(_) => "float".into(),
        Value::Str(_) => "string".into(),
        Value::Array(_) => "array".into(),
        Value::Object(o) => crate::objects::class_name(o),
        Value::Resource(_) => "resource".into(),
        Value::Reference(r) => type_name(&r.get()),
    }
}

/// Reads a declared property slot without visibility checks.
#[must_use]
pub fn raw_prop(o: &Obj, name: &str) -> Option<Value> {
    let cls = php_vm::object::class_of(o).clone();
    let n = ZStr::from(name);
    let pi = cls.props.iter().rev().find(|p| p.name == n)?;
    php_vm::object::data(o)
        .slots
        .borrow()
        .get(pi.slot as usize)
        .cloned()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_or_string_boundaries() {
        assert!(matches!(long_or_string(5), Value::Int(5)));
        assert!(matches!(long_or_string(u64::MAX - 1), Value::Str(_)));
        assert!(matches!(long_or_string(i64::MAX as u64), Value::Str(_)));
        assert!(matches!(affected(u64::MAX), Value::Int(-1)));
        assert!(matches!(affected(3), Value::Int(3)));
    }

    #[test]
    fn error_list_shape() {
        let v = error_list(&[php_mysqlnd::ErrorEntry {
            errno: 1064,
            sqlstate: "42000".into(),
            error: b"syntax".to_vec(),
        }]);
        let Value::Array(a) = v else { panic!("array") };
        assert_eq!(a.len(), 1);
        let Some(Value::Array(row)) = a.get(&ArrayKey::Int(0)) else {
            panic!("row")
        };
        assert!(matches!(
            row.get(&ArrayKey::from("errno")),
            Some(Value::Int(1064))
        ));
    }

    #[test]
    fn type_names() {
        assert_eq!(type_name(&Value::Null), "null");
        assert_eq!(type_name(&Value::Int(1)), "int");
        assert_eq!(type_name(&Value::Bool(false)), "false");
    }
}
