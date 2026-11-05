//! `BcMath\Number`: the class registration, its methods and the object
//! handlers (operators, comparison, casts, virtual readonly properties).
//!
//! The object's internal state is a [`State`]; `None` until the
//! constructor (or `__unserialize`) has run.

use std::cmp::Ordering;

use php_bcmath::{BcError, BinaryOp as BcOp, Number, Operand};
use php_runtime::convert::{double_fits_long, is_long_compatible};
use php_runtime::float::format_double_h;
use php_runtime::{BinaryOp, CastTarget, Obj, PhpRef, PropertyPurpose, Value, ZArray, ZStr};
use php_vm::ext::{
    acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, PropCtx, VmResult,
};
use php_vm::object::{internal, set_internal};
use php_vm::Vm;

use crate::{is_number, lift, rounding_mode, scale_arg, E_DEPRECATED};

/// Internal state of a `BcMath\Number` object.
#[derive(Debug, Clone, Default)]
pub(crate) struct State(Option<Number>);

const CLASS: &str = "BcMath\\Number";

/// The number held by `o` (`None` when uninitialized or not a Number).
fn number_of(o: &Obj) -> Option<Number> {
    internal::<State, _>(o, |s| s.0.clone()).flatten()
}

/// Wraps `n` in a new `BcMath\Number` object.
fn new_number(vm: &mut Vm, n: Number) -> VmResult<Value> {
    let Some(class) = vm.find_class(CLASS.as_bytes()) else {
        return Err(vm.throw("Error", "Class \"BcMath\\Number\" not found"));
    };
    let o = vm.new_object(&class)?;
    set_internal(&o, State(Some(n)));
    Ok(Value::Object(o))
}

/// An operand after parameter parsing.
enum Arg {
    Num(Number),
    Str(ZStr),
    Int(i64),
}

impl Arg {
    fn operand(&self) -> Operand<'_> {
        match self {
            Self::Num(n) => Operand::Number(n),
            Self::Str(s) => Operand::Str(s.as_bytes()),
            Self::Int(i) => Operand::Int(*i),
        }
    }
}

/// `zend_parse_arg_long_weak` for a float: `None` for NaN/out-of-range,
/// with the "loses precision" deprecation for fractional values.
fn float_to_long(vm: &mut Vm, d: f64) -> VmResult<Option<i64>> {
    if d.is_nan() || !double_fits_long(d) {
        return Ok(None);
    }
    #[allow(clippy::cast_possible_truncation)]
    let l = d as i64;
    if !is_long_compatible(d, l) {
        vm.raise(
            E_DEPRECATED,
            &format!(
                "Implicit conversion from float {} to int loses precision",
                format_double_h(d, -1)
            ),
        )?;
    }
    Ok(Some(l))
}

/// Whether the calling code (the code below the running native frame, or
/// the running code for operator handlers) uses `strict_types`
/// (`ZEND_ARG_USES_STRICT_TYPES()`).
fn caller_strict(vm: &Vm) -> bool {
    let mut frames = vm.frames.iter().rev();
    match frames.next() {
        Some(f) if matches!(f.kind, php_vm::vm::FrameKind::Builtin) => {
            frames.next().is_some_and(|f| f.code.strict_types)
        }
        Some(f) => f.code.strict_types,
        None => false,
    }
}

/// `bcmath_number_parse_num` (operator operands). `None` is `FAILURE`.
fn operator_arg(vm: &mut Vm, v: &Value) -> VmResult<Option<Arg>> {
    Ok(match v {
        Value::Reference(r) => return operator_arg(vm, &r.get()),
        Value::Object(o) if is_number(o) => number_of(o).map(Arg::Num),
        Value::Int(i) => Some(Arg::Int(*i)),
        Value::Str(s) => Some(Arg::Str(s.clone())),
        // `zend_parse_arg_long_slow` refuses everything in strict mode.
        _ if caller_strict(vm) => None,
        Value::Bool(b) => Some(Arg::Int(i64::from(*b))),
        Value::Float(d) => float_to_long(vm, *d)?.map(Arg::Int),
        _ => None,
    })
}

