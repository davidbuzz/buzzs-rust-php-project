//! The `Phar` / `PharData` instance methods.

use std::cell::RefCell;
use std::rc::Rc;

use php_ext_streams::bridge;
use php_phar::consts::DEFAULT_CONSTRUCT_FLAGS;
use php_phar::error::PharError;
use php_phar::object::{
    BuildItem, BuildResult, Converted, ExtractFiles, MTime, MetadataGet, PharObject,
};
use php_phar::registry as reg;
use php_runtime::{ArrayKey, Obj, Value, ZArray, ZStr};
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

use crate::state;
use crate::util::{bytes, check_paths, opt_int, opt_str, run, string_arg, throw, throw_chained};

type PharRef = Rc<RefCell<PharObject>>;

/// `$this` and its phar state.
fn this_state(a: &Args) -> (Obj, PharRef) {
    let this = a.this_obj();
    let is_data = php_vm::object::class_of(&this).instance_of_name(b"phardata");
    let st = state::phar(&this, is_data);
    (this, st)
}

fn st(a: &Args) -> PharRef {
    state::sweep();
    let st = this_state(a).1;
    refresh(&st);
    st
}

/// Re-points an object at the request-local copy of a cached
/// (`phar.cache_list`) archive once it was copied on write
/// (`phar_copy_cached_phar()` updates the registered objects).
fn refresh(st: &PharRef) {
    let Ok(ar) = st.borrow().archive() else {
        return;
    };
    let (persistent, fname) = {
        let a = ar.borrow();
        (a.is_persistent, a.fname.clone())
    };
    if !persistent {
        return;
    }
    let Some(cur) = reg::find_by_fname(&fname) else {
        return;
    };
    if Rc::ptr_eq(&cur, &ar) || cur.borrow().is_persistent {
        return;
    }
    let (is_data, info) = {
        let s = st.borrow();
        (s.is_data_class, s.info_class_is_pharfileinfo)
    };
    let mut n = PharObject::new(is_data);
    n.info_class_is_pharfileinfo = info;
    if n.construct(&fname, None, 0).is_ok() {
        *st.borrow_mut() = n;
    }
}

/// Calls an SPL method of `class` on `this` (the parent constructors).
pub fn call_spl(
    vm: &mut Vm,
    class: &str,
    method: &str,
    this: &Obj,
    args: &[Value],
) -> VmResult<Value> {
    php_ext_spl::iter::with_host(vm, |h| {
        php_spl::call_method(h, class, method, Some(this), args)
    })
}

/// The SPL info class of a `Phar` object (`setInfoClass()`).
fn info_class(this: &Obj) -> Option<ZStr> {
    php_ext_spl::host::spl_state_of(this).and_then(|s| s.file(|f| f.info_class.clone()).ok())
}

fn set_info_class(this: &Obj, class: &str) {
    if let Some(s) = php_ext_spl::host::spl_state_of(this) {
        let _ = s.file(|f| f.info_class = ZStr::from(class));
    }
}

/// `Phar::__construct()` / `PharData::__construct()`.
pub fn construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    state::sweep();
    let (this, st) = this_state(a);
    let is_data = st.borrow().is_data_class;
    check_paths(vm, a, &[0])?;
    let fname = a.str(0).as_bytes().to_vec();
    let flags = if a.has(1) {
        a.int(1)
    } else {
        DEFAULT_CONSTRUCT_FLAGS
    };
    let alias = opt_str(a, 2);
    let format = if is_data && a.has(3) { a.int(3) } else { 0 };
    let c = run(vm, || {
        st.borrow_mut().construct(&fname, alias.as_deref(), format)
    })?;
    call_spl(
        vm,
        "RecursiveDirectoryIterator",
        "__construct",
        &this,
        &[bytes(&c.iterator_path), Value::Int(flags)],
    )?;
    set_info_class(&this, "PharFileInfo");
    Ok(Value::Null)
}

/// `Phar::__destruct()`.
pub fn destruct(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    if let Some(this) = &a.this {
        state::release_phar(this);
    }
    Ok(Value::Null)
}

