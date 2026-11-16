// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/standard/filestat.c`: the stat family, permissions, ownership,
//! `touch()` and disk space.

use php_runtime::Value;
use php_streams::filestat::{self as lib, FsType, Principal};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::bridge;
use crate::files::arg_error;
use crate::res::path_arg;

fn stat_fn(vm: &mut Vm, a: &Args, ty: FsType) -> VmResult<Value> {
    let path = path_arg(vm, a, 0)?;
    let r = bridge::run(vm, || lib::php_stat(&path, ty))?;
    Ok(bridge::from_lib(&r.to_value()))
}

macro_rules! stat_fns {
    ($($name:ident => $ty:ident),* $(,)?) => {
        $(fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            stat_fn(vm, a, FsType::$ty)
        })*
    };
}

stat_fns! {
    fileperms => Perms, fileinode => Inode, filesize => Size, fileowner => Owner,
    filegroup => Group, fileatime => Atime, filemtime => Mtime, filectime => Ctime,
    filetype => Type, is_writable => IsW, is_readable => IsR, is_executable => IsX,
    is_file => IsFile, is_dir => IsDir, is_link => IsLink, file_exists => Exists,
    lstat => Lstat, stat => Stat,
}

fn clearstatcache(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let clear = a.bool(0);
    let f = if a.has(1) {
        Some(path_arg(vm, a, 1)?)
    } else {
        None
    };
    lib::clearstatcache(clear, f.as_deref());
    Ok(Value::Null)
}

fn principal(a: &Args, n: usize) -> Principal {
    match a.at(n) {
        Value::Int(i) => Principal::Id(i),
        v => Principal::Name(
            v.as_str()
                .map(|s| s.as_bytes().to_vec())
                .unwrap_or_default(),
        ),
    }
}

fn chown_fn(vm: &mut Vm, a: &Args, f: fn(&[u8], &Principal) -> bool) -> VmResult<Value> {
    let path = path_arg(vm, a, 0)?;
    let who = principal(a, 1);
    let r = bridge::run(vm, || f(&path, &who))?;
    Ok(Value::Bool(r))
}

fn chown(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    chown_fn(vm, a, lib::chown)
}

fn chgrp(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    chown_fn(vm, a, lib::chgrp)
}

fn lchown(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    chown_fn(vm, a, lib::lchown)
}

fn lchgrp(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    chown_fn(vm, a, lib::lchgrp)
}

fn chmod(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = path_arg(vm, a, 0)?;
    let mode = a.int(1);
    let r = bridge::run(vm, || lib::chmod(&path, mode))?;
    Ok(Value::Bool(r))
}

fn touch(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = path_arg(vm, a, 0)?;
    let mtime = a.opt(1).map(|_| a.int(1));
    let atime = a.opt(2).map(|_| a.int(2));
    match bridge::run(vm, || lib::touch(&path, mtime, atime))? {
        Ok(b) => Ok(Value::Bool(b)),
        Err(e) => Err(arg_error(vm, a, &e)),
    }
}

fn disk_fn(vm: &mut Vm, a: &Args, total: bool) -> VmResult<Value> {
    let path = path_arg(vm, a, 0)?;
    let r = bridge::run(vm, || lib::disk_space(&path, total))?;
    Ok(r.map_or(Value::Bool(false), Value::Float))
}

fn disk_free_space(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    disk_fn(vm, a, false)
}

fn disk_total_space(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    disk_fn(vm, a, true)
}

/// Registers the functions of this module.
pub fn register(m: &mut ModuleBuilder<'_>) {
    for (name, ret, f) in [
        ("fileatime", "int|false", fileatime as php_vm::ext::NativeFn),
        ("filectime", "int|false", filectime),
        ("filegroup", "int|false", filegroup),
        ("fileinode", "int|false", fileinode),
        ("filemtime", "int|false", filemtime),
        ("fileowner", "int|false", fileowner),
        ("fileperms", "int|false", fileperms),
        ("filesize", "int|false", filesize),
        ("filetype", "string|false", filetype),
        ("file_exists", "bool", file_exists),
        ("is_writable", "bool", is_writable),
        ("is_writeable", "bool", is_writable),
        ("is_readable", "bool", is_readable),
        ("is_executable", "bool", is_executable),
        ("is_file", "bool", is_file),
        ("is_dir", "bool", is_dir),
        ("is_link", "bool", is_link),
        ("stat", "array|false", stat),
        ("lstat", "array|false", lstat),
    ] {
        m.function(&format!("{name}(string $filename): {ret}"), f);
    }
    m.function_static("chown(string $filename, string|int $user): bool", chown)
        .function_static("chgrp(string $filename, string|int $group): bool", chgrp)
        .function_static("lchown(string $filename, string|int $user): bool", lchown)
        .function_static("lchgrp(string $filename, string|int $group): bool", lchgrp)
        .function_static("chmod(string $filename, int $permissions): bool", chmod)
        .function_static(
            "touch(string $filename, ?int $mtime = null, ?int $atime = null): bool",
            touch,
        )
        .function_static(
            "clearstatcache(bool $clear_realpath_cache = false, string $filename = \"\"): void",
            clearstatcache,
        )
        .function_static(
            "disk_total_space(string $directory): float|false",
            disk_total_space,
        )
        .function_static(
            "disk_free_space(string $directory): float|false",
            disk_free_space,
        )
        .function_static(
            "diskfreespace(string $directory): float|false",
            disk_free_space,
        );
}
