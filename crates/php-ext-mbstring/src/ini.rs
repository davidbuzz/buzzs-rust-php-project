// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `mbstring.*` INI table and its on-modify semantics.
//!
//! The directives are registered without a built-in modify handler; the
//! `OnUpdate_mbstring_*` handlers of `php-mbstring` ([`MbState::ini_set`])
//! run as `ini_on_modify` hooks, before `ini_set()` stores a value (a
//! rejected value is not stored) and after `ini_restore()`. PHP's own
//! `default_charset`, `internal_encoding`, `input_encoding` and
//! `output_encoding` run mbstring's `php_internal_encoding_changed` hook
//! the same way.

use php_mbstring::MbState;
use php_runtime::ZStr;
use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_support::Diagnostic;
use php_vm::ext::{IniStage, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::state;

/// `E_WARNING`.
const E_WARNING: i64 = 2;
/// `E_NOTICE`.
const E_NOTICE: i64 = 8;
/// `E_DEPRECATED`.
const E_DEPRECATED: i64 = 8192;

/// Directives whose handler lives in `php-mbstring`, in registration order.
const HANDLED: [&str; 9] = [
    "mbstring.language",
    "mbstring.detect_order",
    "mbstring.http_input",
    "mbstring.http_output",
    "mbstring.internal_encoding",
    "mbstring.substitute_character",
    "mbstring.encoding_translation",
    "mbstring.http_output_conv_mimetypes",
    "mbstring.strict_detection",
];

/// PHP core directives that trigger mbstring's encoding hook.
const CORE_HOOKED: [&str; 4] = [
    "default_charset",
    "internal_encoding",
    "input_encoding",
    "output_encoding",
];

/// Registers the INI entries (`PHP_INI_BEGIN` in `mbstring.c`).
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let none = ModifyHandler::None;
    let dirs = [
        ("mbstring.language", Some("neutral"), Access::ALL, none),
        ("mbstring.detect_order", None, Access::ALL, none),
        ("mbstring.http_input", None, Access::ALL, none),
        ("mbstring.http_output", None, Access::ALL, none),
        ("mbstring.internal_encoding", None, Access::ALL, none),
        ("mbstring.substitute_character", None, Access::ALL, none),
        (
            "mbstring.encoding_translation",
            Some("0"),
            Access(Access::SYSTEM.0 | Access::PERDIR.0),
            ModifyHandler::Bool,
        ),
        (
            "mbstring.http_output_conv_mimetypes",
            Some(r"^(text/|application/xhtml\+xml)"),
            Access::ALL,
            none,
        ),
        (
            "mbstring.strict_detection",
            Some("0"),
            Access::ALL,
            ModifyHandler::Bool,
        ),
        (
            "mbstring.regex_stack_limit",
            Some("100000"),
            Access::ALL,
            ModifyHandler::Long,
        ),
        (
            "mbstring.regex_retry_limit",
            Some("1000000"),
            Access::ALL,
            ModifyHandler::Long,
        ),
    ];
    for (name, default, access, handler) in dirs {
        m.ini(IniDirective::new(
            name, default, access, handler, "mbstring",
        ));
    }
}

/// `MINIT` + `RINIT`: builds the state from the startup INI values and
/// reports the handlers' diagnostics as `PHP Startup` errors.
///
/// The default regex options and syntax (`mb_regex_set_options()`) are
/// module globals php-src sets in `PHP_GINIT(mb_regex)` and no request
/// resets: a VM made for a later request on this thread (`php --repeat`)
/// starts with what the thread's previous request left, as php-src's
/// second run does.
pub(crate) fn startup(vm: &mut Vm) {
    let mut st = state_from_ini(vm);
    let diags = st.take_diagnostics();
    st.set_regex_defaults(state::with(|prev| prev.regex_defaults()));
    state::replace(st);
    // Startup errors are displayed before the request: they do not count
    // as sent headers (other modules' INI handlers check that).
    let headers_sent = vm.headers_sent;
    for d in diags {
        let level = match d.level {
            php_mbstring::Level::Warning => E_WARNING,
            php_mbstring::Level::Notice => E_NOTICE,
            php_mbstring::Level::Deprecated => E_DEPRECATED,
        };
        let _ = vm.error_at(
            level,
            &format!("PHP Startup: {}", d.message),
            &ZStr::from("Unknown"),
            0,
        );
    }
    vm.headers_sent = headers_sent;
}