macro_rules! simple {
    ($name:ident, $method:ident, $conv:expr) => {
        #[doc = concat!("`Phar::", stringify!($method), "()`.")]
        pub fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let st = st(a);
            let r = run(vm, || st.borrow().$method())?;
            #[allow(clippy::redundant_closure_call)]
            Ok(($conv)(r))
        }
    };
}

simple!(get_alias, get_alias, |r: Option<Vec<u8>>| r
    .map_or(Value::Null, bytes));
simple!(get_path, get_path, |r: Vec<u8>| bytes(r));
simple!(get_version, get_version, |r: String| bytes(r));
simple!(get_modified, get_modified, Value::Bool);
simple!(is_buffering, is_buffering, Value::Bool);
simple!(is_writable, is_writable, Value::Bool);
simple!(has_metadata, has_metadata, Value::Bool);
simple!(get_stub, get_stub, |r: Vec<u8>| bytes(r));
simple!(is_compressed, is_compressed, |r: Option<i64>| r
    .map_or(Value::Bool(false), Value::Int));
simple!(start_buffering, start_buffering, |()| Value::Null);
simple!(stop_buffering, stop_buffering, |()| Value::Null);

/// `Phar::count()`.
pub fn count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    Ok(Value::Int(run(vm, || st.borrow().count())?))
}

/// `Phar::isFileFormat()`.
pub fn is_file_format(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let f = a.int(0);
    Ok(Value::Bool(run(vm, || st.borrow().is_file_format(f))?))
}

/// `Phar::getSignature()`.
pub fn get_signature(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    Ok(match run(vm, || st.borrow().get_signature())? {
        Some(sig) => Value::Array(ZArray::from_pairs([
            ("hash", bytes(&sig.hash)),
            ("hash_type", bytes(&sig.hash_type)),
        ])),
        None => Value::Bool(false),
    })
}

/// `Phar::delete()`.
pub fn delete(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    check_paths(vm, a, &[0])?;
    let name = a.str(0);
    Ok(Value::Bool(run(vm, || {
        st.borrow_mut().delete(name.as_bytes())
    })?))
}

/// `Phar::setAlias()`.
pub fn set_alias(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let alias = a.str(0);
    Ok(Value::Bool(run(vm, || {
        st.borrow_mut().set_alias(alias.as_bytes())
    })?))
}

/// The rest of a stream (`None` when unreadable) and its `fstat()` mode.
type StreamData = (Option<Vec<u8>>, Option<u32>);

/// Reads the rest of a stream resource; `None` if the value is not a
/// stream.
fn read_stream(vm: &mut Vm, v: &Value, maxlen: Option<usize>) -> VmResult<Option<StreamData>> {
    let Value::Resource(r) = v else {
        return Ok(None);
    };
    let Some(cell) = php_ext_streams::res::cell_of(r) else {
        return Ok(None);
    };
    let out = bridge::run(vm, || {
        let mut s = cell.get()?;
        let content = s.copy_to_mem(maxlen);
        let mode = s
            .stat()
            .and_then(|sb| u32::try_from(sb.mode & 0xFFFF_FFFF).ok());
        Some((content, mode))
    })?;
    Ok(out)
}

