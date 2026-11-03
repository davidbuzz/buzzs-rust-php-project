//! The class/function table of the extension (from `php_zip.stub.php`),
//! gated like the stub: parameter names, types and defaults for the VM's
//! argument parsing and reflection.

/// One parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Param {
    /// Name without `$`.
    pub name: &'static str,
    /// Declared type (empty when untyped).
    pub ty: &'static str,
    /// Default value expression (`None` when required).
    pub default: Option<&'static str>,
    /// By reference.
    pub by_ref: bool,
    /// `#[\SensitiveParameter]`.
    pub sensitive: bool,
}

/// One method or function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Method {
    /// Name.
    pub name: &'static str,
    /// Parameters.
    pub params: &'static [Param],
    /// Return type (empty when undeclared).
    pub ret: &'static str,
    /// `static`.
    pub is_static: bool,
    /// Whether the return type is tentative (`@tentative-return-type`):
    /// reflection reports it through `getTentativeReturnType()`.
    pub tentative: bool,
    /// `#[\Deprecated]`: `(since, message)`.
    pub deprecated: Option<(&'static str, Option<&'static str>)>,
}

const fn p(name: &'static str, ty: &'static str) -> Param {
    Param {
        name,
        ty,
        default: None,
        by_ref: false,
        sensitive: false,
    }
}

const fn o(name: &'static str, ty: &'static str, default: &'static str) -> Param {
    Param {
        name,
        ty,
        default: Some(default),
        by_ref: false,
        sensitive: false,
    }
}

const fn r(name: &'static str) -> Param {
    Param {
        name,
        ty: "",
        default: None,
        by_ref: true,
        sensitive: false,
    }
}

const fn s(mut param: Param) -> Param {
    param.sensitive = true;
    param
}

/// A method with a tentative return type (most of `ZipArchive`).
const fn m(name: &'static str, params: &'static [Param], ret: &'static str) -> Method {
    Method {
        name,
        params,
        ret,
        is_static: false,
        tentative: !ret.is_empty(),
        deprecated: None,
    }
}

/// A method with a real return type.
const fn mr(name: &'static str, params: &'static [Param], ret: &'static str) -> Method {
    Method {
        name,
        params,
        ret,
        is_static: false,
        tentative: false,
        deprecated: None,
    }
}

const fn f(
    name: &'static str,
    params: &'static [Param],
    ret: &'static str,
    message: Option<&'static str>,
) -> Method {
    Method {
        name,
        params,
        ret,
        is_static: false,
        tentative: false,
        deprecated: Some(("8.0", message)),
    }
}

/// The deprecated procedural functions.
pub const FUNCTIONS: &[Method] = &[
    f(
        "zip_open",
        &[p("filename", "string")],
        "",
        Some("use ZipArchive::open() instead"),
    ),
    f(
        "zip_close",
        &[p("zip", "")],
        "void",
        Some("use ZipArchive::close() instead"),
    ),
    f(
        "zip_read",
        &[p("zip", "")],
        "",
        Some("use ZipArchive::statIndex() instead"),
    ),
    f(
        "zip_entry_open",
        &[
            p("zip_dp", ""),
            p("zip_entry", ""),
            o("mode", "string", "\"rb\""),
        ],
        "bool",
        None,
    ),
    f("zip_entry_close", &[p("zip_entry", "")], "true", None),
    f(
        "zip_entry_read",
        &[p("zip_entry", ""), o("len", "int", "1024")],
        "string|false",
        Some("use ZipArchive::getFromIndex() instead"),
    ),
    f(
        "zip_entry_name",
        &[p("zip_entry", "")],
        "string|false",
        Some("use ZipArchive::statIndex() instead"),
    ),
    f(
        "zip_entry_compressedsize",
        &[p("zip_entry", "")],
        "int|false",
        Some("use ZipArchive::statIndex() instead"),
    ),
    f(
        "zip_entry_filesize",
        &[p("zip_entry", "")],
        "int|false",
        Some("use ZipArchive::statIndex() instead"),
    ),
    f(
        "zip_entry_compressionmethod",
        &[p("zip_entry", "")],
        "string|false",
        Some("use ZipArchive::statIndex() instead"),
    ),
];

