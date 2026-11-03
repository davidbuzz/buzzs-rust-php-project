//! The extension API: how native modules (`ext/*` bindings) register
//! functions, constants, INI entries and classes with the VM.
//!
//! An extension is a type implementing [`Extension`]. The CLI passes the
//! list of extensions to [`Vm::with_extensions`]; each one receives a
//! [`ModuleBuilder`] during VM startup (`MINIT`) and registers its symbols.
//!
//! Native functions are declared with a PHP stub signature, e.g.
//! `"ctype_digit(mixed $text): bool"`. The VM parses it once and then
//! performs PHP's argument handling for every call: arity errors
//! (`ArgumentCountError`), named arguments, by-reference parameters,
//! scalar coercion following the caller's `strict_types`, the
//! "Passing null to parameter #N" deprecation and the
//! "func(): Argument #N ($x) must be of type T, U given" `TypeError`. The
//! handler then receives already-coerced values in [`Args`].
//!
//! See `crates/php-vm/EXTENSIONS.md` for a walkthrough.

use crate::builtins::sig::parse_sig;
use crate::class::Class;
use crate::func::{flags, Builtin, Func};
use crate::vm::Vm;
use php_runtime::{BinaryOp, CastTarget, Obj, PropertyPurpose, Value, ZArray, ZStr};
use php_support::ini::IniDirective;
use std::rc::Rc;

pub use crate::builtins::classbuilder::ClassBuilder;
pub use crate::func::{Args, BuiltinFn as NativeFn};
pub use crate::vm::{Throw, VmResult};

/// Method/property flag bits for [`ClassBuilder`].
pub mod acc {
    pub use crate::func::flags::{ABSTRACT, FINAL, PRIVATE, PROTECTED, PUBLIC, READONLY, STATIC};
}

/// Class flag bits for [`ClassBuilder::flags`].
pub mod class_flags {
    pub use crate::class::cflags::{
        ABSTRACT, ALLOW_DYNAMIC, FINAL, INTERFACE, NOT_SERIALIZABLE, NO_CLONE, NO_NEW,
    };
}

/// A native extension module.
pub trait Extension {
    /// The extension name as reported by `get_loaded_extensions()`
    /// (`"ctype"`, `"json"`, ...).
    fn name(&self) -> &'static str;

    /// Registers functions, constants, INI entries and classes (`MINIT`).
    fn register(&self, module: &mut ModuleBuilder<'_>);
}

/// Registration interface handed to [`Extension::register`].
pub struct ModuleBuilder<'a> {
    vm: &'a mut Vm,
    name: &'static str,
}

impl std::fmt::Debug for ModuleBuilder<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ModuleBuilder({})", self.name)
    }
}

impl<'a> ModuleBuilder<'a> {
    pub(crate) fn new(vm: &'a mut Vm, name: &'static str) -> Self {
        ModuleBuilder { vm, name }
    }

    /// The VM (for registrations needing other classes, e.g. parents).
    pub fn vm(&mut self) -> &mut Vm {
        self.vm
    }

    /// The extension name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Registers a function from a stub signature
    /// (`"name(type $param = default, ...): ret"`; `&$x` for by-reference,
    /// `...$rest` for variadics). Re-registering a name replaces it.
    pub fn function(&mut self, signature: &str, handler: NativeFn) -> &mut Self {
        crate::builtins::reg(self.vm, signature, handler);
        self
    }

    /// Registers `alias` for an already registered function.
    pub fn alias(&mut self, alias: &str, target: &str) -> &mut Self {
        crate::builtins::alias(self.vm, alias, &target.to_ascii_lowercase());
        self
    }

    /// Defines a global constant.
    pub fn constant(&mut self, name: &str, value: Value) -> &mut Self {
        self.vm.constants.insert(ZStr::from(name), value);
        self
    }

    /// Registers an INI directive. Values given with `-d` before startup
    /// apply.
    pub fn ini(&mut self, directive: IniDirective) -> &mut Self {
        let mut sink: Vec<php_support::Diagnostic> = Vec::new();
        self.vm.ini.register(directive, &mut sink);
        self
    }

