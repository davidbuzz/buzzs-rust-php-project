// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The plain functions of `test.stub.php` (`ext_functions[]`).

use crate::patch::{deprecate_function, edit_function};
use crate::state;
use crate::util::{attribute, s, value_name, E_ERROR, E_WARNING};
use php_runtime::{Value, ZArray, ZStr};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::func::flags;
use php_vm::vm::FrameKind;
use php_vm::Vm;
use std::rc::Rc;

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

/// `zend_test_opcache_preloading()`: `opcache_preloading()`, whether the
/// opcache preload script is running.
fn opcache_preloading(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(Vm::is_preloading()))
}

fn ret_one(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(1))
}

/// `zend_trigger_bailout()`: `zend_error(E_ERROR, "Bailout")`.
fn trigger_bailout(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    vm.raise(E_ERROR, "Bailout")?;
    Ok(Value::Null)
}

fn serialization_bailout(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Err(vm.serialization_bailout(a.bool(0), a.bool(1)))
}

fn serialization_state(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(vm.serialization_state()))
}

/// `zend_return_non_interned_string()`: a fresh copy of the string
/// (`RETURN_STRINGL`), never an interned one, so a callback's return value
/// is a refcounted string in the leak tests that use it.
fn return_non_interned_string(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Str(ZStr::from(a.str(0).as_bytes().to_vec())))
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
    // Small bins, then large runs and huge blocks: whole pages.
    MM_BINS
        .iter()
        .find(|&&b| b >= size)
        .copied()
        .unwrap_or_else(|| size.div_ceil(MM_PAGE) * MM_PAGE)
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
            // Without an object, even an instance method runs (without
            // `$this`) with the class as the called scope.
            if let Some(r) = vm.call_class_method(&c, &method.to_string_lossy(), args.clone()) {
                return r;
            }
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
    let o = vm.new_object(&cls)?;
    // `get_constructor` handlers refuse direct construction.
    if let Some(h) = cls.hooks.as_ref().and_then(|h| h.get_constructor) {
        if let Err(e) = h(vm, &o) {
            ctor_failed(&o);
            return Err(e);
        }
    }
    let Some(ctor) = cls.magic.construct.clone() else {
        // No constructor: positional arguments are ignored, named ones are
        // unknown parameters.
        if let Some((n, _)) = named.first() {
            ctor_failed(&o);
            return Err(vm.throw(
                "Error",
                &format!("Unknown named parameter ${}", n.to_string_lossy()),
            ));
        }
        return Ok(Value::Object(o));
    };
    if ctor.visibility() != php_vm::func::flags::PUBLIC {
        ctor_failed(&o);
        let vis = if ctor.visibility() == php_vm::func::flags::PRIVATE {
            "private"
        } else {
            "protected"
        };
        return Err(vm.throw(
            "Error",
            &format!(
                "Call to {vis} {}::__construct() from global scope",
                cls.name.to_string_lossy()
            ),
        ));
    }
    if let Err(e) = vm.call_method_named(&o, "__construct", args, named) {
        ctor_failed(&o);
        return Err(e);
    }
    Ok(Value::Object(o))
}

/// `zend_object_store_ctor_failed()`: the object is released without
/// running its destructor.
fn ctor_failed(o: &php_runtime::Obj) {
    let d = php_vm::object::data(o);
    d.flags
        .set(d.flags.get() | php_vm::object::oflags::DESTRUCTED);
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
    // `zend_call_function` moves a consumed argument into the callee
    // unless it is a reference or the parameter is by-reference (a value
    // sent there is wrapped in a new reference, `must_wrap`); the bits of
    // the moved ones come back.
    let mut actual = 0_i64;
    for (i, v) in list.iter().enumerate().take(32) {
        if consumed & (1_i64 << i) != 0
            && !matches!(v, Value::Reference(_))
            && !vm.callable_arg_by_ref(&cb, i)?
        {
            actual |= 1_i64 << i;
        }
    }
    let r = vm.call_value(&cb, list)?;
    Ok(Value::Array(ZArray::from_pairs([
        ("consumed_args", Value::Int(actual)),
        ("retval", r),
    ])))
}

