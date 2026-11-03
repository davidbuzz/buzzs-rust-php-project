//! The `LIBXML_*` constants registered by `ext/libxml` (values from the
//! linked libxml2 headers).

use crate::ffi;

/// `LIBXML_VERSION` (compiled-against version, 2.9.14).
pub const LIBXML_VERSION: i64 = 20914;
/// `LIBXML_DOTTED_VERSION`.
pub const LIBXML_DOTTED_VERSION: &str = "2.9.14";

/// `LIBXML_RECOVER`.
pub const LIBXML_RECOVER: i32 = ffi::XML_PARSE_RECOVER;
/// `LIBXML_NOENT`.
pub const LIBXML_NOENT: i32 = ffi::XML_PARSE_NOENT;
/// `LIBXML_DTDLOAD`.
pub const LIBXML_DTDLOAD: i32 = ffi::XML_PARSE_DTDLOAD;
/// `LIBXML_DTDATTR`.
pub const LIBXML_DTDATTR: i32 = ffi::XML_PARSE_DTDATTR;
/// `LIBXML_DTDVALID`.
pub const LIBXML_DTDVALID: i32 = ffi::XML_PARSE_DTDVALID;
/// `LIBXML_NOERROR`.
pub const LIBXML_NOERROR: i32 = ffi::XML_PARSE_NOERROR;
/// `LIBXML_NOWARNING`.
pub const LIBXML_NOWARNING: i32 = ffi::XML_PARSE_NOWARNING;
/// `LIBXML_NOBLANKS`.
pub const LIBXML_NOBLANKS: i32 = ffi::XML_PARSE_NOBLANKS;
/// `LIBXML_XINCLUDE`.
pub const LIBXML_XINCLUDE: i32 = ffi::XML_PARSE_XINCLUDE;
/// `LIBXML_NSCLEAN`.
pub const LIBXML_NSCLEAN: i32 = ffi::XML_PARSE_NSCLEAN;
/// `LIBXML_NOCDATA`.
pub const LIBXML_NOCDATA: i32 = ffi::XML_PARSE_NOCDATA;
/// `LIBXML_NONET`.
pub const LIBXML_NONET: i32 = ffi::XML_PARSE_NONET;
/// `LIBXML_PEDANTIC`.
pub const LIBXML_PEDANTIC: i32 = ffi::XML_PARSE_PEDANTIC;
/// `LIBXML_COMPACT`.
pub const LIBXML_COMPACT: i32 = ffi::XML_PARSE_COMPACT;
/// `LIBXML_NOXMLDECL` (`XML_SAVE_NO_DECL`).
pub const LIBXML_NOXMLDECL: i32 = ffi::XML_SAVE_NO_DECL;
/// `LIBXML_PARSEHUGE`.
pub const LIBXML_PARSEHUGE: i32 = ffi::XML_PARSE_HUGE;
/// `LIBXML_BIGLINES`.
pub const LIBXML_BIGLINES: i32 = ffi::XML_PARSE_BIG_LINES;
/// `LIBXML_NOEMPTYTAG` (`LIBXML_SAVE_NOEMPTYTAG`, `1 << 2`).
pub const LIBXML_NOEMPTYTAG: i32 = 1 << 2;
/// `LIBXML_SCHEMA_CREATE`.
pub const LIBXML_SCHEMA_CREATE: i32 = ffi::XML_SCHEMA_VAL_VC_I_CREATE;
/// `LIBXML_HTML_NOIMPLIED`.
pub const LIBXML_HTML_NOIMPLIED: i32 = ffi::HTML_PARSE_NOIMPLIED;
/// `LIBXML_HTML_NODEFDTD`.
pub const LIBXML_HTML_NODEFDTD: i32 = ffi::HTML_PARSE_NODEFDTD;
/// `LIBXML_ERR_NONE`.
pub const LIBXML_ERR_NONE: i32 = ffi::XML_ERR_NONE;
/// `LIBXML_ERR_WARNING`.
pub const LIBXML_ERR_WARNING: i32 = ffi::XML_ERR_WARNING;
/// `LIBXML_ERR_ERROR`.
pub const LIBXML_ERR_ERROR: i32 = ffi::XML_ERR_ERROR;
/// `LIBXML_ERR_FATAL`.
pub const LIBXML_ERR_FATAL: i32 = ffi::XML_ERR_FATAL;

