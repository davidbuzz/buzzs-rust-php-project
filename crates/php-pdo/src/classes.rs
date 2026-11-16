// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The class and function table of `ext/pdo` (from the `*.stub.php`
//! files) for the VM: `PDOException`, `PDO`, `PDOStatement`, `PDORow`, the
//! driver-specific subclasses (`Pdo\Sqlite`) and `pdo_drivers()`.

use crate::consts::{pdo_class_constants, ConstValue};

/// A parameter of a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamDecl {
    /// The name (without `$`).
    pub name: &'static str,
    /// The declared type.
    pub ty: &'static str,
    /// The default value as PHP source (`None`: required).
    pub default: Option<&'static str>,
    /// Passed by reference.
    pub by_ref: bool,
    /// Variadic (`...$name`).
    pub variadic: bool,
}

/// A method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodDecl {
    /// The name.
    pub name: &'static str,
    /// `static`.
    pub is_static: bool,
    /// The parameters.
    pub params: &'static [ParamDecl],
    /// The return type.
    pub ret: &'static str,
}

/// A declared property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropDecl {
    /// The name.
    pub name: &'static str,
    /// `"public"`, `"protected"` or `"private"`.
    pub visibility: &'static str,
    /// The type (`None`: untyped).
    pub ty: Option<&'static str>,
    /// The default as PHP source (`None`: uninitialized typed property).
    pub default: Option<&'static str>,
}

/// A class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassDecl {
    /// The fully qualified name.
    pub name: &'static str,
    /// The parent class.
    pub parent: Option<&'static str>,
    /// Implemented interfaces.
    pub interfaces: &'static [&'static str],
    /// `final`.
    pub is_final: bool,
    /// `ZEND_ACC_NOT_SERIALIZABLE`.
    pub not_serializable: bool,
    /// Class constants in declaration order.
    pub constants: Vec<(&'static str, ConstValue)>,
    /// Declared properties.
    pub properties: &'static [PropDecl],
    /// Methods.
    pub methods: &'static [MethodDecl],
}

const fn p(name: &'static str, ty: &'static str) -> ParamDecl {
    ParamDecl {
        name,
        ty,
        default: None,
        by_ref: false,
        variadic: false,
    }
}

const fn o(name: &'static str, ty: &'static str, default: &'static str) -> ParamDecl {
    ParamDecl {
        name,
        ty,
        default: Some(default),
        by_ref: false,
        variadic: false,
    }
}

const fn r(name: &'static str, ty: &'static str) -> ParamDecl {
    ParamDecl {
        name,
        ty,
        default: None,
        by_ref: true,
        variadic: false,
    }
}

const fn v(name: &'static str, ty: &'static str) -> ParamDecl {
    ParamDecl {
        name,
        ty,
        default: None,
        by_ref: false,
        variadic: true,
    }
}

const fn m(name: &'static str, params: &'static [ParamDecl], ret: &'static str) -> MethodDecl {
    MethodDecl {
        name,
        is_static: false,
        params,
        ret,
    }
}

const CONNECT_PARAMS: &[ParamDecl] = &[
    p("dsn", "string"),
    o("username", "?string", "null"),
    o("password", "?string", "null"),
    o("options", "?array", "null"),
];

/// The methods of `PDO`.
pub const PDO_METHODS: &[MethodDecl] = &[
    m("__construct", CONNECT_PARAMS, ""),
    MethodDecl {
        name: "connect",
        is_static: true,
        params: CONNECT_PARAMS,
        ret: "static",
    },
    m("beginTransaction", &[], "bool"),
    m("commit", &[], "bool"),
    m("errorCode", &[], "?string"),
    m("errorInfo", &[], "array"),
    m("exec", &[p("statement", "string")], "int|false"),
    m("getAttribute", &[p("attribute", "int")], "mixed"),
    MethodDecl {
        name: "getAvailableDrivers",
        is_static: true,
        params: &[],
        ret: "array",
    },
    m("inTransaction", &[], "bool"),
    m(
        "lastInsertId",
        &[o("name", "?string", "null")],
        "string|false",
    ),
    m(
        "prepare",
        &[p("query", "string"), o("options", "array", "[]")],
        "PDOStatement|false",
    ),
    m(
        "query",
        &[
            p("query", "string"),
            o("fetchMode", "?int", "null"),
            v("fetchModeArgs", "mixed"),
        ],
        "PDOStatement|false",
    ),
    m(
        "quote",
        &[p("string", "string"), o("type", "int", "PDO::PARAM_STR")],
        "string|false",
    ),
    m("rollBack", &[], "bool"),
    m(
        "setAttribute",
        &[p("attribute", "int"), p("value", "mixed")],
        "bool",
    ),
];

