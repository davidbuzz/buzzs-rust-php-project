//! PHP's `ext/pdo` core for phprust: the `PDO`, `PDOStatement` and `PDORow`
//! classes as Rust handles, the driver interface, the fetch engine and the
//! error modes, plus the adapter for the `SQLite` driver of `php-sqlite`.
//!
//! References: `ext/pdo/pdo.c`, `ext/pdo/pdo_dbh.c`, `ext/pdo/pdo_stmt.c`,
//! `ext/pdo/php_pdo_driver.h`, `ext/pdo/*.stub.php`.
//!
//! # Overview
//!
//! * [`driver`] — [`DbhDriver`] / [`StmtDriver`] (the `pdo_dbh_methods` /
//!   `pdo_stmt_methods` tables) and the driver registry
//!   ([`driver::drivers`], [`driver::register_driver`]); [`sqlite`] adapts
//!   `php_sqlite::pdo` to them.
//! * [`Pdo`] — the state of a `PDO` object: construction (DSN aliases,
//!   `uri:`, persistent connections, `PDO::connect()` driver subclasses),
//!   attributes, transactions, `exec`/`prepare`/`query`/`quote`, errors.
//! * [`PdoStatement`] — the state of a `PDOStatement` object: binding,
//!   `execute`, every fetch mode ([`fetch`]), metadata,
//!   `debugDumpParams()`, iteration ([`StmtIterator`]).
//! * [`PdoRow`] — the lazy `PDO::FETCH_LAZY` row object.
//! * [`PdoHost`] — what the VM provides (object creation, constructor and
//!   callable calls, property writes, `PDOException` objects, output,
//!   `php.ini`, the [`PersistentList`]).
//! * [`classes`] / [`consts`] — the class table and constants for the VM.
//!
//! Every public method takes the host and returns `RtResult`: `Err` is
//! the exception the PHP method throws (warnings go through
//! [`PdoHost::docref`]).

pub mod classes;
pub mod consts;
pub mod dbh;
pub mod driver;
pub mod dsn;
pub mod error;
pub mod fetch;
pub mod host;
pub mod persistent;
pub mod row;
pub mod sqlite;
pub mod stmt;

pub use dbh::{ConnectArgs, Pdo};
pub use driver::{DbhDriver, StmtDriver};
pub use fetch::StmtIterator;
pub use host::{ClassInfo, Ctx, DriverCx, NullHost, PdoException, PdoHost};
pub use persistent::PersistentList;
pub use row::PdoRow;
pub use stmt::{ParamId, PdoStatement};
