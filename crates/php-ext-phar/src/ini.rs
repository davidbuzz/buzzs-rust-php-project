// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `phar.*` INI directives.
//!
//! The directives are registered with the VM (for `ini_get()`, `-d`
//! values and `ini_get_all()`) and mirrored into `php-phar`'s registry:
//! at startup the (possibly `-d`-overridden) values become the "original"
//! values (`PHAR_G(readonly_orig)`), and `ini_set()` / `ini_restore()` go
//! through `phar_ini_modify_handler` (an `on_modify` hook): a value that
//! was originally on cannot be turned off at runtime.

use php_phar::registry as reg;
use php_support::ini::{parse_bool, Access, IniDirective, IniDisplayer, ModifyHandler};
use php_vm::ext::{ModuleBuilder, VmResult};
use php_vm::Vm;

const BOOL_DIRECTIVES: [&str; 2] = ["phar.readonly", "phar.require_hash"];

/// Registers the directives and their modify handlers.
pub fn register(m: &mut ModuleBuilder<'_>) {
    for name in BOOL_DIRECTIVES {
        // STD_PHP_INI_BOOLEAN in ext/phar/phar.c.
        m.ini(
            IniDirective::new(name, Some("1"), Access::ALL, ModifyHandler::None, "Phar")
                .with_displayer(IniDisplayer::Boolean),
        );
    }
    m.ini(IniDirective::new(
        "phar.cache_list",
        Some(""),
        Access::SYSTEM,
        ModifyHandler::None,
        "Phar",
    ));
    m.ini_on_modify("phar.readonly", modify_readonly);
    m.ini_on_modify("phar.require_hash", modify_require_hash);
    let vm = m.vm();
    for name in BOOL_DIRECTIVES {
        let v = vm.ini_string(name).unwrap_or_default();
        reg::set_ini(name, parse_bool(&v), true);
    }
    if let Some(list) = vm.ini_string("phar.cache_list") {
        if !list.is_empty() {
            reg::load_cache_list(&list);
        }
    }
}

#[allow(clippy::unnecessary_wraps)] // signature fixed by `IniModifyFn`
fn modify_readonly(_vm: &mut Vm, value: &[u8]) -> VmResult<bool> {
    Ok(reg::set_ini("phar.readonly", parse_bool(value), false))
}

#[allow(clippy::unnecessary_wraps)] // signature fixed by `IniModifyFn`
fn modify_require_hash(_vm: &mut Vm, value: &[u8]) -> VmResult<bool> {
    Ok(reg::set_ini("phar.require_hash", parse_bool(value), false))
}

#[cfg(test)]
mod tests {
    use super::BOOL_DIRECTIVES;

    #[test]
    fn directives() {
        assert_eq!(BOOL_DIRECTIVES, ["phar.readonly", "phar.require_hash"]);
    }
}
