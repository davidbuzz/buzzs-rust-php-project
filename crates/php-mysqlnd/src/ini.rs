// Derived from php-src ext/mysqlnd/php_mysqlnd.c. Similarity metric to original: 35%
//   Copyright © The PHP Group and Contributors.
//   Authors: Andrey Hristov <andrey@php.net>
//            Ulf Wendel <uw@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The `mysqlnd.*` INI settings (ref: `php_mysqlnd.c`).
//!
//! Settings live in a thread-local table; the embedding SAPI forwards INI
//! changes with [`set`] and reads defaults for registration from [`ENTRIES`].

use std::cell::RefCell;

use crate::consts::NET_CMD_BUFFER_MIN_SIZE;

/// Where an INI entry may be changed (`PHP_INI_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifiable {
    /// `PHP_INI_ALL`
    All,
    /// `PHP_INI_SYSTEM`
    System,
    /// `PHP_INI_PERDIR`
    PerDir,
}

/// One registered INI entry: name, default (`None` = NULL) and scope.
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    /// Directive name.
    pub name: &'static str,
    /// Default value (`None` is a NULL default, shown as "no value").
    pub default: Option<&'static str>,
    /// Modification scope.
    pub modifiable: Modifiable,
}

/// The `mysqlnd.*` directives in registration order.
pub const ENTRIES: &[Entry] = &[
    Entry {
        name: "mysqlnd.collect_statistics",
        default: Some("1"),
        modifiable: Modifiable::All,
    },
    Entry {
        name: "mysqlnd.collect_memory_statistics",
        default: Some("0"),
        modifiable: Modifiable::System,
    },
    Entry {
        name: "mysqlnd.debug",
        default: None,
        modifiable: Modifiable::System,
    },
    Entry {
        name: "mysqlnd.trace_alloc",
        default: None,
        modifiable: Modifiable::System,
    },
    Entry {
        name: "mysqlnd.net_cmd_buffer_size",
        default: Some("4096"),
        modifiable: Modifiable::All,
    },
    Entry {
        name: "mysqlnd.net_read_buffer_size",
        default: Some("32768"),
        modifiable: Modifiable::All,
    },
    Entry {
        name: "mysqlnd.net_read_timeout",
        default: Some("86400"),
        modifiable: Modifiable::All,
    },
    Entry {
        name: "mysqlnd.log_mask",
        default: Some("0"),
        modifiable: Modifiable::All,
    },
    Entry {
        name: "mysqlnd.mempool_default_size",
        default: Some("16000"),
        modifiable: Modifiable::All,
    },
    Entry {
        name: "mysqlnd.sha256_server_public_key",
        default: None,
        modifiable: Modifiable::PerDir,
    },
];

/// Current values of the `mysqlnd.*` globals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `mysqlnd.collect_statistics`
    pub collect_statistics: bool,
    /// `mysqlnd.collect_memory_statistics`
    pub collect_memory_statistics: bool,
    /// `mysqlnd.debug`
    pub debug: Option<String>,
    /// `mysqlnd.trace_alloc`
    pub trace_alloc: Option<String>,
    /// `mysqlnd.net_cmd_buffer_size`
    pub net_cmd_buffer_size: usize,
    /// `mysqlnd.net_read_buffer_size`
    pub net_read_buffer_size: usize,
    /// `mysqlnd.net_read_timeout` (seconds)
    pub net_read_timeout: i64,
    /// `mysqlnd.log_mask`
    pub log_mask: i64,
    /// `mysqlnd.mempool_default_size`
    pub mempool_default_size: i64,
    /// `mysqlnd.sha256_server_public_key` (a PEM file path)
    pub sha256_server_public_key: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            collect_statistics: true,
            collect_memory_statistics: false,
            debug: None,
            trace_alloc: None,
            net_cmd_buffer_size: NET_CMD_BUFFER_MIN_SIZE,
            net_read_buffer_size: 32768,
            net_read_timeout: 86400,
            log_mask: 0,
            mempool_default_size: 16000,
            sha256_server_public_key: None,
        }
    }
}

thread_local! {
    static SETTINGS: RefCell<Settings> = RefCell::new(Settings::default());
}

