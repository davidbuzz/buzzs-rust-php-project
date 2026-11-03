//! The services SPL needs from the VM: the [`SplHost`] trait and the
//! [`ObjectIterator`] protocol (`zend_object_iterator`).
//!
//! The VM implements [`SplHost`] once. SPL never holds a borrow of an
//! object's state while it calls back into the host, so user code invoked
//! from a callback (an overridden `compare()`, a `CallbackFilterIterator`
//! callback, `__toString`, ...) may freely use the same SPL object again.

use crate::object::SplObject;
use php_runtime::{Obj, RtResult, RuntimeError, Value, ZArray, ZStr};
use php_stdarr::ArrayContext;
use std::rc::Rc;

/// Class metadata for `class_implements()`, `class_parents()` and
/// `class_uses()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClassInfo {
    /// The class name as declared.
    pub name: ZStr,
    /// Parent classes, nearest first.
    pub parents: Vec<ZStr>,
    /// All implemented interfaces in `ce->interfaces` order.
    pub interfaces: Vec<ZStr>,
    /// Traits used directly by the class (`ce->trait_names`).
    pub traits: Vec<ZStr>,
}

/// What the VM provides to SPL.
///
/// Diagnostics, casts, comparisons and property tables come from
/// [`php_runtime::RuntimeContext`]; `count()` on objects and enum checks
/// from [`ArrayContext`].
pub trait SplHost: ArrayContext {
    /// The SPL internal state of an object, created by
    /// [`crate::object::create_state`] when the object was instantiated
    /// (`None` for objects that are not instances of an SPL class).
    fn spl_state(&mut self, obj: &Obj) -> Option<Rc<SplObject>>;

    /// `zend_call_method`: calls `$obj->name(...$args)` with normal method
    /// dispatch. When the method resolves to an internal SPL method the VM
    /// routes the call back to [`crate::call_method`].
    fn call_method(&mut self, obj: &Obj, name: &str, args: &[Value]) -> RtResult<Value>;

    /// Whether the method `name` (any case) of the object's class is
    /// implemented in user code, i.e. a user subclass overrides the SPL
    /// implementation (the `fptr_* != NULL` checks of the C code).
    fn has_user_method(&mut self, obj: &Obj, name: &str) -> bool;

    /// Whether the object's class has a method `name` at all.
    fn has_method(&mut self, obj: &Obj, name: &str) -> bool;

    /// `instanceof`: whether `obj` is an instance of the class or interface
    /// `class` (case-insensitive).
    fn instance_of(&mut self, obj: &Obj, class: &str) -> bool;

    /// `object_init_ex`: creates an instance of `class` without calling its
    /// constructor (SPL state is initialised by
    /// [`crate::object::create_state`]).
    fn create_object(&mut self, class: &ZStr) -> RtResult<Obj>;

    /// `new $class(...$args)`.
    fn instantiate(&mut self, class: &ZStr, args: &[Value]) -> RtResult<Obj>;

    /// Whether `class` exists (looked up without autoloading) and is a
    /// subclass of (or equal to) `parent`; `None` when it does not exist.
    fn class_is_a(&mut self, class: &ZStr, parent: &str) -> Option<bool>;

    /// Calls a PHP callable value.
    fn call_callable(&mut self, callable: &Value, args: &[Value]) -> RtResult<Value>;

    /// `zend_is_callable_ex`: `Err(reason)` when not callable, where reason
    /// completes "must be a valid callback, ..." (e.g. `function "x" not
    /// found or invalid function name`).
    fn check_callable(&mut self, callable: &Value) -> Result<(), String>;

    /// Iterators for traversable objects whose iteration the VM implements
    /// itself (generators, internal classes of other extensions). Return
    /// `None` for user classes and SPL classes: [`crate::iter::get_iterator`]
    /// handles `Iterator`, `IteratorAggregate` and SPL objects.
    fn foreign_iterator(&mut self, obj: &Obj) -> Option<RtResult<Box<dyn ObjectIterator>>> {
        let _ = obj;
        None
    }

    /// Whether a non-SPL object has custom `get_properties` /
    /// `get_properties_for` handlers (e.g. `DateTime`, `Closure`), which
    /// makes it unusable as `ArrayObject` storage ("Overloaded object of
    /// type %s is not compatible with %s").
    fn is_overloaded_object(&mut self, obj: &Obj) -> bool {
        let _ = obj;
        false
    }

    /// `zend_std_get_properties`: the object's property table with mangled
    /// keys (declared and dynamic properties).
    fn std_properties(&mut self, obj: &Obj) -> ZArray;

