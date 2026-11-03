//! `CP50220`, `CP50221`, `CP50222`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c`.
//!
//! STUB: codecs are placeholders until the port lands.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};

/// `CP50220`.
pub static ENC_CP50220: Encoding = Encoding {
    no: EncodingNo::Cp50220,
    name: "CP50220",
    mime_name: Some("ISO-2022-JP"),
    aliases: &["cp50220raw", "cp50220-raw", "JIS-ms"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `CP50221`.
pub static ENC_CP50221: Encoding = Encoding {
    no: EncodingNo::Cp50221,
    name: "CP50221",
    mime_name: Some("ISO-2022-JP"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `CP50222`.
pub static ENC_CP50222: Encoding = Encoding {
    no: EncodingNo::Cp50222,
    name: "CP50222",
    mime_name: Some("ISO-2022-JP"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