/// A constant value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstValue {
    /// An `int` constant.
    Int(i64),
    /// A `string` constant.
    Str(String),
}

/// All `LIBXML_*` constants in registration order (`LIBXML_NO_XXE` only
/// exists with libxml2 >= 2.13 and is therefore absent).
pub fn all() -> Vec<(&'static str, ConstValue)> {
    let ints: [(&'static str, i32); 24] = [
        ("LIBXML_RECOVER", LIBXML_RECOVER),
        ("LIBXML_NOENT", LIBXML_NOENT),
        ("LIBXML_DTDLOAD", LIBXML_DTDLOAD),
        ("LIBXML_DTDATTR", LIBXML_DTDATTR),
        ("LIBXML_DTDVALID", LIBXML_DTDVALID),
        ("LIBXML_NOERROR", LIBXML_NOERROR),
        ("LIBXML_NOWARNING", LIBXML_NOWARNING),
        ("LIBXML_NOBLANKS", LIBXML_NOBLANKS),
        ("LIBXML_XINCLUDE", LIBXML_XINCLUDE),
        ("LIBXML_NSCLEAN", LIBXML_NSCLEAN),
        ("LIBXML_NOCDATA", LIBXML_NOCDATA),
        ("LIBXML_NONET", LIBXML_NONET),
        ("LIBXML_PEDANTIC", LIBXML_PEDANTIC),
        ("LIBXML_COMPACT", LIBXML_COMPACT),
        ("LIBXML_NOXMLDECL", LIBXML_NOXMLDECL),
        ("LIBXML_PARSEHUGE", LIBXML_PARSEHUGE),
        ("LIBXML_BIGLINES", LIBXML_BIGLINES),
        ("LIBXML_NOEMPTYTAG", LIBXML_NOEMPTYTAG),
        ("LIBXML_SCHEMA_CREATE", LIBXML_SCHEMA_CREATE),
        ("LIBXML_HTML_NOIMPLIED", LIBXML_HTML_NOIMPLIED),
        ("LIBXML_HTML_NODEFDTD", LIBXML_HTML_NODEFDTD),
        ("LIBXML_ERR_NONE", LIBXML_ERR_NONE),
        ("LIBXML_ERR_WARNING", LIBXML_ERR_WARNING),
        ("LIBXML_ERR_ERROR", LIBXML_ERR_ERROR),
    ];
    let mut out = vec![
        ("LIBXML_VERSION", ConstValue::Int(LIBXML_VERSION)),
        (
            "LIBXML_DOTTED_VERSION",
            ConstValue::Str(LIBXML_DOTTED_VERSION.to_owned()),
        ),
        (
            "LIBXML_LOADED_VERSION",
            ConstValue::Str(crate::raw::loaded_version()),
        ),
    ];
    out.extend(
        ints.iter()
            .map(|&(n, v)| (n, ConstValue::Int(i64::from(v)))),
    );
    out.push((
        "LIBXML_ERR_FATAL",
        ConstValue::Int(i64::from(LIBXML_ERR_FATAL)),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_match_php() {
        // Values as printed by PHP 8 built against libxml2 2.9.14.
        assert_eq!(LIBXML_NOENT, 2);
        assert_eq!(LIBXML_PARSEHUGE, 524_288);
        assert_eq!(LIBXML_BIGLINES, 4_194_304);
        assert_eq!(LIBXML_NOXMLDECL, 2);
        assert_eq!(LIBXML_HTML_NOIMPLIED, 8192);
        assert_eq!(LIBXML_HTML_NODEFDTD, 4);
        assert_eq!(LIBXML_NOEMPTYTAG, 4);
        let all = all();
        assert_eq!(all.len(), 28);
        assert_eq!(
            all[2],
            ("LIBXML_LOADED_VERSION", ConstValue::Str("20914".to_owned()))
        );
    }
}
