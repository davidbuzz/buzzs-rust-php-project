//! Stub.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE, ENCTYPE_WCS2, ENCTYPE_WCS4};

/// `UTF-16`.
pub static ENC_UTF16: Encoding = Encoding {
    no: EncodingNo::Utf16,
    name: "UTF-16",
    mime_name: Some("UTF-16"),
    aliases: &["utf16"],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `UTF-16BE`.
pub static ENC_UTF16BE: Encoding = Encoding {
    no: EncodingNo::Utf16be,
    name: "UTF-16BE",
    mime_name: Some("UTF-16BE"),
    aliases: &[],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `UTF-16LE`.
pub static ENC_UTF16LE: Encoding = Encoding {
    no: EncodingNo::Utf16le,
    name: "UTF-16LE",
    mime_name: Some("UTF-16LE"),
    aliases: &[],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
