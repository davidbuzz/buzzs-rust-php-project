// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The SPL functions (`php_spl.c`): `spl_object_hash()`, `spl_object_id()`,
//! `class_implements()`, `class_parents()`, `class_uses()`,
//! `spl_classes()`, the autoload registry and `spl_autoload()`, and the
//! `iterator_*()` functions.

use crate::args::Args;
use crate::host::SplHost;
use crate::observer::object_hash;
use php_runtime::{
    ArrayKey, ErrorLevel, HashTable, Obj, PropertyPurpose, RtResult, RuntimeError, Value, ZArray,
    ZStr,
};
use std::cell::RefCell;

/// The default `spl_autoload()` extensions.
pub const DEFAULT_EXTENSIONS: &str = ".inc,.php";

/// The autoload registry (`zend_autoload.c` class loaders) and the
/// `spl_autoload_extensions()` setting; request-global.
#[derive(Debug, Default, Clone)]
pub struct Autoload {
    /// Registered loaders in call order. `None` stands for the default
    /// `spl_autoload` loader.
    pub loaders: Vec<Value>,
    /// `spl_autoload_extensions()`, `None` for the default.
    pub extensions: Option<ZStr>,
}

thread_local! {
    static AUTOLOAD: RefCell<Autoload> = RefCell::new(Autoload::default());
}

/// Resets the request-global state (`PHP_RINIT(spl)`).
pub fn reset_request() {
    AUTOLOAD.with(|a| *a.borrow_mut() = Autoload::default());
    crate::observer::reset_get_hash_depth();
}

/// A snapshot of the registered loaders.
#[must_use]
pub fn autoloaders() -> Vec<Value> {
    AUTOLOAD.with(|a| a.borrow().loaders.clone())
}

/// A canonical identity for a callable, used to detect duplicate
/// registrations (`zend_fcc_equals`).
fn callable_identity(host: &mut dyn SplHost, v: &Value) -> Vec<u8> {
    match v.deref() {
        Value::Str(s) => {
            let b = s.as_bytes();
            let b = b.strip_prefix(b"\\").unwrap_or(b);
            b.to_ascii_lowercase()
        }
        Value::Array(a) if a.len() == 2 => {
            let target = match a.get_int(0).map(Value::deref) {
                Some(Value::Object(o)) => format!("#{}", o.handle()).into_bytes(),
                Some(Value::Str(c)) => {
                    let b = c.as_bytes();
                    b.strip_prefix(b"\\").unwrap_or(b).to_ascii_lowercase()
                }
                _ => Vec::new(),
            };
            let method = match a.get_int(1).map(Value::deref) {
                Some(Value::Str(m)) => m.as_bytes().to_ascii_lowercase(),
                _ => Vec::new(),
            };
            let mut out = target;
            out.extend_from_slice(b"::");
            out.extend_from_slice(&method);
            out
        }
        Value::Object(o) if o.class_name().eq_ignore_ascii_case(b"Closure") => {
            closure_identity(host, &o).unwrap_or_else(|| format!("#{}", o.handle()).into_bytes())
        }
        // An invokable object stands for its `__invoke()` method.
        Value::Object(o) => format!("#{}::__invoke", o.handle()).into_bytes(),
        _ => Vec::new(),
    }
}

/// `zend_fcc_equals` for first-class callables: a closure created by
/// `f(...)` / `$obj->m(...)` / `C::m(...)` is the same autoloader as the
/// function or method it wraps (its debug table names the `function`
/// and, for instance methods, the bound `this`). Real closures (debug
/// table with `name`) are identified by the object.
fn closure_identity(host: &mut dyn SplHost, o: &Obj) -> Option<Vec<u8>> {
    let props = host.object_properties(o, PropertyPurpose::Debug).ok()??;
    let Some(Value::Str(func)) = props.get_str(b"function").map(Value::deref) else {
        return None;
    };
    let func = func.as_bytes().to_ascii_lowercase();
    match props.get_str(b"this").map(Value::deref) {
        Some(Value::Object(this)) => {
            let method = func
                .windows(2)
                .rposition(|w| w == b"::")
                .map_or(&func[..], |p| &func[p + 2..]);
            let mut out = format!("#{}::", this.handle()).into_bytes();
            out.extend_from_slice(method);
            Some(out)
        }
        _ => Some(func),
    }
}

