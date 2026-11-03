//! The plain functions of `test.stub.php` (`ext_functions[]`).

use crate::patch::{deprecate_function, edit_function};
use crate::state;
use crate::util::{attribute, s, value_name, E_ERROR, E_WARNING};
use php_runtime::{Value, ZArray, ZStr};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::func::flags;
use php_vm::vm::FrameKind;
use php_vm::Vm;

/// Handler of the functions that only parse (no) parameters and return
/// `null`.
fn none(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

fn ret_true(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(true))
}

fn ret_false(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(false))
}

fn ret_one(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(1))
}

/// `zend_trigger_bailout()`: `zend_error(E_ERROR, "Bailout")`.
fn trigger_bailout(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    vm.raise(E_ERROR, "Bailout")?;
    Ok(Value::Null)
}

/// `zend_test_array_return()`: returns nothing although declared `array`
/// (a release build hands back `null`).
fn array_return(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

/// `&zend_test_forward_reference(mixed &$value)`.
fn forward_reference(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(a.values.first().cloned().unwrap_or_default())
}

/// `zend_create_unterminated_string(string $str)`: a copy.
fn create_unterminated_string(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(ZStr::from(a.str(0).as_bytes())))
}

/// `zend_leak_variable(mixed $variable)`.
fn leak_variable(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let refcounted = matches!(
        a.at(0),
        Value::Array(_) | Value::Object(_) | Value::Resource(_) | Value::Reference(_)
    ) || matches!(a.at(0), Value::Str(ref st) if !st.is_empty());
    if !refcounted {
        vm.raise(E_WARNING, "Cannot leak variable that is not refcounted")?;
        return Ok(Value::Null);
    }
    state::leak(a.at(0));
    Ok(Value::Null)
}

/// `zend_test_erealloc_block_size(int $old_size, int $new_size)`: the
/// Zend MM block sizes of a reallocated and a fresh block.
fn erealloc_block_size(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (old, new) = (a.int(0), a.int(1));
    if old < 1 {
        return Err(a.value_error(vm, 1, "must be greater than 0"));
    }
    if new < 1 {
        return Err(a.value_error(vm, 2, "must be greater than 0"));
    }
    let new = u64::try_from(new).unwrap_or_default();
    let old = u64::try_from(old).unwrap_or_default();
    // erealloc keeps a huge or large block in place when shrinking
    // within the same allocation class.
    let realloc = realloc_block_size(old, new);
    let fresh = mm_block_size(new);
    Ok(Value::Array(ZArray::from_values([
        Value::Int(i64::try_from(realloc).unwrap_or(i64::MAX)),
        Value::Int(i64::try_from(fresh).unwrap_or(i64::MAX)),
    ])))
}

/// `ZEND_MM_BINS_INFO` small-bin sizes.
const MM_BINS: [u64; 30] = [
    8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640,
    768, 896, 1024, 1280, 1536, 1792, 2048, 2560, 3072,
];
const MM_PAGE: u64 = 4096;
const MM_CHUNK: u64 = 2 * 1024 * 1024;
const MM_MAX_LARGE: u64 = MM_CHUNK - MM_PAGE;

/// `zend_mem_block_size` of a fresh `emalloc(size)`.
pub(crate) fn mm_block_size(size: u64) -> u64 {
    if let Some(b) = MM_BINS.iter().find(|&&b| b >= size) {
        *b
    } else if size <= MM_MAX_LARGE {
        size.div_ceil(MM_PAGE) * MM_PAGE
    } else {
        size.div_ceil(MM_PAGE) * MM_PAGE
    }
}

/// `zend_mem_block_size` after `erealloc(emalloc(old), new)`.
fn realloc_block_size(old: u64, new: u64) -> u64 {
    let old_bs = mm_block_size(old);
    if old > MM_MAX_LARGE && new > MM_MAX_LARGE {
        // Huge blocks are resized in place.
        return new.div_ceil(MM_PAGE) * MM_PAGE;
    }
    if (MM_BINS[MM_BINS.len() - 1]..=MM_MAX_LARGE).contains(&old)
        && old > MM_BINS[MM_BINS.len() - 1]
        && new > MM_BINS[MM_BINS.len() - 1]
        && new <= MM_MAX_LARGE
    {
        // Large runs shrink or grow in place when possible.
        return new.div_ceil(MM_PAGE) * MM_PAGE;
    }
    if new <= old_bs && old <= MM_BINS[MM_BINS.len() - 1] {
        let fresh = mm_block_size(new);
        // A small block is kept when the new size still fits and is not
        // much smaller.
        if fresh == old_bs {
            return old_bs;
        }
        return fresh;
    }
    mm_block_size(new)
}

