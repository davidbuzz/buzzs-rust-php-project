//! `UUENCODE` (stub).

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE, ENCTYPE_SBCS};

/// `UUENCODE`.
pub static ENC_UUENCODE: Encoding = Encoding {
    no: EncodingNo::Uuencode,
    name: "UUENCODE",
    mime_name: Some("x-uuencode"),
    aliases: &[],
    mblen_table: None,
    flag: ENCTYPE_SBCS,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
