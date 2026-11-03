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
    pub use crate::func::flags::{
        ABSTRACT, FINAL, PRIVATE, PROTECTED, PUBLIC, READONLY, STATIC, STATIC_KEEPS_THIS, VIRTUAL,
    };
}

/// Class flag bits for [`ClassBuilder::flags`].
pub mod class_flags {
    pub use crate::class::cflags::{
        ABSTRACT, ALLOW_DYNAMIC, FINAL, INTERFACE, NOT_SERIALIZABLE, NO_CLONE,
        NO_DYNAMIC_PROPERTIES, NO_NEW, READONLY,
    };
}

/// An INI `on_modify` handler (`ZEND_INI_MH`): receives the new value
/// before `ini_set()` applies it; `Ok(false)` rejects the change
/// (`ini_set()` returns `false`). After `ini_restore()` it is called with
/// the restored value (the result is ignored).
pub type IniModifyFn = fn(&mut Vm, &[u8]) -> VmResult<bool>;

/// One element of an extension's `phpinfo()` section (`PHP_MINFO`):
/// the `php_info_print_table_*` calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InfoItem {
    /// `php_info_print_table_start()`.
    TableStart,
    /// `php_info_print_table_end()`.
    TableEnd,
    /// `php_info_print_table_header(n, ...)`.
    Header(Vec<String>),
    /// `php_info_print_table_colspan_header(n, text)`.
    ColspanHeader(String),
    /// `php_info_print_table_row(n, ...)`.
    Row(Vec<String>),
    /// `DISPLAY_INI_ENTRIES()`.
    IniEntries,
}

/// Why [`ClassHooks::read_dimension`] is being called (the `type`
/// argument of `read_dimension`: `BP_VAR_R`/`IS`/`W`/`RW`/`UNSET`), read
/// with [`Vm::dimension_context`] inside the hook.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DimensionCtx {
    /// `$o[k]` read.
    #[default]
    Read,
    /// `isset($o[k][..])` / `??` (quiet).
    Is,
    /// Intermediate of a nested write (`$o[k][..] = v`, `&$o[k]`).
    Write,
    /// Compound assignment / increment (`$o[k] .= v`, `$o[k][..]++`).
    ReadWrite,
    /// Intermediate of a nested unset (`unset($o[k][..])`).
    Unset,
    /// `$o[][..] = v`: the key passed is null.
    Append,
}

/// Why [`ClassHooks::read_property`] is being called (`BP_VAR_*` of
/// `read_property`/`get_property_ptr_ptr`), read with
/// [`Vm::property_context`] inside the hook.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PropCtx {
    /// `$o->p` read.
    #[default]
    Read,
    /// Quiet read: `isset($o->p->q)`, `$o->p ?? x`.
    Isset,
    /// Intermediate of a nested write (`$o->p->q = v`, `$o->p[] = v`).
    Write,
    /// Compound assignment / increment (`$o->p .= v`, `$o->p[k]++`).
    ReadWrite,
    /// Intermediate of a nested unset (`unset($o->p->q)`).
    Unset,
    /// Reference taken (`&$o->p`, by-ref argument, `foreach (... as &$v)`).
    Ref,
    /// `property_exists()` asking `has_property` (`ZEND_PROPERTY_EXISTS`):
    /// answer whether the property exists at all, even if null.
    Exists,
}

/// Result of an include opener hook.
#[derive(Debug)]
pub enum IncludeOpen {
    /// Opened: the source and the path to record (`__FILE__`, `*_once`).
    Opened {
        /// The file contents.
        source: Vec<u8>,
        /// The opened path (e.g. a `phar://` URL).
        opened_path: ZStr,
    },
    /// Failed; the opener already emitted its warnings. The VM adds
    /// "Failed opening ... for inclusion" / the `require` fatal.
    Failed,
    /// Not for this opener: the VM opens a plain file itself.
    NotHandled,
}

/// `zend_stream_open_function`: opens `include`/`require` targets
/// (receives the name as written and "include"/"require_once"/...).
pub type IncludeOpenerFn = fn(&mut Vm, &[u8], &str) -> VmResult<IncludeOpen>;

/// Transforms script source before compilation (main script and
/// includes), e.g. phar stubs (`zend_compile_file` override).
pub type SourceFilterFn = fn(&mut Vm, &ZStr, Vec<u8>) -> VmResult<Vec<u8>>;

