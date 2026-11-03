//! `ext/intl` for the phprust VM: binds the `php-intl` ICU library through
//! the `php_vm::ext` extension API.
//!
//! The class, method and function declarations come from the php-src
//! stubs ([`stubs`], generated); every entry is bound by its key
//! (`"Class::method"`, or the function name) to a handler in one of the
//! class modules.  Procedural aliases share the method handler: when
//! called without `$this` the object is the first argument (see
//! [`core::target`]).  The intl error model (`intl_error_set()`, the
//! `intl.error_level`/`intl.use_exceptions` INI settings) lives in
//! [`core`].
#![allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]

mod calendar;
mod collator;
mod core;
mod dateformat;
mod dt;
mod iter;
mod locale;
mod misc;
mod registry;
mod stubs;
mod timezone;

use std::cell::Cell;
use std::collections::HashMap;

use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_vm::ext::{Args, Extension, ModuleBuilder, NativeFn, VmResult};
use php_vm::Vm;
use php_runtime::Value;

/// The `intl` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Intl;

thread_local! {
    static ORIG_INI_SET: Cell<Option<NativeFn>> = const { Cell::new(None) };
    static ORIG_INI_RESTORE: Cell<Option<NativeFn>> = const { Cell::new(None) };
}

/// Stores `intl.default_locale` (`zend_alter_ini_entry` from
/// `Locale::setDefault()`) and syncs the library's copy.
pub(crate) fn set_default_locale_ini(vm: &mut Vm, value: &str) {
    let mut sink: Vec<php_support::Diagnostic> = Vec::new();
    let _ = vm.ini.ini_set("intl.default_locale", value.as_bytes(), &mut sink);
    sync_default_locale(vm);
}

/// Copies `intl.default_locale` into the library.
pub(crate) fn sync_default_locale(vm: &Vm) {
    let v = vm.ini_string("intl.default_locale");
    let v = v.map(|b| String::from_utf8_lossy(&b).into_owned());
    php_intl::common::set_ini_default_locale(v.as_deref());
}

fn run_orig(cell: &'static std::thread::LocalKey<Cell<Option<NativeFn>>>, vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match cell.with(Cell::get) {
        Some(f) => f(vm, a),
        None => Ok(Value::Bool(false)),
    }
}

/// `ini_set()` wrapper: `OnUpdateErrorLevel`'s deprecation and the
/// default-locale sync.
fn ini_set_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let name = a.str(0).to_string_lossy();
    let r = run_orig(&ORIG_INI_SET, vm, a)?;
    if name == "intl.error_level" && !matches!(r, Value::Bool(false)) && vm.ini_long("intl.error_level") != 0 {
        vm.php_deprecated(php_intl::error::ERROR_LEVEL_DEPRECATION)?;
    }
    if name == "intl.default_locale" {
        sync_default_locale(vm);
    }
    Ok(r)
}

fn ini_restore_hook(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = run_orig(&ORIG_INI_RESTORE, vm, a)?;
    sync_default_locale(vm);
    Ok(r)
}

fn install_ini_hooks(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    if let Some(b) = vm.native_function("ini_set") {
        ORIG_INI_SET.with(|c| c.set(Some(b.handler)));
        vm.register_function(
            "ini_set(string $option, string|int|float|bool|null $value): string|false",
            ini_set_hook,
        );
        vm.register_function(
            "ini_alter(string $option, string|int|float|bool|null $value): string|false",
            ini_set_hook,
        );
    }
    if let Some(b) = vm.native_function("ini_restore") {
        ORIG_INI_RESTORE.with(|c| c.set(Some(b.handler)));
        vm.register_function("ini_restore(string $option): void", ini_restore_hook);
    }
}

fn all_handlers() -> HashMap<&'static str, NativeFn> {
    let mut h = HashMap::new();
    for list in [
        misc::handlers(),
        locale::handlers(),
        collator::handlers(),
        iter::handlers(),
        timezone::handlers(),
        calendar::handlers(),
        dateformat::handlers(),
    ] {
        for (k, f) in list {
            h.insert(k, f);
        }
    }
    h
}

fn class_opts(name: &str) -> registry::ClassOpts {
    use php_vm::ext::{class_flags, ClassHooks};
    let mut o = registry::ClassOpts::default();
    match name {
        "Collator" | "IntlNumberFormatter" | "IntlListFormatter" | "IntlNumberRangeFormatter" | "ResourceBundle"
        | "IntlRelativeDateTimeFormatter" => o.flags |= class_flags::NO_CLONE,
        "IntlIterator" | "IntlPartsIterator" => {
            o.flags |= class_flags::NO_CLONE;
            o.hooks = ClassHooks { get_iterator: Some(iter::get_iterator), ..ClassHooks::default() };
        }
        "IntlTimeZone" => {
            o.hooks = ClassHooks {
                clone: Some(timezone::clone),
                compare: Some(timezone::compare),
                properties_for: Some(timezone::debug_info),
                ..ClassHooks::default()
            };
        }
        "IntlDateFormatter" => {
            o.hooks = ClassHooks { clone: Some(dateformat::clone), ..ClassHooks::default() };
        }
        "IntlCalendar" | "IntlGregorianCalendar" => {
            o.hooks = ClassHooks {
                clone: Some(calendar::clone),
                properties_for: Some(calendar::debug_info),
                ..ClassHooks::default()
            };
        }
        _ => {}
    }
    o
}

impl Extension for Intl {
    fn name(&self) -> &'static str {
        "intl"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        m.ini(IniDirective::new("intl.default_locale", None, Access::ALL, ModifyHandler::StringUnempty, "intl"));
        m.ini(IniDirective::new("intl.error_level", Some("0"), Access::ALL, ModifyHandler::Long, "intl"));
        m.ini(IniDirective::new("intl.use_exceptions", Some("0"), Access::ALL, ModifyHandler::Bool, "intl"));
        sync_default_locale(m.vm());
        let handlers = all_handlers();
        registry::register(m, &handlers, &mut class_opts);
        install_ini_hooks(m);
        m.on_request_startup(|vm| {
            core::reset_global();
            sync_default_locale(vm);
        });
    }
}