/// `BCMATH_PARAM_NUMBER_OR_STR_OR_LONG` for method argument `n`.
fn method_arg(vm: &mut Vm, a: &Args, n: usize, method: &str, pname: &str) -> VmResult<Arg> {
    let v = a.at(n);
    let func = format!("{CLASS}::{method}");
    match &v {
        Value::Object(o) if is_number(o) => {
            if let Some(num) = number_of(o) {
                return Ok(Arg::Num(num));
            }
        }
        Value::Int(i) => return Ok(Arg::Int(*i)),
        Value::Str(s) => return Ok(Arg::Str(s.clone())),
        _ if caller_strict(vm) => {}
        Value::Bool(b) => return Ok(Arg::Int(i64::from(*b))),
        Value::Null => {
            vm.raise(
                E_DEPRECATED,
                &format!(
                    "{func}(): Passing null to parameter #{} (${pname}) of type {CLASS}|string|int is deprecated",
                    n + 1
                ),
            )?;
            return Ok(Arg::Int(0));
        }
        Value::Float(d) => {
            if let Some(l) = float_to_long(vm, *d)? {
                return Ok(Arg::Int(l));
            }
            return Ok(Arg::Str(vm.value_to_string(&v)?));
        }
        Value::Object(obj)
            if php_vm::object::class_of(obj)
                .find_method(b"__tostring")
                .is_some() =>
        {
            return Ok(Arg::Str(vm.value_to_string(&v)?));
        }
        _ => {}
    }
    let msg = format!(
        "{func}(): Argument #{} (${pname}) must be of type int, string, or {CLASS}, {} given",
        n + 1,
        v.value_name()
    );
    Err(vm.throw("TypeError", &msg))
}

/// `$this`'s number.
fn this_number(vm: &mut Vm, a: &Args) -> VmResult<Number> {
    match a.this.as_ref().and_then(number_of) {
        Some(n) => Ok(n),
        None => Err(vm.throw(
            "Error",
            "Typed property BcMath\\Number::$value must not be accessed before initialization",
        )),
    }
}

/// Whether `$this` already holds a number.
fn this_initialized(a: &Args) -> bool {
    a.this.as_ref().and_then(number_of).is_some()
}

fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = a.at(0);
    let arg = match &v {
        Value::Int(i) => Arg::Int(*i),
        _ => Arg::Str(vm.value_to_string(&v)?),
    };
    let r = Number::construct(this_initialized(a), arg.operand());
    let n = lift(vm, r)?;
    set_internal(&a.this_obj(), State(Some(n)));
    Ok(Value::Null)
}

type Calc = fn(&Number, Operand<'_>, Option<i64>) -> Result<Number, BcError>;

fn calc_method(vm: &mut Vm, a: &Args, name: &str, pname: &str, f: Calc) -> VmResult<Value> {
    let this = this_number(vm, a)?;
    let arg = method_arg(vm, a, 0, name, pname)?;
    let r = f(&this, arg.operand(), scale_arg(a, 1));
    let n = lift(vm, r)?;
    new_number(vm, n)
}

fn add(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    calc_method(vm, a, "add", "num", Number::add)
}

fn sub(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    calc_method(vm, a, "sub", "num", Number::sub)
}

fn mul(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    calc_method(vm, a, "mul", "num", Number::mul)
}

fn div(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    calc_method(vm, a, "div", "num", Number::div)
}

fn modulo(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    calc_method(vm, a, "mod", "num", Number::modulo)
}

fn pow(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    calc_method(vm, a, "pow", "exponent", Number::pow)
}

fn divmod(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = this_number(vm, a)?;
    let arg = method_arg(vm, a, 0, "divmod", "num")?;
    let r = this.divmod(arg.operand(), scale_arg(a, 1));
    let (q, r) = lift(vm, r)?;
    let q = new_number(vm, q)?;
    let r = new_number(vm, r)?;
    Ok(Value::Array(ZArray::from_values([q, r])))
}

fn powmod(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = this_number(vm, a)?;
    let exponent = method_arg(vm, a, 0, "powmod", "exponent")?;
    let modulus = method_arg(vm, a, 1, "powmod", "modulus")?;
    let res = this.powmod(exponent.operand(), modulus.operand(), scale_arg(a, 2));
    let num = lift(vm, res)?;
    new_number(vm, num)
}

fn sqrt(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = this_number(vm, a)?;
    let r = this.sqrt(scale_arg(a, 0));
    let n = lift(vm, r)?;
    new_number(vm, n)
}

fn floor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = this_number(vm, a)?;
    new_number(vm, this.floor())
}

fn ceil(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = this_number(vm, a)?;
    new_number(vm, this.ceil())
}

