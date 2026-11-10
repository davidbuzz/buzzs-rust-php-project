//! The ZPP probes (`zend_bool`, `zend_int_or_null_slow_zpp`, ...): each
//! returns its parsed argument. Where the stub type and the `Z_PARAM_*`
//! macro agree, the VM's own argument parsing does the work; the class
//! name, resource and object-of-class-or-scalar specifiers have their
//! own rules and messages, implemented here.

use crate::util::{caller_strict, value_name};
use php_runtime::{Value, ZStr};
use php_vm::class::Class;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;
use std::rc::Rc;

/// The parsed argument, unchanged.
fn identity(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(a.at(0))
}

/// `Z_PARAM_CLASS[_OR_NULL]` on argument `n` (0-based): `Ok(None)` for an
/// allowed `null`.
pub(crate) fn class_arg(
    vm: &mut Vm,
    a: &Args,
    n: usize,
    nullable: bool,
) -> VmResult<Option<Rc<Class>>> {
    let v = a.at(n);
    let what = if nullable {
        "a valid class name or null"
    } else {
        "a valid class name"
    };
    let name = match &v {
        Value::Null if nullable => return Ok(None),
        Value::Str(s) => s.clone(),
        Value::Object(o) if !caller_strict(vm) && has_to_string(o) => vm.value_to_string(&v)?,
        other => {
            return Err(a.type_error(
                vm,
                n + 1,
                &format!("must be {what}, {} given", value_name(other)),
            ))
        }
    };
    match vm.lookup_class(&name)? {
        Some(c) => Ok(Some(c)),
        None => Err(a.type_error(
            vm,
            n + 1,
            &format!("must be {what}, \"{}\" given", name.to_string_lossy()),
        )),
    }
}

fn has_to_string(o: &php_runtime::Obj) -> bool {
    php_vm::object::class_of(o).magic.to_string.is_some()
}

fn class_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(class_arg(vm, a, 0, false)?.map_or(Value::Null, |c| Value::Str(c.name.clone())))
}

fn class_name_or_null(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(class_arg(vm, a, 0, true)?.map_or(Value::Null, |c| Value::Str(c.name.clone())))
}

/// `Z_PARAM_OBJ_OR_CLASS_NAME[_OR_NULL]`.
fn obj_or_class_name_impl(vm: &mut Vm, a: &Args, nullable: bool) -> VmResult<Value> {
    let v = a.at(0);
    let what = if nullable {
        "an object, a valid class name, or null"
    } else {
        "an object or a valid class name"
    };
    match &v {
        Value::Null if nullable => Ok(Value::Null),
        Value::Object(o) => Ok(Value::Str(php_vm::object::class_of(o).name.clone())),
        Value::Str(s) => match vm.lookup_class(s)? {
            Some(c) => Ok(Value::Str(c.name.clone())),
            None => Err(a.type_error(
                vm,
                1,
                &format!("must be {what}, \"{}\" given", s.to_string_lossy()),
            )),
        },
        other => Err(a.type_error(
            vm,
            1,
            &format!("must be {what}, {} given", value_name(other)),
        )),
    }
}

fn obj_or_class_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    obj_or_class_name_impl(vm, a, false)
}

fn obj_or_class_name_or_null(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    obj_or_class_name_impl(vm, a, true)
}

/// `Z_PARAM_RESOURCE[_OR_NULL]`.
fn resource_impl(vm: &mut Vm, a: &Args, nullable: bool) -> VmResult<Value> {
    match a.at(0) {
        v @ Value::Resource(_) => Ok(v),
        Value::Null if nullable => Ok(Value::Null),
        other => {
            let what = if nullable {
                "resource or null"
            } else {
                "resource"
            };
            Err(a.type_error(
                vm,
                1,
                &format!("must be of type {what}, {} given", value_name(&other)),
            ))
        }
    }
}

fn resource(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    resource_impl(vm, a, false)
}

fn resource_or_null(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    resource_impl(vm, a, true)
}

