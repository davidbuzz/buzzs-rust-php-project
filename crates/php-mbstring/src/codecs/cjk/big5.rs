//! `BIG-5`, `CP950`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c`.
//!
//! STUB: codecs are placeholders until the port lands.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};

/// `BIG-5`.
pub static ENC_BIG5: Encoding = Encoding {
    no: EncodingNo::Big5,
    name: "BIG-5",
    mime_name: Some("BIG5"),
    aliases: &["CN-BIG5", "BIG-FIVE", "BIGFIVE"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `CP950`.
pub static ENC_CP950: Encoding = Encoding {
    no: EncodingNo::Cp950,
    name: "CP950",
    mime_name: Some("BIG5"),
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
