//! `ext/standard/dir.c`: directory handles, `scandir()`, `glob()` and the
//! `Directory` class.

use std::cell::RefCell;
use std::rc::Rc;

use php_runtime::{Obj, Resource, Value, ZArray};
use php_streams::open::{self, ScandirSort};
use php_streams::stream::flags;
use php_streams::wrapper::REPORT_ERRORS;
use php_streams::{glob, paths, state};
use php_vm::class::Class;
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::bridge::{self, bytes};
use crate::res::{self, context_arg, fname, path_arg, StreamCell};

thread_local! {
    /// `DIRG(default_dir)`: the last opened directory handle.
    static DEFAULT_DIR: RefCell<Option<Resource>> = const { RefCell::new(None) };
    static DIRECTORY_CLASS: RefCell<Option<Rc<Class>>> = const { RefCell::new(None) };
}

fn set_default_dir(r: Option<Resource>) {
    DEFAULT_DIR.with(|d| *d.borrow_mut() = r);
}

fn is_default_dir(r: &Resource) -> bool {
    DEFAULT_DIR.with(|d| d.borrow().as_ref().is_some_and(|x| x.handle() == r.handle()))
}

/// Opens a directory stream; returns the resource.
fn do_opendir(vm: &mut Vm, a: &Args) -> VmResult<Option<Resource>> {
    let path = path_arg(vm, a, 0)?;
    let ctx = context_arg(vm, a, 1, false)?;
    crate::userwrap::clear_last_object();
    let s = bridge::run(vm, || open::opendir(&path, REPORT_ERRORS, ctx.as_ref()))?;
    let Some(mut s) = s else {
        return Ok(None);
    };
    s.add_flags(flags::NO_FCLOSE);
    let data = if s.ops().is_userspace() {
        crate::userwrap::take_last_object()
    } else {
        None
    };
    let Value::Resource(r) = res::new_stream(s, data) else {
        return Ok(None);
    };
    set_default_dir(Some(r.clone()));
    Ok(Some(r))
}

fn opendir(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(do_opendir(vm, a)?.map_or(Value::Bool(false), Value::Resource))
}

fn dir(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(r) = do_opendir(vm, a)? else {
        return Ok(Value::Bool(false));
    };
    let Some(cls) = DIRECTORY_CLASS.with(|c| c.borrow().clone()) else {
        return Ok(Value::Bool(false));
    };
    let o = vm.new_object(&cls)?;
    vm.set_property_raw(&o, "path", Value::Str(a.str(0)));
    vm.set_property_raw(&o, "handle", Value::Resource(r));
    Ok(Value::Object(o))
}

/// `php_dir_get_directory_stream_from_user_arg`.
fn dir_from_arg(vm: &mut Vm, a: &Args) -> VmResult<(Resource, Rc<StreamCell>)> {
    let r = match a.opt(0) {
        None => {
            vm.php_deprecated(
                "Passing null is deprecated, instead the last opened directory stream should be provided",
            )?;
            match DEFAULT_DIR.with(|d| d.borrow().clone()) {
                Some(r) => r,
                None => return Err(vm.throw("TypeError", "No resource supplied")),
            }
        }
        Some(_) => res::resource_arg(vm, a, 0)?,
    };
    let cell = res::stream_of_resource(vm, a, &r)?;
    let is_dir = cell.get().is_some_and(|s| s.has_flag(flags::IS_DIR));
    if !is_dir {
        return Err(a.type_error(vm, 1, "must be a valid Directory resource"));
    }
    Ok((r, cell))
}

/// `php_dir_get_directory_stream_from_this`.
fn dir_from_this(vm: &mut Vm, a: &Args) -> VmResult<(Resource, Rc<StreamCell>)> {
    let Some(this) = a.this.clone() else {
        return Err(vm.throw("Error", "Internal directory stream has been altered"));
    };
    let Value::Resource(r) = vm.get_property(&this, "handle")?.deref() else {
        return Err(vm.throw("Error", "Internal directory stream has been altered"));
    };
    let cell = match res::cell_of(&r) {
        Some(c) if !r.is_closed() && !c.is_closed() => c,
        _ => {
            return Err(vm.throw(
                "TypeError",
                &format!(
                    "Directory::{}(): cannot use Directory resource after it has been closed",
                    fname(a)
                ),
            ))
        }
    };
    if !cell.get().is_some_and(|s| s.has_flag(flags::IS_DIR)) {
        return Err(vm.throw("Error", "Internal directory stream has been altered"));
    }
    Ok((r, cell))
}

fn close_dir(vm: &mut Vm, r: &Resource, cell: &StreamCell) -> VmResult<Value> {
    if let Some(mut s) = cell.take() {
        r.close();
        bridge::run(vm, || s.close())?;
    } else {
        r.close();
    }
    if is_default_dir(r) {
        set_default_dir(None);
    }
    Ok(Value::Null)
}

fn read_dir(vm: &mut Vm, cell: &StreamCell) -> VmResult<Value> {
    let r = res::with_stream(vm, cell, php_streams::stream::Stream::readdir)?.flatten();
    Ok(r.map_or(Value::Bool(false), bytes))
}

fn rewind_dir(vm: &mut Vm, cell: &StreamCell) -> VmResult<Value> {
    res::with_stream(vm, cell, php_streams::stream::Stream::rewinddir)?;
    Ok(Value::Null)
}

