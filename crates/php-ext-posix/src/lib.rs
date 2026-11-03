//! `ext/posix` for the phprust VM: binds the `php-posix` library through
//! the `php_vm::ext` extension API (ref: `ext/posix/posix.c`).
//!
//! The library owns the module state (`POSIX_G(last_error)`) and all libc
//! calls; this crate converts PHP values, implements the manual
//! `int|resource` argument handling of `posix_isatty()`,
//! `posix_ttyname()` and `posix_fpathconf()` (stream resources are cast to
//! their file descriptor through `php-ext-streams`), and turns
//! [`PosixError`]s into PHP warnings, `ValueError`s or `false`.

// Native handlers share one signature (`fn(&mut Vm, &mut Args) ->
// VmResult<Value>`), so infallible ones still return `VmResult`.
#![allow(clippy::unnecessary_wraps)]

mod fdarg;

use std::cell::RefCell;

use php_posix::{Field, Posix, PosixError, PosixResult, RlimitValue, Rlimits};
use php_runtime::{ArrayKey, Value, ZArray, ZStr};
use php_vm::ext::{Args, Extension, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::fdarg::{fd_arg, FdArg};

thread_local! {
    /// The per-request module globals.
    static STATE: RefCell<Posix> = RefCell::new(Posix::new());
}

/// Runs `f` on the module state.
fn with<R>(f: impl FnOnce(&mut Posix) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// The `ext/posix` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct PosixExt;

/// The running function's name (for `ValueError` prefixes).
fn fname(a: &Args) -> String {
    a.func
        .as_ref()
        .map_or_else(String::new, |f| f.name.to_string_lossy())
}

/// A PHP string from bytes.
fn bytes(b: impl AsRef<[u8]>) -> Value {
    Value::Str(ZStr::from(b.as_ref()))
}

/// Converts a failed call into PHP behaviour: `false`, a warning plus
/// `false`, or a thrown `ValueError`.
fn failure(vm: &mut Vm, a: &Args, e: PosixError) -> VmResult<Value> {
    match e {
        PosixError::Failed(_) => Ok(Value::Bool(false)),
        PosixError::Warning(msg) => {
            vm.php_warning(&msg)?;
            Ok(Value::Bool(false))
        }
        PosixError::ValueError(msg) => {
            Err(vm.throw("ValueError", &format!("{}(): {msg}", fname(a))))
        }
    }
}

/// Maps a result to a PHP value with `ok`, or to the failure behaviour.
fn finish<T>(
    vm: &mut Vm,
    a: &Args,
    r: PosixResult<T>,
    ok: impl FnOnce(T) -> Value,
) -> VmResult<Value> {
    match r {
        Ok(v) => Ok(ok(v)),
        Err(e) => failure(vm, a, e),
    }
}

/// A [`Field`] as a PHP value.
fn field_value(f: Field) -> Value {
    match f {
        Field::Str(s) => bytes(s),
        Field::Int(n) => Value::Int(n),
        Field::Null => Value::Null,
        Field::List(items) => Value::Array(ZArray::from_values(items.into_iter().map(bytes))),
    }
}

/// An associative array from `entries()`.
fn entries_value(entries: Vec<(&'static str, Field)>) -> Value {
    let mut arr = ZArray::with_capacity(entries.len());
    let t = arr.make_mut();
    for (k, v) in entries {
        t.insert(ArrayKey::Str(ZStr::from(k)), field_value(v));
    }
    Value::Array(arr)
}

fn rlimit_value(v: RlimitValue) -> Value {
    field_value(v.to_field())
}

fn true_on_ok(vm: &mut Vm, a: &Args, r: PosixResult<()>) -> VmResult<Value> {
    finish(vm, a, r, |()| Value::Bool(true))
}

// ---------------------------------------------------------------------------
// Process identification
// ---------------------------------------------------------------------------

fn posix_kill(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(|p| p.kill(a.int(0), a.int(1)));
    true_on_ok(vm, a, r)
}

fn posix_getpid(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with(|p| p.getpid())))
}

fn posix_getppid(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with(|p| p.getppid())))
}

fn posix_getuid(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with(|p| p.getuid())))
}

fn posix_geteuid(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with(|p| p.geteuid())))
}

fn posix_getgid(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with(|p| p.getgid())))
}

fn posix_getegid(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with(|p| p.getegid())))
}

fn posix_setuid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(|p| p.setuid(a.int(0)));
    true_on_ok(vm, a, r)
}

fn posix_seteuid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(|p| p.seteuid(a.int(0)));
    true_on_ok(vm, a, r)
}

fn posix_setgid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(|p| p.setgid(a.int(0)));
    true_on_ok(vm, a, r)
}

fn posix_setegid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(|p| p.setegid(a.int(0)));
    true_on_ok(vm, a, r)
}

fn posix_getgroups(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(Posix::getgroups);
    finish(vm, a, r, |g| {
        Value::Array(ZArray::from_values(g.into_iter().map(Value::Int)))
    })
}

fn posix_getlogin(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(Posix::getlogin);
    finish(vm, a, r, bytes)
}

fn posix_getpgrp(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with(|p| p.getpgrp())))
}