const NAME_FLAGS: &[Param] = &[p("name", "string"), o("flags", "int", "0")];
const INDEX_FLAGS: &[Param] = &[p("index", "int"), o("flags", "int", "0")];

/// The `ZipArchive` methods in declaration order (the class implements
/// `Countable`).
pub const METHODS: &[Method] = &[
    m(
        "open",
        &[p("filename", "string"), o("flags", "int", "0")],
        "bool|int",
    ),
    mr(
        "openString",
        &[o("data", "string", "''"), o("flags", "int", "0")],
        "bool|int",
    ),
    m("setPassword", &[s(p("password", "string"))], "bool"),
    m("close", &[], "bool"),
    mr("closeString", &[], "string|false"),
    m("count", &[], "int"),
    mr("__serialize", &[], "array"),
    mr("__unserialize", &[p("data", "array")], "void"),
    m("getStatusString", &[], "string"),
    mr("clearError", &[], "void"),
    m(
        "addEmptyDir",
        &[p("dirname", "string"), o("flags", "int", "0")],
        "bool",
    ),
    m(
        "addFromString",
        &[
            p("name", "string"),
            p("content", "string"),
            o("flags", "int", "ZipArchive::FL_OVERWRITE"),
        ],
        "bool",
    ),
    m(
        "addFile",
        &[
            p("filepath", "string"),
            o("entryname", "string", "\"\""),
            o("start", "int", "0"),
            o("length", "int", "ZipArchive::LENGTH_TO_END"),
            o("flags", "int", "ZipArchive::FL_OVERWRITE"),
        ],
        "bool",
    ),
    m(
        "replaceFile",
        &[
            p("filepath", "string"),
            p("index", "int"),
            o("start", "int", "0"),
            o("length", "int", "ZipArchive::LENGTH_TO_END"),
            o("flags", "int", "0"),
        ],
        "bool",
    ),
    m(
        "addGlob",
        &[
            p("pattern", "string"),
            o("flags", "int", "0"),
            o("options", "array", "[]"),
        ],
        "array|false",
    ),
    m(
        "addPattern",
        &[
            p("pattern", "string"),
            o("path", "string", "\".\""),
            o("options", "array", "[]"),
        ],
        "array|false",
    ),
    m(
        "renameIndex",
        &[p("index", "int"), p("new_name", "string")],
        "bool",
    ),
    m(
        "renameName",
        &[p("name", "string"), p("new_name", "string")],
        "bool",
    ),
    m("setArchiveComment", &[p("comment", "string")], "bool"),
    m(
        "getArchiveComment",
        &[o("flags", "int", "0")],
        "string|false",
    ),
    mr(
        "setArchiveFlag",
        &[p("flag", "int"), p("value", "int")],
        "bool",
    ),
    mr(
        "getArchiveFlag",
        &[p("flag", "int"), o("flags", "int", "0")],
        "int",
    ),
    m(
        "setCommentIndex",
        &[p("index", "int"), p("comment", "string")],
        "bool",
    ),
    m(
        "setCommentName",
        &[p("name", "string"), p("comment", "string")],
        "bool",
    ),
    m(
        "setMtimeIndex",
        &[
            p("index", "int"),
            p("timestamp", "int"),
            o("flags", "int", "0"),
        ],
        "bool",
    ),
    m(
        "setMtimeName",
        &[
            p("name", "string"),
            p("timestamp", "int"),
            o("flags", "int", "0"),
        ],
        "bool",
    ),
    m("getCommentIndex", INDEX_FLAGS, "string|false"),
    m("getCommentName", NAME_FLAGS, "string|false"),
    m("deleteIndex", &[p("index", "int")], "bool"),
    m("deleteName", &[p("name", "string")], "bool"),
    m("statName", NAME_FLAGS, "array|false"),
    m("statIndex", INDEX_FLAGS, "array|false"),
    m("locateName", NAME_FLAGS, "int|false"),
    m("getNameIndex", INDEX_FLAGS, "string|false"),
    m("unchangeArchive", &[], "bool"),
    m("unchangeAll", &[], "bool"),
    m("unchangeIndex", &[p("index", "int")], "bool"),
    m("unchangeName", &[p("name", "string")], "bool"),
    m(
        "extractTo",
        &[
            p("pathto", "string"),
            o("files", "array|string|null", "null"),
        ],
        "bool",
    ),
    m(
        "getFromName",
        &[
            p("name", "string"),
            o("len", "int", "0"),
            o("flags", "int", "0"),
        ],
        "string|false",
    ),
    m(
        "getFromIndex",
        &[
            p("index", "int"),
            o("len", "int", "0"),
            o("flags", "int", "0"),
        ],
        "string|false",
    ),
    m("getStreamIndex", INDEX_FLAGS, ""),
    m("getStreamName", NAME_FLAGS, ""),
    m("getStream", &[p("name", "string")], ""),
    m(
        "setExternalAttributesName",
        &[
            p("name", "string"),
            p("opsys", "int"),
            p("attr", "int"),
            o("flags", "int", "0"),
        ],
        "bool",
    ),
    m(
        "setExternalAttributesIndex",
        &[
            p("index", "int"),
            p("opsys", "int"),
            p("attr", "int"),
            o("flags", "int", "0"),
        ],
        "bool",
    ),
    m(
        "getExternalAttributesName",
        &[
            p("name", "string"),
            r("opsys"),
            r("attr"),
            o("flags", "int", "0"),
        ],
        "bool",
    ),
    m(
        "getExternalAttributesIndex",
        &[
            p("index", "int"),
            r("opsys"),
            r("attr"),
            o("flags", "int", "0"),
        ],
        "bool",
    ),
    m(
        "setCompressionName",
        &[
            p("name", "string"),
            p("method", "int"),
            o("compflags", "int", "0"),
        ],
        "bool",
    ),
    m(
        "setCompressionIndex",
        &[
            p("index", "int"),
            p("method", "int"),
            o("compflags", "int", "0"),
        ],
        "bool",
    ),
    m(
        "setEncryptionName",
        &[
            p("name", "string"),
            p("method", "int"),
            s(o("password", "?string", "null")),
        ],
        "bool",
    ),
    m(
        "setEncryptionIndex",
        &[
            p("index", "int"),
            p("method", "int"),
            s(o("password", "?string", "null")),
        ],
        "bool",
    ),
    m(
        "registerProgressCallback",
        &[p("rate", "float"), p("callback", "callable")],
        "bool",
    ),
    m(
        "registerCancelCallback",
        &[p("callback", "callable")],
        "bool",
    ),
    Method {
        name: "isCompressionMethodSupported",
        params: &[p("method", "int"), o("enc", "bool", "true")],
        ret: "bool",
        is_static: true,
        tentative: false,
        deprecated: None,
    },
    Method {
        name: "isEncryptionMethodSupported",
        params: &[p("method", "int"), o("enc", "bool", "true")],
        ret: "bool",
        is_static: true,
        tentative: false,
        deprecated: None,
    },
];

/// The declared (virtual) properties with their types.
pub const PROPERTIES: &[(&str, &str)] = &[
    ("lastId", "int"),
    ("status", "int"),
    ("statusSys", "int"),
    ("numFiles", "int"),
    ("filename", "string"),
    ("comment", "string"),
];

/// Looks up a method by (case-insensitive) name.
pub fn method(name: &str) -> Option<&'static Method> {
    METHODS.iter().find(|m| m.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        let open = method("OPEN").unwrap();
        assert_eq!(open.params[0].name, "filename");
        assert!(method("isCompressionMethodSupported").unwrap().is_static);
        assert!(method("getExternalAttributesIndex").unwrap().params[1].by_ref);
        assert_eq!(FUNCTIONS.len(), 10);
        assert_eq!(PROPERTIES.len(), crate::archive::PROPERTY_NAMES.len());
    }
}
