//! `mb_convert_kana` support: `mb_convert_kana_codepoint` and the
//! `MBFL_HAN2ZEN_*` / `MBFL_ZEN2HAN_*` mode flags.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c`,
//! `libmbfl/filters/translit_kana_jisx0201_jisx0208.h`.
//!
//! STUB: the conversion is a placeholder until the port lands.

/// Placeholder: returns `c` unchanged.
pub fn convert_kana_codepoint(
    c: u32,
    _next: u32,
    _consumed: Option<&mut bool>,
    _second: &mut u32,
    _mode: u32,
) -> u32 {
    c
}