/// The identities of the registered loaders (computed outside the
/// registry borrow: identifying a closure asks the host).
fn loader_identities(host: &mut dyn SplHost) -> Vec<Vec<u8>> {
    autoloaders()
        .iter()
        .map(|l| {
            if l.is_null() {
                b"spl_autoload".to_vec()
            } else {
                callable_identity(host, l)
            }
        })
        .collect()
}

/// The callable as `spl_autoload_functions()` reports it: `"C::m"`
/// strings become `[class, method]` arrays.
fn normalize_callable(v: &Value) -> Value {
    if let Value::Str(s) = v.deref() {
        let b = s.as_bytes();
        if let Some(p) = b.windows(2).position(|w| w == b"::") {
            let class = b[..p].strip_prefix(b"\\").unwrap_or(&b[..p]);
            return Value::Array(ZArray::from_values([
                Value::Str(ZStr::from(class)),
                Value::Str(ZStr::from(&b[p + 2..])),
            ]));
        }
    }
    v.deref()
}

/// Dispatches an SPL function by lowercase name.
pub(crate) fn call(host: &mut dyn SplHost, name: &str, args: &[Value]) -> Option<RtResult<Value>> {
    Some(match name {
        "spl_object_hash" => (|| {
            let a = Args::new("spl_object_hash", args, &["object"], 1, Some(1))?;
            let o = a.object(0)?;
            Ok(Value::Str(object_hash(&o)))
        })(),
        "spl_object_id" => (|| {
            let a = Args::new("spl_object_id", args, &["object"], 1, Some(1))?;
            let o = a.object(0)?;
            Ok(Value::Int(i64::from(o.handle())))
        })(),
        "class_implements" | "class_parents" | "class_uses" => class_list(host, name, args),
        "spl_classes" => Args::none("spl_classes", args).map(|()| spl_classes()),
        "iterator_to_array" => crate::iter::iterator_to_array(host, args),
        "iterator_count" => crate::iter::iterator_count(host, args),
        "iterator_apply" => crate::iter::iterator_apply(host, args),
        "spl_autoload_extensions" => (|| {
            let a = Args::new(
                "spl_autoload_extensions",
                args,
                &["file_extensions"],
                0,
                Some(1),
            )?;
            if let Some(e) = a.str_or_null(host, 0)? {
                AUTOLOAD.with(|s| s.borrow_mut().extensions = Some(e));
            }
            Ok(Value::Str(AUTOLOAD.with(|s| {
                s.borrow()
                    .extensions
                    .clone()
                    .unwrap_or_else(|| ZStr::from(DEFAULT_EXTENSIONS))
            })))
        })(),
        "spl_autoload" => (|| {
            let a = Args::new(
                "spl_autoload",
                args,
                &["class", "file_extensions"],
                1,
                Some(2),
            )?;
            let class = a.str(host, 0)?;
            let exts = a.str_or_null(host, 1)?;
            spl_autoload(host, &class, exts.as_ref())?;
            Ok(Value::Null)
        })(),
        "spl_autoload_register" => autoload_register(host, args),
        "spl_autoload_unregister" => autoload_unregister(host, args),
        "spl_autoload_functions" => Args::none("spl_autoload_functions", args)
            .map(|()| Value::Array(ZArray::from_values(autoloaders()))),
        "spl_autoload_call" => (|| {
            let a = Args::new("spl_autoload_call", args, &["class"], 1, Some(1))?;
            let class = a.str(host, 0)?;
            perform_autoload(host, &class)?;
            Ok(Value::Null)
        })(),
        _ => return None,
    })
}