/// Snapshot of the current settings.
#[must_use]
pub fn settings() -> Settings {
    SETTINGS.with(|s| s.borrow().clone())
}

/// `MYSQLND_G(collect_statistics)`. `false` once the thread's settings are
/// gone: a persistent connection cached in another thread-local can be
/// closed after them while the process exits (module shutdown).
#[must_use]
pub fn collect_statistics() -> bool {
    SETTINGS
        .try_with(|s| s.borrow().collect_statistics)
        .unwrap_or(false)
}

/// `OnUpdateBool`: `zend_ini_parse_bool()`.
fn parse_bool(v: &str) -> bool {
    php_support::ini::parse_bool(v.as_bytes())
}

/// `OnUpdateLong`: `zend_ini_parse_quantity()` (`"64K"`, `"0x10"`).
fn parse_long(v: &str) -> i64 {
    php_support::ini::parse_quantity(v.as_bytes()).0
}

/// Applies an INI change. Returns `false` when the directive is unknown or
/// the value is rejected (`OnUpdateNetCmdBufferSize` refuses values below 4096).
/// `None` resets a string directive to NULL.
pub fn set(name: &str, value: Option<&str>) -> bool {
    SETTINGS.with(|s| {
        let mut s = s.borrow_mut();
        let v = value.unwrap_or("");
        match name {
            "mysqlnd.collect_statistics" => s.collect_statistics = parse_bool(v),
            "mysqlnd.collect_memory_statistics" => s.collect_memory_statistics = parse_bool(v),
            "mysqlnd.debug" => s.debug = value.map(str::to_string),
            "mysqlnd.trace_alloc" => s.trace_alloc = value.map(str::to_string),
            "mysqlnd.net_cmd_buffer_size" => {
                // OnUpdateNetCmdBufferSize reads it with ZEND_ATOL().
                let n = php_support::atol(v.as_bytes());
                match usize::try_from(n) {
                    Ok(n) if n >= NET_CMD_BUFFER_MIN_SIZE => s.net_cmd_buffer_size = n,
                    _ => return false,
                }
            }
            "mysqlnd.net_read_buffer_size" => {
                s.net_read_buffer_size = usize::try_from(parse_long(v)).unwrap_or(0);
            }
            "mysqlnd.net_read_timeout" => s.net_read_timeout = parse_long(v),
            "mysqlnd.log_mask" => s.log_mask = parse_long(v),
            "mysqlnd.mempool_default_size" => s.mempool_default_size = parse_long(v),
            "mysqlnd.sha256_server_public_key" => {
                s.sha256_server_public_key = value.map(str::to_string);
            }
            _ => return false,
        }
        true
    })
}

/// Restores every directive to its default.
pub fn reset() {
    SETTINGS.with(|s| *s.borrow_mut() = Settings::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_updates() {
        reset();
        assert!(collect_statistics());
        assert_eq!(settings().net_read_timeout, 86400);
        assert!(!set("mysqlnd.net_cmd_buffer_size", Some("100")));
        assert!(set("mysqlnd.net_cmd_buffer_size", Some("8192")));
        assert_eq!(settings().net_cmd_buffer_size, 8192);
        assert!(set("mysqlnd.collect_statistics", Some("off")));
        assert!(!collect_statistics());
        assert!(!set("mysqlnd.bogus", Some("1")));
        reset();
        assert_eq!(ENTRIES.len(), 10);
    }

    #[test]
    fn values_parse_like_their_handlers() {
        reset();
        // OnUpdateLong: zend_ini_parse_quantity().
        assert!(set("mysqlnd.net_read_buffer_size", Some("64K")));
        assert_eq!(settings().net_read_buffer_size, 65536);
        assert!(set("mysqlnd.log_mask", Some("0x10")));
        assert_eq!(settings().log_mask, 16);
        // OnUpdateNetCmdBufferSize: ZEND_ATOL(), no suffix.
        assert!(!set("mysqlnd.net_cmd_buffer_size", Some("8K")));
        // OnUpdateBool: zend_ini_parse_bool() does not trim.
        assert!(set("mysqlnd.collect_statistics", Some(" on")));
        assert!(!collect_statistics());
        reset();
    }
}