/// `zend_test_refcount(mixed $value)`: values are not reference counted
/// the same way here; arrays and objects report their handle count. A
/// string counts like `debug_zval_dump()` does, without this call's own
/// copies (the frame's argument copy and `a.at(0)`'s clone).
fn refcount(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(match a.at(0) {
        Value::Array(arr) => Value::Int(i64::try_from(arr.refcount()).unwrap_or(1)),
        Value::Object(o) => Value::Int(i64::try_from(o.refcount()).unwrap_or(1)),
        Value::Str(st) if !st.is_empty() && !st.is_interned() => {
            Value::Int(i64::try_from(st.refcount().saturating_sub(2).max(1)).unwrap_or(1))
        }
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
/// carry no UTF-8 flag here; report validity. A compile-time string that
/// interning replaces by an equal permanent string (a class, property,
/// method or function name registered by an extension, which carries no
/// flag) is unmarked, as in php-src.
fn is_valid_utf8(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    let b = s.as_bytes();
    if b.len() > 1 && s.is_interned() && is_permanent_name(vm, b) {
        return Ok(Value::Bool(false));
    }
    Ok(Value::Bool(std::str::from_utf8(b).is_ok()))
}

/// Whether `name` is a permanent interned string: a name an extension
/// registered (single characters are known strings with the flag).
fn is_permanent_name(vm: &Vm, name: &[u8]) -> bool {
    use php_vm::class::cflags;
    let internal_fn = matches!(
        vm.functions.get(&ZStr::from(name)),
        Some(php_vm::func::Func::Builtin(_))
    );
    internal_fn
        || vm.classes.values().any(|c| {
            c.flags & cflags::INTERNAL != 0
                && (c.lc_name.as_bytes() == name
                    || c.methods.contains_key(name)
                    || c.props.iter().any(|p| p.name.as_bytes() == name))
        })
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

thread_local! {
    /// The hidden class standing in for `le_throwing_resource_dtor`.
    static THROWING_DTOR: std::cell::RefCell<Option<Rc<php_vm::class::Class>>> =
        const { std::cell::RefCell::new(None) };
}

/// `le_throwing_resource_dtor`: the native destructor of the hidden object
/// a throwing resource owns. Releasing the resource releases the object,
/// so the exception is thrown where the resource is released, the way the
/// VM runs object destructors.
fn throwing_resource_dtor(vm: &mut Vm, _o: &php_runtime::Obj) -> VmResult<()> {
    Err(vm.throw("Exception", "Throwing resource destructor called"))
}

/// Registers the hidden destructor class (outside the class table).
fn register_throwing_resource(vm: &mut Vm) {
    let cls = php_vm::ext::ClassBuilder::new("ZendTestThrowingResourceDtor")
        .flags(php_vm::ext::class_flags::FINAL)
        .hooks(php_vm::ext::ClassHooks {
            destruct: Some(throwing_resource_dtor),
            ..Default::default()
        })
        .register(vm);
    vm.classes.remove(&cls.lc_name);
    vm.class_order.retain(|n| *n != cls.lc_name);
    THROWING_DTOR.with(|c| *c.borrow_mut() = Some(cls));
}

/// `zend_test_create_throwing_resource()`.
fn create_throwing_resource(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let cls = THROWING_DTOR.with(|c| c.borrow().clone());
    let data = match cls {
        Some(cls) => Some(Box::new(vm.new_object(&cls)?) as Box<dyn std::any::Any>),
        None => None,
    };
    Ok(Value::Resource(php_runtime::Resource::new(
        "throwing resource",
        data,
    )))
}

/// `get_open_basedir()`: `PG(open_basedir)`, which holds the expanded
/// directories of a runtime `ini_set()` (`OnUpdateBaseDir`), not the INI
/// string.
fn open_basedir(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(match vm.ini_string("open_basedir") {
        Some(v) if !v.is_empty() => {
            Value::Str(ZStr::from(php_ext_streams::ini::effective_open_basedir(v)))
        }
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
    let has_table = vm.frames[idx].kind != FrameKind::Function;
    let Some(vars) = vm.rebuild_symbol_table(idx) else {
        return Ok(Value::Bool(false));
    };
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
    vm.sync_symbol_table_writes(&vars);
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
/// The attributes (`zend_add_function_attribute`,
/// `zend_add_global_constant_attribute`) and exposed doc comments that
/// `test_arginfo.h` attaches to functions and constants.
fn register_stub_metadata(vm: &mut Vm) {
    let message = |m: &str| vec![(Some("message"), s(m))];
    let functions = [
        (
            "zend_test_deprecated_attr",
            vec![attribute("Deprecated", message("custom message"))],
        ),
        (
            "zend_test_nodiscard",
            vec![attribute("NoDiscard", message("custom message"))],
        ),
        (
            "zend_test_deprecated_nodiscard",
            vec![
                attribute("Deprecated", message("custom message")),
                attribute("NoDiscard", message("custom message 2")),
            ],
        ),
        (
            "zend_test_attribute_with_named_argument",
            vec![attribute(
                "ZendTestAttributeWithArguments",
                vec![(Some("arg"), s("foo"))],
            )],
        ),
    ];
    for (name, attrs) in functions {
        vm.builtin_attributes
            .insert(ZStr::from(name), Rc::new(attrs));
    }
    vm.builtin_doc_comments.insert(
        ZStr::from("zend_test_nullable_array_return"),
        ZStr::from(
            "/**\n     * \"Lorem ipsum\"\n     * @see https://www.php.net\n     * @since 8.3\n     */",
        ),
    );
    vm.const_attributes.insert(
        ZStr::from("ZEND_TEST_ATTRIBUTED_CONSTANT"),
        Rc::new(vec![attribute(
            "Deprecated",
            vec![
                (Some("message"), s("use something else")),
                (Some("since"), s("version 1.5")),
            ],
        )]),
    );
}

/// `zend_test_child_init_count()`.
#[allow(clippy::unnecessary_wraps)]
fn child_init_count(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(crate::child_init_count()))
}

pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function_static("zend_test_child_init_count(): int", child_init_count);
    m.function_static("zend_trigger_bailout(): never", trigger_bailout);
    m.function_static(
        "zend_test_serialization_bailout(bool $unserialize, bool $locked): never",
        serialization_bailout,
    );
    m.function_static(
        "zend_test_serialization_state(): array",
        serialization_state,
    );
    m.function_static("zend_test_array_return(): array", array_return);
    m.function_static(
        "zend_return_non_interned_string(string $str): string",
        return_non_interned_string,
    );
    m.function_static("zend_test_nullable_array_return(): ?array", none);
    m.function_static("zend_test_void_return(): void", none);
    m.function_static(
        "&zend_test_forward_reference(mixed &$value): mixed",
        forward_reference,
    );
    m.function_static(
        "zend_test_compile_string(string $source_string, string $filename, int $position): void",
        crate::compile::compile_string,
    );
    m.function_static("zend_test_deprecated(mixed $arg = null): void", none);
    m.function_static("zend_test_deprecated_attr(): void", none);
    m.function_static("zend_test_nodiscard(): int", ret_one);
    m.function_static("zend_test_deprecated_nodiscard(): int", ret_one);
    m.function_static("zend_test_aliased(): void", none);
    m.function_static("zend_test_deprecated_aliased(): void", none);
    m.function_static(
        "zend_create_unterminated_string(string $str): string",
        create_unterminated_string,
    );
    m.function_static("zend_terminate_string(string &$str): void", none);
    m.function_static("zend_leak_variable(mixed $variable): void", leak_variable);
    m.function_static(
        "zend_test_handle_post_data(mixed $stream, string $content_type, int $content_length): void",
        crate::post::handle_post_data,
    );
    m.function_static("zend_leak_bytes(int $bytes = 3): void", none);
    m.function_static("zend_delref(mixed $variable): void", none);
    m.function_static(
        "zend_test_erealloc_block_size(int $old_size, int $new_size): array",
        erealloc_block_size,
    );
    m.function_static("zend_test_emalloc(int $size): void", crate::stack::emalloc);
    m.function_static(
        "zend_test_callback(callable $callback): void",
        test_callback,
    );
    m.function_static(
        "zend_iterable(iterable $arg1, ?iterable $arg2 = null): void",
        none,
    );
    m.function_static(
        "zend_weakmap_attach(object $object, mixed $value): bool",
        weakmap_attach,
    );
    m.function_static("zend_weakmap_remove(object $object): bool", weakmap_remove);
    m.function_static("zend_weakmap_dump(): array", weakmap_dump);
    m.function_static("zend_get_unit_enum(): ZendTestUnitEnum", get_unit_enum);
    m.function_static(
        "zend_test_parameter_with_attribute(string $parameter): int",
        ret_one,
    );
    m.function_static("zend_test_attribute_with_named_argument(): void", none);
    m.function_static("zend_get_current_func_name(): string", current_func_name);
    m.function_static(
        "zend_call_method($obj_or_class, string $method, mixed $arg1 = UNKNOWN, mixed $arg2 = UNKNOWN): mixed",
        call_method,
    );
    m.function_static(
        "zend_object_init_with_constructor($class, mixed ...$args): mixed",
        object_init_with_constructor,
    );
    m.function_static(
        "zend_call_method_if_exists(object $obj, string $method, mixed ...$args): mixed",
        call_method_if_exists,
    );
    m.function_static(
        "zend_test_call_with_consumed_args(callable $cb, array $args, int $consumed_args): array",
        call_with_consumed_args,
    );
    m.function_static("zend_test_refcount(mixed $value): int", refcount);
    m.function_static(
        "zend_test_zend_ini_parse_quantity(string $str): int",
        ini_parse_quantity,
    );
    m.function_static(
        "zend_test_zend_ini_parse_uquantity(string $str): int",
        ini_parse_uquantity,
    );
    m.function_static("zend_test_zend_ini_str(): string", ini_str);
    m.function_static("zend_test_zstr_init_literal(): string", zstr_init_literal);
    m.function_static("zend_test_zend_call_stack_get(): ?array", call_stack_get);
    m.function_static(
        "zend_test_zend_call_stack_use_all(): int",
        call_stack_use_all,
    );
    m.function_static(
        "zend_test_is_string_marked_as_valid_utf8(string $string): bool",
        is_valid_utf8,
    );
    m.function_static("zend_get_map_ptr_last(): int", map_ptr_last);
    m.function_static("zend_test_crash(?string $message = null): void", crash);
    m.function_static(
        "zend_test_raise_and_throw(int $signal): void",
        raise_and_throw,
    );
    m.function_static(
        "zend_test_fill_packed_array(array &$array): void",
        fill_packed_array,
    );
    m.function_static(
        "zend_test_create_throwing_resource()",
        create_throwing_resource,
    );
    m.function_static("get_open_basedir(): ?string", open_basedir);
    m.function_static("zend_test_override_libxml_global_state(): void", none);
    m.function_static("zend_test_is_pcre_bundled(): bool", ret_false);
    m.function_static("zend_test_cast_fread($stream): void", cast_fread);
    m.function_static("zend_test_is_zend_ptr(int $addr): bool", ret_false);
    m.function_static("zend_test_log_err_debug(string $str): void", log_err_debug);
    m.function_static(
        "zend_test_compile_to_ast(string $str): string",
        compile_to_ast,
    );
    m.function_static("zend_test_gh18756(): void", none);
    m.function_static("zend_test_opcache_preloading(): bool", opcache_preloading);
    m.function_static("zend_test_gh19792(): void", gh19792);
    m.function_static("zend_new_singleton_array(): array", singleton_array);
    m.function_static(
        "zend_test_set_local_var(string $name, mixed $value): bool",
        set_local_var,
    );
    m.function_static("ZendTestNS2\\namespaced_func(): bool", ret_true);
    m.function_static("ZendTestNS2\\namespaced_deprecated_func(): void", none);
    m.function_static("ZendTestNS2\\namespaced_aliased_func(): void", none);
    m.function_static(
        "ZendTestNS2\\namespaced_deprecated_aliased_func(): void",
        none,
    );
    m.function_static("ZendTestNS2\\ZendSubNS\\namespaced_func(): bool", ret_true);
    m.function_static(
        "ZendTestNS2\\ZendSubNS\\namespaced_deprecated_func(): void",
        none,
    );
    m.function_static(
        "ZendTestNS2\\ZendSubNS\\namespaced_aliased_func(): void",
        none,
    );
    m.function_static(
        "ZendTestNS2\\ZendSubNS\\namespaced_deprecated_aliased_func(): void",
        none,
    );
    m.function_static(
        "zend_test_internal_dnf_arguments(Iterator|(Traversable&Countable) $arg): Iterator|(Traversable&Countable)",
        internal_dnf_arguments,
    );
    m.function_static(
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
    register_stub_metadata(vm);
    register_throwing_resource(vm);
    let dnf = crate::classes::type_of(vm, "Iterator|(Traversable&Countable)");
    edit_function(vm, "zend_test_internal_dnf_arguments", |b| {
        if let Some(p) = b.params.first_mut() {
            p.ty.clone_from(&dnf);
            // The stub-signature parser ends the parameter list at the
            // DNF group's `)`.
            p.name = ZStr::from("arg");
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
