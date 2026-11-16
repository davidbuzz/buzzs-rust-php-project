// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Program execution: `escapeshellarg`/`escapeshellcmd`, `exec`, `system`,
//! `passthru`, `shell_exec`, `proc_nice`.

use crate::ctx::RtExt;
use php_runtime::{ErrorLevel, RtResult, RuntimeContext, Value, ZStr};
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
    let r = exec::exec(cmd.as_bytes(), output, vm);
    pipe_read_done();
    let (ret, out, status) = r.vm(vm)?;
    if let Some(out) = out {
        a.set_ref(1, Value::Array(out));
    }
    if a.has(2) {
        a.set_ref(2, Value::Int(status));
    }
    Ok(ret)
}

/// PHP reads the command's output through a `popen()` stream, whose reads
/// clear the stat cache (`php_stdiop_read`): a file the command changed
/// is stat()ed afresh (bug #72666).
fn pipe_read_done() {
    php_streams::state::clear_stat_cache();
}

/// `exec::system` / `exec::passthru` with a diagnostics collector as the
/// context, so the output callback can hold the VM.
type Runner =
    fn(&[u8], &mut dyn FnMut(&[u8]), &mut DeferredDiagnostics) -> RtResult<exec::ExecResult>;

/// Collects the library's diagnostics (only "Unable to fork", raised before
/// any output) to be reported once the command finished.
#[derive(Default)]
struct DeferredDiagnostics(Vec<(ErrorLevel, String)>);

impl RuntimeContext for DeferredDiagnostics {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        self.0.push((level, message.to_string()));
        Ok(())
    }
}

/// Runs `system()`/`passthru()` writing the child's output through the
/// output layer as it arrives (`PHPWRITE` per read), so large outputs
/// never accumulate in memory.
fn run_printing(vm: &mut Vm, a: &Args, f: Runner) -> VmResult<Value> {
    let cmd = a.str(0);
    vm.flush_stdout();
    let mut diags = DeferredDiagnostics::default();
    let r = f(cmd.as_bytes(), &mut |b: &[u8]| vm.output(b), &mut diags);
    pipe_read_done();
    for (level, message) in diags.0 {
        vm.raise(level.bits(), &message)?;
    }
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
    let r = exec::shell_exec(cmd.as_bytes(), vm);
    pipe_read_done();
    r.vm(vm)
}

fn proc_nice(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    exec::proc_nice(a.int(0), vm).vm(vm).map(Value::Bool)
}

/// Registers the functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function_static("escapeshellarg(string $arg): string", escapeshellarg)
        .function_static("escapeshellcmd(string $command): string", escapeshellcmd)
        .function_static(
            "exec(string $command, &$output = null, &$result_code = null): string|false",
            exec_fn,
        )
        .function_static(
            "system(string $command, &$result_code = null): string|false",
            system,
        )
        .function_static(
            "passthru(string $command, &$result_code = null): false|null",
            passthru,
        )
        .function_static("shell_exec(string $command): string|false|null", shell_exec)
        .function_static("proc_nice(int $priority): bool", proc_nice);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthru_streams_output_and_defers_diagnostics() {
        let mut diags = DeferredDiagnostics::default();
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        let r = exec::passthru(
            b"printf ab; printf cd",
            &mut |b: &[u8]| chunks.push(b.to_vec()),
            &mut diags,
        )
        .expect("passthru");
        assert_eq!(r.status, 0);
        assert_eq!(chunks.concat(), b"abcd");
        assert!(diags.0.is_empty());
        diags
            .diagnostic(ErrorLevel::Warning, "passthru(): Unable to fork [x]")
            .expect("deferred");
        assert_eq!(diags.0.len(), 1);
    }
}