/// `zend_test_callback(callable $callback)`.
fn test_callback(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cb = a.at(0);
    vm.call_value(&cb, Vec::new())?;
    Ok(Value::Null)
}

/// `zend_weakmap_attach(object $object, mixed $value)`.
fn weakmap_attach(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Value::Object(o) = a.at(0) else {
        return Ok(Value::Bool(false));
    };
    Ok(Value::Bool(state::weakmap_attach(&o, a.at(1))))
}

/// `zend_weakmap_remove(object $object)`.
fn weakmap_remove(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Value::Object(o) = a.at(0) else {
        return Ok(Value::Bool(false));
    };
    Ok(Value::Bool(state::weakmap_remove(&o)))
}

/// `zend_weakmap_dump()`.
fn weakmap_dump(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(state::weakmap_dump()))
}

/// `zend_get_unit_enum()`: `ZendTestUnitEnum::Foo`.
fn get_unit_enum(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    vm.call_function("constant", vec![s("ZendTestUnitEnum::Foo")])
}

/// `zend_get_current_func_name()`: the name of the calling function
/// (`get_function_or_method_name` of the previous frame).
fn current_func_name(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let n = vm.frames.len();
    let name = if n >= 2 {
        let f = &vm.frames[n - 2];
        match (&f.func, f.kind) {
            (Some(func), FrameKind::Function | FrameKind::Builtin) => {
                let fname = func.name().to_string_lossy();
                match &f.scope {
                    Some(c) if !fname.contains("{closure") => {
                        format!("{}::{fname}", c.name.to_string_lossy())
                    }
                    _ => fname,
                }
            }
            _ => "main".to_string(),
        }
    } else {
        "main".to_string()
    };
    Ok(s(&name))
}

/// `zend_call_method(object|string $obj_or_class, string $method,
/// mixed $arg1 = UNKNOWN, mixed $arg2 = UNKNOWN)`.
fn call_method(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let target = a.at(0);
    let method = a.str(1);
    let mut args = Vec::new();
    for i in 2..4 {
        if let Some(v) = a.get(i) {
            args.push(v);
        }
    }
    let callable = match &target {
        Value::Object(_) => Value::Array(ZArray::from_values([target.clone(), Value::Str(method)])),
        Value::Str(cn) => {
            let Some(c) = vm.lookup_class(cn)? else {
                vm.raise(
                    E_ERROR,
                    &format!("Unknown class '{}'", cn.to_string_lossy()),
                )?;
                return Ok(Value::Null);
            };
            Value::Array(ZArray::from_values([
                Value::Str(c.name.clone()),
                Value::Str(method),
            ]))
        }
        other => {
            return Err(a.type_error(
                vm,
                1,
                &format!("must be of type object|string, {} given", value_name(other)),
            ));
        }
    };
    vm.call_value(&callable, args)
}

/// `zend_object_init_with_constructor(string $class, mixed ...$args)`.
fn object_init_with_constructor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(cls) = crate::zpp::class_arg(vm, a, 0, false)? else {
        return Ok(Value::Null);
    };
    let args: Vec<Value> = a.values.iter().skip(1).cloned().collect();
    let named = std::mem::take(&mut a.named);
    if cls.is_interface() || cls.is_trait() || cls.is_abstract() || cls.is_enum() {
        let what = if cls.is_interface() {
            "interface"
        } else if cls.is_trait() {
            "trait"
        } else if cls.is_enum() {
            "enum"
        } else {
            "abstract class"
        };
        return Err(vm.throw(
            "Error",
            &format!("Cannot instantiate {what} {}", cls.name.to_string_lossy()),
        ));
    }
    if cls.magic.construct.is_none() && (!args.is_empty() || !named.is_empty()) {
        // object_init_with_constructor() with arguments but no
        // constructor.
        return Err(vm.throw(
            "ArgumentCountError",
            &format!(
                "{}::__construct() does not accept any arguments",
                cls.name.to_string_lossy()
            ),
        ));
    }
    let o = vm.new_object_named(&cls, args, named)?;
    Ok(Value::Object(o))
}

