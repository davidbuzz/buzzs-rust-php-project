// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `PDO::*` constants (`php_pdo_driver.h`, `pdo_dbh.stub.php`).
//!
//! Values follow the php-src reference tree, where the fetch flags were
//! renumbered (`PDO::FETCH_GROUP` is `1 << 5`, ...).

#![allow(missing_docs)]

pub const PARAM_NULL: i64 = 0;
pub const PARAM_INT: i64 = 1;
pub const PARAM_STR: i64 = 2;
pub const PARAM_LOB: i64 = 3;
pub const PARAM_STMT: i64 = 4;
pub const PARAM_BOOL: i64 = 5;
pub const PARAM_INPUT_OUTPUT: i64 = 0x8000_0000;
pub const PARAM_STR_NATL: i64 = 0x4000_0000;
pub const PARAM_STR_CHAR: i64 = 0x2000_0000;
/// `PDO_PARAM_FLAGS`.
pub const PARAM_FLAGS: i64 = 0xFFFF_0000;

pub const PARAM_EVT_ALLOC: i64 = 0;
pub const PARAM_EVT_FREE: i64 = 1;
pub const PARAM_EVT_EXEC_PRE: i64 = 2;
pub const PARAM_EVT_EXEC_POST: i64 = 3;
pub const PARAM_EVT_FETCH_PRE: i64 = 4;
pub const PARAM_EVT_FETCH_POST: i64 = 5;
pub const PARAM_EVT_NORMALIZE: i64 = 6;

pub const FETCH_DEFAULT: i64 = 0;
pub const FETCH_LAZY: i64 = 1;
pub const FETCH_ASSOC: i64 = 2;
pub const FETCH_NUM: i64 = 3;
pub const FETCH_BOTH: i64 = 4;
pub const FETCH_OBJ: i64 = 5;
pub const FETCH_BOUND: i64 = 6;
pub const FETCH_COLUMN: i64 = 7;
pub const FETCH_CLASS: i64 = 8;
pub const FETCH_INTO: i64 = 9;
pub const FETCH_FUNC: i64 = 10;
pub const FETCH_NAMED: i64 = 11;
pub const FETCH_KEY_PAIR: i64 = 12;
/// `PDO_FETCH_FLAGS`: the flag bits of a fetch mode.
pub const FETCH_FLAGS: i64 = 0xFFFF_FFF0;
pub const FETCH_GROUP: i64 = 1 << 5;
pub const FETCH_UNIQUE: i64 = 1 << 6;
pub const FETCH_CLASSTYPE: i64 = 1 << 7;
pub const FETCH_PROPS_LATE: i64 = 1 << 8;
pub const FETCH_SERIALIZE: i64 = 1 << 9;
/// `PDO_FIRST_INVALID_FLAG`.
pub const FIRST_INVALID_FLAG: i64 = 1 << 10;

pub const FETCH_ORI_NEXT: i64 = 0;
pub const FETCH_ORI_PRIOR: i64 = 1;
pub const FETCH_ORI_FIRST: i64 = 2;
pub const FETCH_ORI_LAST: i64 = 3;
pub const FETCH_ORI_ABS: i64 = 4;
pub const FETCH_ORI_REL: i64 = 5;

pub const ATTR_AUTOCOMMIT: i64 = 0;
pub const ATTR_PREFETCH: i64 = 1;
pub const ATTR_TIMEOUT: i64 = 2;
pub const ATTR_ERRMODE: i64 = 3;
pub const ATTR_SERVER_VERSION: i64 = 4;
pub const ATTR_CLIENT_VERSION: i64 = 5;
pub const ATTR_SERVER_INFO: i64 = 6;
pub const ATTR_CONNECTION_STATUS: i64 = 7;
pub const ATTR_CASE: i64 = 8;
pub const ATTR_CURSOR_NAME: i64 = 9;
pub const ATTR_CURSOR: i64 = 10;
pub const ATTR_ORACLE_NULLS: i64 = 11;
pub const ATTR_PERSISTENT: i64 = 12;
pub const ATTR_STATEMENT_CLASS: i64 = 13;
pub const ATTR_FETCH_TABLE_NAMES: i64 = 14;
pub const ATTR_FETCH_CATALOG_NAMES: i64 = 15;
pub const ATTR_DRIVER_NAME: i64 = 16;
pub const ATTR_STRINGIFY_FETCHES: i64 = 17;
pub const ATTR_MAX_COLUMN_LEN: i64 = 18;
pub const ATTR_DEFAULT_FETCH_MODE: i64 = 19;
pub const ATTR_EMULATE_PREPARES: i64 = 20;
pub const ATTR_DEFAULT_STR_PARAM: i64 = 21;
/// `PDO_ATTR_DRIVER_SPECIFIC`.
pub const ATTR_DRIVER_SPECIFIC: i64 = 1000;

