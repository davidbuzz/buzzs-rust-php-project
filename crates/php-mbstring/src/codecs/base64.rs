//! `BASE64` (stub).

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE, ENCTYPE_SBCS};

/// `BASE64`.
pub static ENC_BASE64: Encoding = Encoding {
    no: EncodingNo::Base64,
    name: "BASE64",
    mime_name: Some("BASE64"),
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
