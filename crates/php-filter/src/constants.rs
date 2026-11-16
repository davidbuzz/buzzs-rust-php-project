// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Filter ids, flags, the filter list and the PHP constants of `ext/filter`
//! (`filter_private.h`, `filter.stub.php`).

/// `FILTER_FLAG_NONE`.
pub const FILTER_FLAG_NONE: i64 = 0x0000;
/// `FILTER_REQUIRE_ARRAY`.
pub const FILTER_REQUIRE_ARRAY: i64 = 0x0100_0000;
/// `FILTER_REQUIRE_SCALAR`.
pub const FILTER_REQUIRE_SCALAR: i64 = 0x0200_0000;
/// `FILTER_FORCE_ARRAY`.
pub const FILTER_FORCE_ARRAY: i64 = 0x0400_0000;
/// `FILTER_NULL_ON_FAILURE`.
pub const FILTER_NULL_ON_FAILURE: i64 = 0x0800_0000;
/// `FILTER_THROW_ON_FAILURE`.
pub const FILTER_THROW_ON_FAILURE: i64 = 0x1000_0000;

/// `FILTER_FLAG_ALLOW_OCTAL`.
pub const FILTER_FLAG_ALLOW_OCTAL: i64 = 0x0001;
/// `FILTER_FLAG_ALLOW_HEX`.
pub const FILTER_FLAG_ALLOW_HEX: i64 = 0x0002;
/// `FILTER_FLAG_STRIP_LOW`.
pub const FILTER_FLAG_STRIP_LOW: i64 = 0x0004;
/// `FILTER_FLAG_STRIP_HIGH`.
pub const FILTER_FLAG_STRIP_HIGH: i64 = 0x0008;
/// `FILTER_FLAG_ENCODE_LOW`.
pub const FILTER_FLAG_ENCODE_LOW: i64 = 0x0010;
/// `FILTER_FLAG_ENCODE_HIGH`.
pub const FILTER_FLAG_ENCODE_HIGH: i64 = 0x0020;
/// `FILTER_FLAG_ENCODE_AMP`.
pub const FILTER_FLAG_ENCODE_AMP: i64 = 0x0040;
/// `FILTER_FLAG_NO_ENCODE_QUOTES`.
pub const FILTER_FLAG_NO_ENCODE_QUOTES: i64 = 0x0080;
/// `FILTER_FLAG_EMPTY_STRING_NULL`.
pub const FILTER_FLAG_EMPTY_STRING_NULL: i64 = 0x0100;
/// `FILTER_FLAG_STRIP_BACKTICK`.
pub const FILTER_FLAG_STRIP_BACKTICK: i64 = 0x0200;
/// `FILTER_FLAG_ALLOW_FRACTION`.
pub const FILTER_FLAG_ALLOW_FRACTION: i64 = 0x1000;
/// `FILTER_FLAG_ALLOW_THOUSAND`.
pub const FILTER_FLAG_ALLOW_THOUSAND: i64 = 0x2000;
/// `FILTER_FLAG_ALLOW_SCIENTIFIC`.
pub const FILTER_FLAG_ALLOW_SCIENTIFIC: i64 = 0x4000;
/// `FILTER_FLAG_SCHEME_REQUIRED` (no longer exported to PHP).
pub const FILTER_FLAG_SCHEME_REQUIRED: i64 = 0x01_0000;
/// `FILTER_FLAG_HOST_REQUIRED` (no longer exported to PHP).
pub const FILTER_FLAG_HOST_REQUIRED: i64 = 0x02_0000;
/// `FILTER_FLAG_PATH_REQUIRED`.
pub const FILTER_FLAG_PATH_REQUIRED: i64 = 0x04_0000;
/// `FILTER_FLAG_QUERY_REQUIRED`.
pub const FILTER_FLAG_QUERY_REQUIRED: i64 = 0x08_0000;
/// `FILTER_FLAG_IPV4`.
pub const FILTER_FLAG_IPV4: i64 = 0x0010_0000;
/// `FILTER_FLAG_IPV6`.
pub const FILTER_FLAG_IPV6: i64 = 0x0020_0000;
/// `FILTER_FLAG_NO_RES_RANGE`.
pub const FILTER_FLAG_NO_RES_RANGE: i64 = 0x0040_0000;
/// `FILTER_FLAG_NO_PRIV_RANGE`.
pub const FILTER_FLAG_NO_PRIV_RANGE: i64 = 0x0080_0000;
/// `FILTER_FLAG_GLOBAL_RANGE`.
pub const FILTER_FLAG_GLOBAL_RANGE: i64 = 0x2000_0000;
/// `FILTER_FLAG_HOSTNAME`.
pub const FILTER_FLAG_HOSTNAME: i64 = 0x10_0000;
/// `FILTER_FLAG_EMAIL_UNICODE`.
pub const FILTER_FLAG_EMAIL_UNICODE: i64 = 0x10_0000;

