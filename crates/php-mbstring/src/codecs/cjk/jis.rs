//! ISO-2022-JP family: `JIS`, `ISO-2022-JP`, `ISO-2022-JP-MOBILE#KDDI`, `ISO-2022-JP-MS`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c`.
//!
//! STUB: codecs are placeholders until the port lands.

use crate::codecs::{stub_from_wchar, stub_to_wchar};
#[allow(unused_imports)]
use crate::encoding::{Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};

/// `JIS`.
pub static ENC_JIS: Encoding = Encoding {
    no: EncodingNo::Jis,
    name: "JIS",
    mime_name: Some("ISO-2022-JP"),
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

/// `ISO-2022-JP`.
pub static ENC_2022JP: Encoding = Encoding {
    no: EncodingNo::Iso2022Jp,
    name: "ISO-2022-JP",
    mime_name: Some("ISO-2022-JP"),
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

/// `ISO-2022-JP-MOBILE#KDDI`.
pub static ENC_2022JP_KDDI: Encoding = Encoding {
    no: EncodingNo::Iso2022JpKddi,
    name: "ISO-2022-JP-MOBILE#KDDI",
    mime_name: Some("ISO-2022-JP"),
    aliases: &["ISO-2022-JP-KDDI"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};

/// `ISO-2022-JP-MS`.
pub static ENC_2022JPMS: Encoding = Encoding {
    no: EncodingNo::Iso2022JpMs,
    name: "ISO-2022-JP-MS",
    mime_name: Some("ISO-2022-JP"),
    aliases: &["ISO2022JPMS"],
    mblen_table: None,
    flag: ENCTYPE_GL_UNSAFE,
    input_filter: None,
    output_filter: None,
    to_wchar: stub_to_wchar,
    from_wchar: stub_from_wchar,
    check: None,
    cut: None,
};