pub const ERRMODE_SILENT: i64 = 0;
pub const ERRMODE_WARNING: i64 = 1;
pub const ERRMODE_EXCEPTION: i64 = 2;

pub const CASE_NATURAL: i64 = 0;
pub const CASE_UPPER: i64 = 1;
pub const CASE_LOWER: i64 = 2;

pub const NULL_NATURAL: i64 = 0;
pub const NULL_EMPTY_STRING: i64 = 1;
pub const NULL_TO_STRING: i64 = 2;

pub const CURSOR_FWDONLY: i64 = 0;
pub const CURSOR_SCROLL: i64 = 1;

/// `PDO::ERR_NONE` / `PDO_ERR_NONE`.
pub const ERR_NONE: &str = "00000";

/// `PDO_PARAM_TYPE(x)`: the parameter type without its flag bits.
pub fn param_type(x: i64) -> i64 {
    x & 0xFFFF
}

/// The integer class constants of `PDO`, in stub declaration order
/// (`ReflectionClass::getConstants()` order); `PDO::ERR_NONE` (a string)
/// follows `NULL_TO_STRING`, see [`pdo_class_constants`].
pub const PDO_INT_CONSTANTS: &[(&str, i64)] = &[
    ("PARAM_NULL", PARAM_NULL),
    ("PARAM_BOOL", PARAM_BOOL),
    ("PARAM_INT", PARAM_INT),
    ("PARAM_STR", PARAM_STR),
    ("PARAM_LOB", PARAM_LOB),
    ("PARAM_STMT", PARAM_STMT),
    ("PARAM_INPUT_OUTPUT", PARAM_INPUT_OUTPUT),
    ("PARAM_STR_NATL", PARAM_STR_NATL),
    ("PARAM_STR_CHAR", PARAM_STR_CHAR),
    ("PARAM_EVT_ALLOC", PARAM_EVT_ALLOC),
    ("PARAM_EVT_FREE", PARAM_EVT_FREE),
    ("PARAM_EVT_EXEC_PRE", PARAM_EVT_EXEC_PRE),
    ("PARAM_EVT_EXEC_POST", PARAM_EVT_EXEC_POST),
    ("PARAM_EVT_FETCH_PRE", PARAM_EVT_FETCH_PRE),
    ("PARAM_EVT_FETCH_POST", PARAM_EVT_FETCH_POST),
    ("PARAM_EVT_NORMALIZE", PARAM_EVT_NORMALIZE),
    ("FETCH_DEFAULT", FETCH_DEFAULT),
    ("FETCH_LAZY", FETCH_LAZY),
    ("FETCH_ASSOC", FETCH_ASSOC),
    ("FETCH_NUM", FETCH_NUM),
    ("FETCH_BOTH", FETCH_BOTH),
    ("FETCH_OBJ", FETCH_OBJ),
    ("FETCH_BOUND", FETCH_BOUND),
    ("FETCH_COLUMN", FETCH_COLUMN),
    ("FETCH_CLASS", FETCH_CLASS),
    ("FETCH_INTO", FETCH_INTO),
    ("FETCH_FUNC", FETCH_FUNC),
    ("FETCH_GROUP", FETCH_GROUP),
    ("FETCH_UNIQUE", FETCH_UNIQUE),
    ("FETCH_KEY_PAIR", FETCH_KEY_PAIR),
    ("FETCH_CLASSTYPE", FETCH_CLASSTYPE),
    ("FETCH_SERIALIZE", FETCH_SERIALIZE),
    ("FETCH_PROPS_LATE", FETCH_PROPS_LATE),
    ("FETCH_NAMED", FETCH_NAMED),
    ("ATTR_AUTOCOMMIT", ATTR_AUTOCOMMIT),
    ("ATTR_PREFETCH", ATTR_PREFETCH),
    ("ATTR_TIMEOUT", ATTR_TIMEOUT),
    ("ATTR_ERRMODE", ATTR_ERRMODE),
    ("ATTR_SERVER_VERSION", ATTR_SERVER_VERSION),
    ("ATTR_CLIENT_VERSION", ATTR_CLIENT_VERSION),
    ("ATTR_SERVER_INFO", ATTR_SERVER_INFO),
    ("ATTR_CONNECTION_STATUS", ATTR_CONNECTION_STATUS),
    ("ATTR_CASE", ATTR_CASE),
    ("ATTR_CURSOR_NAME", ATTR_CURSOR_NAME),
    ("ATTR_CURSOR", ATTR_CURSOR),
    ("ATTR_ORACLE_NULLS", ATTR_ORACLE_NULLS),
    ("ATTR_PERSISTENT", ATTR_PERSISTENT),
    ("ATTR_STATEMENT_CLASS", ATTR_STATEMENT_CLASS),
    ("ATTR_FETCH_TABLE_NAMES", ATTR_FETCH_TABLE_NAMES),
    ("ATTR_FETCH_CATALOG_NAMES", ATTR_FETCH_CATALOG_NAMES),
    ("ATTR_DRIVER_NAME", ATTR_DRIVER_NAME),
    ("ATTR_STRINGIFY_FETCHES", ATTR_STRINGIFY_FETCHES),
    ("ATTR_MAX_COLUMN_LEN", ATTR_MAX_COLUMN_LEN),
    ("ATTR_EMULATE_PREPARES", ATTR_EMULATE_PREPARES),
    ("ATTR_DEFAULT_FETCH_MODE", ATTR_DEFAULT_FETCH_MODE),
    ("ATTR_DEFAULT_STR_PARAM", ATTR_DEFAULT_STR_PARAM),
    ("ERRMODE_SILENT", ERRMODE_SILENT),
    ("ERRMODE_WARNING", ERRMODE_WARNING),
    ("ERRMODE_EXCEPTION", ERRMODE_EXCEPTION),
    ("CASE_NATURAL", CASE_NATURAL),
    ("CASE_LOWER", CASE_LOWER),
    ("CASE_UPPER", CASE_UPPER),
    ("NULL_NATURAL", NULL_NATURAL),
    ("NULL_EMPTY_STRING", NULL_EMPTY_STRING),
    ("NULL_TO_STRING", NULL_TO_STRING),
    ("FETCH_ORI_NEXT", FETCH_ORI_NEXT),
    ("FETCH_ORI_PRIOR", FETCH_ORI_PRIOR),
    ("FETCH_ORI_FIRST", FETCH_ORI_FIRST),
    ("FETCH_ORI_LAST", FETCH_ORI_LAST),
    ("FETCH_ORI_ABS", FETCH_ORI_ABS),
    ("FETCH_ORI_REL", FETCH_ORI_REL),
    ("CURSOR_FWDONLY", CURSOR_FWDONLY),
    ("CURSOR_SCROLL", CURSOR_SCROLL),
];