/// A named native output handler (`php_output_handler_alias_ctor_t`):
/// receives the buffer and the `PHP_OUTPUT_HANDLER_*` mode; `Ok(None)`
/// means failure (the buffer passes through).
pub type OutputHandlerFn = fn(&mut Vm, &[u8], i32) -> VmResult<Option<Vec<u8>>>;

/// A `PHP_MINFO` callback.
pub type ModuleInfoFn = fn(&mut Vm) -> Vec<InfoItem>;

/// Why an [`IniModifyFn`] runs (`ZEND_INI_STAGE_*`), read with
/// [`Vm::ini_modify_stage`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IniStage {
    /// `ini_set()` / `ini_alter()` (`ZEND_INI_STAGE_RUNTIME`).
    #[default]
    Runtime,
    /// `ini_restore()` / request end (`ZEND_INI_STAGE_DEACTIVATE`).
    Restore,
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
    last_function: Option<ZStr>,
}

impl std::fmt::Debug for ModuleBuilder<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ModuleBuilder({})", self.name)
    }
}

impl<'a> ModuleBuilder<'a> {
    pub(crate) fn new(vm: &'a mut Vm, name: &'static str) -> Self {
        ModuleBuilder {
            vm,
            name,
            last_function: None,
        }
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
        let name = crate::builtins::reg(self.vm, signature, handler);
        crate::builtins::info::note_function(&name, self.name);
        self.last_function = Some(name);
        self
    }

    /// Marks the function registered last as `#[\Deprecated]`
    /// (`since`/`message` may be empty): calls emit
    /// `Deprecated: Function f() is deprecated since 8.4, message` before
    /// argument parsing.
    pub fn deprecated(&mut self, since: &str, message: &str) -> &mut Self {
        if let Some(lc) = self.last_function.clone() {
            if let Some(crate::func::Func::Builtin(b)) = self.vm.functions.get(&lc).cloned() {
                let mut nb = (*b).clone();
                nb.flags |= crate::func::flags::DEPRECATED;
                self.vm
                    .functions
                    .insert(lc.clone(), crate::func::Func::Builtin(Rc::new(nb)));
                let suffix = crate::attrs::format_suffix(
                    Some(&ZStr::from(since)),
                    Some(&ZStr::from(message)),
                );
                self.vm.builtin_deprecations.insert(lc, suffix);
            }
        }
        self
    }

    /// Registers `alias` for an already registered function.
    pub fn alias(&mut self, alias: &str, target: &str) -> &mut Self {
        crate::builtins::alias(self.vm, alias, &target.to_ascii_lowercase());
        self
    }

    /// Defines a global constant.
    pub fn constant(&mut self, name: &str, value: Value) -> &mut Self {
        let n = ZStr::from(name);
        crate::builtins::info::note_constant(&n, self.name);
        if self.vm.constants.insert(n.clone(), value).is_none() {
            crate::reflection::note_new(crate::reflection::Table::Constant, n.as_bytes());
        }
        self
    }

    /// Defines a deprecated global constant: reading it emits
    /// `Deprecated: Constant NAME is deprecated since X, message`
    /// (`since`/`message` may be empty).
    pub fn constant_deprecated(
        &mut self,
        name: &str,
        value: Value,
        since: &str,
        message: &str,
    ) -> &mut Self {
        let n = ZStr::from(name);
        crate::builtins::info::note_constant(&n, self.name);
        if self.vm.constants.insert(n.clone(), value).is_none() {
            crate::reflection::note_new(crate::reflection::Table::Constant, n.as_bytes());
        }
        let suffix =
            crate::attrs::format_suffix(Some(&ZStr::from(since)), Some(&ZStr::from(message)));
        self.vm.deprecated_constants.insert(n, suffix);
        self
    }

    /// Registers an INI directive. Values given with `-d` before startup
    /// apply.
    pub fn ini(&mut self, directive: IniDirective) -> &mut Self {
        let mut sink: Vec<php_support::Diagnostic> = Vec::new();
        self.vm.ini.register(directive, &mut sink);
        self
    }

    /// Registers a named output handler usable as `ob_start('name')`
    /// without being a function (`ob_gzhandler`, `ob_iconv_handler`,
    /// `mb_output_handler`).
    pub fn output_handler(&mut self, name: &str, f: OutputHandlerFn) -> &mut Self {
        self.vm
            .output_handler_aliases
            .insert(ZStr::from(name.to_ascii_lowercase()), f);
        self
    }

