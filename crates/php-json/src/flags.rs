//! The `JSON_*` option constants (`php_json.h`).

/// `JSON_OBJECT_AS_ARRAY` (decode).
pub const OBJECT_AS_ARRAY: i64 = 1 << 0;
/// `JSON_BIGINT_AS_STRING` (decode).
pub const BIGINT_AS_STRING: i64 = 1 << 1;

/// `JSON_HEX_TAG` (encode).
pub const HEX_TAG: i64 = 1 << 0;
/// `JSON_HEX_AMP` (encode).
pub const HEX_AMP: i64 = 1 << 1;
/// `JSON_HEX_APOS` (encode).
pub const HEX_APOS: i64 = 1 << 2;
/// `JSON_HEX_QUOT` (encode).
pub const HEX_QUOT: i64 = 1 << 3;
/// `JSON_FORCE_OBJECT` (encode).
pub const FORCE_OBJECT: i64 = 1 << 4;
/// `JSON_NUMERIC_CHECK` (encode).
pub const NUMERIC_CHECK: i64 = 1 << 5;
/// `JSON_UNESCAPED_SLASHES` (encode).
pub const UNESCAPED_SLASHES: i64 = 1 << 6;
/// `JSON_PRETTY_PRINT` (encode).
pub const PRETTY_PRINT: i64 = 1 << 7;
/// `JSON_UNESCAPED_UNICODE` (encode).
pub const UNESCAPED_UNICODE: i64 = 1 << 8;
/// `JSON_PARTIAL_OUTPUT_ON_ERROR` (encode).
pub const PARTIAL_OUTPUT_ON_ERROR: i64 = 1 << 9;
/// `JSON_PRESERVE_ZERO_FRACTION` (encode).
pub const PRESERVE_ZERO_FRACTION: i64 = 1 << 10;
/// `JSON_UNESCAPED_LINE_TERMINATORS` (encode).
pub const UNESCAPED_LINE_TERMINATORS: i64 = 1 << 11;

/// `JSON_INVALID_UTF8_IGNORE` (encode, decode and validate).
pub const INVALID_UTF8_IGNORE: i64 = 1 << 20;
/// `JSON_INVALID_UTF8_SUBSTITUTE` (encode and decode).
pub const INVALID_UTF8_SUBSTITUTE: i64 = 1 << 21;
/// `JSON_THROW_ON_ERROR` (encode and decode).
pub const THROW_ON_ERROR: i64 = 1 << 22;

/// `PHP_JSON_PARSER_DEFAULT_DEPTH`: the default `$depth` argument.
pub const DEFAULT_DEPTH: i64 = 512;

/// All `JSON_*` constants registered by the extension, as `(name, value)`
/// pairs in the order of `json.stub.php` (including the `JSON_ERROR_*`
/// codes).
pub const CONSTANTS: &[(&str, i64)] = &[
    ("JSON_HEX_TAG", HEX_TAG),
    ("JSON_HEX_AMP", HEX_AMP),
    ("JSON_HEX_APOS", HEX_APOS),
    ("JSON_HEX_QUOT", HEX_QUOT),
    ("JSON_FORCE_OBJECT", FORCE_OBJECT),
    ("JSON_NUMERIC_CHECK", NUMERIC_CHECK),
    ("JSON_UNESCAPED_SLASHES", UNESCAPED_SLASHES),
    ("JSON_PRETTY_PRINT", PRETTY_PRINT),
    ("JSON_UNESCAPED_UNICODE", UNESCAPED_UNICODE),
    ("JSON_PARTIAL_OUTPUT_ON_ERROR", PARTIAL_OUTPUT_ON_ERROR),
    ("JSON_PRESERVE_ZERO_FRACTION", PRESERVE_ZERO_FRACTION),
    (
        "JSON_UNESCAPED_LINE_TERMINATORS",
        UNESCAPED_LINE_TERMINATORS,
    ),
    ("JSON_OBJECT_AS_ARRAY", OBJECT_AS_ARRAY),
    ("JSON_BIGINT_AS_STRING", BIGINT_AS_STRING),
    ("JSON_INVALID_UTF8_IGNORE", INVALID_UTF8_IGNORE),
    ("JSON_INVALID_UTF8_SUBSTITUTE", INVALID_UTF8_SUBSTITUTE),
    ("JSON_THROW_ON_ERROR", THROW_ON_ERROR),
    ("JSON_ERROR_NONE", 0),
    ("JSON_ERROR_DEPTH", 1),
    ("JSON_ERROR_STATE_MISMATCH", 2),
    ("JSON_ERROR_CTRL_CHAR", 3),
    ("JSON_ERROR_SYNTAX", 4),
    ("JSON_ERROR_UTF8", 5),
    ("JSON_ERROR_RECURSION", 6),
    ("JSON_ERROR_INF_OR_NAN", 7),
    ("JSON_ERROR_UNSUPPORTED_TYPE", 8),
    ("JSON_ERROR_INVALID_PROPERTY_NAME", 9),
    ("JSON_ERROR_UTF16", 10),
    ("JSON_ERROR_NON_BACKED_ENUM", 11),
];
