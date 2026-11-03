//! The deprecated procedural API (`zip_open()`, `zip_read()`,
//! `zip_entry_*()`) over the "Zip Directory" / "Zip Entry" resources.

use std::cell::RefCell;

use php_runtime::{Resource, Value, ZStr};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;
use php_zip::procedural::{
    self as lib, ZipDir, ZipEntry, ZipOpenResult, DIR_RESOURCE_NAME, ENTRY_RESOURCE_NAME,
};

use crate::host;

/// The payload of a "Zip Entry" resource (`zip_entry_close()` mutates it).
type EntryCell = RefCell<ZipEntry>;

/// The function name for resource error messages.
fn fname(a: &Args) -> String {
    php_ext_streams::res::fname(a)
}

/// `zend_fetch_resource()` failure.
fn invalid(vm: &mut Vm, a: &Args, kind: &str) -> php_vm::Throw {
    let msg = format!(
        "{}(): supplied resource is not a valid {kind} resource",
        fname(a)
    );
    vm.throw("TypeError", &msg)
}

/// The resource of argument `n`; untyped parameters accept anything, so
/// the type check happens here (`Z_PARAM_RESOURCE`).
fn resource_arg(vm: &mut Vm, a: &Args, n: usize) -> VmResult<Resource> {
    php_ext_streams::res::resource_arg(vm, a, n)
}

/// Runs `f` with the open directory of argument `n`.
fn with_dir<R>(vm: &mut Vm, a: &Args, n: usize, f: impl FnOnce(&ZipDir) -> R) -> VmResult<R> {
    let r = resource_arg(vm, a, n)?;
    let out = {
        let data = r.data();
        let dir = data
            .as_ref()
            .and_then(|d| d.downcast_ref::<ZipDir>())
            .filter(|d| !d.is_closed() && !r.is_closed());
        dir.map(f)
    };
    out.ok_or_else(|| invalid(vm, a, DIR_RESOURCE_NAME))
}

/// Runs `f` with the entry of argument `n`.
fn with_entry<R>(
    vm: &mut Vm,
    a: &Args,
    n: usize,
    f: impl FnOnce(&mut ZipEntry) -> R,
) -> VmResult<R> {
    let r = resource_arg(vm, a, n)?;
    let out = if r.is_closed() {
        None
    } else {
        let data = r.data();
        data.as_ref()
            .and_then(|d| d.downcast_ref::<EntryCell>())
            .map(|c| f(&mut c.borrow_mut()))
    };
    out.ok_or_else(|| invalid(vm, a, ENTRY_RESOURCE_NAME))
}

fn zip_open(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0).as_bytes().to_vec();
    Ok(match host::run(vm, |_| lib::zip_open(&name))? {
        ZipOpenResult::Dir(d) => {
            Value::Resource(Resource::new(DIR_RESOURCE_NAME, Some(Box::new(d))))
        }
        ZipOpenResult::False => Value::Bool(false),
        ZipOpenResult::Error(e) => Value::Int(e),
    })
}

fn zip_close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = resource_arg(vm, a, 0)?;
    with_dir(vm, a, 0, |_| ())?;
    host::run(vm, |_| {
        if let Some(d) = r.close() {
            if let Some(d) = d.downcast_ref::<ZipDir>() {
                d.close();
            }
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn zip_read(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let entry = with_dir(vm, a, 0, ZipDir::read)?;
    Ok(entry.map_or(Value::Bool(false), |e| {
        let cell: EntryCell = RefCell::new(e);
        Value::Resource(Resource::new(ENTRY_RESOURCE_NAME, Some(Box::new(cell))))
    }))
}

fn zip_entry_open(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    // The entry is fetched before the directory, like the C code.
    let ok = with_entry(vm, a, 1, |e| e.open())?;
    with_dir(vm, a, 0, |_| ())?;
    Ok(Value::Bool(ok))
}

fn zip_entry_close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = resource_arg(vm, a, 0)?;
    with_entry(vm, a, 0, ZipEntry::close)?;
    r.close();
    Ok(Value::Bool(true))
}

fn zip_entry_read(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let len = if a.has(1) { a.int(1) } else { 1024 };
    let r = with_entry(vm, a, 0, |e| e.read(len))?;
    Ok(r.map_or(Value::Bool(false), |b| Value::Str(ZStr::from(b))))
}

fn zip_entry_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with_entry(vm, a, 0, |e| e.name())?;
    Ok(r.map_or(Value::Bool(false), |b| Value::Str(ZStr::from(b))))
}

fn zip_entry_compressedsize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with_entry(vm, a, 0, |e| e.compressed_size())?;
    Ok(r.map_or(Value::Bool(false), Value::Int))
}

fn zip_entry_filesize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with_entry(vm, a, 0, |e| e.filesize())?;
    Ok(r.map_or(Value::Bool(false), Value::Int))
}

fn zip_entry_compressionmethod(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with_entry(vm, a, 0, |e| e.compression_method())?;
    Ok(r.map_or(Value::Bool(false), |s| Value::Str(ZStr::from(s))))
}

/// Registers the procedural functions with their `#[\Deprecated]`
/// attributes.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    const STAT: &str = "use ZipArchive::statIndex() instead";
    m.function("zip_open(string $filename)", zip_open)
        .deprecated("8.0", "use ZipArchive::open() instead")
        .function("zip_close($zip): void", zip_close)
        .deprecated("8.0", "use ZipArchive::close() instead")
        .function("zip_read($zip)", zip_read)
        .deprecated("8.0", STAT)
        .function(
            "zip_entry_open($zip_dp, $zip_entry, string $mode = \"rb\"): bool",
            zip_entry_open,
        )
        .deprecated("8.0", "")
        .function("zip_entry_close($zip_entry): true", zip_entry_close)
        .deprecated("8.0", "")
        .function(
            "zip_entry_read($zip_entry, int $len = 1024): string|false",
            zip_entry_read,
        )
        .deprecated("8.0", "use ZipArchive::getFromIndex() instead")
        .function("zip_entry_name($zip_entry): string|false", zip_entry_name)
        .deprecated("8.0", STAT)
        .function(
            "zip_entry_compressedsize($zip_entry): int|false",
            zip_entry_compressedsize,
        )
        .deprecated("8.0", STAT)
        .function(
            "zip_entry_filesize($zip_entry): int|false",
            zip_entry_filesize,
        )
        .deprecated("8.0", STAT)
        .function(
            "zip_entry_compressionmethod($zip_entry): string|false",
            zip_entry_compressionmethod,
        )
        .deprecated("8.0", STAT);
}