/// `zend_call_method_if_exists(object $obj, string $method, mixed ...$args)`.
fn call_method_if_exists(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Value::Object(o) = a.at(0) else {
        return Ok(Value::Null);
    };
    let method = a.str(1);
    let args: Vec<Value> = a.values.iter().skip(2).cloned().collect();
    let callable = Value::Array(ZArray::from_values([Value::Object(o), Value::Str(method)]));
    if !vm.is_callable(&callable) {
        return Ok(Value::Null);
    }
    let r = vm.call_value(&callable, args)?;
    Ok(match r {
        Value::Reference(r) => r.get(),
        v => v,
    })
}

/// `zend_test_call_with_consumed_args(callable $cb, array $args, int $consumed_args)`.
fn call_with_consumed_args(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cb = a.at(0);
    let args = a.array(1);
    let consumed = a.int(2);
    if !(0..=i64::from(u32::MAX)).contains(&consumed) {
        return Err(a.value_error(vm, 3, "must be between 0 and 4294967295"));
    }
    let list: Vec<Value> = args.values().cloned().collect();
    let r = vm.call_value(&cb, list)?;
    Ok(Value::Array(ZArray::from_pairs([
        ("consumed_args", Value::Int(0)),
        ("retval", r),
    ])))
}

/// `zend_test_refcount(mixed $value)`: values are not reference counted
/// the same way here; report what php-src does for a fresh value.
fn refcount(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(match a.at(0) {
        Value::Array(arr) => Value::Int(i64::try_from(arr.refcount()).unwrap_or(1)),
        Value::Object(o) => Value::Int(i64::try_from(o.refcount()).unwrap_or(1)),
        Value::Str(st) if !st.is_empty() => Value::Int(1),
        _ => Value::Int(-1),
    })
}

fn ini_parse_quantity(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (v, err) = php_support::ini::parse_quantity(a.str(0).as_bytes());
    if let Some(e) = err {
        vm.raise(E_WARNING, &e)?;
    }
    Ok(Value::Int(v))
}

fn ini_parse_uquantity(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (v, err) = php_support::ini::parse_uquantity(a.str(0).as_bytes());
    if let Some(e) = err {
        vm.raise(E_WARNING, &e)?;
    }
    Ok(Value::Int(i64::from_ne_bytes(v.to_ne_bytes())))
}

/// `zend_test_zend_ini_str()`: `ZT_G(str_test)`.
fn ini_str(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let v = vm.ini_string("zend_test.str_test").unwrap_or_default();
    Ok(Value::Str(ZStr::from(v)))
}

fn zstr_init_literal(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(ZStr::from(&b"foo\0bar"[..])))
}

/// `zend_test_zend_call_stack_get()`: the native stack of this thread.
fn call_stack_get(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let marker = 0u8;
    let pos = std::ptr::addr_of!(marker) as usize;
    let Some((base, size)) = crate::stack::thread_stack() else {
        return Ok(Value::Null);
    };
    let limit = base
        .saturating_sub(size)
        .saturating_add(crate::stack::RESERVED);
    Ok(Value::Array(ZArray::from_pairs([
        ("base", s(&format!("{base:#x}"))),
        ("max_size", s(&format!("{size:#x}"))),
        ("position", s(&format!("{pos:#x}"))),
        ("EG(stack_limit)", s(&format!("{limit:#x}"))),
    ])))
}

/// `zend_test_zend_call_stack_use_all()`: recurses until the native
/// stack is nearly exhausted; returns the depth reached.
fn call_stack_use_all(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let Some((base, size)) = crate::stack::thread_stack() else {
        return Ok(Value::Null);
    };
    let limit = base.saturating_sub(size).saturating_add(4096 * 4);
    Ok(Value::Int(crate::stack::use_all(limit)))
}

/// `zend_test_is_string_marked_as_valid_utf8(string $string)`: strings
/// carry no UTF-8 flag here; report validity.
fn is_valid_utf8(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(
        std::str::from_utf8(a.str(0).as_bytes()).is_ok(),
    ))
}

/// `zend_get_map_ptr_last()`.
fn map_ptr_last(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(0))
}

/// `zend_test_crash(?string $message = null)`.
fn crash(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    if let Some(m) = a.opt(0) {
        let m = vm.value_to_string(&m)?;
        vm.output(m.as_bytes());
    }
    vm.flush_stdout();
    std::process::abort();
}

