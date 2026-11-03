//! JIS X 0213:2004 encodings: `ISO-2022-JP-2004`, `EUC-JP-2004`, `SJIS-2004`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c`.
//!
//! STUB: codecs are placeholders until the port lands.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};

/// `ISO-2022-JP-2004`.
pub static ENC_2022JP_2004: Encoding = Encoding {
    no: EncodingNo::Iso2022Jp2004,
    name: "ISO-2022-JP-2004",
    mime_name: Some("ISO-2022-JP-2004"),
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

/// `EUC-JP-2004`.
pub static ENC_EUCJP2004: Encoding = Encoding {
    no: EncodingNo::EucJp2004,
    name: "EUC-JP-2004",
    mime_name: Some("EUC-JP"),
    aliases: &["EUC_JP-2004"],
    mblen_table: None,
    flag: 0,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `SJIS-2004`.
pub static ENC_SJIS2004: Encoding = Encoding {
    no: EncodingNo::Sjis2004,
    name: "SJIS-2004",
    mime_name: Some("Shift_JIS"),
    aliases: &["SJIS2004", "Shift_JIS-2004"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
