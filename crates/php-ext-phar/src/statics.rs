// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The static `Phar` methods.

use php_phar::object as po;
use php_runtime::{Value, ZArray};
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

use crate::state;
use crate::util::{bytes, check_paths, executed, halt_defined, opt_str, run, run_ok};

fn str_list(v: &[&str]) -> Value {
    Value::Array(ZArray::from_values(v.iter().map(bytes)))
}

/// `Phar::apiVersion()`.
pub fn api_version(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(bytes(po::api_version()))
}

/// `Phar::canCompress()`.
pub fn can_compress(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let m = if a.has(0) { a.int(0) } else { 0 };
    Ok(Value::Bool(po::can_compress(m)))
}

/// `Phar::canWrite()`.
pub fn can_write(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(po::can_write()))
}

/// `Phar::createDefaultStub()`.
pub fn create_default_stub(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[0, 1])?;
    let index = opt_str(a, 0);
    let web = opt_str(a, 1);
    let s = run(vm, || {
        po::create_default_stub(index.as_deref(), web.as_deref())
    })?;
    Ok(bytes(s))
}

/// `Phar::getSupportedCompression()`.
pub fn get_supported_compression(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(str_list(&po::get_supported_compression()))
}

/// `Phar::getSupportedSignatures()`.
pub fn get_supported_signatures(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(str_list(&po::get_supported_signatures()))
}

/// `Phar::interceptFileFuncs()`.
pub fn intercept_file_funcs(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    po::intercept_file_funcs();
    Ok(Value::Null)
}

/// `Phar::isValidPharFilename()`.
pub fn is_valid_phar_filename(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[0])?;
    let f = a.str(0);
    let exec = !a.has(1) || a.bool(1);
    Ok(Value::Bool(po::is_valid_phar_filename(f.as_bytes(), exec)))
}

/// `Phar::loadPhar()`.
pub fn load_phar(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[0])?;
    let f = a.str(0);
    let alias = opt_str(a, 1);
    Ok(Value::Bool(run(vm, || {
        po::load_phar(f.as_bytes(), alias.as_deref())
    })?))
}

/// `Phar::mapPhar()`.
pub fn map_phar(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let alias = opt_str(a, 0);
    let ex = executed(vm);
    let halt = halt_defined(vm);
    Ok(Value::Bool(run(vm, || {
        po::map_phar(ex.as_deref(), alias.as_deref(), halt)
    })?))
}

/// `Phar::running()`.
pub fn running(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ret = !a.has(0) || a.bool(0);
    let ex = executed(vm);
    Ok(bytes(po::running(ex.as_deref(), ret)))
}

/// `Phar::mount()`.
pub fn mount(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = a.str(0);
    let actual = a.str(1);
    check_paths(vm, a, &[0, 1])?;
    let ex = executed(vm);
    run(vm, || {
        po::mount(ex.as_deref(), path.as_bytes(), actual.as_bytes())
    })?;
    Ok(Value::Null)
}

/// `Phar::mungServer()`.
pub fn mung_server(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arr = a.array(0);
    let values: Vec<Option<Vec<u8>>> = arr
        .iter()
        .map(|(_, v)| match v.deref() {
            Value::Str(s) => Some(s.as_bytes().to_vec()),
            _ => None,
        })
        .collect();
    run(vm, || php_phar::web::mung_server(&values))?;
    Ok(Value::Null)
}

/// `Phar::unlinkArchive()`.
pub fn unlink_archive(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    check_paths(vm, a, &[0])?;
    state::sweep();
    let f = a.str(0);
    let ex = executed(vm);
    Ok(Value::Bool(run(vm, || {
        po::unlink_archive(f.as_bytes(), ex.as_deref())
    })?))
}

/// `Phar::webPhar()`.
pub fn web_phar(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    crate::web::web_phar(vm, a)
}

/// Drops state of freed objects (exposed for the wrapper hooks).
pub fn sweep(vm: &mut Vm) -> VmResult<()> {
    run_ok(vm, state::sweep)
}