fn round(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = this_number(vm, a)?;
    let precision = if a.has(0) { a.int(0) } else { 0 };
    let mode = rounding_mode(vm, a.opt(1))?;
    let r = this.round(precision, mode);
    let n = lift(vm, r)?;
    new_number(vm, n)
}

fn compare(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = this_number(vm, a)?;
    let arg = method_arg(vm, a, 0, "compare", "num")?;
    let r = this.compare(arg.operand(), scale_arg(a, 1));
    lift(vm, r).map(Value::Int)
}

fn to_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = this_number(vm, a)?;
    Ok(Value::Str(ZStr::from(this.to_php_string())))
}

fn serialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = this_number(vm, a)?;
    let mut arr = ZArray::new();
    arr.make_mut()
        .insert("value", Value::Str(ZStr::from(this.serialize())));
    Ok(Value::Array(arr))
}

fn unserialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.array(0);
    let value = match data.get_str(b"value").map(Value::deref) {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    };
    let r = Number::unserialize(this_initialized(a), value.as_ref().map(ZStr::as_bytes));
    let n = lift(vm, r)?;
    set_internal(&a.this_obj(), State(Some(n)));
    Ok(Value::Null)
}

// ------------------------------------------------------------- handlers

fn create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    set_internal(o, State::default());
    Ok(())
}

fn clone(_vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    set_internal(new, State(number_of(old)));
    Ok(())
}

fn properties_for(_vm: &mut Vm, o: &Obj, _p: PropertyPurpose) -> VmResult<Option<ZArray>> {
    let mut arr = ZArray::new();
    if let Some(n) = number_of(o) {
        let t = arr.make_mut();
        t.insert("value", Value::Str(ZStr::from(n.value())));
        t.insert("scale", Value::Int(n.scale()));
    }
    Ok(Some(arr))
}

