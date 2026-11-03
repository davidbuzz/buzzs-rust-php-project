//! The `GMP` class: constructor, (un)serialization and the object handlers
//! (operator overloading, comparison, casts, `var_dump()` table).

use php_gmp::php::{self as gmp, BinaryOp as GmpOp, CountOperand, GmpError};
use php_gmp::Mpz;
use php_runtime::convert::{double_fits_long, dval_to_lval_silent, is_long_compatible};
use php_runtime::float::format_double_h;
use php_runtime::numeric::{is_numeric_string_ex, Number};
use php_runtime::{
    ArrayKey, BinaryOp, CastTarget, HashTable, Obj, PropertyPurpose, Value, ZArray, ZStr,
};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::object::{data, set_internal};
use php_vm::Vm;

use crate::funcs::init_number;
use crate::{check_bits, is_gmp, mpz_of, new_gmp, throw, GmpState};

/// `E_DEPRECATED`.
const E_DEPRECATED: i64 = 8192;

/// Throws an operator-context error (no function prefix).
fn op_throw(vm: &mut Vm, e: &GmpError) -> php_vm::ext::Throw {
    throw(vm, "", e)
}

fn deprecate_fraction(vm: &mut Vm, d: f64) -> VmResult<()> {
    vm.raise(
        E_DEPRECATED,
        &format!(
            "Implicit conversion from float {} to int loses precision",
            format_double_h(d, -1)
        ),
    )
}

/// `gmp_zend_parse_arg_into_mpz_ex(..., is_operator = true)`: `None` is a
/// failure without exception.
fn operand(vm: &mut Vm, v: &Value) -> VmResult<Option<Mpz>> {
    Ok(match v {
        Value::Reference(r) => return operand(vm, &r.get()),
        Value::Object(o) if is_gmp(o) => mpz_of(o),
        Value::Str(s) => match gmp::parse_str(s.as_bytes(), 0, 0) {
            Ok(m) => Some(m),
            Err(e) => return Err(op_throw(vm, &e)),
        },
        Value::Int(i) => Some(Mpz::from(*i)),
        Value::Bool(b) => Some(Mpz::from(i64::from(*b))),
        Value::Float(d) => {
            if d.is_nan() || !double_fits_long(*d) {
                None
            } else {
                #[allow(clippy::cast_possible_truncation)]
                let l = *d as i64;
                if !is_long_compatible(*d, l) {
                    deprecate_fraction(vm, *d)?;
                }
                Some(Mpz::from(l))
            }
        }
        _ => None,
    })
}

/// [`operand`] with the `Number must be of type GMP|string|int` error.
fn operand_or_type_error(vm: &mut Vm, v: &Value) -> VmResult<Mpz> {
    match operand(vm, v)? {
        Some(m) => Ok(m),
        None => Err(op_throw(vm, &GmpError::operand_type(&v.value_name()))),
    }
}

/// `shift_operator_helper` (`**`, `<<`, `>>`).
fn shift_op(vm: &mut Vm, op: GmpOp, l: &Value, r: &Value) -> VmResult<Value> {
    let unsupported = |vm: &mut Vm| {
        op_throw(
            vm,
            &GmpError::unsupported_operands(&l.type_name(), op, &r.type_name()),
        )
    };
    let amount = match r.deref() {
        Value::Int(i) => CountOperand::Int(i),
        Value::Object(o) if is_gmp(&o) => {
            let m = mpz_of(&o).unwrap_or_default();
            let count = gmp::shift_amount(op, CountOperand::Gmp(&m));
            return finish_shift(vm, op, l, count, unsupported);
        }
        Value::Float(d) => {
            let lval = if double_fits_long(d) {
                dval_to_lval_silent(d)
            } else {
                0
            };
            if !is_long_compatible(d, lval) {
                deprecate_fraction(vm, d)?;
            }
            CountOperand::Int(lval)
        }
        Value::Str(s) => match is_numeric_string_ex(s.as_bytes(), true) {
            Some(n) if n.overflow == 0 => match n.value {
                Number::Int(i) => CountOperand::Int(i),
                Number::Float(_) => return Err(op_throw(vm, &GmpError::not_integer_string())),
            },
            _ => return Err(op_throw(vm, &GmpError::not_integer_string())),
        },
        _ => return Err(unsupported(vm)),
    };
    let count = gmp::shift_amount(op, amount);
    finish_shift(vm, op, l, count, unsupported)
}

