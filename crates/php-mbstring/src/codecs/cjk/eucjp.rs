//! `EUC-JP`, `eucJP-win`, `CP51932`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c`.
//!
//! STUB: codecs are placeholders until the port lands.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};

/// `EUC-JP`.
pub static ENC_EUC_JP: Encoding = Encoding {
    no: EncodingNo::EucJp,
    name: "EUC-JP",
    mime_name: Some("EUC-JP"),
    aliases: &["EUC", "EUC_JP", "eucJP", "x-euc-jp"],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `eucJP-win`.
pub static ENC_EUCJP_WIN: Encoding = Encoding {
    no: EncodingNo::EucJpWin,
    name: "eucJP-win",
    mime_name: Some("EUC-JP"),
    aliases: &["eucJP-open", "eucJP-ms"],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `CP51932`.
pub static ENC_CP51932: Encoding = Encoding {
    no: EncodingNo::Cp51932,
    name: "CP51932",
    mime_name: Some("CP51932"),
    aliases: &["cp51932"],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
