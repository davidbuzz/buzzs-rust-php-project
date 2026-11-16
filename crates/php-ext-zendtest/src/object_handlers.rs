// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `object_handlers.c`: classes with custom `do_operation`, `cast_object`,
//! `compare` and dimension handlers.

use php_runtime::{BinaryOp, CastTarget, Obj, Value};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::Vm;

/// Declared property slot `n` of `o` (`OBJ_PROP_NUM`).
fn slot(o: &Obj, n: usize) -> Value {
    php_vm::object::data(o)
        .slots
        .borrow()
        .get(n)
        .cloned()
        .flatten()
        .unwrap_or_default()
}

fn set_slot(o: &Obj, n: usize, v: Value) {
    if let Some(s) = php_vm::object::data(o).slots.borrow_mut().get_mut(n) {
        *s = Some(v);
    }
}

/// The `create_object` handler storing `0` / `0.0` in slot 0.
fn init_val(float: bool) -> fn(&mut Vm, &Obj) -> VmResult<()> {
    if float {
        |_vm, o| {
            set_slot(o, 0, Value::Float(0.0));
            Ok(())
        }
    } else {
        |_vm, o| {
            set_slot(o, 0, Value::Int(0));
            Ok(())
        }
    }
}

/// `__construct(int|float $val)`: stores the argument in slot 0.
fn ctor(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    set_slot(&a.this_obj(), 0, a.at(0));
    Ok(Value::Null)
}

fn is_class(v: &Value, name: &[u8]) -> Option<Obj> {
    match v {
        Value::Object(o) if php_vm::object::class_of(o).instance_of_name(name) => Some(o.clone()),
        _ => None,
    }
}

/// `donc_do_operation`: `+` and `*` produce a new `DoOperationNoCast`.
#[allow(clippy::many_single_char_names)]
fn donc_do_operation(vm: &mut Vm, op: BinaryOp, a: &Value, b: &Value) -> VmResult<Option<Value>> {
    if !matches!(op, BinaryOp::Add | BinaryOp::Mul) {
        return Ok(None);
    }
    let mut val = |v: &Value| -> VmResult<i64> {
        match is_class(v, b"dooperationnocast") {
            Some(o) => Ok(match slot(&o, 0) {
                Value::Int(i) => i,
                _ => 0,
            }),
            None => vm.value_to_int(v),
        }
    };
    let (x, y) = (val(a)?, val(b)?);
    let r = if matches!(op, BinaryOp::Add) {
        x.wrapping_add(y)
    } else {
        x.wrapping_mul(y)
    };
    let Some(cls) = vm.find_class(b"dooperationnocast") else {
        return Ok(None);
    };
    let o = vm.new_object(&cls)?;
    set_slot(&o, 0, Value::Int(r));
    Ok(Some(Value::Object(o)))
}

fn long_cast(_vm: &mut Vm, o: &Obj, t: CastTarget) -> VmResult<Option<Value>> {
    Ok(matches!(t, CastTarget::Int).then(|| slot(o, 0)))
}

fn float_cast(_vm: &mut Vm, o: &Obj, t: CastTarget) -> VmResult<Option<Value>> {
    Ok(matches!(t, CastTarget::Float).then(|| slot(o, 0)))
}

fn numeric_cast(_vm: &mut Vm, o: &Obj, t: CastTarget) -> VmResult<Option<Value>> {
    Ok(matches!(t, CastTarget::Number).then(|| slot(o, 0)))
}

/// `vm_interrupt_comparable_compare`.
#[allow(clippy::many_single_char_names)]
fn interrupt_compare(_vm: &mut Vm, a: &Value, b: &Value) -> VmResult<Option<i32>> {
    let (Some(x), Some(y)) = (
        is_class(a, b"vminterruptcomparable"),
        is_class(b, b"vminterruptcomparable"),
    ) else {
        return Ok(None);
    };
    php_vm::interrupt::request_interrupt();
    let n = |o: &Obj| match slot(o, 0) {
        Value::Int(i) => i,
        _ => 0,
    };
    Ok(Some(match n(&x).cmp(&n(&y)) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }))
}

/// Slots of `DimensionHandlersNoArrayAccess`.
const READ: usize = 0;
const WRITE: usize = 1;
const HAS: usize = 2;
const UNSET: usize = 3;
const READ_TYPE: usize = 4;
const HAS_OFFSET: usize = 5;
const CHECK_EMPTY: usize = 6;
const OFFSET: usize = 7;

fn dim_common(o: &Obj, offset: Option<&Value>, which: usize) {
    set_slot(o, which, Value::Bool(true));
    set_slot(o, HAS_OFFSET, Value::Bool(offset.is_some()));
    if let Some(k) = offset {
        set_slot(o, OFFSET, k.clone());
    }
}

