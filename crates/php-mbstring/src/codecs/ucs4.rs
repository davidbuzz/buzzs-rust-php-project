//! Stub.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE, ENCTYPE_WCS2, ENCTYPE_WCS4};

/// `UCS-4`.
pub static ENC_UCS4: Encoding = Encoding {
    no: EncodingNo::Ucs4,
    name: "UCS-4",
    mime_name: Some("UCS-4"),
    aliases: &["ISO-10646-UCS-4", "UCS4"],
    mblen_table: None,
    flag: ENCTYPE_WCS4,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `UCS-4BE`.
pub static ENC_UCS4BE: Encoding = Encoding {
    no: EncodingNo::Ucs4be,
    name: "UCS-4BE",
    mime_name: Some("UCS-4BE"),
    aliases: &["byte4be"],
    mblen_table: None,
    flag: ENCTYPE_WCS4,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `UCS-4LE`.
pub static ENC_UCS4LE: Encoding = Encoding {
    no: EncodingNo::Ucs4le,
    name: "UCS-4LE",
    mime_name: Some("UCS-4LE"),
    aliases: &["byte4le"],
    mblen_table: None,
    flag: ENCTYPE_WCS4,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