/// `Phar::setStub()`.
pub fn set_stub(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    // The checks that precede argument parsing.
    {
        let ar = match st.borrow().archive() {
            Ok(ar) => ar,
            Err(e) => return Err(throw(vm, &e)),
        };
        let ar = ar.borrow();
        if reg::readonly() && !ar.is_data {
            return Err(throw(
                vm,
                &PharError::unexpected("Cannot change stub, phar is read-only"),
            ));
        }
        if ar.is_data {
            return Err(throw(
                vm,
                &PharError::unexpected(if ar.is_tar {
                    "A Phar stub cannot be set in a plain tar archive"
                } else {
                    "A Phar stub cannot be set in a plain zip archive"
                }),
            ));
        }
    }
    let stub = a.at(0);
    if let Value::Resource(_) = &stub {
        if a.has(1) && !matches!(a.at(1), Value::Int(_)) {
            // "r|l" failed quietly: fall through to the string form.
        } else {
            vm.raise(
                8192,
                "Calling Phar::setStub(resource $stub, int $length) is deprecated",
            )?;
            let len = if a.has(1) { a.int(1) } else { -1 };
            let maxlen = usize::try_from(len).ok().filter(|&l| l > 0);
            return match read_stream(vm, &stub, maxlen)? {
                Some((content, _)) => {
                    let r = run(vm, || {
                        st.borrow_mut().set_stub_from_stream(content.as_deref())
                    })?;
                    Ok(Value::Bool(r))
                }
                None => Err(throw(
                    vm,
                    &PharError::unexpected("Cannot change stub, unable to read from input stream"),
                )),
            };
        }
    }
    if a.len() > 1 && a.has(1) {
        return Err(vm.throw(
            "ArgumentCountError",
            &format!(
                "Phar::setStub() expects exactly 1 argument, {} given",
                a.len()
            ),
        ));
    }
    let s = string_arg(vm, a, 0, false)?;
    Ok(Value::Bool(run(vm, || st.borrow_mut().set_stub(&s))?))
}

/// `Phar::setDefaultStub()`.
pub fn set_default_stub(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    check_paths(vm, a, &[0, 1])?;
    let index = opt_str(a, 0);
    let web = opt_str(a, 1);
    let r = run(vm, || {
        st.borrow_mut()
            .set_default_stub(index.as_deref(), web.as_deref())
    })?;
    Ok(Value::Bool(r))
}

/// `Phar::setSignatureAlgorithm()`.
pub fn set_signature_algorithm(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let algo = a.int(0);
    let key = opt_str(a, 1);
    run(vm, || {
        st.borrow_mut()
            .set_signature_algorithm(algo, key.as_deref())
    })?;
    Ok(Value::Null)
}

/// Unserializes metadata with the caller's options.
pub fn metadata_value(vm: &mut Vm, m: MetadataGet, options: Option<Value>) -> VmResult<Value> {
    match m {
        MetadataGet::None => Ok(Value::Null),
        MetadataGet::Value(v) => Ok(v),
        MetadataGet::Serialized(s) => {
            let mut args = vec![bytes(s)];
            if let Some(o) = options {
                args.push(o);
            }
            vm.call_function("unserialize", args)
        }
    }
}

/// `Phar::getMetadata()`.
pub fn get_metadata(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let opts = a.get(0);
    let m = run(vm, || st.borrow().get_metadata(opts.is_some()))?;
    metadata_value(vm, m, opts)
}

/// Serializes a metadata value (`serialize_metadata_or_throw`).
pub fn serialize_metadata(vm: &mut Vm, v: &Value) -> VmResult<Vec<u8>> {
    let s = vm.call_function("serialize", vec![v.clone()])?;
    Ok(match s {
        Value::Str(s) => s.as_bytes().to_vec(),
        _ => Vec::new(),
    })
}

/// `Phar::setMetadata()`.
pub fn set_metadata(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let v = a.at(0);
    let precheck = st
        .borrow()
        .archive()
        .map(|ar| reg::readonly() && !ar.borrow().is_data);
    if !matches!(precheck, Ok(false)) {
        // Let the library report the uninitialised / read-only error.
        run(vm, || st.borrow_mut().set_metadata(Value::Null, Vec::new()))?;
        return Ok(Value::Null);
    }
    let ser = serialize_metadata(vm, &v)?;
    run(vm, || st.borrow_mut().set_metadata(v, ser))?;
    Ok(Value::Null)
}

/// `Phar::delMetadata()`.
pub fn del_metadata(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    Ok(Value::Bool(run(vm, || st.borrow_mut().del_metadata())?))
}

/// `Phar::offsetExists()`.
pub fn offset_exists(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (this, st) = {
        state::sweep();
        this_state(a)
    };
    refresh(&st);
    let name = string_arg(vm, a, 0, true)?;
    let is_info = info_class(&this).is_none_or(|c| {
        vm.find_class(c.as_bytes())
            .is_some_and(|cls| cls.instance_of_name(b"pharfileinfo"))
    });
    st.borrow_mut().info_class_is_pharfileinfo = is_info;
    Ok(Value::Bool(run(vm, || st.borrow().offset_exists(&name))?))
}