    /// Registers the module's `phpinfo()` section (`PHP_MINFO`), used by
    /// `phpinfo()` and `ReflectionExtension::info()`.
    pub fn info(&mut self, f: ModuleInfoFn) -> &mut Self {
        self.vm.module_info.push((self.name.to_string(), f));
        self
    }

    /// Registers an `on_modify` handler for INI directive `name` (validate
    /// or react to `ini_set()`/`ini_restore()`; see [`IniModifyFn`]).
    pub fn ini_on_modify(&mut self, name: &str, f: IniModifyFn) -> &mut Self {
        self.vm.ini_modify_hooks.insert(name.to_string(), f);
        self
    }

    /// Registers a callback run at request shutdown, after shutdown
    /// functions and destructors, before output is flushed (`RSHUTDOWN`).
    pub fn on_request_shutdown(&mut self, f: fn(&mut Vm)) -> &mut Self {
        self.vm.shutdown_hooks.push(f);
        self
    }

    /// Registers a callback run once all extensions are registered
    /// (`MINIT` work that needs other modules' classes or functions).
    pub fn on_module_startup(&mut self, f: fn(&mut Vm)) -> &mut Self {
        self.vm.module_startup_hooks.push(f);
        self
    }

    /// Registers a callback run at request startup, before the script
    /// (`RINIT`): reset per-request state here.
    pub fn on_request_startup(&mut self, f: fn(&mut Vm)) -> &mut Self {
        self.vm.startup_hooks.push(f);
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
    /// `has_property(check_empty)`: `Ok(Some(true))` means "set and not
    /// null" (`isset`) or, with `check_empty`, "set and not empty" — the
    /// same convention as `has_dimension`. `Ok(None)` falls back.
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
    /// `get_constructor`: runs before the constructor on `new`; throw
    /// here for classes that cannot be constructed ("Cannot directly
    /// construct X, use Y()").
    pub get_constructor: Option<fn(&mut Vm, &Obj) -> VmResult<()>>,
    /// `dtor_obj` without a PHP-visible `__destruct` method: runs when the
    /// object is released (and at shutdown), like a destructor.
    pub destruct: Option<fn(&mut Vm, &Obj) -> VmResult<()>>,
    /// `interface_gets_implemented` (set on an interface): called with
    /// each class that implements it, when the class is linked.
    pub interface_gets_implemented: Option<fn(&mut Vm, &Rc<Class>) -> VmResult<()>>,
    /// `get_properties`: the property table seen by `get_object_vars()`
    /// and `foreach` over the object (unlike `properties_for`, which
    /// serves var_dump/(array)/serialize). `Ok(None)` falls back.
    pub get_properties: Option<fn(&mut Vm, &Obj) -> VmResult<Option<ZArray>>>,
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
            if !vm
                .loaded_extensions
                .iter()
                .any(|n| n.eq_ignore_ascii_case(e.name()))
            {
                vm.loaded_extensions.push(e.name().to_string());
            }
            // Attribute the functions/classes/constants it adds to it
            // (ReflectionExtension, get_defined_constants(true)).
            crate::reflection::record_module(&vm, e.name(), true);
            let mut m = ModuleBuilder::new(&mut vm, e.name());
            e.register(&mut m);
            crate::reflection::record_module(&vm, e.name(), false);
        }
        for hook in std::mem::take(&mut vm.module_startup_hooks) {
            hook(&mut vm);
        }
        vm.disable_functions();
        vm
    }

    /// `zend_disable_functions`: functions named in `disable_functions`
    /// are removed after module startup.
    pub(crate) fn disable_functions(&mut self) {
        let list = self
            .ini
            .get("disable_functions")
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        for name in list
            .split(|&c| c == b',' || c == b' ' || c == b'\t')
            .filter(|n| !n.is_empty())
        {
            self.functions
                .remove(&ZStr::from(name.to_ascii_lowercase()));
        }
    }

    /// Throws an exception of class `class` (any registered `Throwable`)
    /// with `message`. Use as `return Err(vm.throw(...))`.
    /// `zend_parse_arg_long_weak`: loose int coercion for a non-strict
    /// call (with its "Implicit conversion" deprecations); `None` when
    /// the value is not acceptable as int.
    pub fn parse_long_weak(&mut self, v: &Value) -> VmResult<Option<i64>> {
        self.parse_arg_long_weak(v)
    }

    /// The method name a native `__call`/`__callStatic` handler was
    /// invoked for (its arguments are the call's arguments, unpacked).
    pub fn magic_call_name(&self) -> Option<ZStr> {
        self.magic_call_name.clone()
    }

    /// The stage of the running `ini_on_modify` handler.
    pub fn ini_modify_stage(&self) -> IniStage {
        self.ini_stage
    }

    /// Where output started (`php_output_get_start_filename/lineno`),
    /// once anything was sent: for `headers_sent(&$file, &$line)` and
    /// "headers already sent (output started at f:l)".
    pub fn output_started(&self) -> Option<(ZStr, u32)> {
        crate::builtins::headers::output_start()
    }

    /// The context of the running `read_property` hook call.
    pub fn property_context(&self) -> PropCtx {
        self.prop_ctx
    }

    /// The context of the running `read_dimension` hook call.
    pub fn dimension_context(&self) -> DimensionCtx {
        self.dim_ctx
    }

    /// Installs the include opener (stream wrappers for `include`).
    pub fn set_include_opener(&mut self, f: IncludeOpenerFn) {
        self.include_opener = Some(f);
    }

    /// Installs a source filter run before compiling scripts.
    pub fn set_source_filter(&mut self, f: SourceFilterFn) {
        self.source_filter = Some(f);
    }

    /// The `phpinfo()` section of extension `name`, if it registered one.
    pub fn module_info(&mut self, name: &str) -> Option<Vec<InfoItem>> {
        let f = self
            .module_info
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, f)| *f)?;
        Some(f(self))
    }

    /// Names of the extensions with a `phpinfo()` section, in
    /// registration order.
    pub fn modules_with_info(&self) -> Vec<String> {
        self.module_info.iter().map(|(n, _)| n.clone()).collect()
    }

    /// Registers an interrupt handler, run at the next safe point after
    /// [`crate::interrupt::request_interrupt`] (async signals, timers,
    /// client aborts — `zend_interrupt_function`).
    pub fn register_interrupt_handler(&mut self, f: crate::interrupt::InterruptFn) {
        self.interrupt_handlers.push(f);
    }

    /// Registers a native tick handler (`php_add_tick_function`), run by
    /// `declare(ticks=N)` code with fiber switching blocked.
    pub fn register_tick_handler(&mut self, f: crate::interrupt::TickFn) {
        self.tick_hooks.push(f);
    }

    /// `zend_fiber_switch_block()` / `unblock()`: Fiber switches throw
    /// "Cannot switch fibers in current execution context" meanwhile
    /// (e.g. while dispatching signal handlers).
    pub fn block_fiber_switch(&mut self, block: bool) {
        if block {
            self.fiber_switch_block += 1;
        } else {
            self.fiber_switch_block = self.fiber_switch_block.saturating_sub(1);
        }
    }

    /// Converts a runtime-layer error into a VM throw (exception object).
    pub fn throw_runtime_error(&mut self, e: php_runtime::RuntimeError) -> Throw {
        self.rt_throw(e)
    }

    /// Wraps a VM throw for code returning `RtResult` (runtime hooks).
    pub fn runtime_error_from(&mut self, t: Throw) -> php_runtime::RuntimeError {
        self.to_rt(t)
    }

    /// Wraps a native iterator in an `InternalIterator` object (what
    /// `getIterator()` of a native `IteratorAggregate` returns).
    pub fn new_internal_iterator(&mut self, it: Box<dyn NativeIterator>) -> VmResult<Obj> {
        crate::weakref::new_internal_iterator(self, it)
    }

    pub fn throw(&mut self, class: &str, message: &str) -> Throw {
        self.make_throw(class, message)
    }

    /// Emits a warning prefixed with the running native function
    /// (`"func(): message"`, `php_error_docref`).
    pub fn php_warning(&mut self, message: &str) -> VmResult<()> {
        self.docref(crate::errors::level::WARNING, message)
    }

    /// Binary-safe [`Vm::php_warning`].
    pub fn php_warning_bytes(&mut self, message: &[u8]) -> VmResult<()> {
        self.docref_bytes(crate::errors::level::WARNING, message)
    }

    /// Binary-safe docref error of any level.
    pub fn php_error_bytes(&mut self, level: i64, message: &[u8]) -> VmResult<()> {
        self.docref_bytes(level, message)
    }

    /// Binary-safe [`Vm::raise`].
    pub fn raise_bytes(&mut self, level: i64, message: &[u8]) -> VmResult<()> {
        self.error_bytes(level, message)
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
    pub fn call_method_by_name(
        &mut self,
        o: &Obj,
        name: &str,
        args: Vec<Value>,
    ) -> VmResult<Value> {
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

    /// Writes a property as code in class `scope` would (private/protected
    /// access, readonly initialization scope), e.g. PDO's fetch-into-class.
    pub fn set_property_scoped(
        &mut self,
        o: &Obj,
        name: &str,
        v: Value,
        scope: &Rc<Class>,
    ) -> VmResult<()> {
        let idx = self
            .frames
            .iter()
            .rposition(|f| f.kind != crate::vm::FrameKind::Builtin);
        let Some(idx) = idx else {
            return self.set_property(o, name, v);
        };
        let saved = std::mem::replace(&mut self.frames[idx].scope, Some(scope.clone()));
        let r = self.write_property(o, &ZStr::from(name), v).map(|_| ());
        if let Some(f) = self.frames.get_mut(idx) {
            f.scope = saved;
        }
        r
    }

    /// Instantiates `class` and calls its constructor with positional and
    /// named arguments (`new C(...$args, name: $v)`).
    pub fn new_object_named(
        &mut self,
        class: &Rc<Class>,
        args: Vec<Value>,
        named: Vec<(ZStr, Value)>,
    ) -> VmResult<Obj> {
        self.construct_with(class, args, named)
    }

    /// Calls a method with positional and named arguments.
    pub fn call_method_named(
        &mut self,
        o: &Obj,
        method: &str,
        args: Vec<Value>,
        named: Vec<(ZStr, Value)>,
    ) -> VmResult<Value> {
        let cb = Value::Array(php_runtime::ZArray::from_values([
            Value::Object(o.clone()),
            Value::Str(ZStr::from(method)),
        ]));
        self.call_value_named(&cb, args, named)
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

#[cfg(test)]
mod tests {
    use super::{Args, Extension, ModuleBuilder, VmResult};
    use crate::{Vm, VmOptions};
    use php_runtime::Value;
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Sink(Rc<RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn answer(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
        Ok(Value::Int(a.int(0) + i64::from(a.has(0))))
    }

    struct TestExt;

    impl Extension for TestExt {
        fn name(&self) -> &'static str {
            "testext"
        }
        fn register(&self, m: &mut ModuleBuilder<'_>) {
            m.function("old_fn(int $x = 41): int", answer)
                .deprecated("8.4", "use new_fn()");
            m.constant_deprecated("OLD_CONST", Value::Int(7), "", "gone soon");
            let vm = m.vm();
            crate::ext::ClassBuilder::new("Suit")
                .backed_enum("string")
                .case("Hearts", Some(Value::Str("H".into())))
                .case("Spades", Some(Value::Str("S".into())))
                .deprecated_constant("OLD", Value::Int(1), "", "")
                .register(vm);
            crate::ext::ClassBuilder::new("Unit")
                .unit_enum()
                .case("A", None)
                .register(vm);
        }
    }

    fn run(src: &str) -> String {
        let sink = Sink::default();
        let mut vm =
            Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &[&TestExt]);
        vm.run_script(src.as_bytes(), "test.php");
        drop(vm);
        let out = sink.0.borrow().clone();
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn native_enums() {
        let out = run(
            "<?php var_dump(Suit::from('S'), Suit::tryFrom('x'), Suit::Hearts->value, Suit::cases()[1]->name,\n\
             Unit::A instanceof UnitEnum, Suit::Hearts instanceof BackedEnum, Unit::A->name);",
        );
        assert_eq!(
            out,
            "enum(Suit::Spades)\nNULL\nstring(1) \"H\"\nstring(6) \"Spades\"\nbool(true)\nbool(true)\nstring(1) \"A\"\n"
        );
    }

    #[test]
    fn deprecations() {
        let out = run("<?php echo old_fn(), \"\\n\", OLD_CONST, \"\\n\", Suit::OLD, \"\\n\";");
        assert_eq!(
            out,
            "\nDeprecated: Function old_fn() is deprecated since 8.4, use new_fn() in test.php on line 1\n41\n\
             \nDeprecated: Constant OLD_CONST is deprecated, gone soon in test.php on line 1\n7\n\
             \nDeprecated: Constant Suit::OLD is deprecated in test.php on line 1\n1\n"
        );
    }
}