fn class_list(host: &mut dyn SplHost, name: &str, args: &[Value]) -> RtResult<Value> {
    let a = Args::new(name, args, &["object_or_class", "autoload"], 1, Some(2))?;
    let autoload = a.opt_bool(host, 1, true)?;
    let info = match a.raw(0) {
        Value::Object(o) => host.object_class_info(&o),
        Value::Str(s) => {
            if let Some(i) = host.class_info(&s, autoload) {
                i
            } else {
                host.diagnostic(
                    ErrorLevel::Warning,
                    &format!(
                        "{name}(): Class {s} does not exist{}",
                        if autoload {
                            " and could not be loaded"
                        } else {
                            ""
                        }
                    ),
                )?;
                return Ok(Value::Bool(false));
            }
        }
        _ => return Err(a.type_error(0, "object|string")),
    };
    let names = match name {
        "class_parents" => info.parents,
        "class_implements" => info.interfaces,
        _ => info.traits,
    };
    let mut out = HashTable::new();
    for n in names {
        let _ = out.add(ArrayKey::Str(n.clone()), Value::Str(n));
    }
    Ok(Value::Array(ZArray::from_table(out)))
}

/// `spl_classes()`: SPL's classes and interfaces, keyed by name, in the
/// order of `SPL_LIST_CLASSES`.
#[must_use]
pub fn spl_classes() -> Value {
    const LIST: &[&str] = &[
        "AppendIterator",
        "ArrayIterator",
        "ArrayObject",
        "BadFunctionCallException",
        "BadMethodCallException",
        "CachingIterator",
        "CallbackFilterIterator",
        "DirectoryIterator",
        "DomainException",
        "EmptyIterator",
        "FilesystemIterator",
        "FilterIterator",
        "GlobIterator",
        "InfiniteIterator",
        "InvalidArgumentException",
        "IteratorIterator",
        "LengthException",
        "LimitIterator",
        "LogicException",
        "MultipleIterator",
        "NoRewindIterator",
        "OuterIterator",
        "OutOfBoundsException",
        "OutOfRangeException",
        "OverflowException",
        "ParentIterator",
        "RangeException",
        "RecursiveArrayIterator",
        "RecursiveCachingIterator",
        "RecursiveCallbackFilterIterator",
        "RecursiveDirectoryIterator",
        "RecursiveFilterIterator",
        "RecursiveIterator",
        "RecursiveIteratorIterator",
        "RecursiveRegexIterator",
        "RecursiveTreeIterator",
        "RegexIterator",
        "RuntimeException",
        "SeekableIterator",
        "SplDoublyLinkedList",
        "SplFileInfo",
        "SplFileObject",
        "SplFixedArray",
        "SplHeap",
        "SplMinHeap",
        "SplMaxHeap",
        "SplObjectStorage",
        "SplObserver",
        "SplPriorityQueue",
        "SplQueue",
        "SplStack",
        "SplSubject",
        "SplTempFileObject",
        "UnderflowException",
        "UnexpectedValueException",
    ];
    let mut out = HashTable::new();
    for n in LIST {
        out.insert(ArrayKey::from(*n), Value::from(*n));
    }
    Value::Array(ZArray::from_table(out))
}

/// `spl_autoload()`: tries `lowercase(class) . ext` for each extension.
pub fn spl_autoload(host: &mut dyn SplHost, class: &ZStr, exts: Option<&ZStr>) -> RtResult<()> {
    let exts = exts
        .cloned()
        .or_else(|| AUTOLOAD.with(|s| s.borrow().extensions.clone()))
        .unwrap_or_else(|| ZStr::from(DEFAULT_EXTENSIONS));
    let name = class.as_bytes();
    let name = name.strip_prefix(b"\\").unwrap_or(name);
    let lc: Vec<u8> = name.to_ascii_lowercase();
    for ext in exts.as_bytes().split(|&c| c == b',') {
        let mut file = lc.clone();
        file.extend_from_slice(ext);
        for c in &mut file {
            if *c == b'\\' {
                *c = b'/';
            }
        }
        if host.autoload_include(&ZStr::from(file))?
            && host.class_info(&ZStr::from(lc.as_slice()), false).is_some()
        {
            break;
        }
    }
    Ok(())
}