/// `bcmath_number_read_property`. `bcmath_number_get_property_ptr_ptr`
/// returns NULL (every property is virtual), so a reference fetch
/// (`$x = &$n->value`) binds to a temporary copy of the value instead of
/// failing on the readonly declaration (GH-18641).
fn read_property(vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<Option<Value>> {
    let Some(n) = number_of(o) else {
        return Ok(None);
    };
    let v = match name.as_bytes() {
        b"value" => Value::Str(ZStr::from(n.value())),
        b"scale" => Value::Int(n.scale()),
        _ => return Ok(None),
    };
    Ok(Some(if vm.property_context() == PropCtx::Ref {
        Value::Reference(PhpRef::new(v))
    } else {
        v
    }))
}

fn write_property(vm: &mut Vm, _o: &Obj, name: &ZStr, _v: &Value) -> VmResult<bool> {
    match name.as_bytes() {
        b"value" | b"scale" => Err(vm.throw(
            "Error",
            &BcError::readonly_modification(&name.to_string_lossy()).message,
        )),
        // Readonly classes forbid dynamic properties.
        _ => Err(vm.throw(
            "Error",
            &format!(
                "Cannot create dynamic property {CLASS}::${}",
                name.to_string_lossy()
            ),
        )),
    }
}

fn unset_property(vm: &mut Vm, _o: &Obj, name: &ZStr) -> VmResult<bool> {
    match name.as_bytes() {
        b"value" | b"scale" => Err(vm.throw(
            "Error",
            &BcError::readonly_unset(&name.to_string_lossy()).message,
        )),
        _ => Ok(false),
    }
}

/// `bcmath_number_has_property`: `isset()` (`check_empty == false`:
/// whether it is set) and `empty()` (`check_empty == true`: C's
/// `ZEND_PROPERTY_NOT_EMPTY`, whether it is set and not empty; the VM
/// negates it for `empty()`, as the `ClassHooks::has_property` contract
/// says).
fn has_property(_vm: &mut Vm, o: &Obj, name: &ZStr, check_empty: bool) -> VmResult<Option<bool>> {
    let name = name.to_string_lossy();
    let set_and_not_empty = number_of(o)
        .and_then(|n| n.has_property(&name, check_empty))
        .unwrap_or(false);
    Ok(Some(set_and_not_empty))
}

fn cast(vm: &mut Vm, o: &Obj, target: CastTarget) -> VmResult<Option<Value>> {
    Ok(match target {
        CastTarget::Bool => Some(Value::Bool(number_of(o).is_some_and(|n| n.is_truthy()))),
        // `(array)` uses `get_properties_for(ZEND_PROP_PURPOSE_ARRAY_CAST)`.
        CastTarget::Array => properties_for(vm, o, PropertyPurpose::ArrayCast)?.map(Value::Array),
        _ => None,
    })
}

fn do_operation(vm: &mut Vm, op: BinaryOp, l: &Value, r: &Value) -> VmResult<Option<Value>> {
    let op = match op {
        BinaryOp::Add => BcOp::Add,
        BinaryOp::Sub => BcOp::Sub,
        BinaryOp::Mul => BcOp::Mul,
        BinaryOp::Div => BcOp::Div,
        BinaryOp::Mod => BcOp::Mod,
        BinaryOp::Pow => BcOp::Pow,
        _ => return Ok(None),
    };
    let Some(la) = operator_arg(vm, l)? else {
        return Ok(None);
    };
    let Some(ra) = operator_arg(vm, r)? else {
        return Ok(None);
    };
    let res = Number::do_operation(op, la.operand(), ra.operand());
    let n = lift(vm, res)?;
    new_number(vm, n).map(Some)
}

fn compare_handler(vm: &mut Vm, l: &Value, r: &Value) -> VmResult<Option<i32>> {
    // ZEND_UNCOMPARABLE is 1.
    let Some(la) = operator_arg(vm, l)? else {
        return Ok(Some(1));
    };
    let Some(ra) = operator_arg(vm, r)? else {
        return Ok(Some(1));
    };
    let res = Number::compare_operands(la.operand(), ra.operand());
    Ok(Some(match lift(vm, res)? {
        None | Some(Ordering::Greater) => 1,
        Some(Ordering::Less) => -1,
        Some(Ordering::Equal) => 0,
    }))
}

pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    let hooks = ClassHooks {
        create: Some(create),
        clone: Some(clone),
        properties_for: Some(properties_for),
        compare: Some(compare_handler),
        cast: Some(cast),
        do_operation: Some(do_operation),
        read_property: Some(read_property),
        write_property: Some(write_property),
        has_property: Some(has_property),
        unset_property: Some(unset_property),
        ..ClassHooks::default()
    };
    ClassBuilder::new(CLASS)
        .flags(class_flags::FINAL)
        .implements(vm, &["Stringable"])
        .prop("value", acc::PUBLIC | acc::READONLY, Some("string"), None)
        .prop("scale", acc::PUBLIC | acc::READONLY, Some("int"), None)
        .method("__construct(string|int $num)", acc::PUBLIC, construct)
        .method(
            "add(mixed $num, ?int $scale = null): BcMath\\Number",
            acc::PUBLIC,
            add,
        )
        .method(
            "sub(mixed $num, ?int $scale = null): BcMath\\Number",
            acc::PUBLIC,
            sub,
        )
        .method(
            "mul(mixed $num, ?int $scale = null): BcMath\\Number",
            acc::PUBLIC,
            mul,
        )
        .method(
            "div(mixed $num, ?int $scale = null): BcMath\\Number",
            acc::PUBLIC,
            div,
        )
        .method(
            "mod(mixed $num, ?int $scale = null): BcMath\\Number",
            acc::PUBLIC,
            modulo,
        )
        .method(
            "divmod(mixed $num, ?int $scale = null): array",
            acc::PUBLIC,
            divmod,
        )
        .method(
            "powmod(mixed $exponent, mixed $modulus, ?int $scale = null): BcMath\\Number",
            acc::PUBLIC,
            powmod,
        )
        .method(
            "pow(mixed $exponent, ?int $scale = null): BcMath\\Number",
            acc::PUBLIC,
            pow,
        )
        .method("sqrt(?int $scale = null): BcMath\\Number", acc::PUBLIC, sqrt)
        .method("floor(): BcMath\\Number", acc::PUBLIC, floor)
        .method("ceil(): BcMath\\Number", acc::PUBLIC, ceil)
        .method(
            "round(int $precision = 0, RoundingMode $mode = RoundingMode::HalfAwayFromZero): BcMath\\Number",
            acc::PUBLIC,
            round,
        )
        .method(
            "compare(mixed $num, ?int $scale = null): int",
            acc::PUBLIC,
            compare,
        )
        .method("__toString(): string", acc::PUBLIC, to_string)
        .method("__serialize(): array", acc::PUBLIC, serialize)
        .method("__unserialize(array $data): void", acc::PUBLIC, unserialize)
        .hooks(hooks)
        .register(vm);
}
