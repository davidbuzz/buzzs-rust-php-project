//! Shift-JIS family: `SJIS`, `SJIS-mac`, `SJIS-Mobile#*`, `CP932`, `SJIS-win`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c`.
//!
//! STUB: codecs are placeholders until the port lands.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};

/// `SJIS`.
pub static ENC_SJIS: Encoding = Encoding {
    no: EncodingNo::Sjis,
    name: "SJIS",
    mime_name: Some("Shift_JIS"),
    aliases: &["x-sjis", "SHIFT-JIS"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `SJIS-mac`.
pub static ENC_SJIS_MAC: Encoding = Encoding {
    no: EncodingNo::SjisMac,
    name: "SJIS-mac",
    mime_name: Some("Shift_JIS"),
    aliases: &["MacJapanese", "x-Mac-Japanese"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `SJIS-Mobile#DOCOMO`.
pub static ENC_SJIS_DOCOMO: Encoding = Encoding {
    no: EncodingNo::SjisDocomo,
    name: "SJIS-Mobile#DOCOMO",
    mime_name: Some("Shift_JIS"),
    aliases: &["SJIS-DOCOMO", "shift_jis-imode", "x-sjis-emoji-docomo"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `SJIS-Mobile#KDDI`.
pub static ENC_SJIS_KDDI: Encoding = Encoding {
    no: EncodingNo::SjisKddi,
    name: "SJIS-Mobile#KDDI",
    mime_name: Some("Shift_JIS"),
    aliases: &["SJIS-KDDI", "shift_jis-kddi", "x-sjis-emoji-kddi"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `SJIS-Mobile#SOFTBANK`.
pub static ENC_SJIS_SB: Encoding = Encoding {
    no: EncodingNo::SjisSb,
    name: "SJIS-Mobile#SOFTBANK",
    mime_name: Some("Shift_JIS"),
    aliases: &[
        "SJIS-SOFTBANK",
        "shift_jis-softbank",
        "x-sjis-emoji-softbank",
    ],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `CP932`.
pub static ENC_CP932: Encoding = Encoding {
    no: EncodingNo::Cp932,
    name: "CP932",
    mime_name: Some("Shift_JIS"),
    aliases: &["MS932", "Windows-31J", "MS_Kanji"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `SJIS-win`.
pub static ENC_SJISWIN: Encoding = Encoding {
    no: EncodingNo::SjisWin,
    name: "SJIS-win",
    mime_name: Some("Shift_JIS"),
    aliases: &["SJIS-ms", "SJIS-open"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