    /// Registers a callback run at request shutdown, after shutdown
    /// functions and destructors, before output is flushed (`RSHUTDOWN`).
    pub fn on_request_shutdown(&mut self, f: fn(&mut Vm)) -> &mut Self {
        self.vm.shutdown_hooks.push(f);
        self
    }

    /// Starts a class definition; call [`ClassBuilder::register`] with
    /// [`ModuleBuilder::vm`] to finish it.
    pub fn class(&mut self, name: &str) -> ClassBuilder {
        ClassBuilder::new(name)
    }
}

/// A native iterator returned by [`ClassHooks::get_iterator`] (the
/// `get_iterator` object handler) for `foreach` over native objects.
pub trait NativeIterator {
    /// Moves to the first element.
    fn rewind(&mut self, vm: &mut Vm) -> VmResult<()>;
    /// Whether the current position is valid.
    fn valid(&mut self, vm: &mut Vm) -> VmResult<bool>;
    /// The current value.
    fn current(&mut self, vm: &mut Vm) -> VmResult<Value>;
    /// The current key.
    fn key(&mut self, vm: &mut Vm) -> VmResult<Value>;
    /// Advances.
    fn next(&mut self, vm: &mut Vm) -> VmResult<()>;
}

/// Object handlers of a native class (`zend_object_handlers`). Every hook
/// is optional; `Ok(None)` from a hook means "use the default behaviour".
/// Hooks are inherited by subclasses (including user classes).
#[derive(Clone, Default)]
#[allow(clippy::type_complexity)]
pub struct ClassHooks {
    /// Called when an instance is created (before any constructor), to set
    /// up internal state with [`crate::object::set_internal`].
    pub create: Option<fn(&mut Vm, &Obj) -> VmResult<()>>,
    /// `clone_obj`: copy internal state from `old` to `new`.
    pub clone: Option<fn(&mut Vm, &Obj, &Obj) -> VmResult<()>>,
    /// `get_properties_for` (`var_dump`, `(array)`, `serialize`, ...).
    pub properties_for: Option<fn(&mut Vm, &Obj, PropertyPurpose) -> VmResult<Option<ZArray>>>,
    /// `compare` (both operands, at least one is an instance).
    pub compare: Option<fn(&mut Vm, &Value, &Value) -> VmResult<Option<i32>>>,
    /// `cast_object`.
    pub cast: Option<fn(&mut Vm, &Obj, CastTarget) -> VmResult<Option<Value>>>,
    /// `do_operation` (operator overloading).
    pub do_operation: Option<fn(&mut Vm, BinaryOp, &Value, &Value) -> VmResult<Option<Value>>>,
    /// `count_elements`.
    pub count: Option<fn(&mut Vm, &Obj) -> VmResult<Option<i64>>>,
    /// `read_property`; `Ok(None)` falls back to the standard handler.
    pub read_property: Option<fn(&mut Vm, &Obj, &ZStr) -> VmResult<Option<Value>>>,
    /// `write_property`; returns `true` if handled.
    pub write_property: Option<fn(&mut Vm, &Obj, &ZStr, &Value) -> VmResult<bool>>,
    /// `has_property`; `Ok(None)` falls back.
    pub has_property: Option<fn(&mut Vm, &Obj, &ZStr, bool) -> VmResult<Option<bool>>>,
    /// `unset_property`; returns `true` if handled.
    pub unset_property: Option<fn(&mut Vm, &Obj, &ZStr) -> VmResult<bool>>,
    /// `read_dimension` (`$o[$k]`); `Ok(None)` falls back to `ArrayAccess`.
    pub read_dimension: Option<fn(&mut Vm, &Obj, &Value) -> VmResult<Option<Value>>>,
    /// `write_dimension` (`$o[$k] = $v`, key `None` for `$o[] = $v`).
    pub write_dimension: Option<fn(&mut Vm, &Obj, Option<&Value>, &Value) -> VmResult<bool>>,
    /// `has_dimension` (`isset`/`empty`, with `check_empty`).
    pub has_dimension: Option<fn(&mut Vm, &Obj, &Value, bool) -> VmResult<Option<bool>>>,
    /// `unset_dimension`.
    pub unset_dimension: Option<fn(&mut Vm, &Obj, &Value) -> VmResult<bool>>,
    /// `get_iterator` for `foreach` (by-reference flag given).
    pub get_iterator: Option<fn(&mut Vm, &Obj, bool) -> VmResult<Box<dyn NativeIterator>>>,
    /// Serialization (`__serialize`-like array); `Ok(None)` falls back.
    pub serialize: Option<fn(&mut Vm, &Obj) -> VmResult<Option<ZArray>>>,
    /// Unserialization from the array produced by `serialize`.
    pub unserialize: Option<fn(&mut Vm, &Obj, ZArray) -> VmResult<bool>>,
}

