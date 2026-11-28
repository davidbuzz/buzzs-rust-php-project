// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/pcre` for the phprust VM: binds the `php-pcre` library (PHP's
//! `preg_*` semantics over PCRE2) through the `php_vm::ext` extension API.
//!
//! This crate is the glue of `ext/pcre/php_pcre.c`: argument handling,
//! array/string subjects, `$matches`/`$count` by-reference outputs, user
//! callbacks, the per-request cache and `preg_last_error()` state (see
//! [`state`]), and the `PREG_*` / `PCRE_*` constants. Pattern parsing,
//! matching, replacement and splitting live in `php-pcre`.
//!
//! The `pcre.*` INI directives are registered with on-modify handlers
//! that mark the cached match configuration stale (PHP's handlers update
//! the shared match context instead). Locale-aware character tables follow the C library's current
//! `LC_CTYPE`, which is what `setlocale()` changes.

mod convert;
mod functions;
mod memory;
mod state;

use php_runtime::Value;
use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_vm::ext::{Extension, InfoItem, ModuleBuilder, VmResult};
use php_vm::Vm;

/// The `pcre` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pcre;

impl Extension for Pcre {
    fn name(&self) -> &'static str {
        "pcre"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        functions::register(m);
        register_constants(m);
        register_ini(m);
        m.info(module_info);
        m.on_request_startup(|_| state::reset());
        php_vm::ext::set_preg_leaf_hook(state::leaf_cached);
        m.on_request_shutdown(|_| state::reset());
    }
}

/// `PHP_INI_BEGIN()` of `php_pcre.c`. (Directives the VM core already
/// registered keep their existing definition.)
fn register_ini(m: &mut ModuleBuilder<'_>) {
    let d = |name: &str, default: &str, handler| {
        IniDirective::new(name, Some(default), Access::ALL, handler, "pcre")
    };
    m.ini(d("pcre.backtrack_limit", "1000000", ModifyHandler::Long))
        .ini(d("pcre.recursion_limit", "100000", ModifyHandler::Long))
        .ini(d("pcre.jit", "1", ModifyHandler::Bool));
    for name in ["pcre.backtrack_limit", "pcre.recursion_limit", "pcre.jit"] {
        m.ini_on_modify(name, on_modify);
    }
}

/// `OnUpdateBacktrackLimit` / `OnUpdateRecursionLimit` / `OnUpdateJit`:
/// the value itself is parsed by the registry; the cached match
/// configuration is refreshed before the next operation.
#[allow(clippy::unnecessary_wraps)] // the `IniModifyFn` signature
fn on_modify(_vm: &mut Vm, _value: &[u8]) -> VmResult<bool> {
    state::invalidate_config();
    Ok(true)
}

/// `PHP_MINFO_FUNCTION(pcre)`.
fn module_info(_vm: &mut Vm) -> Vec<InfoItem> {
    info_items()
}

/// The rows of the `pcre` `phpinfo()` section.
fn info_items() -> Vec<InfoItem> {
    let row = |k: &str, v: String| InfoItem::Row(vec![k.to_string(), v]);
    let mut items = vec![
        InfoItem::TableStart,
        row(
            "PCRE (Perl Compatible Regular Expressions) Support",
            "enabled".into(),
        ),
        row("PCRE Library Version", php_pcre::pcre_version()),
        row("PCRE Unicode Version", php_pcre::unicode_version()),
        row(
            "PCRE JIT Support",
            if php_pcre::jit_support() {
                "enabled"
            } else {
                "disabled"
            }
            .into(),
        ),
    ];
    if let Some(target) = php_pcre::jit_target() {
        items.push(row("PCRE JIT Target", target));
    }
    items.push(InfoItem::TableEnd);
    items.push(InfoItem::IniEntries);
    items
}

fn register_constants(m: &mut ModuleBuilder<'_>) {
    let ints: [(&str, i64); 15] = [
        ("PREG_PATTERN_ORDER", php_pcre::PREG_PATTERN_ORDER),
        ("PREG_SET_ORDER", php_pcre::PREG_SET_ORDER),
        ("PREG_OFFSET_CAPTURE", php_pcre::PREG_OFFSET_CAPTURE),
        ("PREG_UNMATCHED_AS_NULL", php_pcre::PREG_UNMATCHED_AS_NULL),
        ("PREG_SPLIT_NO_EMPTY", php_pcre::PREG_SPLIT_NO_EMPTY),
        (
            "PREG_SPLIT_DELIM_CAPTURE",
            php_pcre::PREG_SPLIT_DELIM_CAPTURE,
        ),
        (
            "PREG_SPLIT_OFFSET_CAPTURE",
            php_pcre::PREG_SPLIT_OFFSET_CAPTURE,
        ),
        ("PREG_GREP_INVERT", php_pcre::PREG_GREP_INVERT),
        ("PREG_NO_ERROR", php_pcre::PREG_NO_ERROR),
        ("PREG_INTERNAL_ERROR", php_pcre::PREG_INTERNAL_ERROR),
        (
            "PREG_BACKTRACK_LIMIT_ERROR",
            php_pcre::PREG_BACKTRACK_LIMIT_ERROR,
        ),
        (
            "PREG_RECURSION_LIMIT_ERROR",
            php_pcre::PREG_RECURSION_LIMIT_ERROR,
        ),
        ("PREG_BAD_UTF8_ERROR", php_pcre::PREG_BAD_UTF8_ERROR),
        (
            "PREG_BAD_UTF8_OFFSET_ERROR",
            php_pcre::PREG_BAD_UTF8_OFFSET_ERROR,
        ),
        (
            "PREG_JIT_STACKLIMIT_ERROR",
            php_pcre::PREG_JIT_STACKLIMIT_ERROR,
        ),
    ];
    for (name, v) in ints {
        m.constant(name, Value::Int(v));
    }
    let (major, minor) = php_pcre::version_numbers();
    m.constant("PCRE_VERSION", Value::from(php_pcre::pcre_version()))
        .constant("PCRE_VERSION_MAJOR", Value::Int(major))
        .constant("PCRE_VERSION_MINOR", Value::Int(minor))
        .constant("PCRE_JIT_SUPPORT", Value::Bool(php_pcre::jit_support()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phpinfo_rows() {
        let items = info_items();
        assert_eq!(items.first(), Some(&InfoItem::TableStart));
        assert_eq!(items.last(), Some(&InfoItem::IniEntries));
        let row = |k: &str| {
            items
                .iter()
                .any(|i| matches!(i, InfoItem::Row(c) if c[0] == k))
        };
        assert!(row("PCRE (Perl Compatible Regular Expressions) Support"));
        assert!(row("PCRE Library Version"));
        assert!(row("PCRE Unicode Version"));
        assert!(row("PCRE JIT Support"));
    }
}