fn finish_shift(
    vm: &mut Vm,
    op: GmpOp,
    l: &Value,
    count: Result<u64, GmpError>,
    unsupported: impl FnOnce(&mut Vm) -> php_vm::ext::Throw,
) -> VmResult<Value> {
    let count = count.map_err(|e| op_throw(vm, &e))?;
    let Some(base) = operand(vm, l)? else {
        return Err(unsupported(vm));
    };
    let bits = match op {
        GmpOp::Pow => crate::funcs::pow_bits(&base, count),
        GmpOp::Shl if !base.is_zero() => u128::from(base.bit_len()) + u128::from(count),
        _ => 0,
    };
    check_bits(vm, bits)?;
    new_gmp(vm, gmp::count_op(op, &base, count))
}

fn do_operation(vm: &mut Vm, op: BinaryOp, l: &Value, r: &Value) -> VmResult<Option<Value>> {
    let op = match op {
        BinaryOp::Add => GmpOp::Add,
        BinaryOp::Sub => GmpOp::Sub,
        BinaryOp::Mul => GmpOp::Mul,
        BinaryOp::Div => GmpOp::Div,
        BinaryOp::Mod => GmpOp::Mod,
        BinaryOp::Pow => GmpOp::Pow,
        BinaryOp::ShiftLeft => GmpOp::Shl,
        BinaryOp::ShiftRight => GmpOp::Shr,
        BinaryOp::BitOr => GmpOp::BitOr,
        BinaryOp::BitAnd => GmpOp::BitAnd,
        BinaryOp::BitXor => GmpOp::BitXor,
        BinaryOp::BitNot => {
            let Value::Object(o) = l.deref() else {
                return Ok(None);
            };
            let Some(m) = mpz_of(&o) else {
                return Ok(None);
            };
            return new_gmp(vm, gmp::bitwise_not(&m)).map(Some);
        }
        _ => return Ok(None),
    };
    if op.takes_count() {
        return shift_op(vm, op, l, r).map(Some);
    }
    let a = operand_or_type_error(vm, l)?;
    let b = operand_or_type_error(vm, r)?;
    let res = gmp::binary_op(op, &a, &b).map_err(|e| op_throw(vm, &e))?;
    new_gmp(vm, res).map(Some)
}

fn compare(vm: &mut Vm, l: &Value, r: &Value) -> VmResult<Option<i32>> {
    let (l, r) = (l.deref(), r.deref());
    match (&l, &r) {
        (Value::Null, _) => return Ok(Some(-1)),
        (_, Value::Null) => return Ok(Some(1)),
        _ => {}
    }
    let a = operand_or_type_error(vm, &l)?;
    let b = operand_or_type_error(vm, &r)?;
    Ok(Some(gmp::compare(&a, &b)))
}

fn cast(_vm: &mut Vm, o: &Obj, target: CastTarget) -> VmResult<Option<Value>> {
    let Some(m) = mpz_of(o) else {
        return Ok(None);
    };
    Ok(match target {
        CastTarget::String => Some(Value::from(gmp::cast_string(&m))),
        CastTarget::Int => Some(Value::Int(gmp::cast_int(&m))),
        CastTarget::Float => Some(Value::Float(gmp::cast_float(&m))),
        CastTarget::Bool => Some(Value::Bool(gmp::cast_bool(&m))),
        CastTarget::Number => Some(match gmp::cast_number(&m) {
            gmp::Number::Int(i) => Value::Int(i),
            gmp::Number::Float(f) => Value::Float(f),
        }),
        CastTarget::Null | CastTarget::Array => None,
    })
}

/// The standard property table (GMP declares no properties, so these are
/// the dynamic ones).
fn std_props(o: &Obj) -> HashTable {
    data(o).dynamic.borrow().clone().unwrap_or_default()
}

fn properties_for(_vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if purpose != PropertyPurpose::Debug {
        return Ok(None);
    }
    // `zend_std_get_properties` + `zend_array_dup`: numeric names show as
    // integer keys.
    let mut t = HashTable::new();
    for (k, v) in &std_props(o) {
        let k = match k {
            ArrayKey::Str(s) => ArrayKey::from_zstr(s),
            k @ ArrayKey::Int(_) => k.clone(),
        };
        t.insert(k, v.clone());
    }
    let num = mpz_of(o).map(|m| gmp::debug_num(&m)).unwrap_or_default();
    t.insert("num", Value::from(num));
    Ok(Some(ZArray::from_table(t)))
}

fn create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    set_internal(o, GmpState::default());
    Ok(())
}

fn clone(_vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    set_internal(new, GmpState(mpz_of(old).unwrap_or_default()));
    Ok(())
}

fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = init_number(vm, a, "GMP::__construct")?;
    set_internal(&a.this_obj(), GmpState(m));
    Ok(Value::Null)
}

