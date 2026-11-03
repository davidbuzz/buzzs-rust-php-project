//! `Quoted-Printable` (stub).

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE, ENCTYPE_SBCS};

/// `Quoted-Printable`.
pub static ENC_QPRINT: Encoding = Encoding {
    no: EncodingNo::Qprint,
    name: "Quoted-Printable",
    mime_name: Some("Quoted-Printable"),
    aliases: &["qprint"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