fn closedir(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (r, cell) = dir_from_arg(vm, a)?;
    close_dir(vm, &r, &cell)
}

fn readdir(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, cell) = dir_from_arg(vm, a)?;
    read_dir(vm, &cell)
}

fn rewinddir(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, cell) = dir_from_arg(vm, a)?;
    rewind_dir(vm, &cell)
}

fn m_close(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (r, cell) = dir_from_this(vm, a)?;
    close_dir(vm, &r, &cell)
}

fn m_read(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, cell) = dir_from_this(vm, a)?;
    read_dir(vm, &cell)
}

fn m_rewind(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (_, cell) = dir_from_this(vm, a)?;
    rewind_dir(vm, &cell)
}

fn m_construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw("Error", "Cannot directly construct Directory, use dir() instead"))
}

fn scandir(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = path_arg(vm, a, 0)?;
    if path.is_empty() {
        return Err(a.value_error(vm, 1, "cannot be empty"));
    }
    let sort = match a.int(1) {
        0 => ScandirSort::Ascending,
        1 => ScandirSort::Descending,
        2 => ScandirSort::None,
        _ => {
            return Err(a.value_error(
                vm,
                2,
                "must be one of the SCANDIR_SORT_ASCENDING, SCANDIR_SORT_DESCENDING, or SCANDIR_SORT_NONE constants",
            ))
        }
    };
    let ctx = if a.opt(2).is_some() {
        context_arg(vm, a, 2, false)?
    } else {
        None
    };
    let r = bridge::run(vm, || {
        let r = open::scandir(&path, sort, ctx.as_ref());
        (r, std::io::Error::last_os_error())
    })?;
    match r {
        (Some(names), _) => {
            let mut arr = ZArray::with_capacity(names.len());
            for n in names {
                let _ = arr.make_mut().push(bytes(n));
            }
            Ok(Value::Array(arr))
        }
        (None, err) => {
            let errno = err.raw_os_error().unwrap_or(0);
            vm.php_warning(&format!(
                "(errno {errno}): {}",
                php_streams::sys::strerror(errno)
            ))?;
            Ok(Value::Bool(false))
        }
    }
}

fn glob_fn(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let pattern = path_arg(vm, a, 0)?;
    let flags = a.int(1);
    let r = bridge::run(vm, || {
        let restricted = !state::settings().open_basedir.is_empty();
        glob::glob_with_basedir(&pattern, flags, |p| {
            !restricted || paths::check_open_basedir_ex(p, false)
        })
    })?;
    match r {
        Ok(list) => {
            let mut arr = ZArray::with_capacity(list.len());
            for p in list {
                let _ = arr.make_mut().push(bytes(p));
            }
            Ok(Value::Array(arr))
        }
        Err(e) => {
            if let Some(w) = e.warning() {
                vm.php_warning(&w)?;
            }
            Ok(Value::Bool(false))
        }
    }
}

/// Registers the functions, constants and the `Directory` class.
pub fn register(m: &mut ModuleBuilder<'_>) {
    m.function("opendir(string $directory, $context = null)", opendir)
        .function("dir(string $directory, $context = null): Directory|false", dir)
        .function("closedir($dir_handle = null): void", closedir)
        .function("rewinddir($dir_handle = null): void", rewinddir)
        .function("readdir($dir_handle = null): string|false", readdir)
        .function(
            "scandir(string $directory, int $sorting_order = SCANDIR_SORT_ASCENDING, $context = null): array|false",
            scandir,
        )
        .function("glob(string $pattern, int $flags = 0): array|false", glob_fn);
    for (name, v) in [
        ("GLOB_BRACE", glob::GLOB_BRACE),
        ("GLOB_ERR", glob::GLOB_ERR),
        ("GLOB_MARK", glob::GLOB_MARK),
        ("GLOB_NOCHECK", glob::GLOB_NOCHECK),
        ("GLOB_NOESCAPE", glob::GLOB_NOESCAPE),
        ("GLOB_NOSORT", glob::GLOB_NOSORT),
        ("GLOB_ONLYDIR", glob::GLOB_ONLYDIR),
        ("GLOB_AVAILABLE_FLAGS", glob::GLOB_AVAILABLE_FLAGS),
    ] {
        m.constant(name, Value::Int(i64::from(v)));
    }
    m.constant("SCANDIR_SORT_ASCENDING", Value::Int(0))
        .constant("SCANDIR_SORT_DESCENDING", Value::Int(1))
        .constant("SCANDIR_SORT_NONE", Value::Int(2));
    let cls = ClassBuilder::new("Directory")
        .flags(class_flags::FINAL | class_flags::NOT_SERIALIZABLE | class_flags::NO_CLONE)
        .prop("path", acc::PUBLIC | acc::READONLY, Some("string"), None)
        .prop("handle", acc::PUBLIC | acc::READONLY, Some("mixed"), None)
        .method("__construct()", acc::PRIVATE, m_construct)
        .method("close(): void", acc::PUBLIC, m_close)
        .method("rewind(): void", acc::PUBLIC, m_rewind)
        .method("read(): string|false", acc::PUBLIC, m_read)
        .register(m.vm());
    DIRECTORY_CLASS.with(|c| *c.borrow_mut() = Some(cls));
}

/// Whether `o` is a `Directory` instance (for tests).
#[allow(dead_code)]
fn is_directory(o: &Obj) -> bool {
    o.class_name().as_bytes() == b"Directory"
}