/// `FILTER_VALIDATE_INT`.
pub const FILTER_VALIDATE_INT: i64 = 0x0101;
/// `FILTER_VALIDATE_BOOL` / `FILTER_VALIDATE_BOOLEAN`.
pub const FILTER_VALIDATE_BOOL: i64 = 0x0102;
/// `FILTER_VALIDATE_FLOAT`.
pub const FILTER_VALIDATE_FLOAT: i64 = 0x0103;
/// `FILTER_VALIDATE_REGEXP`.
pub const FILTER_VALIDATE_REGEXP: i64 = 0x0110;
/// `FILTER_VALIDATE_URL`.
pub const FILTER_VALIDATE_URL: i64 = 0x0111;
/// `FILTER_VALIDATE_EMAIL`.
pub const FILTER_VALIDATE_EMAIL: i64 = 0x0112;
/// `FILTER_VALIDATE_IP`.
pub const FILTER_VALIDATE_IP: i64 = 0x0113;
/// `FILTER_VALIDATE_MAC`.
pub const FILTER_VALIDATE_MAC: i64 = 0x0114;
/// `FILTER_VALIDATE_DOMAIN`.
pub const FILTER_VALIDATE_DOMAIN: i64 = 0x0115;
/// The last validation filter id.
pub const FILTER_VALIDATE_LAST: i64 = 0x0115;
/// The first validation filter id (`FILTER_VALIDATE_ALL`).
pub const FILTER_VALIDATE_ALL: i64 = 0x0100;

/// `FILTER_DEFAULT` / `FILTER_UNSAFE_RAW`.
pub const FILTER_DEFAULT: i64 = 0x0204;
/// `FILTER_UNSAFE_RAW`.
pub const FILTER_UNSAFE_RAW: i64 = 0x0204;
/// `FILTER_SANITIZE_STRING` / `FILTER_SANITIZE_STRIPPED` (deprecated).
pub const FILTER_SANITIZE_STRING: i64 = 0x0201;
/// `FILTER_SANITIZE_ENCODED`.
pub const FILTER_SANITIZE_ENCODED: i64 = 0x0202;
/// `FILTER_SANITIZE_SPECIAL_CHARS`.
pub const FILTER_SANITIZE_SPECIAL_CHARS: i64 = 0x0203;
/// `FILTER_SANITIZE_EMAIL`.
pub const FILTER_SANITIZE_EMAIL: i64 = 0x0205;
/// `FILTER_SANITIZE_URL`.
pub const FILTER_SANITIZE_URL: i64 = 0x0206;
/// `FILTER_SANITIZE_NUMBER_INT`.
pub const FILTER_SANITIZE_NUMBER_INT: i64 = 0x0207;
/// `FILTER_SANITIZE_NUMBER_FLOAT`.
pub const FILTER_SANITIZE_NUMBER_FLOAT: i64 = 0x0208;
/// `FILTER_SANITIZE_FULL_SPECIAL_CHARS`.
pub const FILTER_SANITIZE_FULL_SPECIAL_CHARS: i64 = 0x020a;
/// `FILTER_SANITIZE_ADD_SLASHES`.
pub const FILTER_SANITIZE_ADD_SLASHES: i64 = 0x020b;
/// The last sanitizing filter id.
pub const FILTER_SANITIZE_LAST: i64 = 0x020b;
/// The first sanitizing filter id (`FILTER_SANITIZE_ALL`).
pub const FILTER_SANITIZE_ALL: i64 = 0x0200;
/// `FILTER_CALLBACK`.
pub const FILTER_CALLBACK: i64 = 0x0400;

