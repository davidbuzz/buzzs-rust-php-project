// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `ZipArchive` class: method handlers over [`php_zip::ZipArchive`]
//! and the object handlers (virtual read-only properties, `var_dump()`
//! shape, no cloning, closing on destruction).

use std::rc::Rc;

use php_ext_streams::res;
use php_runtime::{ArrayKey, Obj, PropertyPurpose, RtResult, Value, ZArray, ZStr};
use php_streams::stream::Stream;
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::func::flags;
use php_vm::object;
use php_vm::Vm;
use php_zip::consts::{self, ConstValue};
use php_zip::{EntryStat, ExtractFiles, OpenResult, ZipArchive, ZipHost};

use crate::host;

/// The native state attached to every `ZipArchive` instance.
type State = Rc<ZipArchive>;

/// The state of `$this` (created on demand should the `create` hook not
/// have run).
fn state_of(o: &Obj) -> State {
    if let Some(z) = object::internal::<State, _>(o, |z| Rc::clone(z)) {
        return z;
    }
    let z: State = Rc::new(ZipArchive::new());
    object::set_internal(o, Rc::clone(&z));
    z
}

/// Runs a library method on `$this`.
fn with_zip<R>(
    vm: &mut Vm,
    a: &Args,
    f: impl FnOnce(&ZipArchive, &mut dyn ZipHost) -> RtResult<R>,
) -> VmResult<R> {
    let z = state_of(&a.this_obj());
    host::run(vm, |h| f(&z, h))
}

/// The display name of `$this`'s class (`ZSTR_VAL(object->ce->name)`).
fn class_name(o: &Obj) -> String {
    object::display_name(&object::class_of(o).name).to_string_lossy()
}

fn bytes(b: impl AsRef<[u8]>) -> Value {
    Value::Str(ZStr::from(b.as_ref()))
}

fn bool_v(b: bool) -> Value {
    Value::Bool(b)
}

fn opt_bytes(b: Option<Vec<u8>>) -> Value {
    b.map_or(Value::Bool(false), bytes)
}

fn opt_int(i: Option<i64>) -> Value {
    i.map_or(Value::Bool(false), Value::Int)
}

fn opt_stat(s: Option<EntryStat>) -> Value {
    s.map_or(Value::Bool(false), |s| Value::Array(s.to_array()))
}

fn opt_list(l: Option<Vec<Vec<u8>>>) -> Value {
    l.map_or(Value::Bool(false), |l| {
        let mut arr = ZArray::with_capacity(l.len());
        for s in l {
            let _ = arr.make_mut().push(bytes(s));
        }
        Value::Array(arr)
    })
}

fn opt_stream(s: Option<Stream>) -> Value {
    s.map_or(Value::Bool(false), |s| res::new_stream(s, None))
}

fn open_v(r: OpenResult) -> Value {
    match r {
        OpenResult::Opened => Value::Bool(true),
        OpenResult::False => Value::Bool(false),
        OpenResult::Error(e) => Value::Int(e),
    }
}

/// An `int` argument whose stub default is an expression.
fn int_or(a: &Args, n: usize, default: i64) -> i64 {
    if a.has(n) {
        a.int(n)
    } else {
        default
    }
}

/// A `string` argument as bytes.
fn s(a: &Args, n: usize) -> Vec<u8> {
    a.str(n).as_bytes().to_vec()
}

/// An optional `string` argument with a literal default.
fn s_or(a: &Args, n: usize, default: &[u8]) -> Vec<u8> {
    if a.has(n) {
        s(a, n)
    } else {
        default.to_vec()
    }
}

/// An optional `array` argument.
fn arr_opt(a: &Args, n: usize) -> Option<ZArray> {
    match a.at(n) {
        Value::Array(arr) => Some(arr),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

fn open(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, fl) = (s(a, 0), a.int(1));
    with_zip(vm, a, |z, h| z.open(h, &f, fl)).map(open_v)
}

fn open_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (d, fl) = (s_or(a, 0, b""), a.int(1));
    with_zip(vm, a, |z, h| z.open_string(h, &d, fl)).map(open_v)
}

fn set_password(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = s(a, 0);
    with_zip(vm, a, |z, _| z.set_password(&p)).map(bool_v)
}

fn close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    with_zip(vm, a, |z, h| z.close(h)).map(bool_v)
}

