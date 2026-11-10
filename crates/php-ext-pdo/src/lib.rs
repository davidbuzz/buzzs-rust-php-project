//! `ext/pdo`, `ext/pdo_mysql`, `ext/pdo_sqlite` and `ext/sqlite3` for the
//! phprust VM: binds the `php-pdo` core (with its `MySQL` and `SQLite` drivers) and the `php-sqlite`
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
use php_vm::ext::{Extension, InfoItem, ModuleBuilder};
use php_vm::Vm;

/// The `PDO` extension (also registers `pdo_mysql`, `pdo_sqlite` and
/// `sqlite3`).
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
        m.ini(IniDirective::new(
            "pdo_mysql.default_socket",
            Some(php_pdo::mysql::DEFAULT_SOCKET),
            Access::SYSTEM,
            ModifyHandler::String,
            "pdo_mysql",
        ));
        let vm = m.vm();
        pdo::register(vm);
        stmt::register(vm);
        sqlite3::register(vm);
        // `PHP_MINFO` of the four modules this extension stands for.
        vm.module_info.push(("PDO".to_string(), pdo_info));
        vm.module_info
            .push(("pdo_mysql".to_string(), pdo_mysql_info));
        vm.module_info
            .push(("pdo_sqlite".to_string(), pdo_sqlite_info));
        vm.module_info.push(("sqlite3".to_string(), sqlite3_info));
        for ext in ["pdo_mysql", "pdo_sqlite", "sqlite3"] {
            if !vm.loaded_extensions.iter().any(|e| e == ext) {
                vm.loaded_extensions.push(ext.to_string());
            }
        }
    }
}

/// A two-column `phpinfo()` row.
fn info_row(a: &str, b: &str) -> InfoItem {
    InfoItem::Row(vec![a.to_string(), b.to_string()])
}

/// `PHP_MINFO_FUNCTION(pdo)`: support and the registered drivers.
fn pdo_info(_vm: &mut Vm) -> Vec<InfoItem> {
    let drivers: Vec<String> = php_pdo::driver::available_drivers()
        .values()
        .map(|v| match v.deref() {
            Value::Str(s) => s.to_string_lossy(),
            _ => String::new(),
        })
        .collect();
    vec![
        InfoItem::TableStart,
        info_row("PDO support", "enabled"),
        info_row("PDO drivers", &drivers.join(", ")),
        InfoItem::TableEnd,
    ]
}

/// `PHP_MINFO_FUNCTION(pdo_mysql)` (the client is mysqlnd).
fn pdo_mysql_info(_vm: &mut Vm) -> Vec<InfoItem> {
    vec![
        InfoItem::TableStart,
        info_row("PDO Driver for MySQL", "enabled"),
        info_row("Client API version", php_pdo::mysql::CLIENT_INFO),
        InfoItem::TableEnd,
        InfoItem::IniEntries,
    ]
}

/// `PHP_MINFO_FUNCTION(pdo_sqlite)`.
fn pdo_sqlite_info(_vm: &mut Vm) -> Vec<InfoItem> {
    vec![
        InfoItem::TableStart,
        info_row("PDO Driver for SQLite 3.x", "enabled"),
        info_row("SQLite Library", &php_sqlite::sqlite_version()),
        InfoItem::TableEnd,
    ]
}

/// `PHP_MINFO_FUNCTION(sqlite3)`.
fn sqlite3_info(_vm: &mut Vm) -> Vec<InfoItem> {
    vec![
        InfoItem::TableStart,
        info_row("SQLite3 support", "enabled"),
        info_row("SQLite Library", &php_sqlite::sqlite_version()),
        InfoItem::TableEnd,
        InfoItem::IniEntries,
    ]
}
