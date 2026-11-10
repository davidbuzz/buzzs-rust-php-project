//! `proc_open()`, `proc_close()`, `proc_get_status()`, `proc_terminate()`.
//!
//! The parent ends of the child's pipes are wrapped into `stream`
//! resources of `php-ext-streams` (STDIO streams over the descriptors,
//! `PHP_STREAM_FLAG_NO_SEEK` for pipes); the process itself is a `process`
//! resource whose payload owns the [`Process`] and the pipe resources
//! (closed by `proc_close()`, like `proc_open_rsrc_dtor`).

use crate::ctx::RtExt;
use php_runtime::{ErrorLevel, Resource, RtResult, RuntimeError, Value, ZArray, ZStr};
use php_stdmisc::process::{self, CommandArg, ProcHost, Process};
use php_streams::stream::CastAs;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;
use std::cell::RefCell;
use std::os::fd::{OwnedFd, RawFd};

/// The resource type name.
const PROCESS: &str = "process";

/// The payload of a `process` resource.
struct ProcRes {
    process: RefCell<Process>,
    pipes: RefCell<Vec<Value>>,
}

impl ProcRes {
    /// Closes the parent ends of the pipes (`zend_list_close` of each).
    fn close_pipes(&self) {
        for v in std::mem::take(&mut *self.pipes.borrow_mut()) {
            if let Value::Resource(r) = &v {
                if let Some(cell) = php_ext_streams::res::cell_of(r) {
                    if let Some(mut s) = cell.take() {
                        s.close();
                    }
                }
                r.close();
            }
        }
    }
}

impl Drop for ProcRes {
    fn drop(&mut self) {
        // `proc_open_rsrc_dtor` without `proc_close()`: the pipes are
        // closed and the child is not waited for.
        self.close_pipes();
        self.process.borrow_mut().close(false);
    }
}

/// [`ProcHost`] over plain files and `php-ext-streams` resources. Warnings
/// are collected and reported after `proc_open()` returns.
#[derive(Default)]
struct Host {
    warnings: Vec<String>,
}

fn open_options(mode: &[u8]) -> std::fs::OpenOptions {
    let mut o = std::fs::OpenOptions::new();
    let plus = mode.contains(&b'+');
    match mode.first() {
        Some(b'w') => {
            o.write(true).create(true).truncate(true).read(plus);
        }
        Some(b'a') => {
            o.append(true).create(true).read(plus);
        }
        Some(b'x') => {
            o.write(true).create_new(true).read(plus);
        }
        Some(b'c') => {
            o.write(true).create(true).read(plus);
        }
        _ => {
            o.read(true).write(plus);
        }
    }
    o
}

impl ProcHost for Host {
    fn open_file_fd(&mut self, path: &[u8], mode: &[u8]) -> RtResult<Option<OwnedFd>> {
        use std::os::unix::ffi::OsStrExt;
        match open_options(mode).open(std::ffi::OsStr::from_bytes(path)) {
            Ok(f) => Ok(Some(OwnedFd::from(f))),
            Err(e) => {
                let reason = match e.kind() {
                    std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
                    std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
                    _ => e.to_string(),
                };
                self.warnings
                    .push(format!("proc_open(): Failed to open stream: {reason}"));
                Ok(None)
            }
        }
    }

    fn stream_fd(&mut self, resource: &Resource) -> RtResult<Option<RawFd>> {
        // `zend_fetch_resource2_ex(..., "stream", le_stream, le_pstream)`.
        let Some(cell) = php_ext_streams::res::cell_of(resource)
            .filter(|c| !resource.is_closed() && !c.is_closed())
        else {
            return Err(RuntimeError::TypeError(
                "proc_open(): supplied resource is not a valid stream resource".into(),
            ));
        };
        let fd = cell.get().and_then(|mut s| s.cast(CastAs::Fd, true));
        // The stream layer's own warnings ("N bytes of buffered data lost
        // during stream conversion!", "Cannot represent a stream of type X
        // as a File Descriptor").
        for d in php_ext_streams::bridge::take_diagnostics() {
            self.warnings.push(format!("proc_open(): {d}"));
        }
        Ok(fd)
    }
}

