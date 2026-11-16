// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `XSLTProcessor` class declaration and the extension's constants
//! (`php_xsl.stub.php`), as data the VM registers generically, plus the
//! `phpinfo()` rows (`PHP_MINFO_FUNCTION(xsl)`).

use crate::processor::{
    SECPREF_CREATE_DIRECTORY, SECPREF_DEFAULT, SECPREF_NONE, SECPREF_READ_FILE,
    SECPREF_READ_NETWORK, SECPREF_WRITE_FILE, SECPREF_WRITE_NETWORK,
};
use crate::raw::version;

/// A parameter of a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamDef {
    /// Name without `$`.
    pub name: &'static str,
    /// Declared type as written in the stub.
    pub type_: &'static str,
    /// Default value as PHP source (`None` = required).
    pub default: Option<&'static str>,
}

/// A method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodDef {
    /// Method name as declared.
    pub name: &'static str,
    /// Parameters.
    pub params: &'static [ParamDef],
    /// Return type.
    pub return_type: &'static str,
    /// Whether the return type is tentative.
    pub tentative: bool,
}

impl MethodDef {
    /// Number of required parameters.
    pub fn required(&self) -> usize {
        self.params.iter().filter(|p| p.default.is_none()).count()
    }
}

/// A declared property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropDef {
    /// Name.
    pub name: &'static str,
    /// Declared type.
    pub type_: &'static str,
    /// Default value as PHP source; `None` for the properties initialised
    /// by the object constructor from libxslt's limits
    /// ([`crate::Properties::default`]).
    pub default: Option<&'static str>,
    /// Writes are validated (`>= 0`), references and unset are refused.
    pub validated: bool,
}

/// The class declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassDef {
    /// Class name.
    pub name: &'static str,
    /// `clone_obj = NULL`.
    pub not_cloneable: bool,
    /// Properties, in declaration order.
    pub properties: &'static [PropDef],
    /// Methods.
    pub methods: &'static [MethodDef],
}

const fn p(name: &'static str, type_: &'static str) -> ParamDef {
    ParamDef {
        name,
        type_,
        default: None,
    }
}

const fn o(name: &'static str, type_: &'static str, default: &'static str) -> ParamDef {
    ParamDef {
        name,
        type_,
        default: Some(default),
    }
}

const fn m(
    name: &'static str,
    params: &'static [ParamDef],
    return_type: &'static str,
    tentative: bool,
) -> MethodDef {
    MethodDef {
        name,
        params,
        return_type,
        tentative,
    }
}

const PROPERTIES: &[PropDef] = &[
    PropDef {
        name: "doXInclude",
        type_: "bool",
        default: Some("false"),
        validated: false,
    },
    PropDef {
        name: "cloneDocument",
        type_: "bool",
        default: Some("false"),
        validated: false,
    },
    PropDef {
        name: "maxTemplateDepth",
        type_: "int",
        default: None,
        validated: true,
    },
    PropDef {
        name: "maxTemplateVars",
        type_: "int",
        default: None,
        validated: true,
    },
];

const METHODS: &[MethodDef] = &[
    m(
        "importStylesheet",
        &[p("stylesheet", "object")],
        "bool",
        true,
    ),
    m(
        "transformToDoc",
        &[p("document", "object"), o("returnClass", "?string", "null")],
        "object|false",
        true,
    ),
    m(
        "transformToUri",
        &[p("document", "object"), p("uri", "string")],
        "int",
        true,
    ),
    m(
        "transformToXml",
        &[p("document", "object")],
        "string|null|false",
        true,
    ),
    m(
        "setParameter",
        &[
            p("namespace", "string"),
            p("name", "array|string"),
            o("value", "?string", "null"),
        ],
        "bool",
        true,
    ),
    m(
        "getParameter",
        &[p("namespace", "string"), p("name", "string")],
        "string|false",
        true,
    ),
    m(
        "removeParameter",
        &[p("namespace", "string"), p("name", "string")],
        "bool",
        true,
    ),
    m("hasExsltSupport", &[], "bool", true),
    m(
        "registerPHPFunctions",
        &[o("functions", "array|string|null", "null")],
        "void",
        true,
    ),
    m(
        "registerPHPFunctionNS",
        &[
            p("namespaceURI", "string"),
            p("name", "string"),
            p("callable", "callable"),
        ],
        "void",
        false,
    ),
    m("setProfiling", &[p("filename", "?string")], "true", true),
    m("setSecurityPrefs", &[p("preferences", "int")], "int", true),
    m("getSecurityPrefs", &[], "int", true),
];