/// `Phar::offsetGet()`.
pub fn offset_get(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (this, st) = {
        state::sweep();
        this_state(a)
    };
    refresh(&st);
    let name = string_arg(vm, a, 0, true)?;
    let path = run(vm, || st.borrow().offset_get(&name))?;
    let class = info_class(&this).unwrap_or_else(|| ZStr::from("PharFileInfo"));
    let Some(cls) = vm.lookup_class(&class)? else {
        return Err(vm.throw(
            "Error",
            &format!("Class \"{}\" not found", class.to_string_lossy()),
        ));
    };
    let o = vm.new_object_with_args(&cls, vec![bytes(path)])?;
    Ok(Value::Object(o))
}

/// `Phar::offsetSet()`.
pub fn offset_set(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let name = string_arg(vm, a, 0, true)?;
    let v = a.at(1);
    if let Value::Resource(_) = &v {
        let (content, mode) = read_stream(vm, &v, None)?.unwrap_or((None, None));
        let content = content.unwrap_or_default();
        run(vm, || {
            st.borrow_mut().offset_set_stream(&name, &content, mode)
        })?;
        return Ok(Value::Null);
    }
    let content = string_arg(vm, a, 1, false)?;
    run(vm, || st.borrow_mut().offset_set(&name, &content))?;
    Ok(Value::Null)
}

/// `Phar::offsetUnset()`.
pub fn offset_unset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let name = string_arg(vm, a, 0, true)?;
    run(vm, || st.borrow_mut().offset_unset(&name))?;
    Ok(Value::Null)
}

/// `Phar::addEmptyDir()`.
pub fn add_empty_dir(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    check_paths(vm, a, &[0])?;
    let d = a.str(0);
    run(vm, || st.borrow_mut().add_empty_dir(d.as_bytes()))?;
    Ok(Value::Null)
}

/// `Phar::addFile()`.
pub fn add_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    check_paths(vm, a, &[0])?;
    let file = a.str(0).as_bytes().to_vec();
    let local = opt_str(a, 1);
    run(vm, || {
        let mut ph = st.borrow_mut();
        ph.archive()?;
        let mut opened = None;
        let stream = php_streams::open::open_wrapper(&file, "rb", 0, &mut opened, None);
        let data = stream.map(|mut s| {
            let content = s.copy_to_mem(None).unwrap_or_default();
            let mode = s
                .stat()
                .and_then(|sb| u32::try_from(sb.mode & 0xFFFF_FFFF).ok());
            s.close();
            (content, mode)
        });
        ph.add_file(
            &file,
            local.as_deref(),
            data.as_ref().map(|(c, m)| (c.as_slice(), *m)),
        )
    })?;
    Ok(Value::Null)
}

/// `Phar::addFromString()`.
pub fn add_from_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    check_paths(vm, a, &[0])?;
    let name = a.str(0);
    let content = a.str(1);
    run(vm, || {
        st.borrow_mut()
            .add_from_string(name.as_bytes(), content.as_bytes())
    })?;
    Ok(Value::Null)
}

/// `Phar::copy()`.
pub fn copy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    check_paths(vm, a, &[0, 1])?;
    let from = a.str(0);
    let to = a.str(1);
    Ok(Value::Bool(run(vm, || {
        st.borrow_mut().copy(from.as_bytes(), to.as_bytes())
    })?))
}

/// `Phar::compressFiles()`.
pub fn compress_files(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let m = a.int(0);
    run(vm, || st.borrow_mut().compress_files(m))?;
    Ok(Value::Null)
}

/// `Phar::decompressFiles()`.
pub fn decompress_files(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    Ok(Value::Bool(run(vm, || st.borrow_mut().decompress_files())?))
}