/// `zend_test_raise_and_throw(int $signal)`.
fn raise_and_throw(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let sig = i32::try_from(a.int(0)).unwrap_or(-1);
    if !crate::stack::raise_signal(sig) {
        return Err(vm.throw("Error", "raise() failed"));
    }
    Err(vm.throw("Exception", "Exception after raise()"))
}

/// `zend_test_fill_packed_array(array &$array)`.
fn fill_packed_array(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(Value::Reference(r)) = a.values.first().cloned() else {
        return Ok(Value::Null);
    };
    let Value::Array(mut arr) = r.get() else {
        return Ok(Value::Null);
    };
    if !arr.is_packed() {
        return Err(a.value_error(vm, 1, "must be a packed array"));
    }
    let t = arr.make_mut();
    for i in 0..10 {
        let _ = t.push(Value::Int(i));
    }
    r.set(Value::Array(arr));
    Ok(Value::Null)
}

/// `zend_test_create_throwing_resource()`.
fn create_throwing_resource(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Resource(php_runtime::Resource::new(
        "throwing resource",
        None,
    )))
}

/// `get_open_basedir()`.
fn open_basedir(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(match vm.ini_string("open_basedir") {
        Some(v) if !v.is_empty() => Value::Str(ZStr::from(v)),
        _ => Value::Null,
    })
}

/// `zend_test_log_err_debug(string $str)`.
fn log_err_debug(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let msg = Value::Str(a.str(0));
    vm.call_function("error_log", vec![msg])?;
    Ok(Value::Null)
}

/// `zend_test_compile_to_ast(string $str)`: parses and re-exports the
/// code (`zend_ast_export`).
fn compile_to_ast(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let src = a.str(0);
    match php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()) {
        Ok(p) => Ok(Value::Str(ZStr::from(php_parser::export::export_program(
            &p,
        )))),
        Err(e) => Err(vm.throw("ParseError", &e.message)),
    }
}

/// `zend_test_gh19792()`.
fn gh19792(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    vm.raise(E_WARNING, "a warning")?;
    Err(vm.throw("Error", "an exception"))
}

/// `zend_new_singleton_array()`.
fn singleton_array(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(ZArray::new()))
}

/// `zend_test_set_local_var(string $name, mixed $value)`
/// (`zend_set_local_var(name, value, force: false)`).
fn set_local_var(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0);
    let value = a.at(1);
    let Some(idx) = vm
        .frames
        .iter()
        .rposition(|f| f.kind != FrameKind::Builtin && f.kind != FrameKind::ConstExpr)
    else {
        return Ok(Value::Bool(false));
    };
    let frame = &vm.frames[idx];
    let vars = frame.vars.clone();
    let has_table = frame.kind != FrameKind::Function;
    let old = {
        let mut st = vars.borrow_mut();
        let slot = match st.find(name.as_bytes()) {
            Some(i) => Some(i),
            None if has_table => Some(st.find_or_add(name.as_bytes())),
            None => None,
        };
        let Some(i) = slot else {
            return Ok(Value::Bool(false));
        };
        let i = i as usize;
        match st.slots.get(i).cloned().flatten() {
            Some(Value::Reference(r)) => Some(r.set(value)),
            other => {
                if let Some(s) = st.slots.get_mut(i) {
                    *s = Some(value);
                }
                other
            }
        }
    };
    drop(old);
    vm.run_released_destructors()?;
    Ok(Value::Bool(true))
}

/// `zend_test_internal_dnf_arguments(Iterator|(Traversable&Countable) $arg)`.
fn internal_dnf_arguments(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = a.at(0);
    let Value::Object(o) = &v else {
        return Err(a.type_error(
            vm,
            1,
            &format!(
                "must be of type Iterator|(Traversable&Countable), {} given",
                value_name(&v)
            ),
        ));
    };
    let cls = php_vm::object::class_of(o).clone();
    let ok = cls.instance_of_name(b"iterator")
        || (cls.instance_of_name(b"traversable") && cls.instance_of_name(b"countable"));
    if !ok {
        return Err(a.type_error(
            vm,
            1,
            &format!(
                "must be of type Iterator|(Traversable&Countable), {} given",
                cls.name.to_string_lossy()
            ),
        ));
    }
    Ok(v)
}

/// `zend_iterable_legacy(iterable $arg1, ?iterable $arg2 = null)`.
fn iterable_legacy(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(a.at(0))
}