fn is_std_class(o: &php_runtime::Obj) -> bool {
    php_vm::object::class_of(o).instance_of_name(b"stdclass")
}

/// The "Passing null to parameter" deprecation of a weak-mode internal
/// call with a non-nullable scalar parameter (`zend_null_arg_deprecated`):
/// the type is the declared (arginfo) type.
fn null_deprecated(vm: &mut Vm, a: &Args, declared: &str) -> VmResult<()> {
    let fname = a
        .func
        .as_ref()
        .map_or_else(String::new, |f| f.name.to_string_lossy());
    vm.raise(
        8192,
        &format!(
            "{fname}(): Passing null to parameter #1 ($param) of type {declared} is deprecated"
        ),
    )
}

/// `Z_PARAM_OBJ_OF_CLASS_OR_STR[_OR_NULL]` with `stdClass`.
fn obj_stdclass_or_string_impl(vm: &mut Vm, a: &Args, nullable: bool) -> VmResult<Value> {
    let v = a.at(0);
    let strict = caller_strict(vm);
    let expected = if nullable {
        "stdClass|string|null"
    } else {
        "stdClass|string"
    };
    let err = |vm: &mut Vm, v: &Value| {
        a.type_error(
            vm,
            1,
            &format!("must be of type {expected}, {} given", value_name(v)),
        )
    };
    match &v {
        Value::Object(o) if is_std_class(o) => Ok(v.clone()),
        Value::Str(_) => Ok(v.clone()),
        Value::Null if nullable => Ok(Value::Null),
        Value::Null if !strict => {
            null_deprecated(vm, a, "stdClass|string|null")?;
            Ok(Value::Str(ZStr::from("")))
        }
        Value::Bool(_) | Value::Int(_) | Value::Float(_) if !strict => {
            Ok(Value::Str(vm.value_to_string(&v)?))
        }
        Value::Object(o) if !strict && has_to_string(o) => Ok(Value::Str(vm.value_to_string(&v)?)),
        other => Err(err(vm, other)),
    }
}

fn obj_stdclass_or_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    obj_stdclass_or_string_impl(vm, a, false)
}

fn obj_stdclass_or_string_or_null(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    obj_stdclass_or_string_impl(vm, a, true)
}

/// `Z_PARAM_OBJ_OF_CLASS_OR_LONG[_OR_NULL]` with `stdClass`.
fn obj_stdclass_or_int_impl(vm: &mut Vm, a: &Args, nullable: bool) -> VmResult<Value> {
    let v = a.at(0);
    let strict = caller_strict(vm);
    let expected = if nullable {
        "stdClass|int|null"
    } else {
        "stdClass|int"
    };
    let err = |vm: &mut Vm, v: &Value| {
        a.type_error(
            vm,
            1,
            &format!("must be of type {expected}, {} given", value_name(v)),
        )
    };
    match &v {
        Value::Object(o) if is_std_class(o) => Ok(v.clone()),
        Value::Int(_) => Ok(v.clone()),
        Value::Null if nullable => Ok(Value::Null),
        Value::Null if !strict => {
            null_deprecated(vm, a, "stdClass|int|null")?;
            Ok(Value::Int(0))
        }
        Value::Bool(_) | Value::Float(_) | Value::Str(_) if !strict => {
            match vm.parse_long_weak(&v)? {
                Some(i) => Ok(Value::Int(i)),
                None => Err(err(vm, &v)),
            }
        }
        other => Err(err(vm, other)),
    }
}

fn obj_stdclass_or_int(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    obj_stdclass_or_int_impl(vm, a, false)
}

fn obj_stdclass_or_int_or_null(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    obj_stdclass_or_int_impl(vm, a, true)
}

