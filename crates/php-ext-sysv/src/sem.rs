//! `ext/sysvsem` (ref: `ext/sysvsem/sysvsem.c`).

use php_runtime::Value;
use php_sysv::sem::Semaphore;
use php_sysv::SysvResult;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::failure;
use crate::objects::{register_class, with_state, wrap};

/// The class name.
const CLASS: &str = "SysvSemaphore";

fn sem_get(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut warnings = Vec::new();
    let r = Semaphore::get(a.int(0), a.int(1), a.int(2), a.bool(3), &mut |w| {
        warnings.push(w);
    });
    for w in &warnings {
        vm.php_warning(w)?;
    }
    match r {
        Ok(s) => wrap(vm, CLASS, s),
        Err(e) => failure(vm, a, e),
    }
}

/// Runs `f` on the semaphore and maps the result to `true`/failure.
fn op(vm: &mut Vm, a: &Args, f: impl FnOnce(&mut Semaphore) -> SysvResult<()>) -> VmResult<Value> {
    match with_state::<Semaphore, _>(a, 0, f) {
        Some(Ok(())) => Ok(Value::Bool(true)),
        Some(Err(e)) => failure(vm, a, e),
        None => Ok(Value::Bool(false)),
    }
}

fn sem_acquire(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let non_blocking = a.bool(1);
    op(vm, a, |s| s.acquire(non_blocking))
}

fn sem_release(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    op(vm, a, Semaphore::release)
}

fn sem_remove(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    op(vm, a, Semaphore::remove)
}

/// Registers the class and functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    register_class(m, CLASS);
    m.function_static(
        "sem_get(int $key, int $max_acquire = 1, int $permissions = 438, bool $auto_release = true): SysvSemaphore|false",
        sem_get,
    )
    .function_static(
        "sem_acquire(SysvSemaphore $semaphore, bool $non_blocking = false): bool",
        sem_acquire,
    )
    .function_static("sem_release(SysvSemaphore $semaphore): bool", sem_release)
    .function_static("sem_remove(SysvSemaphore $semaphore): bool", sem_remove);
}
