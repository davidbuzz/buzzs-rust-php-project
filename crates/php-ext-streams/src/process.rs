//! `popen()` / `pclose()`: process pipes as plain streams.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};

use php_runtime::Value;
use php_streams::plain;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::bridge;
use crate::res::{self, path_arg, str_arg};

thread_local! {
    /// Children of open `popen()` streams, by library stream id.
    static CHILDREN: RefCell<HashMap<u64, Child>> = RefCell::new(HashMap::new());
}

fn popen(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cmd = path_arg(vm, a, 0)?;
    let mode = str_arg(a, 1);
    let mut posix = mode.clone();
    if let Some(p) = posix.iter().position(|&c| c == b'b') {
        posix.remove(p);
    }
    let reading = match posix.as_slice() {
        b"r" => true,
        b"w" => false,
        _ => {
            return Err(a.value_error(vm, 2, "must be one of \"r\", \"rb\", \"w\", or \"wb\""));
        }
    };
    vm.flush_stdout();
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(std::ffi::OsStr::new(
        &String::from_utf8_lossy(&cmd).into_owned(),
    ));
    if reading {
        c.stdout(Stdio::piped());
    } else {
        c.stdin(Stdio::piped());
    }
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            vm.php_warning(&php_streams::sys::error_text(&e))?;
            return Ok(Value::Bool(false));
        }
    };
    let fd: Option<OwnedFd> = if reading {
        child.stdout.take().map(OwnedFd::from)
    } else {
        child.stdin.take().map(OwnedFd::from)
    };
    let Some(fd) = fd else {
        return Ok(Value::Bool(false));
    };
    let mode_s = String::from_utf8_lossy(&mode).into_owned();
    let stream = plain::stream_from_file(File::from(fd), &mode_s, true);
    CHILDREN.with(|m| m.borrow_mut().insert(stream.id(), child));
    Ok(res::new_stream(stream, None))
}

fn exit_code(c: Option<Child>) -> i64 {
    c.and_then(|mut c| c.wait().ok()).map_or(-1, |st| {
        st.code()
            .map_or_else(|| st.signal().map_or(-1, i64::from), i64::from)
    })
}

fn pclose(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (r, cell) = res::stream_arg_res(vm, a, 0)?;
    if let Some(mut s) = cell.take() {
        bridge::run(vm, || s.close())?;
    }
    r.close();
    let child = CHILDREN.with(|m| m.borrow_mut().remove(&cell.id()));
    Ok(Value::Int(exit_code(child)))
}

/// Reaps the child of a closed `popen()` stream (called by `fclose()`).
pub fn reap(stream_id: u64) {
    let child = CHILDREN.with(|m| m.borrow_mut().remove(&stream_id));
    let _ = exit_code(child);
}

/// Registers the functions of this module.
pub fn register(m: &mut ModuleBuilder<'_>) {
    m.function("popen(string $command, string $mode)", popen)
        .function("pclose($handle): int", pclose);
}