/// `INPUT_POST`.
pub const INPUT_POST: i64 = 0;
/// `INPUT_GET`.
pub const INPUT_GET: i64 = 1;
/// `INPUT_COOKIE`.
pub const INPUT_COOKIE: i64 = 2;
/// `INPUT_ENV`.
pub const INPUT_ENV: i64 = 4;
/// `INPUT_SERVER`.
pub const INPUT_SERVER: i64 = 5;

/// `PHP_FILTER_ID_EXISTS(id)`: whether `id` names a filter. Note that ids
/// inside the ranges that have no filter (`0x0104`..`0x010f`, `0x0209`)
/// "exist" and silently fall back to `FILTER_DEFAULT`.
#[must_use]
pub fn filter_id_exists(id: i64) -> bool {
    (FILTER_SANITIZE_ALL..=FILTER_SANITIZE_LAST).contains(&id)
        || (FILTER_VALIDATE_ALL..=FILTER_VALIDATE_LAST).contains(&id)
        || id == FILTER_CALLBACK
}

/// The filter implementations (`php_filter_*` functions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterKind {
    /// `php_filter_int`.
    Int,
    /// `php_filter_boolean`.
    Boolean,
    /// `php_filter_float`.
    Float,
    /// `php_filter_validate_regexp`.
    ValidateRegexp,
    /// `php_filter_validate_domain`.
    ValidateDomain,
    /// `php_filter_validate_url`.
    ValidateUrl,
    /// `php_filter_validate_email`.
    ValidateEmail,
    /// `php_filter_validate_ip`.
    ValidateIp,
    /// `php_filter_validate_mac`.
    ValidateMac,
    /// `php_filter_string` (`FILTER_SANITIZE_STRING`, deprecated).
    String,
    /// `php_filter_encoded`.
    Encoded,
    /// `php_filter_special_chars`.
    SpecialChars,
    /// `php_filter_full_special_chars`.
    FullSpecialChars,
    /// `php_filter_unsafe_raw`.
    UnsafeRaw,
    /// `php_filter_email`.
    Email,
    /// `php_filter_url`.
    Url,
    /// `php_filter_number_int`.
    NumberInt,
    /// `php_filter_number_float`.
    NumberFloat,
    /// `php_filter_add_slashes`.
    AddSlashes,
    /// `php_filter_callback`.
    Callback,
}

/// One entry of the filter list (`filter_list_entry`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterEntry {
    /// The name returned by `filter_list()` and accepted by `filter_id()`.
    pub name: &'static str,
    /// The filter id.
    pub id: i64,
    /// The implementation.
    pub kind: FilterKind,
}

