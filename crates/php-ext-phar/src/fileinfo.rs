// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `PharFileInfo` methods.

use std::cell::RefCell;
use std::rc::Rc;

use php_phar::object::PharFileInfo;
use php_phar::registry as reg;
use php_runtime::{Obj, Value};
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

use crate::archive::{call_spl, metadata_value, serialize_metadata};
use crate::state;
use crate::util::{bytes, check_paths, opt_int, run};

type InfoRef = Rc<RefCell<PharFileInfo>>;

/// The `SplFileInfo` file name of `this`.
fn spl_file_name(this: &Obj) -> Option<Vec<u8>> {
    php_ext_spl::host::spl_state_of(this)
        .and_then(|s| s.file(|f| f.file_name.clone()).ok().flatten())
        .map(|z| z.as_bytes().to_vec())
}

/// Rebuilds the state from the object's path (after a copy-on-write of a
/// cached archive, or for objects SPL created without calling the
/// constructor).
fn rebuild(this: &Obj, s: &InfoRef) {
    if let Some(fname) = spl_file_name(this) {
        let mut n = PharFileInfo::new();
        if n.construct(&fname).is_ok() {
            let old = std::mem::replace(&mut *s.borrow_mut(), n);
            drop(old);
        }
    }
}

fn st(a: &Args) -> InfoRef {
    state::sweep();
    let this = a.this_obj();
    let s = state::info(&this);
    let entry = s.borrow().entry_info();
    match entry {
        // `spl_filesystem_object_create_type()` calls the constructor of
        // internal subclasses too; php-spl only calls user constructors.
        Err(_) => rebuild(&this, &s),
        Ok(e) if e.is_persistent => {
            if let Some(fname) = spl_file_name(&this) {
                if let Ok((arch, _)) = reg::split_fname(&fname, 2, 0) {
                    if reg::find_by_fname(&arch).is_some_and(|a| !a.borrow().is_persistent) {
                        rebuild(&this, &s);
                    }
                }
            }
        }
        Ok(_) => {}
    }
    s
}

/// Copies a cached archive on write before modifying one of its entries
/// (`phar_copy_on_write()` in the `PharFileInfo` setters).
fn writable(a: &Args) -> InfoRef {
    let s = st(a);
    let this = a.this_obj();
    let persistent = s.borrow().entry_info().is_ok_and(|e| e.is_persistent);
    if persistent {
        if let Some(fname) = spl_file_name(&this) {
            if let Ok((arch, _)) = reg::split_fname(&fname, 2, 0) {
                if let Some(c) = reg::cached(&arch) {
                    if reg::copy_on_write(&c).is_some() {
                        rebuild(&this, &s);
                    }
                }
            }
        }
    }
    s
}

/// `PharFileInfo::__construct()`.
pub fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    state::sweep();
    let st = state::info(&this);
    check_paths(vm, a, &[0])?;
    let fname = a.str(0).as_bytes().to_vec();
    let arg = run(vm, || st.borrow_mut().construct(&fname))?;
    call_spl(vm, "SplFileInfo", "__construct", &this, &[bytes(arg)])?;
    Ok(Value::Null)
}

/// `PharFileInfo::__destruct()`.
pub fn destruct(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    if let Some(this) = &a.this {
        state::release_info(this);
    }
    Ok(Value::Null)
}

macro_rules! simple {
    ($name:ident, $method:ident, $conv:expr) => {
        #[doc = concat!("`PharFileInfo::", stringify!($method), "()`.")]
        pub fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let st = st(a);
            let r = run(vm, || st.borrow().$method())?;
            #[allow(clippy::redundant_closure_call)]
            Ok(($conv)(r))
        }
    };
}

simple!(get_compressed_size, get_compressed_size, Value::Int);
simple!(get_crc32, get_crc32, Value::Int);
simple!(is_crc_checked, is_crc_checked, Value::Bool);
simple!(get_phar_flags, get_phar_flags, Value::Int);
simple!(has_metadata, has_metadata, Value::Bool);

simple!(get_content, get_content, |r: Vec<u8>| bytes(r));

/// `PharFileInfo::decompress()`.
pub fn decompress(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = writable(a);
    Ok(Value::Bool(run(vm, || st.borrow().decompress())?))
}

/// `PharFileInfo::delMetadata()`.
pub fn del_metadata(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = writable(a);
    Ok(Value::Bool(run(vm, || st.borrow().del_metadata())?))
}

/// `PharFileInfo::chmod()`.
pub fn chmod(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = writable(a);
    let perms = a.int(0);
    run(vm, || st.borrow().chmod(perms))?;
    Ok(Value::Null)
}

/// `PharFileInfo::compress()`.
pub fn compress(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = writable(a);
    let m = a.int(0);
    Ok(Value::Bool(run(vm, || st.borrow().compress(m))?))
}

/// `PharFileInfo::isCompressed()`.
pub fn is_compressed(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let m = opt_int(a, 0);
    Ok(Value::Bool(run(vm, || st.borrow().is_compressed(m))?))
}

/// `PharFileInfo::getMetadata()`.
pub fn get_metadata(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let opts = a.get(0);
    let m = run(vm, || st.borrow().get_metadata(opts.is_some()))?;
    metadata_value(vm, m, opts, "PharFileInfo::getMetadata")
}

/// `PharFileInfo::setMetadata()`.
pub fn set_metadata(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = writable(a);
    let v = a.at(0);
    // Report the uninitialised-object error before serializing.
    run(vm, || st.borrow().entry_info().map(|_| ()))?;
    let ser = serialize_metadata(vm, &v)?;
    run(vm, || st.borrow().set_metadata(v, ser))?;
    Ok(Value::Null)
}
