//! `EUC-CN`, `EUC-TW`, `EUC-KR`, `UHC`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c`.
//!
//! STUB: codecs are placeholders until the port lands.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};

/// `EUC-CN`.
pub static ENC_EUC_CN: Encoding = Encoding {
    no: EncodingNo::EucCn,
    name: "EUC-CN",
    mime_name: Some("CN-GB"),
    aliases: &["CN-GB", "EUC_CN", "eucCN", "x-euc-cn", "gb2312"],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `EUC-TW`.
pub static ENC_EUC_TW: Encoding = Encoding {
    no: EncodingNo::EucTw,
    name: "EUC-TW",
    mime_name: Some("EUC-TW"),
    aliases: &["EUC_TW", "eucTW", "x-euc-tw"],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `EUC-KR`.
pub static ENC_EUC_KR: Encoding = Encoding {
    no: EncodingNo::EucKr,
    name: "EUC-KR",
    mime_name: Some("EUC-KR"),
    aliases: &["EUC_KR", "eucKR", "x-euc-kr"],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `UHC`.
pub static ENC_UHC: Encoding = Encoding {
    no: EncodingNo::Uhc,
    name: "UHC",
    mime_name: Some("UHC"),
    aliases: &["CP949"],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