/// The filter list, in `filter.c` order (this is `filter_list()`).
pub const FILTER_LIST: [FilterEntry; 21] = [
    FilterEntry {
        name: "int",
        id: FILTER_VALIDATE_INT,
        kind: FilterKind::Int,
    },
    FilterEntry {
        name: "boolean",
        id: FILTER_VALIDATE_BOOL,
        kind: FilterKind::Boolean,
    },
    FilterEntry {
        name: "float",
        id: FILTER_VALIDATE_FLOAT,
        kind: FilterKind::Float,
    },
    FilterEntry {
        name: "validate_regexp",
        id: FILTER_VALIDATE_REGEXP,
        kind: FilterKind::ValidateRegexp,
    },
    FilterEntry {
        name: "validate_domain",
        id: FILTER_VALIDATE_DOMAIN,
        kind: FilterKind::ValidateDomain,
    },
    FilterEntry {
        name: "validate_url",
        id: FILTER_VALIDATE_URL,
        kind: FilterKind::ValidateUrl,
    },
    FilterEntry {
        name: "validate_email",
        id: FILTER_VALIDATE_EMAIL,
        kind: FilterKind::ValidateEmail,
    },
    FilterEntry {
        name: "validate_ip",
        id: FILTER_VALIDATE_IP,
        kind: FilterKind::ValidateIp,
    },
    FilterEntry {
        name: "validate_mac",
        id: FILTER_VALIDATE_MAC,
        kind: FilterKind::ValidateMac,
    },
    FilterEntry {
        name: "string",
        id: FILTER_SANITIZE_STRING,
        kind: FilterKind::String,
    },
    FilterEntry {
        name: "stripped",
        id: FILTER_SANITIZE_STRING,
        kind: FilterKind::String,
    },
    FilterEntry {
        name: "encoded",
        id: FILTER_SANITIZE_ENCODED,
        kind: FilterKind::Encoded,
    },
    FilterEntry {
        name: "special_chars",
        id: FILTER_SANITIZE_SPECIAL_CHARS,
        kind: FilterKind::SpecialChars,
    },
    FilterEntry {
        name: "full_special_chars",
        id: FILTER_SANITIZE_FULL_SPECIAL_CHARS,
        kind: FilterKind::FullSpecialChars,
    },
    FilterEntry {
        name: "unsafe_raw",
        id: FILTER_UNSAFE_RAW,
        kind: FilterKind::UnsafeRaw,
    },
    FilterEntry {
        name: "email",
        id: FILTER_SANITIZE_EMAIL,
        kind: FilterKind::Email,
    },
    FilterEntry {
        name: "url",
        id: FILTER_SANITIZE_URL,
        kind: FilterKind::Url,
    },
    FilterEntry {
        name: "number_int",
        id: FILTER_SANITIZE_NUMBER_INT,
        kind: FilterKind::NumberInt,
    },
    FilterEntry {
        name: "number_float",
        id: FILTER_SANITIZE_NUMBER_FLOAT,
        kind: FilterKind::NumberFloat,
    },
    FilterEntry {
        name: "add_slashes",
        id: FILTER_SANITIZE_ADD_SLASHES,
        kind: FilterKind::AddSlashes,
    },
    FilterEntry {
        name: "callback",
        id: FILTER_CALLBACK,
        kind: FilterKind::Callback,
    },
];

/// `php_find_filter`: the first entry with this id, falling back to
/// `FILTER_DEFAULT` (`unsafe_raw`) for unknown ids.
#[must_use]
pub fn find_filter(id: i64) -> FilterEntry {
    FILTER_LIST
        .iter()
        .find(|e| e.id == id)
        .or_else(|| FILTER_LIST.iter().find(|e| e.id == FILTER_DEFAULT))
        .copied()
        .unwrap_or(FILTER_LIST[0])
}

/// `filter_list()`: the names of all filters.
#[must_use]
pub fn filter_list() -> Vec<&'static str> {
    FILTER_LIST.iter().map(|e| e.name).collect()
}

/// `filter_id(string $name)`: the id of a filter by exact (case-sensitive)
/// name; `None` means `false`. Like the C `strcmp`, the comparison stops at
/// the first NUL byte of `name`.
#[must_use]
pub fn filter_id(name: &[u8]) -> Option<i64> {
    let name = name.split(|&b| b == 0).next().unwrap_or_default();
    FILTER_LIST
        .iter()
        .find(|e| e.name.as_bytes() == name)
        .map(|e| e.id)
}

/// A deprecated constant: (since, message) as given by its `#[\Deprecated]`
/// attribute.
pub type ConstantDeprecation = (&'static str, &'static str);