/// The `XSLTProcessor` class.
pub fn class_def() -> ClassDef {
    ClassDef {
        name: "XSLTProcessor",
        not_cloneable: true,
        properties: PROPERTIES,
        methods: METHODS,
    }
}

/// A constant value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstValue {
    /// `int`.
    Int(i64),
    /// `string`.
    Str(String),
}

/// The global constants registered by the extension, in stub order.
pub fn constants() -> Vec<(&'static str, ConstValue)> {
    crate::raw::ensure_init();
    vec![
        ("XSL_CLONE_AUTO", ConstValue::Int(0)),
        ("XSL_CLONE_NEVER", ConstValue::Int(-1)),
        ("XSL_CLONE_ALWAYS", ConstValue::Int(1)),
        ("XSL_SECPREF_NONE", ConstValue::Int(SECPREF_NONE)),
        ("XSL_SECPREF_READ_FILE", ConstValue::Int(SECPREF_READ_FILE)),
        (
            "XSL_SECPREF_WRITE_FILE",
            ConstValue::Int(SECPREF_WRITE_FILE),
        ),
        (
            "XSL_SECPREF_CREATE_DIRECTORY",
            ConstValue::Int(SECPREF_CREATE_DIRECTORY),
        ),
        (
            "XSL_SECPREF_READ_NETWORK",
            ConstValue::Int(SECPREF_READ_NETWORK),
        ),
        (
            "XSL_SECPREF_WRITE_NETWORK",
            ConstValue::Int(SECPREF_WRITE_NETWORK),
        ),
        ("XSL_SECPREF_DEFAULT", ConstValue::Int(SECPREF_DEFAULT)),
        ("LIBXSLT_VERSION", ConstValue::Int(version::libxslt())),
        (
            "LIBXSLT_DOTTED_VERSION",
            ConstValue::Str(version::libxslt_dotted()),
        ),
        ("LIBEXSLT_VERSION", ConstValue::Int(version::libexslt())),
        (
            "LIBEXSLT_DOTTED_VERSION",
            ConstValue::Str(version::libexslt_dotted()),
        ),
    ]
}

fn dotted(v: i64) -> String {
    let major = v / 10000;
    let minor = (v - major * 10000) / 100;
    let sub = v - major * 10000 - minor * 100;
    format!("{major}.{minor}.{sub}")
}

/// The `phpinfo()` table rows of the `xsl` section.
pub fn info_rows() -> Vec<(&'static str, String)> {
    crate::raw::ensure_init();
    vec![
        ("XSL", "enabled".to_string()),
        ("libxslt Version", dotted(version::runtime_libxslt())),
        (
            "libxslt compiled against libxml Version",
            dotted(version::runtime_libxml()),
        ),
        ("EXSLT", "enabled".to_string()),
        ("libexslt Version", version::libexslt_dotted()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_shape() {
        let c = class_def();
        assert_eq!(c.methods.len(), 13);
        let names: Vec<_> = c.properties.iter().map(|p| p.name).collect();
        assert_eq!(names, crate::PROPERTY_NAMES);
        let set = c.methods.iter().find(|m| m.name == "setParameter").unwrap();
        assert_eq!(set.required(), 2);
    }

    #[test]
    fn constants_and_info() {
        let c = constants();
        assert!(c.contains(&("XSL_SECPREF_DEFAULT", ConstValue::Int(44))));
        let v = c.iter().find(|(n, _)| *n == "LIBXSLT_VERSION").unwrap();
        assert!(matches!(v.1, ConstValue::Int(n) if n > 10100));
        let rows = info_rows();
        assert_eq!(rows[0], ("XSL", "enabled".to_string()));
        assert!(rows[1].1.starts_with("1."));
        assert_eq!(dotted(10139), "1.1.39");
        assert_eq!(dotted(20914), "2.9.14");
    }
}