fn close_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    with_zip(vm, a, |z, h| z.close_string(h)).map(opt_bytes)
}

fn count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    with_zip(vm, a, |z, _| z.count()).map(Value::Int)
}

fn serialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let e = ZipArchive::serialize(&class_name(&a.this_obj()));
    Err(host::rt_throw(vm, e))
}

fn unserialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let e = ZipArchive::unserialize(&class_name(&a.this_obj()));
    Err(host::rt_throw(vm, e))
}

fn get_status_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    with_zip(vm, a, |z, _| Ok(z.get_status_string())).map(bytes)
}

fn clear_error(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    with_zip(vm, a, |z, _| {
        z.clear_error();
        Ok(Value::Null)
    })
}

fn add_empty_dir(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (d, fl) = (s(a, 0), a.int(1));
    with_zip(vm, a, |z, _| z.add_empty_dir(&d, fl)).map(bool_v)
}

fn add_from_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, c, fl) = (s(a, 0), s(a, 1), int_or(a, 2, consts::FL_OVERWRITE));
    with_zip(vm, a, |z, _| z.add_from_string(&n, &c, fl)).map(bool_v)
}

fn add_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (p, e) = (s(a, 0), s_or(a, 1, b""));
    let (st, len, fl) = (
        a.int(2),
        int_or(a, 3, consts::LENGTH_TO_END),
        int_or(a, 4, consts::FL_OVERWRITE),
    );
    with_zip(vm, a, |z, _| z.add_file(&p, &e, st, len, fl)).map(bool_v)
}

fn replace_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (p, idx, st) = (s(a, 0), a.int(1), a.int(2));
    let (len, fl) = (int_or(a, 3, consts::LENGTH_TO_END), a.int(4));
    with_zip(vm, a, |z, _| z.replace_file(&p, idx, st, len, fl)).map(bool_v)
}

fn add_glob(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (p, fl, o) = (s(a, 0), a.int(1), arr_opt(a, 2));
    with_zip(vm, a, |z, h| z.add_glob(h, &p, fl, o.as_ref())).map(opt_list)
}

fn add_pattern(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (p, path, o) = (s(a, 0), s_or(a, 1, b"."), arr_opt(a, 2));
    with_zip(vm, a, |z, h| z.add_pattern(h, &p, &path, o.as_ref())).map(opt_list)
}

fn rename_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (i, n) = (a.int(0), s(a, 1));
    with_zip(vm, a, |z, _| z.rename_index(i, &n)).map(bool_v)
}

fn rename_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (o, n) = (s(a, 0), s(a, 1));
    with_zip(vm, a, |z, _| z.rename_name(&o, &n)).map(bool_v)
}

fn set_archive_comment(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let c = s(a, 0);
    with_zip(vm, a, |z, _| z.set_archive_comment(&c)).map(bool_v)
}

fn get_archive_comment(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let fl = a.int(0);
    with_zip(vm, a, |z, _| z.get_archive_comment(fl)).map(opt_bytes)
}

fn set_archive_flag(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, v) = (a.int(0), a.int(1));
    with_zip(vm, a, |z, _| z.set_archive_flag(f, v)).map(bool_v)
}

fn get_archive_flag(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (f, fl) = (a.int(0), a.int(1));
    with_zip(vm, a, |z, _| z.get_archive_flag(f, fl)).map(Value::Int)
}

fn set_comment_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (i, c) = (a.int(0), s(a, 1));
    with_zip(vm, a, |z, _| z.set_comment_index(i, &c)).map(bool_v)
}

fn set_comment_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, c) = (s(a, 0), s(a, 1));
    with_zip(vm, a, |z, _| z.set_comment_name(&n, &c)).map(bool_v)
}

fn set_mtime_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (i, t, fl) = (a.int(0), a.int(1), a.int(2));
    with_zip(vm, a, |z, _| z.set_mtime_index(i, t, fl)).map(bool_v)
}

fn set_mtime_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, t, fl) = (s(a, 0), a.int(1), a.int(2));
    with_zip(vm, a, |z, _| z.set_mtime_name(&n, t, fl)).map(bool_v)
}

fn get_comment_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (i, fl) = (a.int(0), a.int(1));
    with_zip(vm, a, |z, _| z.get_comment_index(i, fl)).map(opt_bytes)
}