/// Constructs the `Phar`/`PharData` object for a converted archive
/// (`phar_convert_to_other()`).
fn converted(vm: &mut Vm, c: &Converted) -> VmResult<Value> {
    let name = if c.is_data { "PharData" } else { "Phar" };
    let Some(cls) = vm.find_class(name.as_bytes()) else {
        return Ok(Value::Null);
    };
    let o = vm.new_object_with_args(&cls, vec![bytes(&c.fname)])?;
    Ok(Value::Object(o))
}

/// `Phar::compress()`.
pub fn compress(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let m = a.int(0);
    let ext = opt_str(a, 1);
    let c = run(vm, || st.borrow_mut().compress(m, ext.as_deref()))?;
    converted(vm, &c)
}

/// `Phar::decompress()`.
pub fn decompress(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let ext = opt_str(a, 0);
    let c = run(vm, || st.borrow_mut().decompress(ext.as_deref()))?;
    converted(vm, &c)
}

/// `Phar::convertToExecutable()`.
pub fn convert_to_executable(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let (f, m, ext) = (opt_int(a, 0), opt_int(a, 1), opt_str(a, 2));
    let c = run(vm, || {
        st.borrow_mut().convert_to_executable(f, m, ext.as_deref())
    })?;
    converted(vm, &c)
}

/// `Phar::convertToData()`.
pub fn convert_to_data(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    let (f, m, ext) = (opt_int(a, 0), opt_int(a, 1), opt_str(a, 2));
    let c = run(vm, || st.borrow_mut().convert_to_data(f, m, ext.as_deref()))?;
    converted(vm, &c)
}

/// `Phar::extractTo()`.
pub fn extract_to(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    check_paths(vm, a, &[0])?;
    let to = a.str(0).as_bytes().to_vec();
    let files = match a.at(1) {
        Value::Str(s) => ExtractFiles::One(s.as_bytes().to_vec()),
        Value::Array(arr) => ExtractFiles::List(
            arr.iter()
                .map(|(_, v)| match v.deref() {
                    Value::Str(s) => Some(s.as_bytes().to_vec()),
                    _ => None,
                })
                .collect(),
        ),
        _ => ExtractFiles::All,
    };
    let overwrite = a.has(2) && a.bool(2);
    Ok(Value::Bool(run(vm, || {
        st.borrow().extract_to(&to, &files, overwrite)
    })?))
}

fn build_result(r: BuildResult) -> Value {
    Value::Array(ZArray::from_pairs(
        r.into_iter()
            .map(|(k, v)| (ArrayKey::Str(ZStr::from(k)), bytes(v))),
    ))
}

/// `Phar::buildFromDirectory()`.
pub fn build_from_directory(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    check_paths(vm, a, &[0])?;
    let dir = a.str(0).as_bytes().to_vec();
    let pattern = if a.has(1) {
        a.str(1).as_bytes().to_vec()
    } else {
        Vec::new()
    };
    let filter = |path: &[u8]| -> bool {
        bridge::with_vm(|vm| {
            match vm.call_function("preg_match", vec![bytes(&pattern), bytes(path)]) {
                Ok(Value::Int(n)) => n > 0,
                Ok(_) => false,
                Err(t) => {
                    bridge::set_pending(t);
                    false
                }
            }
        })
        .unwrap_or(true)
    };
    let filter_ref: Option<php_phar::object::PathFilter<'_>> = if pattern.is_empty() {
        None
    } else {
        Some(&filter)
    };
    let r = run(vm, || {
        st.borrow_mut().build_from_directory(&dir, filter_ref)
    })?;
    Ok(build_result(r))
}

/// One element of the iterator given to `buildFromIterator()`, and the
/// exception a user `getMTime()` threw (chained to the build error).
type Item = (Result<BuildItem, PharError>, Option<php_vm::ext::Throw>);