    /// `zend_std_has_property(obj, name, ZEND_PROPERTY_EXISTS)`: whether a
    /// property `name` exists and is accessible from the calling scope.
    /// The default checks for a public (unmangled) key.
    fn std_has_property(&mut self, obj: &Obj, name: &ZStr) -> bool {
        self.std_properties(obj).get_str(name.as_bytes()).is_some()
    }

    /// Replaces the object's property table (used when an `ArrayObject`
    /// wraps an object and writes to it). Keys are mangled names.
    fn set_std_properties(&mut self, obj: &Obj, props: ZArray) -> RtResult<()>;

    /// Class metadata of a class name (`None` when it does not exist after
    /// optional autoloading).
    fn class_info(&mut self, name: &ZStr, autoload: bool) -> Option<ClassInfo>;

    /// Class metadata of an object's class.
    fn object_class_info(&mut self, obj: &Obj) -> ClassInfo;

    /// Wraps a native iterator in an `InternalIterator` object
    /// (`zend_create_internal_iterator_zval`), as returned by
    /// `SplFixedArray::getIterator()`.
    fn new_internal_iterator(&mut self, it: Box<dyn ObjectIterator>) -> RtResult<Value>;

    /// `spl_autoload()`'s file loading: opens `file` using the include
    /// path and, unless it was already included, compiles and executes it.
    /// Returns whether the file was executed.
    fn autoload_include(&mut self, file: &ZStr) -> RtResult<bool>;

    /// The request's PCRE state (`RegexIterator`): pattern cache, INI
    /// limits and `preg_last_error()`.
    fn pcre(&mut self) -> &mut php_pcre::Pcre;

    /// Writes to the output (`fpassthru`).
    fn write_output(&mut self, bytes: &[u8]) -> RtResult<()>;
}

/// The `zend_object_iterator` protocol used by `foreach`, `IteratorIterator`
/// and the `iterator_*()` functions.
pub trait ObjectIterator {
    /// `rewind`.
    fn rewind(&mut self, host: &mut dyn SplHost) -> RtResult<()>;
    /// `valid`.
    fn valid(&mut self, host: &mut dyn SplHost) -> RtResult<bool>;
    /// `get_current_data`; `None` models a `NULL` result, which ends
    /// `foreach` and `iterator_to_array()` silently.
    fn current(&mut self, host: &mut dyn SplHost) -> RtResult<Option<Value>>;
    /// `get_current_key`; `None` when the iterator has no key function
    /// (`foreach` then uses a running index).
    fn key(&mut self, host: &mut dyn SplHost) -> RtResult<Option<Value>>;
    /// `move_forward`.
    fn next(&mut self, host: &mut dyn SplHost) -> RtResult<()>;

    /// For `zend_user_iterator`s: the `Iterator` object whose methods this
    /// iterator calls. Callers may then call the methods directly instead
    /// of borrowing the iterator (so user code can re-enter it).
    fn method_object(&self) -> Option<&Obj> {
        None
    }
}

/// `zend_user_iterator`: iterates an `Iterator` object by calling its
/// `rewind`/`valid`/`current`/`key`/`next` methods.
#[derive(Debug, Clone)]
pub struct MethodIterator {
    /// The iterator object.
    pub obj: Obj,
}

impl MethodIterator {
    /// Wraps an `Iterator` object.
    #[must_use]
    pub fn new(obj: Obj) -> Self {
        MethodIterator { obj }
    }
}

impl ObjectIterator for MethodIterator {
    fn method_object(&self) -> Option<&Obj> {
        Some(&self.obj)
    }
    fn rewind(&mut self, host: &mut dyn SplHost) -> RtResult<()> {
        host.call_method(&self.obj, "rewind", &[]).map(|_| ())
    }
    fn valid(&mut self, host: &mut dyn SplHost) -> RtResult<bool> {
        let v = host.call_method(&self.obj, "valid", &[])?;
        v.to_bool(host)
    }
    fn current(&mut self, host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        host.call_method(&self.obj, "current", &[]).map(Some)
    }
    fn key(&mut self, host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        host.call_method(&self.obj, "key", &[]).map(Some)
    }
    fn next(&mut self, host: &mut dyn SplHost) -> RtResult<()> {
        host.call_method(&self.obj, "next", &[]).map(|_| ())
    }
}

/// Error used when an object unexpectedly lacks SPL state (cannot happen
/// for objects created through [`crate::object::create_state`]).
#[must_use]
pub fn missing_state(obj: &Obj) -> RuntimeError {
    RuntimeError::Error(format!(
        "Object of class {} is not properly initialized",
        obj.class_name()
    ))
}