/// The methods of `PDOStatement`.
pub const PDO_STATEMENT_METHODS: &[MethodDecl] = &[
    m(
        "bindColumn",
        &[
            p("column", "string|int"),
            r("var", "mixed"),
            o("type", "int", "PDO::PARAM_STR"),
            o("maxLength", "int", "0"),
            o("driverOptions", "mixed", "null"),
        ],
        "bool",
    ),
    m(
        "bindParam",
        &[
            p("param", "string|int"),
            r("var", "mixed"),
            o("type", "int", "PDO::PARAM_STR"),
            o("maxLength", "int", "0"),
            o("driverOptions", "mixed", "null"),
        ],
        "bool",
    ),
    m(
        "bindValue",
        &[
            p("param", "string|int"),
            p("value", "mixed"),
            o("type", "int", "PDO::PARAM_STR"),
        ],
        "bool",
    ),
    m("closeCursor", &[], "bool"),
    m("columnCount", &[], "int"),
    m("debugDumpParams", &[], "?bool"),
    m("errorCode", &[], "?string"),
    m("errorInfo", &[], "array"),
    m("execute", &[o("params", "?array", "null")], "bool"),
    m(
        "fetch",
        &[
            o("mode", "int", "PDO::FETCH_DEFAULT"),
            o("cursorOrientation", "int", "PDO::FETCH_ORI_NEXT"),
            o("cursorOffset", "int", "0"),
        ],
        "mixed",
    ),
    m(
        "fetchAll",
        &[o("mode", "int", "PDO::FETCH_DEFAULT"), v("args", "mixed")],
        "array",
    ),
    m("fetchColumn", &[o("column", "int", "0")], "mixed"),
    m(
        "fetchObject",
        &[
            o("class", "?string", "\"stdClass\""),
            o("constructorArgs", "array", "[]"),
        ],
        "object|false",
    ),
    m("getAttribute", &[p("name", "int")], "mixed"),
    m("getColumnMeta", &[p("column", "int")], "array|false"),
    m("nextRowset", &[], "bool"),
    m("rowCount", &[], "int"),
    m(
        "setAttribute",
        &[p("attribute", "int"), p("value", "mixed")],
        "bool",
    ),
    m(
        "setFetchMode",
        &[p("mode", "int"), v("args", "mixed")],
        "true",
    ),
    m("getIterator", &[], "Iterator"),
];

/// The methods of `Pdo\Sqlite`.
pub const PDO_SQLITE_METHODS: &[MethodDecl] = &[
    m(
        "createAggregate",
        &[
            p("name", "string"),
            p("step", "callable"),
            p("finalize", "callable"),
            o("numArgs", "int", "-1"),
        ],
        "bool",
    ),
    m(
        "createCollation",
        &[p("name", "string"), p("callback", "callable")],
        "bool",
    ),
    m(
        "createFunction",
        &[
            p("function_name", "string"),
            p("callback", "callable"),
            o("num_args", "int", "-1"),
            o("flags", "int", "0"),
        ],
        "bool",
    ),
    m("loadExtension", &[p("name", "string")], "void"),
    m(
        "openBlob",
        &[
            p("table", "string"),
            p("column", "string"),
            p("rowid", "int"),
            o("dbname", "?string", "\"main\""),
            o("flags", "int", "\\Pdo\\Sqlite::OPEN_READONLY"),
        ],
        "",
    ),
    m("setAuthorizer", &[p("callback", "?callable")], "void"),
];

const QUERY_STRING: &[PropDecl] = &[PropDecl {
    name: "queryString",
    visibility: "public",
    ty: Some("string"),
    default: None,
}];

const EXCEPTION_PROPS: &[PropDecl] = &[
    PropDecl {
        name: "code",
        visibility: "protected",
        ty: None,
        default: Some("0"),
    },
    PropDecl {
        name: "errorInfo",
        visibility: "public",
        ty: Some("?array"),
        default: Some("null"),
    },
];

/// Every class `ext/pdo` and the bundled drivers register, parents first.
pub fn classes() -> Vec<ClassDecl> {
    let sqlite_consts = php_sqlite::constants::PDO_SQLITE_CLASS_CONSTANTS
        .iter()
        .map(|&(n, v)| (n, ConstValue::Int(v)))
        .collect();
    vec![
        ClassDecl {
            name: "PDOException",
            parent: Some("RuntimeException"),
            interfaces: &[],
            is_final: false,
            not_serializable: false,
            constants: Vec::new(),
            properties: EXCEPTION_PROPS,
            methods: &[],
        },
        ClassDecl {
            name: "PDO",
            parent: None,
            interfaces: &[],
            is_final: false,
            not_serializable: true,
            constants: pdo_class_constants(),
            properties: &[],
            methods: PDO_METHODS,
        },
        ClassDecl {
            name: "PDOStatement",
            parent: None,
            interfaces: &["IteratorAggregate"],
            is_final: false,
            not_serializable: true,
            constants: Vec::new(),
            properties: QUERY_STRING,
            methods: PDO_STATEMENT_METHODS,
        },
        ClassDecl {
            name: "PDORow",
            parent: None,
            interfaces: &[],
            is_final: true,
            not_serializable: true,
            constants: Vec::new(),
            properties: QUERY_STRING,
            methods: &[],
        },
        ClassDecl {
            name: "Pdo\\Sqlite",
            parent: Some("PDO"),
            interfaces: &[],
            is_final: false,
            not_serializable: false,
            constants: sqlite_consts,
            properties: &[],
            methods: PDO_SQLITE_METHODS,
        },
    ]
}

/// The global functions: `pdo_drivers(): array`.
pub const FUNCTIONS: &[MethodDecl] = &[m("pdo_drivers", &[], "array")];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_table() {
        let c = classes();
        let names: Vec<_> = c.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "PDOException",
                "PDO",
                "PDOStatement",
                "PDORow",
                "Pdo\\Sqlite"
            ]
        );
        let pdo = &c[1];
        assert!(pdo
            .methods
            .iter()
            .any(|m| m.name == "connect" && m.is_static));
        assert!(pdo
            .constants
            .iter()
            .any(|(n, v)| *n == "FETCH_GROUP" && *v == ConstValue::Int(32)));
        assert!(c[4].constants.iter().any(|(n, _)| *n == "ATTR_OPEN_FLAGS"));
    }
}
