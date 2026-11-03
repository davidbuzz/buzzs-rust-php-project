//! Stub.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE, ENCTYPE_WCS2, ENCTYPE_WCS4};

/// `UTF-32`.
pub static ENC_UTF32: Encoding = Encoding {
    no: EncodingNo::Utf32,
    name: "UTF-32",
    mime_name: Some("UTF-32"),
    aliases: &["utf32"],
    mblen_table: None,
    flag: ENCTYPE_WCS4,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `UTF-32BE`.
pub static ENC_UTF32BE: Encoding = Encoding {
    no: EncodingNo::Utf32be,
    name: "UTF-32BE",
    mime_name: Some("UTF-32BE"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_WCS4,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `UTF-32LE`.
pub static ENC_UTF32LE: Encoding = Encoding {
    no: EncodingNo::Utf32le,
    name: "UTF-32LE",
    mime_name: Some("UTF-32LE"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_WCS4,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
