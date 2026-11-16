// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `XMLReader` class declaration (`php_xmlreader.stub.php`) as data,
//! so that the VM can register it generically.

use crate::reader::Property;

/// A parameter of a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamDef {
    /// Name without `$`.
    pub name: &'static str,
    /// Declared type as written in the stub (`"?string"`, `"int"`, …).
    pub type_: &'static str,
    /// Default value as PHP source (`None` = required).
    pub default: Option<&'static str>,
}

/// A method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodDef {
    /// Method name as declared.
    pub name: &'static str,
    /// `static`.
    pub is_static: bool,
    /// Parameters.
    pub params: &'static [ParamDef],
    /// Return type (`""` when undeclared).
    pub return_type: &'static str,
    /// Whether the return type is tentative (`@tentative-return-type`).
    pub tentative: bool,
}

impl MethodDef {
    /// Number of required parameters.
    pub fn required(&self) -> usize {
        self.params.iter().filter(|p| p.default.is_none()).count()
    }
}

/// A class constant (all `XMLReader` constants are `int`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstDef {
    /// Name.
    pub name: &'static str,
    /// Value.
    pub value: i64,
}

/// A declared property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropDef {
    /// Name.
    pub name: &'static str,
    /// Declared type.
    pub type_: &'static str,
    /// `@virtual`: backed by [`crate::XmlReader::read_property`], never
    /// stored in the property table.
    pub is_virtual: bool,
}

/// A class declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassDef {
    /// Class name.
    pub name: &'static str,
    /// Parent class.
    pub parent: Option<&'static str>,
    /// Implemented interfaces.
    pub interfaces: &'static [&'static str],
    /// `@not-serializable`.
    pub not_serializable: bool,
    /// `clone_obj = NULL`.
    pub not_cloneable: bool,
    /// Constants.
    pub constants: &'static [ConstDef],
    /// Properties.
    pub properties: Vec<PropDef>,
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
) -> MethodDef {
    MethodDef {
        name,
        is_static: false,
        params,
        return_type,
        tentative: true,
    }
}

const fn s(
    name: &'static str,
    params: &'static [ParamDef],
    return_type: &'static str,
) -> MethodDef {
    MethodDef {
        name,
        is_static: true,
        params,
        return_type,
        tentative: false,
    }
}

const CONSTANTS: &[ConstDef] = &[
    ConstDef {
        name: "NONE",
        value: 0,
    },
    ConstDef {
        name: "ELEMENT",
        value: 1,
    },
    ConstDef {
        name: "ATTRIBUTE",
        value: 2,
    },
    ConstDef {
        name: "TEXT",
        value: 3,
    },
    ConstDef {
        name: "CDATA",
        value: 4,
    },
    ConstDef {
        name: "ENTITY_REF",
        value: 5,
    },
    ConstDef {
        name: "ENTITY",
        value: 6,
    },
    ConstDef {
        name: "PI",
        value: 7,
    },
    ConstDef {
        name: "COMMENT",
        value: 8,
    },
    ConstDef {
        name: "DOC",
        value: 9,
    },
    ConstDef {
        name: "DOC_TYPE",
        value: 10,
    },
    ConstDef {
        name: "DOC_FRAGMENT",
        value: 11,
    },
    ConstDef {
        name: "NOTATION",
        value: 12,
    },
    ConstDef {
        name: "WHITESPACE",
        value: 13,
    },
    ConstDef {
        name: "SIGNIFICANT_WHITESPACE",
        value: 14,
    },
    ConstDef {
        name: "END_ELEMENT",
        value: 15,
    },
    ConstDef {
        name: "END_ENTITY",
        value: 16,
    },
    ConstDef {
        name: "XML_DECLARATION",
        value: 17,
    },
    ConstDef {
        name: "LOADDTD",
        value: 1,
    },
    ConstDef {
        name: "DEFAULTATTRS",
        value: 2,
    },
    ConstDef {
        name: "VALIDATE",
        value: 3,
    },
    ConstDef {
        name: "SUBST_ENTITIES",
        value: 4,
    },
];

const URI_ARGS: &[ParamDef] = &[
    p("uri", "string"),
    o("encoding", "?string", "null"),
    o("flags", "int", "0"),
];
const SOURCE_ARGS: &[ParamDef] = &[
    p("source", "string"),
    o("encoding", "?string", "null"),
    o("flags", "int", "0"),
];

const METHODS: &[MethodDef] = &[
    m("close", &[], "true"),
    m("getAttribute", &[p("name", "string")], "?string"),
    m("getAttributeNo", &[p("index", "int")], "?string"),
    m(
        "getAttributeNs",
        &[p("name", "string"), p("namespace", "string")],
        "?string",
    ),
    m("getParserProperty", &[p("property", "int")], "bool"),
    m("isValid", &[], "bool"),
    m("lookupNamespace", &[p("prefix", "string")], "?string"),
    m("moveToAttribute", &[p("name", "string")], "bool"),
    m("moveToAttributeNo", &[p("index", "int")], "bool"),
    m(
        "moveToAttributeNs",
        &[p("name", "string"), p("namespace", "string")],
        "bool",
    ),
    m("moveToElement", &[], "bool"),
    m("moveToFirstAttribute", &[], "bool"),
    m("moveToNextAttribute", &[], "bool"),
    m("read", &[], "bool"),
    m("next", &[o("name", "?string", "null")], "bool"),
    s("open", URI_ARGS, ""),
    s("fromUri", URI_ARGS, "static"),
    s(
        "fromStream",
        &[
            p("stream", "mixed"),
            o("encoding", "?string", "null"),
            o("flags", "int", "0"),
            o("documentUri", "?string", "null"),
        ],
        "static",
    ),
    m("readInnerXml", &[], "string"),
    m("readOuterXml", &[], "string"),
    m("readString", &[], "string"),
    m("setSchema", &[p("filename", "?string")], "bool"),
    m(
        "setParserProperty",
        &[p("property", "int"), p("value", "bool")],
        "bool",
    ),
    m("setRelaxNGSchema", &[p("filename", "?string")], "bool"),
    m("setRelaxNGSchemaSource", &[p("source", "?string")], "bool"),
    s("XML", SOURCE_ARGS, ""),
    s("fromString", SOURCE_ARGS, "static"),
    m(
        "expand",
        &[o("baseNode", "?DOMNode", "null")],
        "DOMNode|false",
    ),
];

/// The `XMLReader` class. `open()` and `XML()` are declared static but,
/// like in PHP, also callable on an instance (`xmlreader_get_method`
/// swaps in non-static copies): the VM must dispatch `$obj->open()` to
/// [`crate::XmlReader::open`] and `XMLReader::open()` to
/// [`crate::XmlReader::open_static`].
pub fn class_table() -> ClassDef {
    ClassDef {
        name: "XMLReader",
        parent: None,
        interfaces: &[],
        not_serializable: true,
        not_cloneable: true,
        constants: CONSTANTS,
        properties: Property::ALL
            .iter()
            .map(|p| PropDef {
                name: p.name(),
                type_: p.type_name(),
                is_virtual: true,
            })
            .collect(),
        methods: METHODS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_shape() {
        let t = class_table();
        assert_eq!(t.methods.len(), 28);
        assert_eq!(t.properties.len(), 14);
        let expand = t.methods.iter().find(|m| m.name == "expand").unwrap();
        // bug74457.phpt
        assert_eq!((expand.params.len(), expand.required()), (1, 0));
        assert!(t
            .constants
            .iter()
            .any(|c| c.name == "XML_DECLARATION" && c.value == 17));
    }
}