/// The INI values the module state is made from.
fn ini_settings(vm: &Vm) -> Vec<(&'static str, Vec<u8>)> {
    let mut settings: Vec<(&str, Vec<u8>)> = Vec::new();
    for name in CORE_HOOKED.iter().chain(HANDLED.iter()) {
        if let Some(v) = vm.ini.raw(name) {
            settings.push((name, v.to_vec()));
        }
    }
    settings
}

/// The module state for the INI values in force: `MINIT` from the
/// startup values, then `PHP_RINIT`'s "current" values.
fn state_from_ini(vm: &Vm) -> MbState {
    let settings = ini_settings(vm);
    let refs: Vec<(&str, &[u8])> = settings.iter().map(|(k, v)| (*k, v.as_slice())).collect();
    let mut st = MbState::with_ini(&refs);
    if let Some(v) = raw(vm, "mbstring.http_output_conv_mimetypes") {
        st.ini_set("mbstring.http_output_conv_mimetypes", Some(&v), false);
    }
    st
}

/// `PHP_RINIT(mbstring)` after the previous request's INI changes were
/// undone: php-src's `zend_ini_deactivate()` runs each entry's update
/// handler as it restores the value (`mbstring.language`,
/// `mbstring.internal_encoding`, `mbstring.substitute_character`, ...),
/// and RINIT resets the "current" values (`mb_internal_encoding()`,
/// `mb_substitute_character()`, `mb_detect_order()`) from them. Here
/// the VM restores the values without the handlers, so the state is
/// made again from them ([`MbState::restore_ini`], which keeps what the
/// request's input handling recorded). Diagnostics were reported at
/// startup.
pub(crate) fn request_startup(vm: &mut Vm) {
    let settings = ini_settings(vm);
    let refs: Vec<(&str, &[u8])> = settings.iter().map(|(k, v)| (*k, v.as_slice())).collect();
    let mimetypes = raw(vm, "mbstring.http_output_conv_mimetypes");
    state::with(|st| {
        st.restore_ini(&refs);
        if let Some(v) = mimetypes {
            st.ini_set("mbstring.http_output_conv_mimetypes", Some(&v), false);
        }
        let _ = st.take_diagnostics();
    });
}

/// Stores a value without running any handler (`zend_alter_ini_entry` from
/// `mb_language()`, whose handler already ran).
pub(crate) fn store_raw(vm: &mut Vm, name: &str, value: &[u8]) {
    let mut sink: Vec<Diagnostic> = Vec::new();
    let _ = vm.ini.ini_set(name, value, &mut sink);
}

/// The stored value (`None` for `NULL`).
fn raw(vm: &Vm, name: &str) -> Option<Vec<u8>> {
    vm.ini.raw(name).map(<[u8]>::to_vec)
}

/// `on_modify` for directive `name`: `value` is the new value (runtime)
/// or the restored one (`ini_restore()`, read back as stored so `NULL`
/// stays `NULL`).
fn on_modify(vm: &mut Vm, name: &str, value: &[u8]) -> VmResult<bool> {
    let value = match vm.ini_modify_stage() {
        IniStage::Runtime | IniStage::Activate | IniStage::Htaccess => Some(value.to_vec()),
        IniStage::Restore => raw(vm, name),
    };
    // OnUpdateDefaultCharset (main/main.c) rejects a header-breaking value
    // before php_internal_encoding_changed() runs (bug81518b.phpt).
    if name == "default_charset"
        && value
            .as_deref()
            .is_some_and(|v| v.iter().any(|&c| matches!(c, 0 | b'\r' | b'\n')))
    {
        return Ok(false);
    }
    let ok = state::with(|s| s.ini_set(name, value.as_deref(), true));
    state::flush_diagnostics(vm)?;
    Ok(ok)
}

macro_rules! hooks {
    ($($f:ident => $name:literal),* $(,)?) => {
        $(fn $f(vm: &mut Vm, value: &[u8]) -> VmResult<bool> {
            on_modify(vm, $name, value)
        })*

        /// Registers the `on_modify` hooks.
        pub(crate) fn register_hooks(m: &mut ModuleBuilder<'_>) {
            $(m.ini_on_modify($name, $f);)*
        }
    };
}

hooks! {
    language => "mbstring.language",
    detect_order => "mbstring.detect_order",
    http_input => "mbstring.http_input",
    http_output => "mbstring.http_output",
    internal_encoding => "mbstring.internal_encoding",
    substitute_character => "mbstring.substitute_character",
    conv_mimetypes => "mbstring.http_output_conv_mimetypes",
    strict_detection => "mbstring.strict_detection",
    default_charset => "default_charset",
    php_internal_encoding => "internal_encoding",
    php_input_encoding => "input_encoding",
    php_output_encoding => "output_encoding",
}
