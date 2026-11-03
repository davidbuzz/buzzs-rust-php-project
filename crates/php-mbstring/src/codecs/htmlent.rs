//! `HTML-ENTITIES` (stub).

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE, ENCTYPE_SBCS};

/// `HTML-ENTITIES`.
pub static ENC_HTML_ENT: Encoding = Encoding {
    no: EncodingNo::HtmlEnt,
    name: "HTML-ENTITIES",
    mime_name: Some("HTML-ENTITIES"),
    aliases: &["HTML", "html"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
