//! The mysqlnd character-set table and the charset-aware escaping
//! routines behind `mysqli_real_escape_string` (ref: `mysqlnd_charset.c`).

/// Which "multibyte character length from lead byte" function a charset uses.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharLen {
    Null,
    Big5,
    Ujis,
    Sjis,
    Euckr,
    Gb2312,
    Gbk,
    Utf8mb3,
    Ucs2,
    Eucjpms,
    Utf8,
    Utf16,
    Utf32,
    Cp932,
    Gb18030,
}

/// Which "is this a valid multibyte sequence" function a charset uses.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MbValid {
    Null,
    Big5,
    Ujis,
    Sjis,
    Euckr,
    Gb2312,
    Gbk,
    Utf8mb3,
    Ucs2,
    Eucjpms,
    Utf8,
    Utf16,
    Utf32,
    Cp932,
    Gb18030,
}

/// One entry of `mysqlnd_charsets[]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Charset {
    /// Collation number as used on the wire.
    pub nr: u32,
    /// Character-set name (`utf8mb4`, `latin1`, ...).
    pub name: &'static str,
    /// Collation name.
    pub collation: &'static str,
    /// Minimum bytes per character.
    pub char_minlen: u32,
    /// Maximum bytes per character.
    pub char_maxlen: u32,
    /// Lead-byte length function.
    pub mb_charlen: CharLen,
    /// Sequence validity function.
    pub mb_valid: MbValid,
    /// Bytes below this value are always single-byte characters.
    pub lowest_mb_byte: u32,
}

#[allow(clippy::too_many_arguments)]
const fn cs(
    nr: u32,
    name: &'static str,
    collation: &'static str,
    char_minlen: u32,
    char_maxlen: u32,
    mb_charlen: CharLen,
    mb_valid: MbValid,
    lowest_mb_byte: u32,
) -> Charset {
    Charset { nr, name, collation, char_minlen, char_maxlen, mb_charlen, mb_valid, lowest_mb_byte }
}

/// `MYSQLND_UTF8_MB4_DEFAULT_ID`
pub const UTF8_MB4_DEFAULT_ID: u32 = 45;
/// `MYSQLND_UTF8_MB3_DEFAULT_ID`
pub const UTF8_MB3_DEFAULT_ID: u32 = 33;