fn get_comment_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, fl) = (s(a, 0), a.int(1));
    with_zip(vm, a, |z, _| z.get_comment_name(&n, fl)).map(opt_bytes)
}

fn delete_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let i = a.int(0);
    with_zip(vm, a, |z, _| z.delete_index(i)).map(bool_v)
}

fn delete_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = s(a, 0);
    with_zip(vm, a, |z, _| z.delete_name(&n)).map(bool_v)
}

fn stat_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, fl) = (s(a, 0), a.int(1));
    with_zip(vm, a, |z, _| z.stat_name(&n, fl)).map(opt_stat)
}

fn stat_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (i, fl) = (a.int(0), a.int(1));
    with_zip(vm, a, |z, _| z.stat_index(i, fl)).map(opt_stat)
}

fn locate_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, fl) = (s(a, 0), a.int(1));
    with_zip(vm, a, |z, _| z.locate_name(&n, fl)).map(opt_int)
}

fn get_name_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (i, fl) = (a.int(0), a.int(1));
    with_zip(vm, a, |z, _| z.get_name_index(i, fl)).map(opt_bytes)
}

fn unchange_archive(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    with_zip(vm, a, |z, _| z.unchange_archive()).map(bool_v)
}

fn unchange_all(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    with_zip(vm, a, |z, _| z.unchange_all()).map(bool_v)
}

fn unchange_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let i = a.int(0);
    with_zip(vm, a, |z, _| z.unchange_index(i)).map(bool_v)
}

fn unchange_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = s(a, 0);
    with_zip(vm, a, |z, _| z.unchange_name(&n)).map(bool_v)
}

fn extract_to(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let p = s(a, 0);
    let files = a.at(1);
    with_zip(vm, a, |z, _| {
        let sel = match &files {
            Value::Str(f) => ExtractFiles::One(f.as_bytes()),
            Value::Array(arr) => ExtractFiles::Many(arr),
            _ => ExtractFiles::All,
        };
        z.extract_to(&p, sel)
    })
    .map(bool_v)
}

fn get_from_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, len, fl) = (s(a, 0), a.int(1), a.int(2));
    with_zip(vm, a, |z, _| z.get_from_name(&n, len, fl)).map(opt_bytes)
}

fn get_from_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (i, len, fl) = (a.int(0), a.int(1), a.int(2));
    with_zip(vm, a, |z, _| z.get_from_index(i, len, fl)).map(opt_bytes)
}

fn get_stream_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (i, fl) = (a.int(0), a.int(1));
    with_zip(vm, a, |z, _| z.get_stream_index(i, fl)).map(opt_stream)
}

fn get_stream_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, fl) = (s(a, 0), a.int(1));
    with_zip(vm, a, |z, _| z.get_stream_name(&n, fl)).map(opt_stream)
}

fn get_stream(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = s(a, 0);
    with_zip(vm, a, |z, _| z.get_stream(&n)).map(opt_stream)
}

fn set_external_attributes_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, os, at, fl) = (s(a, 0), a.int(1), a.int(2), a.int(3));
    with_zip(vm, a, |z, _| z.set_external_attributes_name(&n, os, at, fl)).map(bool_v)
}

fn set_external_attributes_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (i, os, at, fl) = (a.int(0), a.int(1), a.int(2), a.int(3));
    with_zip(vm, a, |z, _| z.set_external_attributes_index(i, os, at, fl)).map(bool_v)
}

/// Assigns the by-reference `$opsys` / `$attr` on success.
fn external_attributes_result(a: &Args, r: Option<(i64, i64)>) -> Value {
    match r {
        Some((opsys, attr)) => {
            a.set_ref(1, Value::Int(opsys));
            a.set_ref(2, Value::Int(attr));
            Value::Bool(true)
        }
        None => Value::Bool(false),
    }
}

fn get_external_attributes_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, fl) = (s(a, 0), a.int(3));
    let r = with_zip(vm, a, |z, _| z.get_external_attributes_name(&n, fl))?;
    Ok(external_attributes_result(a, r))
}

fn get_external_attributes_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (i, fl) = (a.int(0), a.int(3));
    let r = with_zip(vm, a, |z, _| z.get_external_attributes_index(i, fl))?;
    Ok(external_attributes_result(a, r))
}

