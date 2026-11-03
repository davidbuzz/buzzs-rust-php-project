//! The class table of ext/soap (`soap.stub.php`): classes, their declared
//! properties (with types and defaults), methods and the functions, for
//! the engine to register.

/// Property/method visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// `public`.
    Public,
    /// `protected`.
    Protected,
    /// `private`.
    Private,
}

/// A declared property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropDef {
    /// Name without `$`.
    pub name: &'static str,
    /// Visibility.
    pub visibility: Visibility,
    /// Declared type as written (`?string`, `mixed`, …); `None` for untyped.
    pub ty: Option<&'static str>,
    /// Default value as PHP source; `None` when the typed property has no
    /// default (uninitialized).
    pub default: Option<&'static str>,
}

/// A parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamDef {
    /// Name without `$`.
    pub name: &'static str,
    /// Type as written.
    pub ty: Option<&'static str>,
    /// Default as PHP source.
    pub default: Option<&'static str>,
    /// By reference.
    pub by_ref: bool,
    /// Variadic.
    pub variadic: bool,
}

/// A method or function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodDef {
    /// Name.
    pub name: &'static str,
    /// Parameters.
    pub params: &'static [ParamDef],
    /// Return type.
    pub return_type: Option<&'static str>,
    /// `@tentative-return-type`.
    pub tentative: bool,
}

/// A class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassDef {
    /// Fully qualified name.
    pub name: &'static str,
    /// Parent class.
    pub parent: Option<&'static str>,
    /// `final`.
    pub is_final: bool,
    /// `@strict-properties` / `@not-serializable` (`Soap\Url`, `Soap\Sdl`).
    pub opaque: bool,
    /// Declared properties in order.
    pub properties: &'static [PropDef],
    /// Methods.
    pub methods: &'static [MethodDef],
}

const fn p(name: &'static str, ty: Option<&'static str>, default: Option<&'static str>) -> ParamDef {
    ParamDef {
        name,
        ty,
        default,
        by_ref: false,
        variadic: false,
    }
}

const fn pub_prop(name: &'static str, ty: &'static str, default: Option<&'static str>) -> PropDef {
    PropDef {
        name,
        visibility: Visibility::Public,
        ty: Some(ty),
        default,
    }
}

const fn priv_prop(name: &'static str, ty: Option<&'static str>, default: Option<&'static str>) -> PropDef {
    PropDef {
        name,
        visibility: Visibility::Private,
        ty,
        default,
    }
}

const fn m(name: &'static str, params: &'static [ParamDef], ret: Option<&'static str>, tentative: bool) -> MethodDef {
    MethodDef {
        name,
        params,
        return_type: ret,
        tentative,
    }
}

/// `SoapClient` property names in slot order (`Z_CLIENT_*_P`).
pub const CLIENT_PROPERTIES: &[PropDef] = &[
    priv_prop("uri", Some("?string"), Some("null")),
    priv_prop("style", Some("?int"), Some("null")),
    priv_prop("use", Some("?int"), Some("null")),
    priv_prop("location", Some("?string"), Some("null")),
    priv_prop("trace", Some("bool"), Some("false")),
    priv_prop("compression", Some("?int"), Some("null")),
    priv_prop("sdl", Some("?Soap\\Sdl"), Some("null")),
    priv_prop("httpsocket", None, Some("null")),
    priv_prop("httpurl", Some("?Soap\\Url"), Some("null")),
    priv_prop("_login", Some("?string"), Some("null")),
    priv_prop("_password", Some("?string"), Some("null")),
    priv_prop("_use_digest", Some("bool"), Some("false")),
    priv_prop("_digest", Some("?string"), Some("null")),
    priv_prop("_proxy_host", Some("?string"), Some("null")),
    priv_prop("_proxy_port", Some("?int"), Some("null")),
    priv_prop("_proxy_login", Some("?string"), Some("null")),
    priv_prop("_proxy_password", Some("?string"), Some("null")),
    priv_prop("_exceptions", Some("bool"), Some("true")),
    priv_prop("_encoding", Some("?string"), Some("null")),
    priv_prop("_classmap", Some("?array"), Some("null")),
    priv_prop("_features", Some("?int"), Some("null")),
    priv_prop("_connection_timeout", Some("int"), Some("0")),
    priv_prop("_stream_context", None, Some("null")),
    priv_prop("_user_agent", Some("?string"), Some("null")),
    priv_prop("_keep_alive", Some("bool"), Some("true")),
    priv_prop("_ssl_method", Some("?int"), Some("null")),
    priv_prop("_soap_version", Some("int"), None),
    priv_prop("_use_proxy", Some("?int"), Some("null")),
    priv_prop("_cookies", Some("array"), Some("[]")),
    priv_prop("__default_headers", Some("?array"), Some("null")),
    priv_prop("__soap_fault", Some("?SoapFault"), Some("null")),
    priv_prop("__last_request", Some("?string"), Some("null")),
    priv_prop("__last_response", Some("?string"), Some("null")),
    priv_prop("__last_request_headers", Some("?string"), Some("null")),
    priv_prop("__last_response_headers", Some("?string"), Some("null")),
    priv_prop("_keep_headers", Some("int"), Some("0")),
    priv_prop("_follow_cross_origin_redirects", Some("bool"), Some("false")),
];