/// `zend_perform_class_autoload`: calls the registered loaders until the
/// class exists.
pub fn perform_autoload(host: &mut dyn SplHost, class: &ZStr) -> RtResult<()> {
    for loader in autoloaders() {
        if loader.is_null() {
            spl_autoload(host, class, None)?;
        } else {
            host.call_callable(&loader, &[Value::Str(class.clone())])?;
        }
        if host.class_info(class, false).is_some() {
            break;
        }
    }
    Ok(())
}

fn autoload_register(host: &mut dyn SplHost, args: &[Value]) -> RtResult<Value> {
    let a = Args::new(
        "spl_autoload_register",
        args,
        &["callback", "throw", "prepend"],
        0,
        Some(3),
    )?;
    let cb = a.raw(0);
    let cb = if cb.is_null() {
        Value::Null
    } else {
        match host.check_callable(&cb) {
            Ok(()) => normalize_callable(&cb),
            Err(reason) => {
                return Err(RuntimeError::TypeError(format!(
                    "{} must be a valid callback or null, {reason}",
                    a.arg_prefix(0)
                )))
            }
        }
    };
    let do_throw = a.opt_bool(host, 1, true)?;
    let prepend = a.opt_bool(host, 2, false)?;
    if !do_throw {
        host.diagnostic(
            ErrorLevel::Notice,
            "spl_autoload_register(): Argument #2 ($do_throw) has been ignored, spl_autoload_register() will always throw",
        )?;
    }
    if !cb.is_null() && callable_identity(host, &cb) == b"spl_autoload_call" {
        return Err(a.value_error(0, "must not be the spl_autoload_call() function"));
    }
    let id = if cb.is_null() {
        b"spl_autoload".to_vec()
    } else {
        callable_identity(host, &cb)
    };
    let exists = loader_identities(host).contains(&id);
    AUTOLOAD.with(|s| {
        let mut s = s.borrow_mut();
        if !exists {
            let entry = if id == b"spl_autoload" {
                Value::from("spl_autoload")
            } else {
                cb
            };
            if prepend {
                s.loaders.insert(0, entry);
            } else {
                s.loaders.push(entry);
            }
        }
    });
    Ok(Value::Bool(true))
}

fn autoload_unregister(host: &mut dyn SplHost, args: &[Value]) -> RtResult<Value> {
    let a = Args::new("spl_autoload_unregister", args, &["callback"], 1, Some(1))?;
    let cb = a.callable(host, 0)?;
    let id = callable_identity(host, &cb);
    if id == b"spl_autoload_call" {
        host.diagnostic(
            ErrorLevel::Deprecated,
            "spl_autoload_unregister(): Using spl_autoload_call() as a callback for spl_autoload_unregister() is deprecated, to remove all registered autoloaders, call spl_autoload_unregister() for all values returned from spl_autoload_functions()",
        )?;
        AUTOLOAD.with(|s| s.borrow_mut().loaders.clear());
        return Ok(Value::Bool(true));
    }
    let ids = loader_identities(host);
    let removed = AUTOLOAD.with(|s| {
        let mut s = s.borrow_mut();
        let before = s.loaders.len();
        let mut keep = ids.iter().map(|i| *i != id);
        s.loaders.retain(|_| keep.next().unwrap_or(true));
        s.loaders.len() != before
    });
    Ok(Value::Bool(removed))
}

/// Error for calls of functions this module does not know.
#[must_use]
pub fn undefined_function(name: &str) -> RuntimeError {
    RuntimeError::Error(format!("Call to undefined function {name}()"))
}
