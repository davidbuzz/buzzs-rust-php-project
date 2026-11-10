//! `ext/sysvshm` (ref: `ext/sysvshm/sysvshm.c`).

use php_runtime::Value;
use php_sysv::shm::{SharedMemory, DATA_CORRUPTED, DEFAULT_INIT_MEM};
use php_sysv::SysvResult;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::objects::{register_class, with_state, wrap};
use crate::{failure, serial};

/// The class name.
const CLASS: &str = "SysvSharedMemory";

/// Runs `f` on the segment handle of argument 0.
fn shm<R>(a: &Args, f: impl FnOnce(&mut SharedMemory) -> SysvResult<R>) -> SysvResult<R> {
    // ZPP guarantees the object; only objects made by shm_attach() exist.
    with_state::<SharedMemory, _>(a, 0, f).unwrap_or(Err(php_sysv::SysvError::Error(
        php_sysv::shm::ALREADY_DESTROYED.into(),
    )))
}

fn shm_attach(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let size = a.opt(1).map(|_| a.int(1));
    match SharedMemory::attach(a.int(0), size, a.int(2), DEFAULT_INIT_MEM) {
        Ok(s) => wrap(vm, CLASS, s),
        Err(e) => failure(vm, a, e),
    }
}

fn shm_detach(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match shm(a, SharedMemory::detach) {
        Ok(()) => Ok(Value::Bool(true)),
        Err(e) => failure(vm, a, e),
    }
}

fn shm_remove(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match shm(a, |s| s.remove()) {
        Ok(()) => Ok(Value::Bool(true)),
        Err(e) => failure(vm, a, e),
    }
}

fn shm_has_var(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let key = a.int(1);
    match shm(a, |s| s.has_var(key)) {
        Ok(b) => Ok(Value::Bool(b)),
        Err(e) => failure(vm, a, e),
    }
}

fn shm_remove_var(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let key = a.int(1);
    match shm(a, |s| s.remove_var(key)) {
        Ok(()) => Ok(Value::Bool(true)),
        Err(e) => failure(vm, a, e),
    }
}

fn shm_put_var(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let key = a.int(1);
    if let Err(e) = shm(a, |s| s.ensure_attached()) {
        return failure(vm, a, e);
    }
    // Serialization may run user code (`__serialize`, `__sleep`), which
    // may even detach this very segment: no borrow is held meanwhile.
    let data = serial::serialize(vm, &a.at(2))?;
    match shm(a, |s| s.put_var(key, &data)) {
        Ok(()) => Ok(Value::Bool(true)),
        Err(e) => failure(vm, a, e),
    }
}

fn shm_get_var(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let key = a.int(1);
    let data = match shm(a, |s| s.get_var(key)) {
        Ok(d) => d,
        Err(e) => return failure(vm, a, e),
    };
    if let Some(v) = serial::unserialize(vm, &data)? {
        Ok(v)
    } else {
        vm.php_warning(DATA_CORRUPTED)?;
        Ok(Value::Bool(false))
    }
}

/// Registers the class and functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    register_class(m, CLASS);
    m.function_static(
        "shm_attach(int $key, ?int $size = null, int $permissions = 438): SysvSharedMemory|false",
        shm_attach,
    )
    .function_static("shm_detach(SysvSharedMemory $shm): true", shm_detach)
    .function_static(
        "shm_has_var(SysvSharedMemory $shm, int $key): bool",
        shm_has_var,
    )
    .function_static("shm_remove(SysvSharedMemory $shm): bool", shm_remove)
    .function_static(
        "shm_put_var(SysvSharedMemory $shm, int $key, mixed $value): bool",
        shm_put_var,
    )
    .function_static(
        "shm_get_var(SysvSharedMemory $shm, int $key): mixed",
        shm_get_var,
    )
    .function_static(
        "shm_remove_var(SysvSharedMemory $shm, int $key): bool",
        shm_remove_var,
    );
}