/// All PHP constants registered by `ext/filter`, in registration order:
/// `(name, value, deprecation)`. A deprecated constant makes the engine emit
/// `E_DEPRECATED` "Constant NAME is deprecated since 8.1, use
/// `htmlspecialchars()` instead" when it is read.
pub const CONSTANTS: [(&str, i64, Option<ConstantDeprecation>); 56] = [
    ("INPUT_POST", INPUT_POST, None),
    ("INPUT_GET", INPUT_GET, None),
    ("INPUT_COOKIE", INPUT_COOKIE, None),
    ("INPUT_ENV", INPUT_ENV, None),
    ("INPUT_SERVER", INPUT_SERVER, None),
    ("FILTER_FLAG_NONE", FILTER_FLAG_NONE, None),
    ("FILTER_REQUIRE_SCALAR", FILTER_REQUIRE_SCALAR, None),
    ("FILTER_REQUIRE_ARRAY", FILTER_REQUIRE_ARRAY, None),
    ("FILTER_FORCE_ARRAY", FILTER_FORCE_ARRAY, None),
    ("FILTER_NULL_ON_FAILURE", FILTER_NULL_ON_FAILURE, None),
    ("FILTER_THROW_ON_FAILURE", FILTER_THROW_ON_FAILURE, None),
    ("FILTER_VALIDATE_INT", FILTER_VALIDATE_INT, None),
    ("FILTER_VALIDATE_BOOLEAN", FILTER_VALIDATE_BOOL, None),
    ("FILTER_VALIDATE_BOOL", FILTER_VALIDATE_BOOL, None),
    ("FILTER_VALIDATE_FLOAT", FILTER_VALIDATE_FLOAT, None),
    ("FILTER_VALIDATE_REGEXP", FILTER_VALIDATE_REGEXP, None),
    ("FILTER_VALIDATE_DOMAIN", FILTER_VALIDATE_DOMAIN, None),
    ("FILTER_VALIDATE_URL", FILTER_VALIDATE_URL, None),
    ("FILTER_VALIDATE_EMAIL", FILTER_VALIDATE_EMAIL, None),
    ("FILTER_VALIDATE_IP", FILTER_VALIDATE_IP, None),
    ("FILTER_VALIDATE_MAC", FILTER_VALIDATE_MAC, None),
    ("FILTER_DEFAULT", FILTER_DEFAULT, None),
    ("FILTER_UNSAFE_RAW", FILTER_UNSAFE_RAW, None),
    (
        "FILTER_SANITIZE_STRING",
        FILTER_SANITIZE_STRING,
        Some(("8.1", "use htmlspecialchars() instead")),
    ),
    (
        "FILTER_SANITIZE_STRIPPED",
        FILTER_SANITIZE_STRING,
        Some(("8.1", "use htmlspecialchars() instead")),
    ),
    ("FILTER_SANITIZE_ENCODED", FILTER_SANITIZE_ENCODED, None),
    (
        "FILTER_SANITIZE_SPECIAL_CHARS",
        FILTER_SANITIZE_SPECIAL_CHARS,
        None,
    ),
    (
        "FILTER_SANITIZE_FULL_SPECIAL_CHARS",
        FILTER_SANITIZE_FULL_SPECIAL_CHARS,
        None,
    ),
    ("FILTER_SANITIZE_EMAIL", FILTER_SANITIZE_EMAIL, None),
    ("FILTER_SANITIZE_URL", FILTER_SANITIZE_URL, None),
    (
        "FILTER_SANITIZE_NUMBER_INT",
        FILTER_SANITIZE_NUMBER_INT,
        None,
    ),
    (
        "FILTER_SANITIZE_NUMBER_FLOAT",
        FILTER_SANITIZE_NUMBER_FLOAT,
        None,
    ),
    (
        "FILTER_SANITIZE_ADD_SLASHES",
        FILTER_SANITIZE_ADD_SLASHES,
        None,
    ),
    ("FILTER_CALLBACK", FILTER_CALLBACK, None),
    ("FILTER_FLAG_ALLOW_OCTAL", FILTER_FLAG_ALLOW_OCTAL, None),
    ("FILTER_FLAG_ALLOW_HEX", FILTER_FLAG_ALLOW_HEX, None),
    ("FILTER_FLAG_STRIP_LOW", FILTER_FLAG_STRIP_LOW, None),
    ("FILTER_FLAG_STRIP_HIGH", FILTER_FLAG_STRIP_HIGH, None),
    (
        "FILTER_FLAG_STRIP_BACKTICK",
        FILTER_FLAG_STRIP_BACKTICK,
        None,
    ),
    ("FILTER_FLAG_ENCODE_LOW", FILTER_FLAG_ENCODE_LOW, None),
    ("FILTER_FLAG_ENCODE_HIGH", FILTER_FLAG_ENCODE_HIGH, None),
    ("FILTER_FLAG_ENCODE_AMP", FILTER_FLAG_ENCODE_AMP, None),
    (
        "FILTER_FLAG_NO_ENCODE_QUOTES",
        FILTER_FLAG_NO_ENCODE_QUOTES,
        None,
    ),
    (
        "FILTER_FLAG_EMPTY_STRING_NULL",
        FILTER_FLAG_EMPTY_STRING_NULL,
        None,
    ),
    (
        "FILTER_FLAG_ALLOW_FRACTION",
        FILTER_FLAG_ALLOW_FRACTION,
        None,
    ),
    (
        "FILTER_FLAG_ALLOW_THOUSAND",
        FILTER_FLAG_ALLOW_THOUSAND,
        None,
    ),
    (
        "FILTER_FLAG_ALLOW_SCIENTIFIC",
        FILTER_FLAG_ALLOW_SCIENTIFIC,
        None,
    ),
    ("FILTER_FLAG_PATH_REQUIRED", FILTER_FLAG_PATH_REQUIRED, None),
    (
        "FILTER_FLAG_QUERY_REQUIRED",
        FILTER_FLAG_QUERY_REQUIRED,
        None,
    ),
    ("FILTER_FLAG_IPV4", FILTER_FLAG_IPV4, None),
    ("FILTER_FLAG_IPV6", FILTER_FLAG_IPV6, None),
    ("FILTER_FLAG_NO_RES_RANGE", FILTER_FLAG_NO_RES_RANGE, None),
    ("FILTER_FLAG_NO_PRIV_RANGE", FILTER_FLAG_NO_PRIV_RANGE, None),
    ("FILTER_FLAG_GLOBAL_RANGE", FILTER_FLAG_GLOBAL_RANGE, None),
    ("FILTER_FLAG_HOSTNAME", FILTER_FLAG_HOSTNAME, None),
    ("FILTER_FLAG_EMAIL_UNICODE", FILTER_FLAG_EMAIL_UNICODE, None),
];

