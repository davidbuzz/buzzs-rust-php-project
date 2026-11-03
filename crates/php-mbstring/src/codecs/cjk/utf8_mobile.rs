//! Japanese mobile UTF-8 variants (`mbfilter_utf8.c`).
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c`.
//!
//! STUB: codecs are placeholders until the port lands.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};

/// `UTF-8-Mobile#DOCOMO`.
pub static ENC_UTF8_DOCOMO: Encoding = Encoding {
    no: EncodingNo::Utf8Docomo,
    name: "UTF-8-Mobile#DOCOMO",
    mime_name: Some("UTF-8"),
    aliases: &["UTF-8-DOCOMO", "UTF8-DOCOMO"],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `UTF-8-Mobile#KDDI-A`.
pub static ENC_UTF8_KDDI_A: Encoding = Encoding {
    no: EncodingNo::Utf8KddiA,
    name: "UTF-8-Mobile#KDDI-A",
    mime_name: Some("UTF-8"),
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

/// `UTF-8-Mobile#KDDI-B`.
pub static ENC_UTF8_KDDI_B: Encoding = Encoding {
    no: EncodingNo::Utf8KddiB,
    name: "UTF-8-Mobile#KDDI-B",
    mime_name: Some("UTF-8"),
    aliases: &["UTF-8-Mobile#KDDI", "UTF-8-KDDI", "UTF8-KDDI"],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `UTF-8-Mobile#SOFTBANK`.
pub static ENC_UTF8_SB: Encoding = Encoding {
    no: EncodingNo::Utf8Sb,
    name: "UTF-8-Mobile#SOFTBANK",
    mime_name: Some("UTF-8"),
    aliases: &["UTF-8-SOFTBANK", "UTF8-SOFTBANK"],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
