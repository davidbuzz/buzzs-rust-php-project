//! Stub.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE, ENCTYPE_WCS2, ENCTYPE_WCS4};

/// `UTF-7`.
pub static ENC_UTF7: Encoding = Encoding {
    no: EncodingNo::Utf7,
    name: "UTF-7",
    mime_name: Some("UTF-7"),
    aliases: &["utf7"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