/// `(stub signature, handler)` of every probe.
const PROBES: &[(&str, php_vm::ext::NativeFn)] = &[
    ("zend_bool(bool $param): bool", identity),
    ("zend_bool_or_null(?bool $param): ?bool", identity),
    ("zend_bool_slow_zpp(bool $param): bool", identity),
    ("zend_bool_or_null_slow_zpp(?bool $param): ?bool", identity),
    ("zend_int(int $param): int", identity),
    ("zend_int_or_null(?int $param): ?int", identity),
    ("zend_int_slow_zpp(int $param): int", identity),
    ("zend_int_or_null_slow_zpp(?int $param): ?int", identity),
    ("zend_float(float $param): float", identity),
    ("zend_float_or_null(?float $param): ?float", identity),
    ("zend_float_slow_zpp(float $param): float", identity),
    (
        "zend_float_or_null_slow_zpp(?float $param): ?float",
        identity,
    ),
    ("zend_number(int|float $param): int|float", identity),
    (
        "zend_number_or_null(int|float|null $param): int|float|null",
        identity,
    ),
    (
        "zend_number_slow_zpp(int|float $param): int|float",
        identity,
    ),
    (
        "zend_number_or_null_slow_zpp(int|float|null $param): int|float|null",
        identity,
    ),
    ("zend_object(object $param): object", identity),
    ("zend_object_or_null(?object $param): ?object", identity),
    ("zend_object_slow_zpp(object $param): object", identity),
    (
        "zend_object_or_null_slow_zpp(?object $param): ?object",
        identity,
    ),
    ("zend_obj(object $param): object", identity),
    ("zend_obj_or_null(?object $param): ?object", identity),
    ("zend_obj_or_class_name($param): string", obj_or_class_name),
    (
        "zend_obj_or_class_name_or_null($param): ?string",
        obj_or_class_name_or_null,
    ),
    ("zend_class_name($param): string", class_name),
    (
        "zend_class_name_or_null($param): ?string",
        class_name_or_null,
    ),
    ("zend_class_name_slow_zpp($param): string", class_name),
    (
        "zend_class_name_or_null_slow_zpp($param): ?string",
        class_name_or_null,
    ),
    ("zend_object_sdtClass(stdClass $param): stdClass", identity),
    (
        "zend_object_sdtClass_or_null(?stdClass $param): ?stdClass",
        identity,
    ),
    (
        "zend_object_sdtClass_slow_zpp(stdClass $param): stdClass",
        identity,
    ),
    (
        "zend_object_sdtClass_or_null_slow_zpp(?stdClass $param): ?stdClass",
        identity,
    ),
    ("zend_obj_sdtClass(stdClass $param): stdClass", identity),
    (
        "zend_obj_sdtClass_or_null(?stdClass $param): ?stdClass",
        identity,
    ),
    ("zend_resource($param)", resource),
    ("zend_resource_or_null($param)", resource_or_null),
    ("zend_resource_slow_zpp($param)", resource),
    ("zend_resource_or_null_slow_zpp($param)", resource_or_null),
    (
        "zend_string_or_object(object|string $param): object|string",
        identity,
    ),
    (
        "zend_string_or_object_or_null(object|string|null $param): object|string|null",
        identity,
    ),
    (
        "zend_obj_stdclass_or_string($param): stdClass|string",
        obj_stdclass_or_string,
    ),
    (
        "zend_obj_stdclass_or_string_or_null($param): stdClass|string|null",
        obj_stdclass_or_string_or_null,
    ),
    (
        "zend_obj_stdclass_or_int($param): stdClass|int",
        obj_stdclass_or_int,
    ),
    (
        "zend_obj_stdclass_or_int_or_null($param): stdClass|int|null",
        obj_stdclass_or_int_or_null,
    ),
    (
        "zend_number_or_string(string|int|float $param): string|int|float",
        identity,
    ),
    (
        "zend_number_or_string_or_null(string|int|float|null $param): string|int|float|null",
        identity,
    ),
];

pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    for &(sig, f) in PROBES {
        m.function_static(sig, f);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn probe_names_are_unique() {
        let mut names: Vec<&str> = super::PROBES
            .iter()
            .map(|(s, _)| s.split('(').next().unwrap_or(""))
            .collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n);
    }
}
