//! The class declarations of `simplexml.stub.php` as data, for generic
//! registration by the VM.

/// A parameter of a method or function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamDef {
    /// Name without `$`.
    pub name: &'static str,
    /// Declared type as written in the stub.
    pub type_: &'static str,
    /// Default value as PHP source (`None` = required).
    pub default: Option<&'static str>,
}

/// A method (or function).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodDef {
    /// Name as declared.
    pub name: &'static str,
    /// Parameters.
    pub params: &'static [ParamDef],
    /// Return type (`""` when undeclared).
    pub return_type: &'static str,
    /// `@tentative-return-type`.
    pub tentative: bool,
    /// `@alias`: the method this one is an alias of.
    pub alias_of: Option<&'static str>,
}

impl MethodDef {
    /// Number of required parameters.
    pub fn required(&self) -> usize {
        self.params.iter().filter(|p| p.default.is_none()).count()
    }
}

/// A class declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassDef {
    /// Class name.
    pub name: &'static str,
    /// Parent class.
    pub parent: Option<&'static str>,
    /// Directly implemented interfaces (`RecursiveIterator` brings
    /// `Iterator` and `Traversable`).
    pub interfaces: &'static [&'static str],
    /// `@not-serializable` (`ZEND_ACC_NOT_SERIALIZABLE`, inherited).
    pub not_serializable: bool,
    /// Methods declared by this class.
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

const fn m(name: &'static str, params: &'static [ParamDef], ret: &'static str) -> MethodDef {
    MethodDef {
        name,
        params,
        return_type: ret,
        tentative: true,
        alias_of: None,
    }
}

const fn f(name: &'static str, params: &'static [ParamDef], ret: &'static str) -> MethodDef {
    MethodDef {
        name,
        params,
        return_type: ret,
        tentative: false,
        alias_of: None,
    }
}

const SXE_METHODS: &[MethodDef] = &[
    m("xpath", &[p("expression", "string")], "array|null|false"),
    m(
        "registerXPathNamespace",
        &[p("prefix", "string"), p("namespace", "string")],
        "bool",
    ),
    m("asXML", &[o("filename", "?string", "null")], "string|bool"),
    MethodDef {
        name: "saveXML",
        params: &[o("filename", "?string", "null")],
        return_type: "string|bool",
        tentative: true,
        alias_of: Some("asXML"),
    },
    m("getNamespaces", &[o("recursive", "bool", "false")], "array"),
    m(
        "getDocNamespaces",
        &[
            o("recursive", "bool", "false"),
            o("fromRoot", "bool", "true"),
        ],
        "array|false",
    ),
    m(
        "children",
        &[
            o("namespaceOrPrefix", "?string", "null"),
            o("isPrefix", "bool", "false"),
        ],
        "?SimpleXMLElement",
    ),
    m(
        "attributes",
        &[
            o("namespaceOrPrefix", "?string", "null"),
            o("isPrefix", "bool", "false"),
        ],
        "?SimpleXMLElement",
    ),
    f(
        "__construct",
        &[
            p("data", "string"),
            o("options", "int", "0"),
            o("dataIsURL", "bool", "false"),
            o("namespaceOrPrefix", "string", "\"\""),
            o("isPrefix", "bool", "false"),
        ],
        "",
    ),
    m(
        "addChild",
        &[
            p("qualifiedName", "string"),
            o("value", "?string", "null"),
            o("namespace", "?string", "null"),
        ],
        "?SimpleXMLElement",
    ),
    m(
        "addAttribute",
        &[
            p("qualifiedName", "string"),
            p("value", "string"),
            o("namespace", "?string", "null"),
        ],
        "void",
    ),
    m("getName", &[], "string"),
    f("__toString", &[], "string"),
    f("__debugInfo", &[], "array"),
    m("count", &[], "int"),
    m("rewind", &[], "void"),
    m("valid", &[], "bool"),
    m("current", &[], "SimpleXMLElement"),
    m("key", &[], "string"),
    m("next", &[], "void"),
    m("hasChildren", &[], "bool"),
    m("getChildren", &[], "?SimpleXMLElement"),
];

/// The functions of the extension.
pub const FUNCTIONS: &[MethodDef] = &[
    f(
        "simplexml_load_file",
        &[
            p("filename", "string"),
            o("class_name", "?string", "SimpleXMLElement::class"),
            o("options", "int", "0"),
            o("namespace_or_prefix", "string", "\"\""),
            o("is_prefix", "bool", "false"),
        ],
        "SimpleXMLElement|false",
    ),
    f(
        "simplexml_load_string",
        &[
            p("data", "string"),
            o("class_name", "?string", "SimpleXMLElement::class"),
            o("options", "int", "0"),
            o("namespace_or_prefix", "string", "\"\""),
            o("is_prefix", "bool", "false"),
        ],
        "SimpleXMLElement|false",
    ),
    f(
        "simplexml_import_dom",
        &[
            p("node", "object"),
            o("class_name", "?string", "SimpleXMLElement::class"),
        ],
        "?SimpleXMLElement",
    ),
];

/// `SimpleXMLElement` and `SimpleXMLIterator`.
///
/// Object handlers to wire (see [`crate::Sxe`]): `read_property`,
/// `write_property`, `read_dimension`, `write_dimension`,
/// `get_property_ptr_ptr` ([`crate::Sxe::property_ptr`]), `has_property`,
/// `unset_property`, `has_dimension`, `unset_dimension`, `get_properties`
/// ([`crate::Sxe::properties`] with `is_debug = false`), `__debugInfo`
/// (`is_debug = true`), `compare`, `cast_object` (after checking for a
/// user `__toString`), `count_elements` (after checking for a user
/// `count`), `clone_obj`, `get_iterator`; `get_closure` is disabled.
pub fn class_table() -> Vec<ClassDef> {
    vec![
        ClassDef {
            name: "SimpleXMLElement",
            parent: None,
            interfaces: &["Stringable", "Countable", "RecursiveIterator"],
            not_serializable: true,
            methods: SXE_METHODS,
        },
        ClassDef {
            name: "SimpleXMLIterator",
            parent: Some("SimpleXMLElement"),
            interfaces: &[],
            not_serializable: true,
            methods: &[],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_shape() {
        let t = class_table();
        assert_eq!(t[0].methods.len(), 22);
        let add = t[0]
            .methods
            .iter()
            .find(|m| m.name == "addAttribute")
            .unwrap();
        assert_eq!((add.params.len(), add.required()), (3, 2));
        assert_eq!(FUNCTIONS.len(), 3);
    }
}