fn proc_open(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let command = a.at(0);
    let spec = a.array(1);
    let cwd = a.opt(3).map(|_| a.str(3));
    let env: Option<ZArray> = a.opt(4).map(|_| a.array(4));
    let cmd = match &command {
        Value::Array(arr) => CommandArg::Argv(arr),
        Value::Str(s) => CommandArg::Shell(s.as_bytes()),
        _ => CommandArg::Shell(b""),
    };
    let mut host = Host::default();
    vm.flush_stdout();
    let r = process::proc_open(
        cmd,
        &spec,
        cwd.as_ref().map(ZStr::as_bytes),
        env.as_ref(),
        &mut host,
        vm,
    );
    for w in std::mem::take(&mut host.warnings) {
        vm.raise(ErrorLevel::Warning.bits(), &w)?;
    }
    let Some((proc, parent_pipes)) = r.vm(vm)? else {
        return Ok(Value::Bool(false));
    };
    let mut pipes = ZArray::new();
    let mut held = Vec::with_capacity(parent_pipes.len());
    for p in parent_pipes {
        let file = std::fs::File::from(p.fd);
        let stream = php_streams::plain::stream_from_file(file, p.mode, false);
        let v = php_ext_streams::res::new_stream(stream, None);
        pipes
            .make_mut()
            .insert(php_runtime::ArrayKey::Int(p.index), v.clone());
        held.push(v);
    }
    a.set_ref(2, Value::Array(pipes));
    let payload = ProcRes {
        process: RefCell::new(proc),
        pipes: RefCell::new(held),
    };
    Ok(Value::Resource(Resource::new(
        PROCESS,
        Some(Box::new(payload)),
    )))
}

/// The `process` resource argument #1, or the "not a valid process
/// resource" `TypeError`.
fn process_arg(vm: &mut Vm, a: &Args) -> VmResult<Resource> {
    if let Value::Resource(r) = a.at(0) {
        let ok = r
            .data()
            .as_ref()
            .is_some_and(|d| d.downcast_ref::<ProcRes>().is_some());
        if ok {
            return Ok(r);
        }
    }
    Err(vm.throw(
        "TypeError",
        &format!(
            "{}(): supplied resource is not a valid process resource",
            a.func_name()
        ),
    ))
}

fn with_proc<R>(r: &Resource, f: impl FnOnce(&ProcRes) -> R) -> Option<R> {
    let d = r.data();
    d.as_ref().and_then(|d| d.downcast_ref::<ProcRes>()).map(f)
}

fn proc_close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = process_arg(vm, a)?;
    with_proc(&r, ProcRes::close_pipes);
    vm.flush_stdout();
    let code = with_proc(&r, |p| p.process.borrow_mut().close(true)).unwrap_or(-1);
    r.close();
    Ok(Value::Int(code))
}

fn proc_get_status(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = process_arg(vm, a)?;
    Ok(
        with_proc(&r, |p| Value::Array(p.process.borrow_mut().status()))
            .unwrap_or(Value::Bool(false)),
    )
}

fn proc_terminate(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = process_arg(vm, a)?;
    let sig = if a.has(1) { a.int(1) } else { process::SIGTERM };
    Ok(Value::Bool(
        with_proc(&r, |p| p.process.borrow().terminate(sig)).unwrap_or(false),
    ))
}

/// Registers the functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function_static(
        "proc_open(array|string $command, array $descriptor_spec, &$pipes, ?string $cwd = null, ?array $env_vars = null, ?array $options = null)",
        proc_open,
    )
    .function_static("proc_close($process): int", proc_close)
    .function_static("proc_terminate($process, int $signal = 15): bool", proc_terminate)
    .function_static("proc_get_status($process): array", proc_get_status);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_stream_resource_is_a_type_error() {
        let mut host = Host::default();
        let ctx = Resource::new("stream-context", None);
        match host.stream_fd(&ctx) {
            Err(RuntimeError::TypeError(m)) => assert_eq!(
                m,
                "proc_open(): supplied resource is not a valid stream resource"
            ),
            other => panic!("unexpected {other:?}"),
        }
    }
}