/// `zend_test_cast_fread($stream)`: reads through the stream (the stdio
/// cast is not observable here).
fn cast_fread(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = a.at(0);
    vm.call_function("fread", vec![st, Value::Int(10240)])?;
    Ok(Value::Null)
}

/// `zend_test_uri_parser` and friends are not provided by this binding.
#[allow(clippy::too_many_lines)]
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function("zend_trigger_bailout(): never", trigger_bailout);
    m.function("zend_test_array_return(): array", array_return);
    m.function("zend_test_nullable_array_return(): ?array", none);
    m.function("zend_test_void_return(): void", none);
    m.function(
        "&zend_test_forward_reference(mixed &$value): mixed",
        forward_reference,
    );
    m.function(
        "zend_test_compile_string(string $source_string, string $filename, int $position): void",
        crate::compile::compile_string,
    );
    m.function("zend_test_deprecated(mixed $arg = null): void", none);
    m.function("zend_test_deprecated_attr(): void", none);
    m.function("zend_test_nodiscard(): int", ret_one);
    m.function("zend_test_deprecated_nodiscard(): int", ret_one);
    m.function("zend_test_aliased(): void", none);
    m.function("zend_test_deprecated_aliased(): void", none);
    m.function(
        "zend_create_unterminated_string(string $str): string",
        create_unterminated_string,
    );
    m.function("zend_terminate_string(string &$str): void", none);
    m.function("zend_leak_variable(mixed $variable): void", leak_variable);
    m.function("zend_leak_bytes(int $bytes = 3): void", none);
    m.function("zend_delref(mixed $variable): void", none);
    m.function(
        "zend_test_erealloc_block_size(int $old_size, int $new_size): array",
        erealloc_block_size,
    );
    m.function("zend_test_emalloc(int $size): void", crate::stack::emalloc);
    m.function(
        "zend_test_callback(callable $callback): void",
        test_callback,
    );
    m.function(
        "zend_iterable(iterable $arg1, ?iterable $arg2 = null): void",
        none,
    );
    m.function(
        "zend_weakmap_attach(object $object, mixed $value): bool",
        weakmap_attach,
    );
    m.function("zend_weakmap_remove(object $object): bool", weakmap_remove);
    m.function("zend_weakmap_dump(): array", weakmap_dump);
    m.function("zend_get_unit_enum(): ZendTestUnitEnum", get_unit_enum);
    m.function(
        "zend_test_parameter_with_attribute(string $parameter): int",
        ret_one,
    );
    m.function("zend_test_attribute_with_named_argument(): void", none);
    m.function("zend_get_current_func_name(): string", current_func_name);
    m.function(
        "zend_call_method($obj_or_class, string $method, mixed $arg1 = UNKNOWN, mixed $arg2 = UNKNOWN): mixed",
        call_method,
    );
    m.function(
        "zend_object_init_with_constructor($class, mixed ...$args): mixed",
        object_init_with_constructor,
    );
    m.function(
        "zend_call_method_if_exists(object $obj, string $method, mixed ...$args): mixed",
        call_method_if_exists,
    );
    m.function(
        "zend_test_call_with_consumed_args(callable $cb, array $args, int $consumed_args): array",
        call_with_consumed_args,
    );
    m.function("zend_test_refcount(mixed $value): int", refcount);
    m.function(
        "zend_test_zend_ini_parse_quantity(string $str): int",
        ini_parse_quantity,
    );
    m.function(
        "zend_test_zend_ini_parse_uquantity(string $str): int",
        ini_parse_uquantity,
    );
    m.function("zend_test_zend_ini_str(): string", ini_str);
    m.function("zend_test_zstr_init_literal(): string", zstr_init_literal);
    m.function("zend_test_zend_call_stack_get(): ?array", call_stack_get);
    m.function(
        "zend_test_zend_call_stack_use_all(): int",
        call_stack_use_all,
    );
    m.function(
        "zend_test_is_string_marked_as_valid_utf8(string $string): bool",
        is_valid_utf8,
    );
    m.function("zend_get_map_ptr_last(): int", map_ptr_last);
    m.function("zend_test_crash(?string $message = null): void", crash);
    m.function(
        "zend_test_raise_and_throw(int $signal): void",
        raise_and_throw,
    );
    m.function(
        "zend_test_fill_packed_array(array &$array): void",
        fill_packed_array,
    );
    m.function(
        "zend_test_create_throwing_resource()",
        create_throwing_resource,
    );
    m.function("get_open_basedir(): ?string", open_basedir);
    m.function("zend_test_override_libxml_global_state(): void", none);
    m.function("zend_test_is_pcre_bundled(): bool", ret_false);
    m.function("zend_test_cast_fread($stream): void", cast_fread);
    m.function("zend_test_is_zend_ptr(int $addr): bool", ret_false);
    m.function("zend_test_log_err_debug(string $str): void", log_err_debug);
    m.function(
        "zend_test_compile_to_ast(string $str): string",
        compile_to_ast,
    );
    m.function("zend_test_gh18756(): void", none);
    m.function("zend_test_opcache_preloading(): bool", ret_false);
    m.function("zend_test_gh19792(): void", gh19792);
    m.function("zend_new_singleton_array(): array", singleton_array);
    m.function(
        "zend_test_set_local_var(string $name, mixed $value): bool",
        set_local_var,
    );
    m.function("ZendTestNS2\\namespaced_func(): bool", ret_true);
    m.function("ZendTestNS2\\namespaced_deprecated_func(): void", none);
    m.function("ZendTestNS2\\namespaced_aliased_func(): void", none);
    m.function(
        "ZendTestNS2\\namespaced_deprecated_aliased_func(): void",
        none,
    );
    m.function("ZendTestNS2\\ZendSubNS\\namespaced_func(): bool", ret_true);
    m.function(
        "ZendTestNS2\\ZendSubNS\\namespaced_deprecated_func(): void",
        none,
    );
    m.function(
        "ZendTestNS2\\ZendSubNS\\namespaced_aliased_func(): void",
        none,
    );
    m.function(
        "ZendTestNS2\\ZendSubNS\\namespaced_deprecated_aliased_func(): void",
        none,
    );
    m.function(
        "zend_test_internal_dnf_arguments(Iterator|(Traversable&Countable) $arg): Iterator|(Traversable&Countable)",
        internal_dnf_arguments,
    );
    m.function(
        "zend_iterable_legacy(iterable $arg1, ?iterable $arg2 = null): iterable",
        iterable_legacy,
    );

    // Constants.
    m.constant_deprecated("ZEND_TEST_DEPRECATED", Value::Int(42), "", "");
    m.constant("ZEND_CONSTANT_A", s("global"));
    m.constant_deprecated(
        "ZEND_TEST_ATTRIBUTED_CONSTANT",
        Value::Int(42),
        "version 1.5",
        "use something else",
    );
    m.constant("ZendTestNS2\\ZEND_CONSTANT_A", s("namespaced"));
    m.constant("ZendTestNS2\\ZendSubNS\\ZEND_CONSTANT_A", s("namespaced"));

    let vm = m.vm();
    deprecate_function(vm, "zend_test_deprecated", "", "");
    deprecate_function(vm, "zend_test_deprecated_attr", "", "custom message");
    deprecate_function(vm, "zend_test_deprecated_nodiscard", "", "custom message");
    deprecate_function(vm, "zend_test_deprecated_aliased", "", "");
    deprecate_function(vm, "ZendTestNS2\\namespaced_deprecated_func", "", "");
    deprecate_function(
        vm,
        "ZendTestNS2\\namespaced_deprecated_aliased_func",
        "",
        "",
    );
    deprecate_function(
        vm,
        "ZendTestNS2\\ZendSubNS\\namespaced_deprecated_func",
        "",
        "",
    );
    deprecate_function(
        vm,
        "ZendTestNS2\\ZendSubNS\\namespaced_deprecated_aliased_func",
        "",
        "",
    );
    for f in ["zend_test_nodiscard", "zend_test_deprecated_nodiscard"] {
        edit_function(vm, f, |b| b.flags |= flags::NODISCARD);
    }
    let dnf = crate::classes::type_of(vm, "Iterator|(Traversable&Countable)");
    edit_function(vm, "zend_test_internal_dnf_arguments", |b| {
        if let Some(p) = b.params.first_mut() {
            p.ty.clone_from(&dnf);
        }
        b.return_type = dnf;
    });
    edit_function(vm, "zend_test_parameter_with_attribute", |b| {
        if let Some(p) = b.params.first_mut() {
            p.attributes = vec![attribute(
                "ZendTestParameterAttribute",
                vec![(None, s("value1"))],
            )];
        }
    });
}