/// `SoapFault` own properties (after those of `Exception`).
pub const FAULT_PROPERTIES: &[PropDef] = &[
    pub_prop("faultstring", "string", None),
    pub_prop("faultcode", "?string", Some("null")),
    pub_prop("faultcodens", "?string", Some("null")),
    pub_prop("faultactor", "?string", Some("null")),
    pub_prop("detail", "mixed", Some("null")),
    pub_prop("_name", "?string", Some("null")),
    pub_prop("headerfault", "mixed", Some("null")),
    pub_prop("lang", "string", Some("\"\"")),
];

/// `SoapVar` properties.
pub const VAR_PROPERTIES: &[PropDef] = &[
    pub_prop("enc_type", "int", None),
    pub_prop("enc_value", "mixed", Some("null")),
    pub_prop("enc_stype", "?string", Some("null")),
    pub_prop("enc_ns", "?string", Some("null")),
    pub_prop("enc_name", "?string", Some("null")),
    pub_prop("enc_namens", "?string", Some("null")),
];

/// `SoapParam` properties.
pub const PARAM_PROPERTIES: &[PropDef] = &[
    pub_prop("param_name", "string", None),
    pub_prop("param_data", "mixed", None),
];

/// `SoapHeader` properties.
pub const HEADER_PROPERTIES: &[PropDef] = &[
    pub_prop("namespace", "string", None),
    pub_prop("name", "string", None),
    pub_prop("data", "mixed", Some("null")),
    pub_prop("mustUnderstand", "bool", None),
    pub_prop("actor", "string|int|null", None),
];

/// `SoapServer` properties.
pub const SERVER_PROPERTIES: &[PropDef] = &[priv_prop("__soap_fault", Some("?SoapFault"), Some("null"))];