impl std::fmt::Debug for ClassHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClassHooks")
    }
}

/// The hooks of an object's class, if any.
pub(crate) fn hooks_of(o: &Obj) -> Option<Rc<ClassHooks>> {
    crate::object::class_of(o).hooks.clone()
}

impl Vm {
    /// Creates a VM and registers the given extensions.
    pub fn with_extensions(
        opts: &crate::VmOptions,
        stdout: Box<dyn std::io::Write>,
        extensions: &[&dyn Extension],
    ) -> Vm {
        let mut vm = Vm::new(opts, stdout);
        for e in extensions {
            vm.loaded_extensions.push(e.name().to_string());
            let mut m = ModuleBuilder::new(&mut vm, e.name());
            e.register(&mut m);
        }
        vm
    }

    /// Throws an exception of class `class` (any registered `Throwable`)
    /// with `message`. Use as `return Err(vm.throw(...))`.
    pub fn throw(&mut self, class: &str, message: &str) -> Throw {
        self.make_throw(class, message)
    }

    /// Emits a warning prefixed with the running native function
    /// (`"func(): message"`, `php_error_docref`).
    pub fn php_warning(&mut self, message: &str) -> VmResult<()> {
        self.docref(crate::errors::level::WARNING, message)
    }

    /// Emits a notice prefixed with the running native function.
    pub fn php_notice(&mut self, message: &str) -> VmResult<()> {
        self.docref(crate::errors::level::NOTICE, message)
    }

    /// Emits a deprecation prefixed with the running native function.
    pub fn php_deprecated(&mut self, message: &str) -> VmResult<()> {
        self.docref(crate::errors::level::DEPRECATED, message)
    }

    /// Emits an error of any level without a function prefix
    /// (`zend_error`).
    pub fn raise(&mut self, level: i64, message: &str) -> VmResult<()> {
        self.error(level, message)
    }

    /// `zval_get_string` (may call `__toString`, warns for arrays).
    pub fn value_to_string(&mut self, v: &Value) -> VmResult<ZStr> {
        self.to_zstr(v)
    }

    /// `zval_get_long`.
    pub fn value_to_int(&mut self, v: &Value) -> VmResult<i64> {
        self.to_long(v)
    }

    /// `zval_get_double`.
    pub fn value_to_float(&mut self, v: &Value) -> VmResult<f64> {
        self.to_double(v)
    }

    /// `zend_is_true`.
    pub fn value_to_bool(&mut self, v: &Value) -> VmResult<bool> {
        self.to_bool(v)
    }

    /// `zend_is_callable`.
    pub fn is_callable(&mut self, v: &Value) -> bool {
        self.is_callable_value(v)
    }

    /// Calls a method by name on an object.
    pub fn call_method_by_name(&mut self, o: &Obj, name: &str, args: Vec<Value>) -> VmResult<Value> {
        self.call_method_name(o, name, args)
    }

    /// Creates an instance of a class (without calling the constructor).
    pub fn new_object(&mut self, class: &Rc<Class>) -> VmResult<Obj> {
        self.instantiate_unchecked(class)
    }

    /// Creates an instance and calls its constructor with `args`.
    pub fn new_object_with_args(&mut self, class: &Rc<Class>, args: Vec<Value>) -> VmResult<Obj> {
        let o = self.instantiate(class)?;
        if let Some(m) = class.magic.construct.clone() {
            self.call_method(&o, &m, args)?;
        }
        Ok(o)
    }