fn dim_read(vm: &mut Vm, o: &Obj, k: &Value) -> VmResult<Option<Value>> {
    use php_vm::ext::DimensionCtx;
    let ctx = vm.dimension_context();
    let appended = matches!(ctx, DimensionCtx::Append | DimensionCtx::AppendRw);
    let offset = (!matches!(k, Value::Null) || !appended).then_some(k);
    dim_common(o, offset, READ);
    // BP_VAR_R / W / RW / IS / FUNC_ARG / UNSET.
    let ty = match ctx {
        DimensionCtx::Read => 0,
        DimensionCtx::Write | DimensionCtx::Append => 1,
        DimensionCtx::ReadWrite | DimensionCtx::AppendRw => 2,
        DimensionCtx::Is => 3,
        DimensionCtx::Unset => 5,
    };
    set_slot(o, READ_TYPE, Value::Int(ty));
    if matches!(ty, 1 | 2 | 5) {
        // `zend_fetch_dimension_address`: a write-context fetch got a
        // temporary (the VM leaves this notice to the handler).
        vm.raise(
            8,
            &format!(
                "Indirect modification of overloaded element of {} has no effect",
                o.class_name().to_string_lossy()
            ),
        )?;
    }
    Ok(Some(Value::Bool(true)))
}

fn dim_write(_vm: &mut Vm, o: &Obj, k: Option<&Value>, _v: &Value) -> VmResult<bool> {
    dim_common(o, k, WRITE);
    Ok(true)
}

fn dim_has(_vm: &mut Vm, o: &Obj, k: &Value, check_empty: bool) -> VmResult<Option<bool>> {
    set_slot(o, CHECK_EMPTY, Value::Int(i64::from(check_empty)));
    dim_common(o, Some(k), HAS);
    Ok(Some(true))
}

fn dim_unset(_vm: &mut Vm, o: &Obj, k: &Value) -> VmResult<bool> {
    dim_common(o, Some(k), UNSET);
    Ok(true)
}

pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    let fin = class_flags::FINAL;
    ClassBuilder::new("DoOperationNoCast")
        .flags(fin)
        .prop("val", acc::PRIVATE, Some("int"), None)
        .method("__construct(int $val)", acc::PUBLIC, ctor)
        .hooks(ClassHooks {
            create: Some(init_val(false)),
            do_operation: Some(donc_do_operation),
            ..Default::default()
        })
        .register(vm);
    ClassBuilder::new("LongCastableNoOperations")
        .flags(fin)
        .prop("val", acc::PRIVATE, Some("int"), None)
        .method("__construct(int $val)", acc::PUBLIC, ctor)
        .hooks(ClassHooks {
            create: Some(init_val(false)),
            cast: Some(long_cast),
            ..Default::default()
        })
        .register(vm);
    ClassBuilder::new("FloatCastableNoOperations")
        .flags(fin)
        .prop("val", acc::PRIVATE, Some("float"), None)
        .method("__construct(float $val)", acc::PUBLIC, ctor)
        .hooks(ClassHooks {
            create: Some(init_val(true)),
            cast: Some(float_cast),
            ..Default::default()
        })
        .register(vm);
    ClassBuilder::new("NumericCastableNoOperations")
        .flags(fin)
        .prop("val", acc::PRIVATE, Some("int|float"), None)
        .method("__construct(int|float $val)", acc::PUBLIC, ctor)
        .hooks(ClassHooks {
            create: Some(init_val(false)),
            cast: Some(numeric_cast),
            ..Default::default()
        })
        .register(vm);
    ClassBuilder::new("VmInterruptComparable")
        .flags(fin)
        .prop("val", acc::PRIVATE, Some("int"), None)
        .method("__construct(int $val)", acc::PUBLIC, ctor)
        .hooks(ClassHooks {
            create: Some(init_val(false)),
            compare: Some(interrupt_compare),
            ..Default::default()
        })
        .register(vm);
    ClassBuilder::new("DimensionHandlersNoArrayAccess")
        .prop("read", acc::PUBLIC, Some("bool"), Some(Value::Bool(false)))
        .prop("write", acc::PUBLIC, Some("bool"), Some(Value::Bool(false)))
        .prop("has", acc::PUBLIC, Some("bool"), Some(Value::Bool(false)))
        .prop("unset", acc::PUBLIC, Some("bool"), Some(Value::Bool(false)))
        .prop("readType", acc::PUBLIC, Some("int"), None)
        .prop(
            "hasOffset",
            acc::PUBLIC,
            Some("bool"),
            Some(Value::Bool(false)),
        )
        .prop("checkEmpty", acc::PUBLIC, Some("int"), None)
        .prop("offset", acc::PUBLIC, Some("mixed"), None)
        .hooks(ClassHooks {
            read_dimension: Some(dim_read),
            read_dimension_is: true,
            write_dimension: Some(dim_write),
            has_dimension: Some(dim_has),
            unset_dimension: Some(dim_unset),
            ..Default::default()
        })
        .register(vm);
}