/// All classes.
pub const CLASSES: &[ClassDef] = &[
    ClassDef {
        name: "Soap\\Url",
        parent: None,
        is_final: true,
        opaque: true,
        properties: &[],
        methods: &[],
    },
    ClassDef {
        name: "Soap\\Sdl",
        parent: None,
        is_final: true,
        opaque: true,
        properties: &[],
        methods: &[],
    },
    ClassDef {
        name: "SoapParam",
        parent: None,
        is_final: false,
        opaque: false,
        properties: PARAM_PROPERTIES,
        methods: &[m("__construct", &[p("data", Some("mixed"), None), p("name", Some("string"), None)], None, false)],
    },
    ClassDef {
        name: "SoapHeader",
        parent: None,
        is_final: false,
        opaque: false,
        properties: HEADER_PROPERTIES,
        methods: &[m(
            "__construct",
            &[
                p("namespace", Some("string"), None),
                p("name", Some("string"), None),
                p("data", Some("mixed"), Some("UNKNOWN")),
                p("mustUnderstand", Some("bool"), Some("false")),
                p("actor", Some("string|int|null"), Some("null")),
            ],
            None,
            false,
        )],
    },
    ClassDef {
        name: "SoapFault",
        parent: Some("Exception"),
        is_final: false,
        opaque: false,
        properties: FAULT_PROPERTIES,
        methods: &[
            m(
                "__construct",
                &[
                    p("code", Some("array|string|null"), None),
                    p("string", Some("string"), None),
                    p("actor", Some("?string"), Some("null")),
                    p("details", Some("mixed"), Some("null")),
                    p("name", Some("?string"), Some("null")),
                    p("headerFault", Some("mixed"), Some("null")),
                    p("lang", Some("string"), Some("\"\"")),
                ],
                None,
                false,
            ),
            m("__toString", &[], Some("string"), false),
        ],
    },
    ClassDef {
        name: "SoapVar",
        parent: None,
        is_final: false,
        opaque: false,
        properties: VAR_PROPERTIES,
        methods: &[m(
            "__construct",
            &[
                p("data", Some("mixed"), None),
                p("encoding", Some("?int"), None),
                p("typeName", Some("?string"), Some("null")),
                p("typeNamespace", Some("?string"), Some("null")),
                p("nodeName", Some("?string"), Some("null")),
                p("nodeNamespace", Some("?string"), Some("null")),
            ],
            None,
            false,
        )],
    },
    ClassDef {
        name: "SoapServer",
        parent: None,
        is_final: false,
        opaque: false,
        properties: SERVER_PROPERTIES,
        methods: &[
            m("__construct", &[p("wsdl", Some("?string"), None), p("options", Some("array"), Some("[]"))], None, false),
            m(
                "fault",
                &[
                    p("code", Some("string"), None),
                    p("string", Some("string"), None),
                    p("actor", Some("string"), Some("\"\"")),
                    p("details", Some("mixed"), Some("null")),
                    p("name", Some("string"), Some("\"\"")),
                    p("lang", Some("string"), Some("\"\"")),
                ],
                Some("void"),
                true,
            ),
            m("addSoapHeader", &[p("header", Some("SoapHeader"), None)], Some("void"), true),
            m("setPersistence", &[p("mode", Some("int"), None)], Some("void"), true),
            m(
                "setClass",
                &[
                    p("class", Some("string"), None),
                    ParamDef {
                        name: "args",
                        ty: Some("mixed"),
                        default: None,
                        by_ref: false,
                        variadic: true,
                    },
                ],
                Some("void"),
                true,
            ),
            m("setObject", &[p("object", Some("object"), None)], Some("void"), true),
            m("getFunctions", &[], Some("array"), true),
            m("addFunction", &[p("functions", None, None)], Some("void"), true),
            m("handle", &[p("request", Some("?string"), Some("null"))], Some("void"), true),
            m("__getLastResponse", &[], Some("?string"), false),
        ],
    },
    ClassDef {
        name: "SoapClient",
        parent: None,
        is_final: false,
        opaque: false,
        properties: CLIENT_PROPERTIES,
        methods: &[
            m("__construct", &[p("wsdl", Some("?string"), None), p("options", Some("array"), Some("[]"))], None, false),
            m("__call", &[p("name", Some("string"), None), p("args", Some("array"), None)], Some("mixed"), true),
            m(
                "__soapCall",
                &[
                    p("name", Some("string"), None),
                    p("args", Some("array"), None),
                    p("options", Some("?array"), Some("null")),
                    p("inputHeaders", None, Some("null")),
                    ParamDef {
                        name: "outputHeaders",
                        ty: None,
                        default: Some("null"),
                        by_ref: true,
                        variadic: false,
                    },
                ],
                Some("mixed"),
                true,
            ),
            m("__getFunctions", &[], Some("?array"), true),
            m("__getTypes", &[], Some("?array"), true),
            m("__getLastRequest", &[], Some("?string"), true),
            m("__getLastResponse", &[], Some("?string"), true),
            m("__getLastRequestHeaders", &[], Some("?string"), true),
            m("__getLastResponseHeaders", &[], Some("?string"), true),
            m(
                "__doRequest",
                &[
                    p("request", Some("string"), None),
                    p("location", Some("string"), None),
                    p("action", Some("string"), None),
                    p("version", Some("int"), None),
                    p("oneWay", Some("bool"), Some("false")),
                    p("uriParserClass", Some("?string"), Some("null")),
                ],
                Some("?string"),
                true,
            ),
            m("__setCookie", &[p("name", Some("string"), None), p("value", Some("?string"), Some("null"))], Some("void"), true),
            m("__getCookies", &[], Some("array"), true),
            m("__setSoapHeaders", &[p("headers", None, Some("null"))], Some("bool"), true),
            m("__setLocation", &[p("location", Some("?string"), Some("null"))], Some("?string"), true),
        ],
    },
];

/// Global functions.
pub const FUNCTIONS: &[MethodDef] = &[
    m("use_soap_error_handler", &[p("enable", Some("bool"), Some("true"))], Some("bool"), false),
    m("is_soap_fault", &[p("object", Some("mixed"), None)], Some("bool"), false),
];

/// Looks a class up (case-insensitive).
#[must_use]
pub fn find_class(name: &str) -> Option<&'static ClassDef> {
    CLASSES.iter().find(|c| c.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_has_37_slots() {
        assert_eq!(CLIENT_PROPERTIES.len(), 37);
        assert_eq!(CLIENT_PROPERTIES[36].name, "_follow_cross_origin_redirects");
        assert!(find_class("soapvar").is_some());
    }
}