/// A class constant value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstValue {
    /// An `int` constant.
    Int(i64),
    /// A `string` constant.
    Str(&'static str),
}

/// Every class constant of `PDO` in declaration order, including the
/// string constant `ERR_NONE` (declared after `NULL_TO_STRING`).
pub fn pdo_class_constants() -> Vec<(&'static str, ConstValue)> {
    let mut out = Vec::with_capacity(PDO_INT_CONSTANTS.len() + 1);
    for &(name, v) in PDO_INT_CONSTANTS {
        out.push((name, ConstValue::Int(v)));
        if name == "NULL_TO_STRING" {
            out.push(("ERR_NONE", ConstValue::Str(ERR_NONE)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_type_masks_flags() {
        assert_eq!(param_type(PARAM_STR | PARAM_INPUT_OUTPUT), PARAM_STR);
        assert_eq!(param_type(PARAM_INT | PARAM_STR_NATL), PARAM_INT);
    }

    #[test]
    fn constant_table_order() {
        let c = pdo_class_constants();
        assert_eq!(c.len(), PDO_INT_CONSTANTS.len() + 1);
        let pos = c.iter().position(|(n, _)| *n == "ERR_NONE").unwrap();
        assert_eq!(c[pos - 1].0, "NULL_TO_STRING");
        assert_eq!(c[pos + 1].0, "FETCH_ORI_NEXT");
    }
}