fn posix_setsid(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with(|p| p.setsid())))
}

fn posix_setpgid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(|p| p.setpgid(a.int(0), a.int(1)));
    true_on_ok(vm, a, r)
}

fn posix_getpgid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(|p| p.getpgid(a.int(0)));
    finish(vm, a, r, Value::Int)
}

fn posix_getsid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(|p| p.getsid(a.int(0)));
    finish(vm, a, r, Value::Int)
}

fn posix_uname(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(Posix::uname);
    finish(vm, a, r, |u| entries_value(u.entries()))
}

fn posix_times(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(Posix::times);
    finish(vm, a, r, |t| entries_value(t.entries()))
}

fn posix_ctermid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(Posix::ctermid);
    finish(vm, a, r, bytes)
}

// ---------------------------------------------------------------------------
// File descriptors (int|resource)
// ---------------------------------------------------------------------------

fn posix_ttyname(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let fd = match fd_arg(vm, a, 0)? {
        FdArg::Fd(fd) => fd,
        FdArg::Fail => return Ok(Value::Bool(false)),
        FdArg::WrongType(given) => {
            vm.php_warning(&php_posix::fd_type_warning(&given))?;
            // PHP continues with zval_get_long().
            let v = a.at(0);
            vm.value_to_int(&v)?
        }
    };
    let r = with(|p| p.ttyname(fd));
    finish(vm, a, r, bytes)
}

fn posix_isatty(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let fd = match fd_arg(vm, a, 0)? {
        FdArg::Fd(fd) => fd,
        FdArg::Fail => return Ok(Value::Bool(false)),
        FdArg::WrongType(given) => {
            vm.php_warning(&php_posix::fd_type_warning(&given))?;
            return Ok(Value::Bool(false));
        }
    };
    let r = with(|p| p.isatty(fd));
    true_on_ok(vm, a, r)
}

fn posix_fpathconf(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let fd = match fd_arg(vm, a, 0)? {
        FdArg::Fd(fd) => fd,
        FdArg::Fail => return Ok(Value::Bool(false)),
        FdArg::WrongType(given) => {
            let msg = format!("{}(): {}", fname(a), php_posix::fd_type_warning(&given));
            return Err(vm.throw("TypeError", &msg));
        }
    };
    let r = with(|p| p.fpathconf(fd, a.int(1)));
    finish(vm, a, r, Value::Int)
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

fn posix_getcwd(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(Posix::getcwd);
    finish(vm, a, r, bytes)
}

fn posix_mkfifo(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = a.str(0);
    let r = with(|p| p.mkfifo(path.as_bytes(), a.int(1)));
    true_on_ok(vm, a, r)
}

fn posix_mknod(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = a.str(0);
    let r = with(|p| p.mknod(path.as_bytes(), a.int(1), a.int(2), a.int(3)));
    true_on_ok(vm, a, r)
}

fn posix_access(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = a.str(0);
    let r = with(|p| p.access(path.as_bytes(), a.int(1)));
    true_on_ok(vm, a, r)
}

fn posix_eaccess(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = a.str(0);
    let r = with(|p| p.eaccess(path.as_bytes(), a.int(1)));
    true_on_ok(vm, a, r)
}

// ---------------------------------------------------------------------------
// User / group database
// ---------------------------------------------------------------------------

fn posix_getgrnam(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0);
    let r = with(|p| p.getgrnam(name.as_bytes()));
    finish(vm, a, r, |g| entries_value(g.entries()))
}

fn posix_getgrgid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(|p| p.getgrgid(a.int(0)));
    finish(vm, a, r, |g| entries_value(g.entries()))
}

fn posix_getpwnam(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0);
    let r = with(|p| p.getpwnam(name.as_bytes()));
    finish(vm, a, r, |pw| entries_value(pw.entries()))
}

fn posix_getpwuid(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(|p| p.getpwuid(a.int(0)));
    finish(vm, a, r, |pw| entries_value(pw.entries()))
}

fn posix_initgroups(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0);
    Ok(Value::Bool(with(|p| {
        p.initgroups(name.as_bytes(), a.int(1))
    })))
}

// ---------------------------------------------------------------------------
// Resource limits, errors, configuration
// ---------------------------------------------------------------------------

fn posix_getrlimit(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let res = match a.at(0) {
        Value::Null => None,
        _ => Some(a.int(0)),
    };
    let r = with(|p| p.getrlimit(res));
    finish(vm, a, r, |l| match l {
        Rlimits::All(list) => {
            let mut arr = ZArray::with_capacity(list.len());
            let t = arr.make_mut();
            for (k, v) in list {
                t.insert(ArrayKey::Str(ZStr::from(k)), rlimit_value(v));
            }
            Value::Array(arr)
        }
        Rlimits::Single(soft, hard) => Value::Array(ZArray::from_values([
            rlimit_value(soft),
            rlimit_value(hard),
        ])),
    })
}

fn posix_setrlimit(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with(|p| p.setrlimit(a.int(0), a.int(1), a.int(2)));
    true_on_ok(vm, a, r)
}

fn posix_get_last_error(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with(|p| p.get_last_error())))
}