fn set_compression_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, m, f) = (s(a, 0), a.int(1), a.int(2));
    with_zip(vm, a, |z, _| z.set_compression_name(&n, m, f)).map(bool_v)
}

fn set_compression_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (i, m, f) = (a.int(0), a.int(1), a.int(2));
    with_zip(vm, a, |z, _| z.set_compression_index(i, m, f)).map(bool_v)
}

fn password_arg(a: &Args, n: usize) -> Option<Vec<u8>> {
    a.opt(n).map(|_| s(a, n))
}

fn set_encryption_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, m, p) = (s(a, 0), a.int(1), password_arg(a, 2));
    with_zip(vm, a, |z, _| z.set_encryption_name(&n, m, p.as_deref())).map(bool_v)
}

fn set_encryption_index(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (i, m, p) = (a.int(0), a.int(1), password_arg(a, 2));
    with_zip(vm, a, |z, _| z.set_encryption_index(i, m, p.as_deref())).map(bool_v)
}

fn register_progress_callback(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (rate, cb) = (a.float(0), a.at(1));
    with_zip(vm, a, |z, _| z.register_progress_callback(rate, cb)).map(bool_v)
}

fn register_cancel_callback(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cb = a.at(0);
    with_zip(vm, a, |z, _| z.register_cancel_callback(cb)).map(bool_v)
}

fn is_compression_method_supported(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (m, enc) = (a.int(0), if a.has(1) { a.bool(1) } else { true });
    host::run(vm, |_| ZipArchive::is_compression_method_supported(m, enc)).map(bool_v)
}

fn is_encryption_method_supported(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (m, enc) = (a.int(0), if a.has(1) { a.bool(1) } else { true });
    host::run(vm, |_| ZipArchive::is_encryption_method_supported(m, enc)).map(bool_v)
}

/// `dtor_obj` (`php_zip_object_dtor()`): closes the archive, running the
/// callbacks, when the object is released or at shutdown.
fn destruct(vm: &mut Vm, o: &Obj) -> VmResult<()> {
    let z = state_of(o);
    if vm.is_executing() {
        host::run(vm, |h| z.destroy(h))
    } else {
        host::run_frameless(vm, |h| z.destroy(h))
    }
}

// ---------------------------------------------------------------------------
// Object handlers (signatures fixed by `ClassHooks`)
// ---------------------------------------------------------------------------

