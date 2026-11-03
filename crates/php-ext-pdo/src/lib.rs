//! `ext/pdo`, `ext/pdo_sqlite` and `ext/sqlite3` for the phprust VM: binds
//! the `php-pdo` core (with its `SQLite` driver) and the `php-sqlite`
//! `SQLite3` classes through the `php_vm::ext` extension API.
//!
//! * [`host`] — [`host::VmHost`], the VM seen through the `PdoHost` and
//!   `php_sqlite::Host` traits, and the error conversions.
//! * [`pdo`] — `PDO`, `PDOException`, `Pdo\Sqlite`, `pdo_drivers()`.
//! * [`stmt`] — `PDOStatement` (iteration, read-only `queryString`) and
//!   `PDORow`.
//! * [`sqlite3`] — `SQLite3`, `SQLite3Stmt`, `SQLite3Result`,
//!   `SQLite3Exception`.

// "SQLite" is a proper noun, not an identifier.
#![allow(clippy::doc_markdown)]
// Native handlers and object hooks must match the VM's fn-pointer types.
#![allow(clippy::unnecessary_wraps)]
// Test snippets are PHP source; uniform raw strings read best.
#![cfg_attr(test, allow(clippy::needless_raw_string_hashes))]

pub mod host;
pub mod pdo;
pub mod sqlite3;
pub mod stmt;
pub mod streams;

#[cfg(test)]
mod tests;

use php_runtime::Value;
use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_vm::ext::{Extension, ModuleBuilder};

/// The `PDO` extension (also registers `pdo_sqlite` and `sqlite3`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Pdo;

impl Extension for Pdo {
    fn name(&self) -> &'static str {
        "PDO"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        for &(name, v) in php_sqlite::constants::SQLITE3_GLOBAL_CONSTANTS {
            m.constant(name, Value::Int(v));
        }
        m.ini(IniDirective::new(
            "sqlite3.extension_dir",
            None,
            Access::SYSTEM,
            ModifyHandler::String,
            "sqlite3",
        ));
        m.ini(IniDirective::new(
            "sqlite3.defensive",
            Some("1"),
            Access::USER,
            ModifyHandler::Bool,
            "sqlite3",
        ));
        let vm = m.vm();
        pdo::register(vm);
        stmt::register(vm);
        sqlite3::register(vm);
        for ext in ["pdo_sqlite", "sqlite3"] {
            if !vm.loaded_extensions.iter().any(|e| e == ext) {
                vm.loaded_extensions.push(ext.to_string());
            }
        }
    }
}