fn posix_strerror(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(with(|p| p.strerror(a.int(0)))))
}

fn posix_sysconf(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with(|p| p.sysconf(a.int(0)))))
}

fn posix_pathconf(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = a.str(0);
    let r = with(|p| p.pathconf(path.as_bytes(), a.int(1)));
    finish(vm, a, r, Value::Int)
}

/// `RINIT`: `POSIX_G(last_error) = 0`.
fn request_startup(_vm: &mut Vm) {
    with(|p| *p = Posix::new());
}

impl Extension for PosixExt {
    fn name(&self) -> &'static str {
        "posix"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        for (name, value) in php_posix::constants() {
            m.constant(name, Value::Int(value));
        }
        m.on_request_startup(request_startup);
        m.function("posix_kill(int $process_id, int $signal): bool", posix_kill)
            .function("posix_getpid(): int", posix_getpid)
            .function("posix_getppid(): int", posix_getppid)
            .function("posix_getuid(): int", posix_getuid)
            .function("posix_setuid(int $user_id): bool", posix_setuid)
            .function("posix_geteuid(): int", posix_geteuid)
            .function("posix_seteuid(int $user_id): bool", posix_seteuid)
            .function("posix_getgid(): int", posix_getgid)
            .function("posix_setgid(int $group_id): bool", posix_setgid)
            .function("posix_getegid(): int", posix_getegid)
            .function("posix_setegid(int $group_id): bool", posix_setegid)
            .function("posix_getgroups(): array|false", posix_getgroups)
            .function("posix_getlogin(): string|false", posix_getlogin)
            .function("posix_getpgrp(): int", posix_getpgrp)
            .function("posix_setsid(): int", posix_setsid)
            .function(
                "posix_setpgid(int $process_id, int $process_group_id): bool",
                posix_setpgid,
            )
            .function("posix_getpgid(int $process_id): int|false", posix_getpgid)
            .function("posix_getsid(int $process_id): int|false", posix_getsid)
            .function("posix_uname(): array|false", posix_uname)
            .function("posix_times(): array|false", posix_times)
            .function("posix_ctermid(): string|false", posix_ctermid)
            .function(
                "posix_ttyname($file_descriptor): string|false",
                posix_ttyname,
            )
            .function("posix_isatty($file_descriptor): bool", posix_isatty)
            .function("posix_getcwd(): string|false", posix_getcwd)
            .function(
                "posix_mkfifo(string $filename, int $permissions): bool",
                posix_mkfifo,
            )
            .function(
                "posix_mknod(string $filename, int $flags, int $major = 0, int $minor = 0): bool",
                posix_mknod,
            )
            .function(
                "posix_access(string $filename, int $flags = 0): bool",
                posix_access,
            )
            .function(
                "posix_eaccess(string $filename, int $flags = 0): bool",
                posix_eaccess,
            )
            .function("posix_getgrnam(string $name): array|false", posix_getgrnam)
            .function("posix_getgrgid(int $group_id): array|false", posix_getgrgid)
            .function(
                "posix_getpwnam(string $username): array|false",
                posix_getpwnam,
            )
            .function("posix_getpwuid(int $user_id): array|false", posix_getpwuid)
            .function(
                "posix_getrlimit(?int $resource = null): array|false",
                posix_getrlimit,
            )
            .function(
                "posix_setrlimit(int $resource, int $soft_limit, int $hard_limit): bool",
                posix_setrlimit,
            )
            .function("posix_get_last_error(): int", posix_get_last_error)
            .function("posix_errno(): int", posix_get_last_error)
            .function("posix_strerror(int $error_code): string", posix_strerror)
            .function(
                "posix_initgroups(string $username, int $group_id): bool",
                posix_initgroups,
            )
            .function("posix_sysconf(int $conf_id): int", posix_sysconf)
            .function(
                "posix_pathconf(string $path, int $name): int|false",
                posix_pathconf,
            )
            .function(
                "posix_fpathconf($file_descriptor, int $name): int|false",
                posix_fpathconf,
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_conversion() {
        assert!(matches!(field_value(Field::Int(3)), Value::Int(3)));
        assert!(matches!(field_value(Field::Null), Value::Null));
        let Value::Array(a) = field_value(Field::List(vec![b"a".to_vec(), b"b".to_vec()])) else {
            panic!("expected array");
        };
        assert_eq!(a.len(), 2);
        assert!(a.is_list());
    }

    #[test]
    fn entries_keep_order() {
        let v = entries_value(vec![("b", Field::Int(1)), ("a", Field::Int(2))]);
        let Value::Array(a) = v else {
            panic!("expected array")
        };
        let keys: Vec<String> = a.keys().map(|k| format!("{k:?}")).collect();
        assert_eq!(keys, ["\"b\"", "\"a\""]);
    }

    #[test]
    fn state_reset() {
        with(|p| {
            let _ = p.kill(i64::from(i32::MAX), 0);
        });
        request_startup_state();
        assert_eq!(with(|p| p.get_last_error()), 0);
    }

    fn request_startup_state() {
        with(|p| *p = Posix::new());
    }
}