#[allow(clippy::unnecessary_wraps)]
fn create(vm: &mut Vm, o: &Obj) -> VmResult<()> {
    let z = ZipArchive::new();
    // `get_object_vars()` and `foreach` walk the standard property table
    // (not `properties_for`), so the declared slots are initialised to make
    // them list the properties; values are always read through
    // `read_property`.
    for (name, v) in z.properties() {
        vm.set_property_raw(o, name, v);
    }
    object::set_internal::<State>(o, Rc::new(z));
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn read_property(_vm: &mut Vm, o: &Obj, name: &ZStr) -> VmResult<Option<Value>> {
    if !ZipArchive::is_virtual_property(name.as_bytes()) {
        return Ok(None);
    }
    Ok(state_of(o).read_property(name.as_bytes()))
}

fn write_property(vm: &mut Vm, o: &Obj, name: &ZStr, _v: &Value) -> VmResult<bool> {
    if !ZipArchive::is_virtual_property(name.as_bytes()) {
        return Ok(false);
    }
    let e = php_zip::error::readonly_property(&class_name(o), &name.to_string_lossy());
    Err(host::rt_throw(vm, e))
}

#[allow(clippy::unnecessary_wraps)]
fn has_property(_vm: &mut Vm, o: &Obj, name: &ZStr, check_empty: bool) -> VmResult<Option<bool>> {
    // Same convention as `php_zip_has_property()`: "set" or, for
    // `empty()`, "set and not empty".
    Ok(state_of(o).has_property(name.as_bytes(), check_empty))
}

fn mangle(pi: &php_vm::class::PropInfo) -> ZStr {
    match pi.visibility() {
        flags::PRIVATE => {
            let mut v = vec![0u8];
            v.extend_from_slice(pi.class.as_bytes());
            v.push(0);
            v.extend_from_slice(pi.name.as_bytes());
            ZStr::from(v)
        }
        flags::PROTECTED => {
            let mut v = b"\0*\0".to_vec();
            v.extend_from_slice(pi.name.as_bytes());
            ZStr::from(v)
        }
        _ => pi.name.clone(),
    }
}

#[allow(clippy::unnecessary_wraps)]
/// `php_zip_get_properties()`: the standard table with the virtual
/// properties' current values.
fn properties_for(_vm: &mut Vm, o: &Obj, purpose: PropertyPurpose) -> VmResult<Option<ZArray>> {
    // php-src: only DatePeriod answers ZEND_PROP_PURPOSE_GET_OBJECT_VARS;
    // everything else falls back to `get_properties`.
    if purpose == PropertyPurpose::GetObjectVars {
        return Ok(None);
    }
    let d = object::data(o);
    if purpose == PropertyPurpose::Debug && d.class.magic.debug_info.is_some() {
        return Ok(None);
    }
    let z = state_of(o);
    let mut out = ZArray::new();
    {
        let t = out.make_mut();
        let slots = d.slots.borrow();
        for pi in &d.class.props {
            if pi.is_virtual() || pi.flags & flags::STATIC != 0 {
                continue;
            }
            let key = mangle(pi);
            if let Some(v) = z.read_property(key.as_bytes()) {
                t.insert(ArrayKey::Str(key), v);
            } else if let Some(Some(v)) = slots.get(pi.slot as usize) {
                t.insert(ArrayKey::Str(key), v.deref());
            }
        }
        if let Some(dynp) = d.dynamic.borrow().as_deref() {
            for (k, v) in dynp {
                t.insert(k.clone(), v.deref());
            }
        }
    }
    Ok(Some(out))
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

type Handler = fn(&mut Vm, &mut Args) -> VmResult<Value>;

/// The methods in stub order.
const METHODS: &[(&str, Handler)] = &[
    ("open(string $filename, int $flags = 0): bool|int", open),
    ("openString(string $data = '', int $flags = 0): bool|int", open_string),
    ("setPassword(#[\\SensitiveParameter] string $password): bool", set_password),
    ("close(): bool", close),
    ("closeString(): string|false", close_string),
    ("count(): int", count),
    ("__serialize(): array", serialize),
    ("__unserialize(array $data): void", unserialize),
    ("getStatusString(): string", get_status_string),
    ("clearError(): void", clear_error),
    ("addEmptyDir(string $dirname, int $flags = 0): bool", add_empty_dir),
    (
        "addFromString(string $name, string $content, int $flags = ZipArchive::FL_OVERWRITE): bool",
        add_from_string,
    ),
    (
        "addFile(string $filepath, string $entryname = \"\", int $start = 0, int $length = ZipArchive::LENGTH_TO_END, int $flags = ZipArchive::FL_OVERWRITE): bool",
        add_file,
    ),
    (
        "replaceFile(string $filepath, int $index, int $start = 0, int $length = ZipArchive::LENGTH_TO_END, int $flags = 0): bool",
        replace_file,
    ),
    (
        "addGlob(string $pattern, int $flags = 0, array $options = []): array|false",
        add_glob,
    ),
    (
        "addPattern(string $pattern, string $path = \".\", array $options = []): array|false",
        add_pattern,
    ),
    ("renameIndex(int $index, string $new_name): bool", rename_index),
    ("renameName(string $name, string $new_name): bool", rename_name),
    ("setArchiveComment(string $comment): bool", set_archive_comment),
    ("getArchiveComment(int $flags = 0): string|false", get_archive_comment),
    ("setArchiveFlag(int $flag, int $value): bool", set_archive_flag),
    ("getArchiveFlag(int $flag, int $flags = 0): int", get_archive_flag),
    ("setCommentIndex(int $index, string $comment): bool", set_comment_index),
    ("setCommentName(string $name, string $comment): bool", set_comment_name),
    (
        "setMtimeIndex(int $index, int $timestamp, int $flags = 0): bool",
        set_mtime_index,
    ),
    (
        "setMtimeName(string $name, int $timestamp, int $flags = 0): bool",
        set_mtime_name,
    ),
    (
        "getCommentIndex(int $index, int $flags = 0): string|false",
        get_comment_index,
    ),
    (
        "getCommentName(string $name, int $flags = 0): string|false",
        get_comment_name,
    ),
    ("deleteIndex(int $index): bool", delete_index),
    ("deleteName(string $name): bool", delete_name),
    ("statName(string $name, int $flags = 0): array|false", stat_name),
    ("statIndex(int $index, int $flags = 0): array|false", stat_index),
    ("locateName(string $name, int $flags = 0): int|false", locate_name),
    ("getNameIndex(int $index, int $flags = 0): string|false", get_name_index),
    ("unchangeArchive(): bool", unchange_archive),
    ("unchangeAll(): bool", unchange_all),
    ("unchangeIndex(int $index): bool", unchange_index),
    ("unchangeName(string $name): bool", unchange_name),
    (
        "extractTo(string $pathto, array|string|null $files = null): bool",
        extract_to,
    ),
    (
        "getFromName(string $name, int $len = 0, int $flags = 0): string|false",
        get_from_name,
    ),
    (
        "getFromIndex(int $index, int $len = 0, int $flags = 0): string|false",
        get_from_index,
    ),
    ("getStreamIndex(int $index, int $flags = 0)", get_stream_index),
    ("getStreamName(string $name, int $flags = 0)", get_stream_name),
    ("getStream(string $name)", get_stream),
    (
        "setExternalAttributesName(string $name, int $opsys, int $attr, int $flags = 0): bool",
        set_external_attributes_name,
    ),
    (
        "setExternalAttributesIndex(int $index, int $opsys, int $attr, int $flags = 0): bool",
        set_external_attributes_index,
    ),
    (
        "getExternalAttributesName(string $name, &$opsys, &$attr, int $flags = 0): bool",
        get_external_attributes_name,
    ),
    (
        "getExternalAttributesIndex(int $index, &$opsys, &$attr, int $flags = 0): bool",
        get_external_attributes_index,
    ),
    (
        "setCompressionName(string $name, int $method, int $compflags = 0): bool",
        set_compression_name,
    ),
    (
        "setCompressionIndex(int $index, int $method, int $compflags = 0): bool",
        set_compression_index,
    ),
    (
        "setEncryptionName(string $name, int $method, #[\\SensitiveParameter] ?string $password = null): bool",
        set_encryption_name,
    ),
    (
        "setEncryptionIndex(int $index, int $method, #[\\SensitiveParameter] ?string $password = null): bool",
        set_encryption_index,
    ),
    (
        "registerProgressCallback(float $rate, callable $callback): bool",
        register_progress_callback,
    ),
    (
        "registerCancelCallback(callable $callback): bool",
        register_cancel_callback,
    ),
];

/// The static methods.
const STATIC_METHODS: &[(&str, Handler)] = &[
    (
        "isCompressionMethodSupported(int $method, bool $enc = true): bool",
        is_compression_method_supported,
    ),
    (
        "isEncryptionMethodSupported(int $method, bool $enc = true): bool",
        is_encryption_method_supported,
    ),
];

/// Registers `ZipArchive`.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    // Internal classes allow dynamic properties unless `@strict-properties`;
    // `clone_obj` is NULL.
    let mut cb = ClassBuilder::new("ZipArchive")
        .flags(class_flags::ALLOW_DYNAMIC | class_flags::NO_CLONE)
        .implements(vm, &["Countable"]);
    for c in consts::class_constants() {
        let v = match c.value {
            ConstValue::Int(i) => Value::Int(i),
            ConstValue::Str(s) => bytes(s),
        };
        cb = match c.deprecated_since {
            Some(since) => cb.deprecated_constant(c.name, v, since, ""),
            None => cb.constant(c.name, v),
        };
    }
    for (name, ty) in php_zip::class::PROPERTIES {
        cb = cb.prop(name, acc::PUBLIC, Some(*ty), None);
    }
    for (sig, h) in METHODS {
        cb = cb.method(sig, acc::PUBLIC, *h);
    }
    for (sig, h) in STATIC_METHODS {
        cb = cb.method(sig, acc::PUBLIC | acc::STATIC, *h);
    }
    cb.hooks(ClassHooks {
        create: Some(create),
        destruct: Some(destruct),
        read_property: Some(read_property),
        write_property: Some(write_property),
        has_property: Some(has_property),
        properties_for: Some(properties_for),
        ..Default::default()
    })
    .register(vm);
}