fn build_item(
    vm: &mut Vm,
    it: &mut dyn php_spl::host::ObjectIterator,
    value: &Value,
    need_key: bool,
    class: &str,
) -> VmResult<Item> {
    let mut key = |vm: &mut Vm| -> VmResult<Option<Vec<u8>>> {
        let k = php_ext_spl::iter::with_host(vm, |h| it.key(h))?;
        Ok(match k.map(|k| k.deref()) {
            Some(Value::Str(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        })
    };
    let item = match value.deref() {
        Value::Str(s) => BuildItem::Path {
            value: s.as_bytes().to_vec(),
            key: if need_key { key(vm)? } else { None },
        },
        v @ Value::Resource(_) => {
            let Some((content, mode)) = read_stream(vm, &v, None)? else {
                let e = PharError::bad_method(format!(
                    "Iterator {class} returned an invalid stream handle"
                ));
                return Ok((Err(e), None));
            };
            BuildItem::Stream {
                key: key(vm)?,
                content: content.unwrap_or_default(),
                mode,
            }
        }
        Value::Object(obj) if php_vm::object::class_of(&obj).instance_of_name(b"splfileinfo") => {
            let user = php_vm::object::class_of(&obj).decl.is_some();
            let pathname = match vm.call_method_by_name(&obj, "getPathname", vec![]) {
                Ok(p) => p.deref(),
                Err(t) => {
                    return Err(throw_chained(
                        vm,
                        "UnexpectedValueException",
                        "getPathname() must return a string",
                        t,
                    ))
                }
            };
            let Value::Str(pathname) = pathname else {
                let e = PharError::unexpected("getPathname() must return a string");
                return Ok((Err(e), None));
            };
            let mut prev = None;
            let mtime = if user {
                match vm.call_method_by_name(&obj, "getMTime", vec![]) {
                    Ok(v) => match v.deref() {
                        Value::Int(t) => MTime::Int(t),
                        _ => MTime::NotInt,
                    },
                    Err(t) => {
                        prev = Some(t);
                        MTime::NotInt
                    }
                }
            } else {
                MTime::Internal
            };
            return Ok((
                Ok(BuildItem::FileInfo {
                    pathname: pathname.as_bytes().to_vec(),
                    mtime,
                }),
                prev,
            ));
        }
        _ => BuildItem::Invalid,
    };
    Ok((Ok(item), None))
}

/// `Phar::buildFromIterator()`.
pub fn build_from_iterator(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = st(a);
    check_paths(vm, a, &[1])?;
    let Value::Object(iter) = a.at(0) else {
        return Ok(Value::Null);
    };
    let base = opt_str(a, 1).unwrap_or_default();
    let class = php_vm::object::display_name(&iter.class_name()).to_string_lossy();
    let mut bs = run(vm, || st.borrow_mut().build_from_iterator_start(&base))?;
    let mut it =
        php_ext_spl::iter::with_host(vm, |h| php_spl::iter::get_iterator(h, &iter, false))?;
    php_ext_spl::iter::with_host(vm, |h| it.rewind(h))?;
    while php_ext_spl::iter::with_host(vm, |h| it.valid(h))? {
        let cur = php_ext_spl::iter::with_host(vm, |h| it.current(h))?;
        let Some(cur) = cur else {
            return Err(throw(
                vm,
                &PharError::unexpected(format!("Iterator {class} returned no value")),
            ));
        };
        let (item, prev) = build_item(vm, it.as_mut(), &cur, base.is_empty(), &class)?;
        let r = match item {
            Ok(i) => bridge::run(vm, || bs.add(i, &class))?,
            Err(e) => Err(e),
        };
        if let Err(e) = r {
            return Err(match prev {
                Some(p) => throw_chained(vm, e.class.name(), &e.message, p),
                None => throw(vm, &e),
            });
        }
        php_ext_spl::iter::with_host(vm, |h| it.next(h))?;
    }
    let r = run(vm, || bs.finish())?;
    Ok(build_result(r))
}

#[cfg(test)]
mod tests {
    use super::build_result;
    use php_runtime::Value;

    #[test]
    fn build_result_is_keyed_by_entry() {
        let v = build_result(vec![(b"a".to_vec(), b"/x/a".to_vec())]);
        let Value::Array(a) = v else {
            panic!("not an array")
        };
        assert_eq!(a.len(), 1);
    }
}
