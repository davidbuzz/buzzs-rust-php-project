//! The `PharFileInfo` methods.

use std::cell::RefCell;
use std::rc::Rc;

use php_phar::object::PharFileInfo;
use php_runtime::Value;
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

use crate::archive::{call_spl, metadata_value, serialize_metadata};
use crate::state;
use crate::util::{bytes, opt_int, run};

fn st(a: &Args) -> Rc<RefCell<PharFileInfo>> {
    state::sweep();
    state::info(&a.this_obj())
}

/// `PharFileInfo::__construct()`.
pub fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let this = a.this_obj();
    let st = st(a);
    let fname = a.str(0).as_bytes().to_vec();
    if fname.contains(&0) {
        return Err(a.value_error(vm, 0, "must not contain any null bytes"));
    }
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
simple!(del_metadata, del_metadata, Value::Bool);
simple!(get_content, get_content, |r: Vec<u8>| bytes(r));
simple!(decompress, decompress, Value::Bool);

/// `PharFileInfo::chmod()`.
pub fn chmod(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let perms = a.int(0);
    run(vm, || st.borrow().chmod(perms))?;
    Ok(Value::Null)
}

/// `PharFileInfo::compress()`.
pub fn compress(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
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
    metadata_value(vm, m, opts)
}

/// `PharFileInfo::setMetadata()`.
pub fn set_metadata(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let v = a.at(0);
    // Report the uninitialised-object error before serializing.
    run(vm, || st.borrow().entry_info().map(|_| ()))?;
    let ser = serialize_metadata(vm, &v)?;
    run(vm, || st.borrow().set_metadata(v, ser))?;
    Ok(Value::Null)
}