/// The `E_DEPRECATED` text for reading a deprecated constant.
#[must_use]
pub fn constant_deprecation_message(name: &str) -> Option<String> {
    CONSTANTS
        .iter()
        .find(|(n, _, _)| *n == name)
        .and_then(|(n, _, d)| {
            d.map(|(since, msg)| format!("Constant {n} is deprecated since {since}, {msg}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_and_ids() {
        assert_eq!(filter_list().len(), 21);
        assert_eq!(filter_id(b"int"), Some(FILTER_VALIDATE_INT));
        assert_eq!(filter_id(b"stripped"), Some(FILTER_SANITIZE_STRING));
        assert_eq!(filter_id(b"INT"), None);
        assert_eq!(filter_id(b"int\0x"), Some(FILTER_VALIDATE_INT));
        assert_eq!(find_filter(FILTER_VALIDATE_BOOL).name, "boolean");
        assert_eq!(find_filter(9999).name, "unsafe_raw");
        assert_eq!(find_filter(0x0104).kind, FilterKind::UnsafeRaw);
        assert!(filter_id_exists(0x0104));
        assert!(!filter_id_exists(0x0116));
        assert!(!filter_id_exists(0));
        assert!(filter_id_exists(FILTER_CALLBACK));
    }

    #[test]
    fn deprecated_constants() {
        assert_eq!(
            constant_deprecation_message("FILTER_SANITIZE_STRING").as_deref(),
            Some("Constant FILTER_SANITIZE_STRING is deprecated since 8.1, use htmlspecialchars() instead")
        );
        assert_eq!(constant_deprecation_message("FILTER_DEFAULT"), None);
    }
}