    /// Reads a property (with visibility of the current scope and magic).
    pub fn get_property(&mut self, o: &Obj, name: &str) -> VmResult<Value> {
        self.read_property(o, &ZStr::from(name), crate::opcode::FetchMode::Read)
    }

    /// Writes a property.
    pub fn set_property(&mut self, o: &Obj, name: &str, v: Value) -> VmResult<()> {
        self.write_property(o, &ZStr::from(name), v).map(|_| ())
    }

    /// Writes a declared or dynamic property directly (no visibility,
    /// readonly or type checks) — for initializing native objects.
    pub fn set_property_raw(&mut self, o: &Obj, name: &str, v: Value) {
        let n = ZStr::from(name);
        let cls = crate::object::class_of(o).clone();
        if let Some(pi) = cls.props.iter().rev().find(|p| p.name == n) {
            crate::object::data(o).slots.borrow_mut()[pi.slot as usize] = Some(v);
            return;
        }
        self.write_dynamic_prop(o, n, v);
    }

    /// An INI value as a string (`None` if unknown).
    pub fn ini_string(&self, name: &str) -> Option<Vec<u8>> {
        self.ini.ini_get(name)
    }

    /// An INI value as an integer.
    pub fn ini_long(&self, name: &str) -> i64 {
        self.ini.get_long(name)
    }

    /// An INI value as a boolean.
    pub fn ini_bool(&self, name: &str) -> bool {
        self.ini.get_bool(name)
    }

    /// Writes output (through output buffering).
    pub fn output(&mut self, data: &[u8]) {
        self.echo(data);
    }

    /// Registers a builtin function at runtime (same as
    /// [`ModuleBuilder::function`]).
    pub fn register_function(&mut self, signature: &str, handler: NativeFn) {
        crate::builtins::reg(self, signature, handler);
    }

    /// The currently registered function `name`, if native.
    pub fn native_function(&self, name: &str) -> Option<Rc<Builtin>> {
        match self.functions.get(name.to_ascii_lowercase().as_bytes()) {
            Some(Func::Builtin(b)) => Some(b.clone()),
            _ => None,
        }
    }
}

impl Args {
    /// String argument `n` (declared `string`; already coerced).
    pub fn str(&self, n: usize) -> ZStr {
        crate::builtins::util::s(self, n)
    }

    /// Integer argument `n` (declared `int`).
    pub fn int(&self, n: usize) -> i64 {
        crate::builtins::util::i(self, n)
    }

    /// Float argument `n` (declared `float`).
    pub fn float(&self, n: usize) -> f64 {
        crate::builtins::util::f(self, n)
    }

    /// Bool argument `n` (declared `bool`).
    pub fn bool(&self, n: usize) -> bool {
        crate::builtins::util::b(self, n)
    }

    /// Array argument `n` (declared `array`).
    pub fn array(&self, n: usize) -> ZArray {
        crate::builtins::util::arr(self, n)
    }

    /// Argument `n` if it was passed and is not `null`.
    pub fn opt(&self, n: usize) -> Option<Value> {
        crate::builtins::util::opt(self, n)
    }

    /// Throws `ValueError("func(): Argument #n ($name) msg")`
    /// (`zend_argument_value_error`); `n` is 1-based.
    pub fn value_error(&self, vm: &mut Vm, n: usize, msg: &str) -> Throw {
        crate::builtins::util::value_error(vm, self, n, msg)
    }

    /// Throws `TypeError("func(): Argument #n ($name) msg")`
    /// (`zend_argument_type_error`); `n` is 1-based.
    pub fn type_error(&self, vm: &mut Vm, n: usize, msg: &str) -> Throw {
        crate::builtins::util::type_error(vm, self, n, msg)
    }
}

/// Parses a stub signature (exposed for tests of extensions).
pub fn check_signature(sig: &str) -> (String, usize) {
    let s = parse_sig(sig);
    let _ = flags::PUBLIC;
    (s.name, s.params.len())
}
