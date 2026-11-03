//! Stub.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE, ENCTYPE_WCS2, ENCTYPE_WCS4};

/// `UCS-2`.
pub static ENC_UCS2: Encoding = Encoding {
    no: EncodingNo::Ucs2,
    name: "UCS-2",
    mime_name: Some("UCS-2"),
    aliases: &["ISO-10646-UCS-2", "UCS2", "UNICODE"],
    mblen_table: None,
    flag: ENCTYPE_WCS2,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `UCS-2BE`.
pub static ENC_UCS2BE: Encoding = Encoding {
    no: EncodingNo::Ucs2be,
    name: "UCS-2BE",
    mime_name: Some("UCS-2BE"),
    aliases: &["byte2be"],
    mblen_table: None,
    flag: ENCTYPE_WCS2,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `UCS-2LE`.
pub static ENC_UCS2LE: Encoding = Encoding {
    no: EncodingNo::Ucs2le,
    name: "UCS-2LE",
    mime_name: Some("UCS-2LE"),
    aliases: &["byte2le"],
    mblen_table: None,
    flag: ENCTYPE_WCS2,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