fn serialize(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let m = mpz_of(&o).unwrap_or_default();
    let mut arr = ZArray::from_values([Value::from(gmp::serialize_number(&m))]);
    let props = std_props(&o);
    if !props.is_empty() {
        let mut t = HashTable::new();
        for (k, v) in &props {
            let k = match k {
                ArrayKey::Str(s) => ArrayKey::from_zstr(s),
                k @ ArrayKey::Int(_) => k.clone(),
            };
            t.insert(k, v.clone());
        }
        let _ = arr.make_mut().push(Value::Array(ZArray::from_table(t)));
    }
    Ok(Value::Array(arr))
}

/// `object_properties_load`: writes `props` as (dynamic) properties,
/// with the usual dynamic-property deprecation.
fn load_props(vm: &mut Vm, o: &Obj, props: &ZArray) -> VmResult<()> {
    for (k, v) in props.iter() {
        let name = match k {
            ArrayKey::Int(i) => i.to_string(),
            ArrayKey::Str(s) => s.to_string_lossy(),
        };
        vm.set_property(o, &name, v.clone())?;
    }
    Ok(())
}

fn unserialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let data = a.array(0);
    // The VM may hand over `__unserialize()` data with integer keys
    // stringified; accept both spellings.
    let element = |i: i64| {
        data.get_int(i)
            .or_else(|| data.get_str(i.to_string().as_bytes()))
            .map(Value::deref)
    };
    let num = match element(0) {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    };
    let m =
        gmp::unserialize_number(num.as_ref().map(ZStr::as_bytes)).map_err(|e| op_throw(vm, &e))?;
    set_internal(&o, GmpState(m));
    if let Some(props) = element(1) {
        let Value::Array(props) = props else {
            return Err(op_throw(vm, &gmp::unserialize_properties_error()));
        };
        load_props(vm, &o, &props)?;
    }
    Ok(Value::Null)
}

/// Splits a leading `s:N:"...";` off a serialized payload.
fn split_serialized_string(p: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = p.strip_prefix(b"s:")?;
    let colon = rest.iter().position(|&c| c == b':')?;
    let len: usize = std::str::from_utf8(&rest[..colon]).ok()?.parse().ok()?;
    let body = rest.get(colon + 1..)?.strip_prefix(b"\"")?;
    let s = body.get(..len)?;
    let tail = body.get(len..)?.strip_prefix(b"\";")?;
    Some((s, tail))
}

/// The legacy `C:3:"GMP":...` payload (`gmp_unserialize`): a serialized
/// decimal string followed by the serialized property array.
fn unserialize_legacy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let payload = a.str(0);
    let Some((num, tail)) = split_serialized_string(payload.as_bytes()) else {
        return Err(op_throw(vm, &gmp::unserialize_number_error()));
    };
    let m = gmp::unserialize_legacy_number(Some(num)).map_err(|e| op_throw(vm, &e))?;
    set_internal(&o, GmpState(m));
    if tail != b"a:0:{}" {
        let props = if tail.starts_with(b"a:") {
            let f = Value::from("unserialize");
            vm.call_value(&f, vec![Value::Str(ZStr::from(tail))])?
        } else {
            Value::Null
        };
        let Value::Array(props) = props else {
            return Err(op_throw(vm, &gmp::unserialize_properties_error()));
        };
        // `zend_hash_copy` into the property table (no deprecation).
        let mut dynamic = data(&o).dynamic.borrow_mut();
        let t = dynamic.get_or_insert_with(HashTable::new);
        for (k, v) in props.iter() {
            t.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Null)
}

pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let hooks = ClassHooks {
        create: Some(create),
        clone: Some(clone),
        properties_for: Some(properties_for),
        compare: Some(compare),
        cast: Some(cast),
        do_operation: Some(do_operation),
        ..ClassHooks::default()
    };
    let vm = m.vm();
    ClassBuilder::new("GMP")
        .flags(class_flags::FINAL)
        .method(
            "__construct(int|string $num = 0, int $base = 0)",
            acc::PUBLIC,
            construct,
        )
        .method("__serialize(): array", acc::PUBLIC, serialize)
        .method("__unserialize(array $data): void", acc::PUBLIC, unserialize)
        .method(
            "unserialize(string $data): void",
            acc::PRIVATE,
            unserialize_legacy,
        )
        .hooks(hooks)
        .register(vm);
}

#[cfg(test)]
mod tests {
    use super::split_serialized_string;

    #[test]
    fn splits_legacy_payload() {
        assert_eq!(
            split_serialized_string(b"s:2:\"42\";a:0:{}"),
            Some((&b"42"[..], &b"a:0:{}"[..]))
        );
        assert_eq!(split_serialized_string(b"i:1;"), None);
    }
}
