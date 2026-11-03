//! `GB18030`, `GB18030-2022`, `CP936`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c`.
//!
//! STUB: codecs are placeholders until the port lands.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};

/// `GB18030`.
pub static ENC_GB18030: Encoding = Encoding {
    no: EncodingNo::Gb18030,
    name: "GB18030",
    mime_name: Some("GB18030"),
    aliases: &["gb-18030", "gb-18030-2000"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `GB18030-2022`.
pub static ENC_GB18030_2022: Encoding = Encoding {
    no: EncodingNo::Gb18030_2022,
    name: "GB18030-2022",
    mime_name: Some("GB18030-2022"),
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

/// `CP936`.
pub static ENC_CP936: Encoding = Encoding {
    no: EncodingNo::Cp936,
    name: "CP936",
    mime_name: Some("CP936"),
    aliases: &["CP-936", "GBK"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