/// The full mysqlnd charset table, in source order (lookups return the
/// first match, so order is significant for name lookups).
pub static CHARSETS: &[Charset] = &[
    cs(1, "big5", "big5_chinese_ci", 1, 2, CharLen::Big5, MbValid::Big5, 0xA1),
    cs(3, "dec8", "dec8_swedish_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(4, "cp850", "cp850_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(6, "hp8", "hp8_english_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(7, "koi8r", "koi8r_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(8, "latin1", "latin1_swedish_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(5, "latin1", "latin1_german1_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(9, "latin2", "latin2_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(2, "latin2", "latin2_czech_cs", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(10, "swe7", "swe7_swedish_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(11, "ascii", "ascii_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(12, "ujis", "ujis_japanese_ci", 1, 3, CharLen::Ujis, MbValid::Ujis, 0x80),
    cs(13, "sjis", "sjis_japanese_ci", 1, 2, CharLen::Sjis, MbValid::Sjis, 0x80),
    cs(16, "hebrew", "hebrew_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(17, "filename", "filename", 1, 5, CharLen::Null, MbValid::Null, 0x100),
    cs(18, "tis620", "tis620_thai_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(19, "euckr", "euckr_korean_ci", 1, 2, CharLen::Euckr, MbValid::Euckr, 0x80),
    cs(21, "latin2", "latin2_hungarian_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(27, "latin2", "latin2_croatian_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(22, "koi8u", "koi8u_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(24, "gb2312", "gb2312_chinese_ci", 1, 2, CharLen::Gb2312, MbValid::Gb2312, 0xA1),
    cs(25, "greek", "greek_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(26, "cp1250", "cp1250_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(28, "gbk", "gbk_chinese_ci", 1, 2, CharLen::Gbk, MbValid::Gbk, 0x80),
    cs(30, "latin5", "latin5_turkish_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(31, "latin1", "latin1_german2_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(15, "latin1", "latin1_danish_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(32, "armscii8", "armscii8_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(33, "utf8", "utf8_general_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(35, "ucs2", "ucs2_general_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(36, "cp866", "cp866_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(37, "keybcs2", "keybcs2_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(38, "macce", "macce_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(39, "macroman", "macroman_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(40, "cp852", "cp852_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(41, "latin7", "latin7_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(20, "latin7", "latin7_estonian_cs", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(57, "cp1256", "cp1256_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(59, "cp1257", "cp1257_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(63, "binary", "binary", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(97, "eucjpms", "eucjpms_japanese_ci", 1, 3, CharLen::Eucjpms, MbValid::Eucjpms, 0x80),
    cs(29, "cp1257", "cp1257_lithuanian_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(31, "latin1", "latin1_german2_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(34, "cp1250", "cp1250_czech_cs", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(42, "latin7", "latin7_general_cs", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(43, "macce", "macce_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(44, "cp1250", "cp1250_croatian_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(45, "utf8mb4", "utf8mb4_general_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(46, "utf8mb4", "utf8mb4_bin", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(47, "latin1", "latin1_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(48, "latin1", "latin1_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(49, "latin1", "latin1_general_cs", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(51, "cp1251", "cp1251_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(14, "cp1251", "cp1251_bulgarian_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(23, "cp1251", "cp1251_ukrainian_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(50, "cp1251", "cp1251_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(52, "cp1251", "cp1251_general_cs", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(53, "macroman", "macroman_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(54, "utf16", "utf16_general_ci", 2, 4, CharLen::Utf16, MbValid::Utf16, 0x00),
    cs(55, "utf16", "utf16_bin", 2, 4, CharLen::Utf16, MbValid::Utf16, 0x00),
    cs(56, "utf16le", "utf16le_general_ci", 2, 4, CharLen::Utf16, MbValid::Utf16, 0x00),
    cs(58, "cp1257", "cp1257_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(60, "utf32", "utf32_general_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(61, "utf32", "utf32_bin", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(62, "utf16le", "utf16le_bin", 2, 4, CharLen::Utf16, MbValid::Utf16, 0x00),
    cs(64, "armscii8", "armscii8_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(65, "ascii", "ascii_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(66, "cp1250", "cp1250_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(67, "cp1256", "cp1256_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(68, "cp866", "cp866_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(69, "dec8", "dec8_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(70, "greek", "greek_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(71, "hebrew", "hebrew_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(72, "hp8", "hp8_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(73, "keybcs2", "keybcs2_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(74, "koi8r", "koi8r_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(75, "koi8u", "koi8u_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(77, "latin2", "latin2_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(78, "latin5", "latin5_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(79, "latin7", "latin7_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(80, "cp850", "cp850_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(81, "cp852", "cp852_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(82, "swe7", "swe7_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(83, "utf8", "utf8_bin", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(84, "big5", "big5_bin", 1, 2, CharLen::Big5, MbValid::Big5, 0xA1),
    cs(85, "euckr", "euckr_bin", 1, 2, CharLen::Euckr, MbValid::Euckr, 0x80),
    cs(86, "gb2312", "gb2312_bin", 1, 2, CharLen::Gb2312, MbValid::Gb2312, 0xA1),
    cs(87, "gbk", "gbk_bin", 1, 2, CharLen::Gbk, MbValid::Gbk, 0x80),
    cs(88, "sjis", "sjis_bin", 1, 2, CharLen::Sjis, MbValid::Sjis, 0x80),
    cs(89, "tis620", "tis620_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(90, "ucs2", "ucs2_bin", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(91, "ujis", "ujis_bin", 1, 3, CharLen::Ujis, MbValid::Ujis, 0x80),
    cs(92, "geostd8", "geostd8_general_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(93, "geostd8", "geostd8_bin", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(94, "latin1", "latin1_spanish_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(95, "cp932", "cp932_japanese_ci", 1, 2, CharLen::Cp932, MbValid::Cp932, 0x80),
    cs(96, "cp932", "cp932_bin", 1, 2, CharLen::Cp932, MbValid::Cp932, 0x80),
    cs(97, "eucjpms", "eucjpms_japanese_ci", 1, 3, CharLen::Eucjpms, MbValid::Eucjpms, 0x80),
    cs(98, "eucjpms", "eucjpms_bin", 1, 3, CharLen::Eucjpms, MbValid::Eucjpms, 0x80),
    cs(99, "cp1250", "cp1250_polish_ci", 1, 1, CharLen::Null, MbValid::Null, 0x100),
    cs(128, "ucs2", "ucs2_unicode_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(129, "ucs2", "ucs2_icelandic_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(130, "ucs2", "ucs2_latvian_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(131, "ucs2", "ucs2_romanian_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(132, "ucs2", "ucs2_slovenian_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(133, "ucs2", "ucs2_polish_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(134, "ucs2", "ucs2_estonian_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(135, "ucs2", "ucs2_spanish_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(136, "ucs2", "ucs2_swedish_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(137, "ucs2", "ucs2_turkish_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(138, "ucs2", "ucs2_czech_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(139, "ucs2", "ucs2_danish_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(140, "ucs2", "ucs2_lithuanian_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(141, "ucs2", "ucs2_slovak_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(142, "ucs2", "ucs2_spanish2_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(143, "ucs2", "ucs2_roman_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(144, "ucs2", "ucs2_persian_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(145, "ucs2", "ucs2_esperanto_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(146, "ucs2", "ucs2_hungarian_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(147, "ucs2", "ucs2_sinhala_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(148, "ucs2", "ucs2_german2_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(149, "ucs2", "ucs2_croatian_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(150, "ucs2", "ucs2_unicode_520_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(151, "ucs2", "ucs2_vietnamese_ci", 2, 2, CharLen::Ucs2, MbValid::Ucs2, 0x00),
    cs(160, "utf32", "utf32_unicode_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(161, "utf32", "utf32_icelandic_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(162, "utf32", "utf32_latvian_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(163, "utf32", "utf32_romanian_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(164, "utf32", "utf32_slovenian_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(165, "utf32", "utf32_polish_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(166, "utf32", "utf32_estonian_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(167, "utf32", "utf32_spanish_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(168, "utf32", "utf32_swedish_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(169, "utf32", "utf32_turkish_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(170, "utf32", "utf32_czech_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(171, "utf32", "utf32_danish_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(172, "utf32", "utf32_lithuanian_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(173, "utf32", "utf32_slovak_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(174, "utf32", "utf32_spanish2_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(175, "utf32", "utf32_roman_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(176, "utf32", "utf32_persian_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(177, "utf32", "utf32_esperanto_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(178, "utf32", "utf32_hungarian_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(179, "utf32", "utf32_sinhala_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(180, "utf32", "utf32_german2_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(181, "utf32", "utf32_croatian_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(182, "utf32", "utf32_unicode_520_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(183, "utf32", "utf32_vietnamese_ci", 4, 4, CharLen::Utf32, MbValid::Utf32, 0x00),
    cs(192, "utf8", "utf8_unicode_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(193, "utf8", "utf8_icelandic_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(194, "utf8", "utf8_latvian_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(195, "utf8", "utf8_romanian_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(196, "utf8", "utf8_slovenian_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(197, "utf8", "utf8_polish_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(198, "utf8", "utf8_estonian_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(199, "utf8", "utf8_spanish_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(200, "utf8", "utf8_swedish_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(201, "utf8", "utf8_turkish_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(202, "utf8", "utf8_czech_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(203, "utf8", "utf8_danish_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(204, "utf8", "utf8_lithuanian_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(205, "utf8", "utf8_slovak_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(206, "utf8", "utf8_spanish2_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(207, "utf8", "utf8_roman_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(208, "utf8", "utf8_persian_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(209, "utf8", "utf8_esperanto_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(210, "utf8", "utf8_hungarian_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(211, "utf8", "utf8_sinhala_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(212, "utf8", "utf8_german2_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(213, "utf8", "utf8_croatian_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(214, "utf8", "utf8_unicode_520_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(215, "utf8", "utf8_vietnamese_ci", 1, 3, CharLen::Utf8mb3, MbValid::Utf8mb3, 0x80),
    cs(224, "utf8mb4", "utf8mb4_unicode_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(225, "utf8mb4", "utf8mb4_icelandic_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(226, "utf8mb4", "utf8mb4_latvian_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(227, "utf8mb4", "utf8mb4_romanian_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(228, "utf8mb4", "utf8mb4_slovenian_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(229, "utf8mb4", "utf8mb4_polish_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(230, "utf8mb4", "utf8mb4_estonian_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(231, "utf8mb4", "utf8mb4_spanish_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(232, "utf8mb4", "utf8mb4_swedish_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(233, "utf8mb4", "utf8mb4_turkish_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(234, "utf8mb4", "utf8mb4_czech_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(235, "utf8mb4", "utf8mb4_danish_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(236, "utf8mb4", "utf8mb4_lithuanian_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(237, "utf8mb4", "utf8mb4_slovak_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(238, "utf8mb4", "utf8mb4_spanish2_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(239, "utf8mb4", "utf8mb4_roman_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(240, "utf8mb4", "utf8mb4_persian_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(241, "utf8mb4", "utf8mb4_esperanto_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(242, "utf8mb4", "utf8mb4_hungarian_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(243, "utf8mb4", "utf8mb4_sinhala_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(244, "utf8mb4", "utf8mb4_german2_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(245, "utf8mb4", "utf8mb4_croatian_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(246, "utf8mb4", "utf8mb4_unicode_520_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(247, "utf8mb4", "utf8mb4_vietnamese_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(248, "gb18030", "gb18030_chinese_ci", 1, 4, CharLen::Gb18030, MbValid::Gb18030, 0x00),
    cs(249, "gb18030", "gb18030_bin", 1, 4, CharLen::Gb18030, MbValid::Gb18030, 0x00),
    cs(254, "utf8", "utf8_general_cs", 1, 3, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(255, "utf8mb4", "utf8mb4_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(256, "utf8mb4", "utf8mb4_de_pb_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(257, "utf8mb4", "utf8mb4_is_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(258, "utf8mb4", "utf8mb4_lv_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(259, "utf8mb4", "utf8mb4_ro_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(260, "utf8mb4", "utf8mb4_sl_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(261, "utf8mb4", "utf8mb4_pl_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(262, "utf8mb4", "utf8mb4_et_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(263, "utf8mb4", "utf8mb4_es_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(264, "utf8mb4", "utf8mb4_sv_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(265, "utf8mb4", "utf8mb4_tr_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(266, "utf8mb4", "utf8mb4_cs_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(267, "utf8mb4", "utf8mb4_da_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(268, "utf8mb4", "utf8mb4_lt_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(269, "utf8mb4", "utf8mb4_sk_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(270, "utf8mb4", "utf8mb4_es_trad_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(271, "utf8mb4", "utf8mb4_la_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(272, "utf8mb4", "utf8mb4_fa_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(273, "utf8mb4", "utf8mb4_eo_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(274, "utf8mb4", "utf8mb4_hu_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(275, "utf8mb4", "utf8mb4_hr_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(276, "utf8mb4", "utf8mb4_si_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(277, "utf8mb4", "utf8mb4_vi_0900_ai_ci", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(278, "utf8mb4", "utf8mb4_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(279, "utf8mb4", "utf8mb4_de_pb_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(280, "utf8mb4", "utf8mb4_is_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(281, "utf8mb4", "utf8mb4_lv_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(282, "utf8mb4", "utf8mb4_ro_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(283, "utf8mb4", "utf8mb4_sl_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(284, "utf8mb4", "utf8mb4_pl_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(285, "utf8mb4", "utf8mb4_et_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(286, "utf8mb4", "utf8mb4_es_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(287, "utf8mb4", "utf8mb4_sv_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(288, "utf8mb4", "utf8mb4_tr_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(289, "utf8mb4", "utf8mb4_cs_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(290, "utf8mb4", "utf8mb4_da_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(291, "utf8mb4", "utf8mb4_lt_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(292, "utf8mb4", "utf8mb4_sk_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(293, "utf8mb4", "utf8mb4_es_trad_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(294, "utf8mb4", "utf8mb4_la_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(295, "utf8mb4", "utf8mb4_fa_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(296, "utf8mb4", "utf8mb4_eo_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(297, "utf8mb4", "utf8mb4_hu_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(298, "utf8mb4", "utf8mb4_hr_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(299, "utf8mb4", "utf8mb4_si_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(300, "utf8mb4", "utf8mb4_vi_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
    cs(303, "utf8mb4", "utf8mb4_ja_0900_as_cs", 1, 4, CharLen::Utf8, MbValid::Utf8, 0x80),
];

/// `mysqlnd_find_charset_nr`
#[must_use]
pub fn find_by_nr(nr: u32) -> Option<&'static Charset> {
    CHARSETS.iter().find(|c| c.nr == nr)
}

/// `mysqlnd_find_charset_name` (ASCII case-insensitive, first match wins).
#[must_use]
pub fn find_by_name(name: &[u8]) -> Option<&'static Charset> {
    CHARSETS.iter().find(|c| c.name.as_bytes().eq_ignore_ascii_case(name))
}

// ------------------------------------------------------------ validity checks

fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

fn utf8_sequence(s: &[u8], allow_4: bool) -> usize {
    let Some(&c) = s.first() else { return 0 };
    let cont = |i: usize| (at(s, i) ^ 0x80) < 0x40;
    if c < 0x80 {
        return 1;
    }
    if c < 0xC2 {
        return 0;
    }
    if c < 0xE0 {
        if s.len() < 2 || !cont(1) {
            return 0;
        }
        return 2;
    }
    if c < 0xF0 {
        if s.len() < 3 || !(cont(1) && cont(2) && (c >= 0xE1 || s[1] >= 0xA0)) {
            return 0;
        }
        return 3;
    }
    if allow_4 && c < 0xF5 {
        if s.len() < 4
            || !(cont(1) && cont(2) && cont(3) && (c >= 0xF1 || s[1] >= 0x90) && (c <= 0xF3 || s[1] <= 0x8F))
        {
            return 0;
        }
        return 4;
    }
    0
}

fn valid_big5head(c: u8) -> bool {
    (0xA1..=0xF9).contains(&c)
}
fn valid_big5tail(c: u8) -> bool {
    (0x40..=0x7E).contains(&c) || (0xA1..=0xFE).contains(&c)
}
fn valid_sjis_head(c: u8) -> bool {
    (0x81..=0x9F).contains(&c) || (0xE0..=0xFC).contains(&c)
}
fn valid_sjis_tail(c: u8) -> bool {
    (0x40..=0x7E).contains(&c) || (0x80..=0xFC).contains(&c)
}
fn valid_euckr(c: u8) -> bool {
    (0xA1..=0xFE).contains(&c)
}
fn valid_eucjp(c: u8) -> bool {
    (0xA1..=0xFE).contains(&c)
}
fn valid_eucjp_kata(c: u8) -> bool {
    (0xA1..=0xDF).contains(&c)
}
fn valid_gb2312_head(c: u8) -> bool {
    (0xA1..=0xF7).contains(&c)
}
fn valid_gb2312_tail(c: u8) -> bool {
    (0xA1..=0xFE).contains(&c)
}
fn valid_gbk_head(c: u8) -> bool {
    (0x81..=0xFE).contains(&c)
}
fn valid_gbk_tail(c: u8) -> bool {
    (0x40..=0x7E).contains(&c) || (0x80..=0xFE).contains(&c)
}
fn is_gb18030_odd(c: u8) -> bool {
    (0x81..=0xFE).contains(&c)
}
fn is_gb18030_even_2(c: u8) -> bool {
    (0x40..=0x7E).contains(&c) || (0x80..=0xFE).contains(&c)
}
fn is_gb18030_even_4(c: u8) -> bool {
    (0x30..=0x39).contains(&c)
}
fn utf16_high_head(c: u8) -> bool {
    c & 0xFC == 0xD8
}
fn utf16_low_head(c: u8) -> bool {
    c & 0xFC == 0xDC
}

/// Length of the valid multibyte sequence at the start of `s` (0 if none),
/// i.e. `mysqlnd_mbvalid_dispatch`.
#[must_use]
pub fn mb_valid(kind: MbValid, s: &[u8]) -> usize {
    let len = s.len();
    match kind {
        MbValid::Null => 0,
        MbValid::Utf8mb3 => match utf8_sequence(s, false) {
            n if n > 1 => n,
            _ => 0,
        },
        MbValid::Utf8 => match utf8_sequence(s, true) {
            n if n > 1 => n,
            _ => 0,
        },
        MbValid::Big5 => usize::from(valid_big5head(at(s, 0)) && len > 1 && valid_big5tail(at(s, 1))) * 2,
        MbValid::Cp932 | MbValid::Sjis => {
            usize::from(valid_sjis_head(at(s, 0)) && len > 1 && valid_sjis_tail(at(s, 1))) * 2
        }
        MbValid::Euckr => {
            if len <= 1 || s[0] < 0x80 {
                0
            } else if valid_euckr(s[1]) {
                2
            } else {
                0
            }
        }
        MbValid::Eucjpms => {
            let c = at(s, 0);
            if c < 0x80 {
                0
            } else if valid_eucjp(c) && len > 1 && valid_eucjp(s[1]) {
                2
            } else if c == 0x8E && len > 1 && valid_eucjp_kata(s[1]) {
                2
            } else if c == 0x8F && len > 2 && valid_eucjp(s[1]) && valid_eucjp(s[2]) {
                3
            } else {
                0
            }
        }
        MbValid::Ujis => {
            // The C version reads start[1] unchecked (relying on the trailing NUL).
            let c = at(s, 0);
            if c < 0x80 {
                0
            } else if valid_eucjp(c) && valid_eucjp(at(s, 1)) {
                2
            } else if c == 0x8E && valid_eucjp_kata(at(s, 1)) {
                2
            } else if c == 0x8F && len > 2 && valid_eucjp(s[1]) && valid_eucjp(s[2]) {
                3
            } else {
                0
            }
        }
        MbValid::Gb2312 => {
            usize::from(valid_gb2312_head(at(s, 0)) && len > 1 && valid_gb2312_tail(at(s, 1))) * 2
        }
        MbValid::Gbk => usize::from(valid_gbk_head(at(s, 0)) && len > 1 && valid_gbk_tail(at(s, 1))) * 2,
        MbValid::Ucs2 => 2,
        MbValid::Utf32 => 4,
        MbValid::Utf16 => {
            if len < 2 {
                0
            } else if utf16_high_head(s[0]) {
                if len >= 4 && utf16_low_head(s[2]) {
                    4
                } else {
                    0
                }
            } else if utf16_low_head(s[0]) {
                0
            } else {
                2
            }
        }
        MbValid::Gb18030 => {
            if len <= 1 || !is_gb18030_odd(s[0]) {
                0
            } else if is_gb18030_even_2(s[1]) {
                2
            } else if len > 3 && is_gb18030_even_4(s[1]) && is_gb18030_odd(s[2]) && is_gb18030_even_4(s[3]) {
                4
            } else {
                0
            }
        }
    }
}

/// `mysqlnd_mbcharlen_dispatch`. `c` is the lead byte *as the C code sees
/// it*: a `char` sign-extended to `unsigned int`, see [`sign_extend`].
#[must_use]
pub fn mb_charlen(kind: CharLen, c: u32) -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    let b = c as u8; // the `(zend_uchar)` casts in the C macros
    match kind {
        CharLen::Null => 0,
        CharLen::Big5 => {
            if valid_big5head(b) {
                2
            } else {
                1
            }
        }
        CharLen::Cp932 | CharLen::Sjis => {
            if valid_sjis_head(b) {
                2
            } else {
                1
            }
        }
        CharLen::Euckr => {
            if valid_euckr(b) {
                2
            } else {
                1
            }
        }
        CharLen::Eucjpms | CharLen::Ujis => {
            if valid_eucjp(b) || b == 0x8E {
                2
            } else if b == 0x8F {
                3
            } else {
                1
            }
        }
        CharLen::Gb2312 => {
            if valid_gb2312_head(b) {
                2
            } else {
                1
            }
        }
        CharLen::Gbk => {
            if valid_gbk_head(b) {
                2
            } else {
                1
            }
        }
        CharLen::Ucs2 => 2,
        CharLen::Utf32 => 4,
        CharLen::Utf16 => {
            if utf16_high_head(b) {
                4
            } else {
                2
            }
        }
        CharLen::Utf8mb3 | CharLen::Utf8 => {
            if c < 0x80 {
                1
            } else if c < 0xC2 {
                0
            } else if c < 0xE0 {
                2
            } else if c < 0xF0 {
                3
            } else if kind == CharLen::Utf8 && c < 0xF8 {
                4
            } else {
                0
            }
        }
        CharLen::Gb18030 => {
            if c <= 0xFF {
                u32::from(!is_gb18030_odd(b))
            } else if c > 0xFFFF || !is_gb18030_odd(((c >> 8) & 0xFF) as u8) {
                0
            } else if is_gb18030_even_2(b) {
                2
            } else if is_gb18030_even_4(b) {
                4
            } else {
                0
            }
        }
    }
}

/// Sign-extends a byte the way x86-64 C code promotes a (signed) `char`
/// to `unsigned int`.
#[must_use]
pub fn sign_extend(b: u8) -> u32 {
    #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
    let v = i32::from(b as i8) as u32;
    v
}

/// `mysqlnd_cset_escape_quotes`: doubles single quotes (used when the
/// server runs with `NO_BACKSLASH_ESCAPES`).
#[must_use]
pub fn escape_quotes(cset: &Charset, input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 2);
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if u32::from(b) >= cset.lowest_mb_byte {
            let len = mb_valid(cset.mb_valid, &input[i..]);
            if len > 0 {
                out.extend_from_slice(&input[i..i + len]);
                i += len;
                continue;
            }
        }
        if b == b'\'' {
            out.extend_from_slice(b"''");
        } else {
            out.push(b);
        }
        i += 1;
    }
    out
}

/// `mysqlnd_cset_escape_slashes`: backslash-escapes `\0 \n \r \\ ' " \x1a`
/// and broken multibyte lead bytes.
#[must_use]
pub fn escape_slashes(cset: &Charset, input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 2);
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        let mut esc: Option<u8> = None;
        if u32::from(b) >= cset.lowest_mb_byte {
            let len = mb_valid(cset.mb_valid, &input[i..]);
            if len > 0 {
                out.extend_from_slice(&input[i..i + len]);
                i += len;
                continue;
            } else if mb_charlen(cset.mb_charlen, sign_extend(b)) > 1 {
                esc = Some(b);
            }
        }
        if esc.is_none() {
            esc = match b {
                0 => Some(b'0'),
                b'\n' => Some(b'n'),
                b'\r' => Some(b'r'),
                b'\\' | b'\'' | b'"' => Some(b),
                0x1A => Some(b'Z'),
                _ => None,
            };
        }
        match esc {
            Some(e) => {
                out.push(b'\\');
                out.push(e);
            }
            None => out.push(b),
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        let c = find_by_name(b"UTF8MB4").unwrap();
        assert_eq!(c.nr, 45);
        assert_eq!(c.collation, "utf8mb4_general_ci");
        assert_eq!((c.char_minlen, c.char_maxlen), (1, 4));
        assert_eq!(find_by_name(b"latin1").unwrap().nr, 8);
        assert_eq!(find_by_name(b"utf8").unwrap().nr, 33);
        assert_eq!(find_by_nr(63).unwrap().name, "binary");
        assert_eq!(find_by_nr(224).unwrap().collation, "utf8mb4_unicode_ci");
        assert!(find_by_name(b"nonexistent").is_none());
        assert!(find_by_nr(0).is_none());
    }

    #[test]
    fn escape_basic() {
        let c = find_by_name(b"latin1").unwrap();
        assert_eq!(escape_slashes(c, b"a'b\"c\\d\0e\nf\rg\x1a"), b"a\\'b\\\"c\\\\d\\0e\\nf\\rg\\Z".to_vec());
        assert_eq!(escape_quotes(c, b"it's"), b"it''s".to_vec());
    }

    #[test]
    fn escape_utf8_multibyte_passthrough() {
        let c = find_by_name(b"utf8mb4").unwrap();
        assert_eq!(escape_slashes(c, "ä'ö".as_bytes()), "ä\\'ö".as_bytes().to_vec());
        // invalid lead byte: copied as-is (charlen of a sign-extended byte is 0)
        assert_eq!(escape_slashes(c, b"\xc3'"), b"\xc3\\'".to_vec());
    }

    #[test]
    fn escape_gbk_broken_lead_byte_is_escaped() {
        let c = find_by_name(b"gbk").unwrap();
        // 0xbf 0x27 is not a valid GBK pair (tail 0x27): lead byte escaped
        assert_eq!(escape_slashes(c, b"\xbf'"), b"\\\xbf\\'".to_vec());
        // valid pair 0xbf 0x5c passes through untouched
        assert_eq!(escape_slashes(c, b"\xbf\x5c"), b"\xbf\x5c".to_vec());
    }

    #[test]
    fn table_is_complete() {
        assert_eq!(CHARSETS.len(), 246);
        for c in CHARSETS {
            assert!(c.char_minlen <= c.char_maxlen);
            if c.char_maxlen == 1 {
                assert_eq!(c.lowest_mb_byte, 0x100, "{}", c.collation);
            }
        }
    }
}
