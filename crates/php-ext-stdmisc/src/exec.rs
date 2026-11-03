//! Program execution: `escapeshellarg`/`escapeshellcmd`, `exec`, `system`,
//! `passthru`, `shell_exec`, `proc_nice`.

use crate::ctx::RtExt;
use php_runtime::{Value, ZStr};
use php_stdmisc::exec;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

fn escapeshellarg(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    exec::escapeshellarg(a.str(0).as_bytes())
        .vm(vm)
        .map(|s| Value::Str(ZStr::from(s)))
}

fn escapeshellcmd(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    exec::escapeshellcmd(a.str(0).as_bytes())
        .vm(vm)
        .map(|s| Value::Str(ZStr::from(s)))
}

fn exec_fn(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cmd = a.str(0);
    let output = a.has(1).then(|| a.at(1));
    vm.flush_stdout();
    let (ret, out, status) = exec::exec(cmd.as_bytes(), output, vm).vm(vm)?;
    if let Some(out) = out {
        a.set_ref(1, Value::Array(out));
    }
    if a.has(2) {
        a.set_ref(2, Value::Int(status));
    }
    Ok(ret)
}

/// `exec::system` / `exec::passthru` over the VM.
type Runner = fn(&[u8], &mut dyn FnMut(&[u8]), &mut Vm) -> php_runtime::RtResult<exec::ExecResult>;

/// Runs `system()`/`passthru()` with the child's output collected and
/// written through the output layer.
fn run_printing(vm: &mut Vm, a: &Args, f: Runner) -> VmResult<Value> {
    let cmd = a.str(0);
    vm.flush_stdout();
    let mut buf = Vec::new();
    let r = f(cmd.as_bytes(), &mut |b: &[u8]| buf.extend_from_slice(b), vm);
    vm.output(&buf);
    let r = r.vm(vm)?;
    if a.has(1) {
        a.set_ref(1, Value::Int(r.status));
    }
    Ok(r.value)
}

fn system(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run_printing(vm, a, |c, o, vm| exec::system(c, o, vm))
}

fn passthru(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = run_printing(vm, a, |c, o, vm| exec::passthru(c, o, vm))?;
    // `passthru()` returns null on success, false when the shell failed.
    Ok(match r {
        Value::Bool(false) => r,
        _ => Value::Null,
    })
}

fn shell_exec(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cmd = a.str(0);
    vm.flush_stdout();
    exec::shell_exec(cmd.as_bytes(), vm).vm(vm)
}

fn proc_nice(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    exec::proc_nice(a.int(0), vm).vm(vm).map(Value::Bool)
}

/// Registers the functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function("escapeshellarg(string $arg): string", escapeshellarg)
        .function("escapeshellcmd(string $command): string", escapeshellcmd)
        .function(
            "exec(string $command, &$output = null, &$result_code = null): string|false",
            exec_fn,
        )
        .function(
            "system(string $command, &$result_code = null): string|false",
            system,
        )
        .function(
            "passthru(string $command, &$result_code = null): false|null",
            passthru,
        )
        .function("shell_exec(string $command): string|false|null", shell_exec)
        .function("proc_nice(int $priority): bool", proc_nice);
}
