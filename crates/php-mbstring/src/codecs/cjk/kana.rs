//! `mb_convert_kana` support: `mb_convert_kana_codepoint`, the
//! `MBFL_HAN2ZEN_*` / `MBFL_ZEN2HAN_*` mode flags, the option-string parser
//! and the conversion loop of `PHP_FUNCTION(mb_convert_kana)`.
//!
//! Ref: `libmbfl/filters/mbfilter_cjk.c` (`mb_convert_kana_codepoint`),
//! `libmbfl/filters/translit_kana_jisx0201_jisx0208.h`,
//! `mbstring.c` (`jp_kana_convert`, `PHP_FUNCTION(mb_convert_kana)`).

use crate::convert::{ConvertBuf, ErrorMode};
use crate::encoding::Encoding;
use crate::error::{MbError, MbResult};
use crate::tables::cjk::misc::{
    HANKANA2ZENHIRA_TABLE, HANKANA2ZENKANA_TABLE, ZENKANA2HANKANA_TABLE,
};

/// `A`/`R`: halfwidth ASCII (except `"`, `'`, `\`, `~`) to fullwidth.
pub const MBFL_HAN2ZEN_ALL: u32 = 0x00001;
/// `R`: halfwidth alphabet to fullwidth.
pub const MBFL_HAN2ZEN_ALPHA: u32 = 0x00002;
/// `N`: halfwidth digits to fullwidth.
pub const MBFL_HAN2ZEN_NUMERIC: u32 = 0x00004;
/// `S`: halfwidth space to fullwidth.
pub const MBFL_HAN2ZEN_SPACE: u32 = 0x00008;
/// `K`: halfwidth katakana to fullwidth katakana.
pub const MBFL_HAN2ZEN_KATAKANA: u32 = 0x00010;
/// `H`: halfwidth katakana to fullwidth hiragana.
pub const MBFL_HAN2ZEN_HIRAGANA: u32 = 0x00020;
/// `M`: special ASCII characters (`\`, `~`, `'`, `"`) to fullwidth symbols.
pub const MBFL_HAN2ZEN_SPECIAL: u32 = 0x00040;
/// `C`: fullwidth hiragana to fullwidth katakana.
pub const MBFL_ZENKAKU_HIRA2KATA: u32 = 0x00080;
/// `a`: fullwidth ASCII (except `"`, `'`, `\`, `~`) to halfwidth.
pub const MBFL_ZEN2HAN_ALL: u32 = 0x00100;
/// `r`: fullwidth alphabet to halfwidth.
pub const MBFL_ZEN2HAN_ALPHA: u32 = 0x00200;
/// `n`: fullwidth digits to halfwidth.
pub const MBFL_ZEN2HAN_NUMERIC: u32 = 0x00400;
/// `s`: fullwidth space to halfwidth.
pub const MBFL_ZEN2HAN_SPACE: u32 = 0x00800;
/// `k`: fullwidth katakana to halfwidth katakana.
pub const MBFL_ZEN2HAN_KATAKANA: u32 = 0x01000;
/// `h`: fullwidth hiragana to halfwidth katakana.
pub const MBFL_ZEN2HAN_HIRAGANA: u32 = 0x02000;
/// `m`: fullwidth special symbols to ASCII.
pub const MBFL_ZEN2HAN_SPECIAL: u32 = 0x04000;
/// `c`: fullwidth katakana to fullwidth hiragana.
pub const MBFL_ZENKAKU_KATA2HIRA: u32 = 0x08000;
/// `V`: combine halfwidth kana with a following (semi-)voiced sound mark.
pub const MBFL_HAN2ZEN_GLUE: u32 = 0x10000;

/// The default mode of `mb_convert_kana` (`"KV"`).
pub const DEFAULT_MODE: u32 = MBFL_HAN2ZEN_KATAKANA | MBFL_HAN2ZEN_GLUE;

/// `mb_convert_kana_flags`: option letters, bit `i` corresponds to entry `i`.
pub const MB_CONVERT_KANA_FLAGS: [u8; 17] = *b"ARNSKHMCarnskhmcV";

#[inline]
fn hankana(tbl: &[u8; 64], c: u32) -> u32 {
    u32::from(tbl[(c - 0xFF60) as usize])
}

#[inline]
fn zenkana(n: u32, second: &mut u32) -> u32 {
    let [first, snd] = ZENKANA2HANKANA_TABLE[n as usize];
    if snd != 0 {
        *second = 0xFF00 + u32::from(snd);
    }
    0xFF00 + u32::from(first)
}

/// Kana glue for halfwidth kana `c` followed by `next` (the
/// `MBFL_HAN2ZEN_GLUE` branches); `katakana` selects the table and whether
/// `ｳﾞ` combines to `ヴ`.
fn glue(c: u32, next: u32, consumed: Option<&mut bool>, katakana: bool) -> u32 {
    let tbl = if katakana {
        &HANKANA2ZENKANA_TABLE
    } else {
        &HANKANA2ZENHIRA_TABLE
    };
    let n = c - 0xFF60;
    if (0xFF61..=0xFF9F).contains(&next) {
        let consume = |v: u32| {
            if let Some(flag) = consumed {
                *flag = true;
            }
            v
        };
        if next == 0xFF9E && ((22..=36).contains(&n) || (42..=46).contains(&n)) {
            return consume(0x3001 + hankana(tbl, c));
        }
        if katakana && next == 0xFF9E && n == 19 {
            return consume(0x30F4);
        }
        if next == 0xFF9F && (42..=46).contains(&n) {
            return consume(0x3002 + hankana(tbl, c));
        }
    }
    0x3000 + hankana(tbl, c)
}

/// `mb_convert_kana_codepoint`: apply the transforms selected by `mode` (a
/// combination of the `MBFL_*` flags, not containing mutually inverse
/// transforms) to `c`.
///
/// `next` is the following code point (0 if none); if it was merged into the
/// result, `*consumed` is set to `true` (otherwise it is left alone). If `c`
/// converts to two code points, the second one is stored in `*second`
/// (otherwise it is left alone). Returns the (first) resulting code point,
/// or `c` unchanged if no transform applies.
#[allow(clippy::too_many_lines)]
pub fn convert_kana_codepoint(
    c: u32,
    next: u32,
    consumed: Option<&mut bool>,
    second: &mut u32,
    mode: u32,
) -> u32 {
    let is = |flag: u32| mode & flag != 0;
    if is(MBFL_HAN2ZEN_ALL)
        && (0x21..=0x7D).contains(&c)
        && c != u32::from(b'"')
        && c != u32::from(b'\'')
        && c != u32::from(b'\\')
    {
        return c + 0xFEE0;
    }
    if is(MBFL_HAN2ZEN_ALPHA)
        && ((u32::from(b'A')..=u32::from(b'Z')).contains(&c)
            || (u32::from(b'a')..=u32::from(b'z')).contains(&c))
    {
        return c + 0xFEE0;
    }
    if is(MBFL_HAN2ZEN_NUMERIC) && (u32::from(b'0')..=u32::from(b'9')).contains(&c) {
        return c + 0xFEE0;
    }
    if is(MBFL_HAN2ZEN_SPACE) && c == u32::from(b' ') {
        return 0x3000;
    }

    if is(MBFL_HAN2ZEN_KATAKANA | MBFL_HAN2ZEN_HIRAGANA) {
        let hankaku = (0xFF61..=0xFF9F).contains(&c);
        if is(MBFL_HAN2ZEN_KATAKANA) && is(MBFL_HAN2ZEN_GLUE) && hankaku {
            return glue(c, next, consumed, true);
        }
        if is(MBFL_HAN2ZEN_HIRAGANA) && is(MBFL_HAN2ZEN_GLUE) && hankaku {
            return glue(c, next, consumed, false);
        }
        if is(MBFL_HAN2ZEN_KATAKANA) && hankaku {
            return 0x3000 + hankana(&HANKANA2ZENKANA_TABLE, c);
        }
        if is(MBFL_HAN2ZEN_HIRAGANA) && hankaku {
            return 0x3000 + hankana(&HANKANA2ZENHIRA_TABLE, c);
        }
    }

    if is(MBFL_HAN2ZEN_SPECIAL) {
        if c == u32::from(b'\\') || c == 0xA5 {
            return 0xFFE5;
        }
        if c == 0x7E || c == 0x203E {
            return 0xFFE3;
        }
        if c == u32::from(b'\'') {
            return 0x2019;
        }
        if c == u32::from(b'"') {
            return 0x201D;
        }
    }

    if is(MBFL_ZEN2HAN_ALL | MBFL_ZEN2HAN_ALPHA | MBFL_ZEN2HAN_NUMERIC | MBFL_ZEN2HAN_SPACE) {
        if is(MBFL_ZEN2HAN_ALL)
            && (0xFF01..=0xFF5D).contains(&c)
            && c != 0xFF02
            && c != 0xFF07
            && c != 0xFF3C
        {
            return c - 0xFEE0;
        }
        if is(MBFL_ZEN2HAN_ALPHA)
            && ((0xFF21..=0xFF3A).contains(&c) || (0xFF41..=0xFF5A).contains(&c))
        {
            return c - 0xFEE0;
        }
        if is(MBFL_ZEN2HAN_NUMERIC) && (0xFF10..=0xFF19).contains(&c) {
            return c - 0xFEE0;
        }
        if is(MBFL_ZEN2HAN_SPACE) && c == 0x3000 {
            return u32::from(b' ');
        }
        if is(MBFL_ZEN2HAN_ALL) && c == 0x2212 {
            return u32::from(b'-');
        }
    }

    if is(MBFL_ZEN2HAN_KATAKANA | MBFL_ZEN2HAN_HIRAGANA) {
        if is(MBFL_ZEN2HAN_KATAKANA) && (0x30A1..=0x30F4).contains(&c) {
            return zenkana(c - 0x30A1, second);
        }
        if is(MBFL_ZEN2HAN_HIRAGANA) && (0x3041..=0x3093).contains(&c) {
            return zenkana(c - 0x3041, second);
        }
        match c {
            0x3001 => return 0xFF64,
            0x3002 => return 0xFF61,
            0x300C => return 0xFF62,
            0x300D => return 0xFF63,
            0x309B => return 0xFF9E,
            0x309C => return 0xFF9F,
            0x30FC => return 0xFF70,
            0x30FB => return 0xFF65,
            _ => {}
        }
    }

    if is(MBFL_ZENKAKU_HIRA2KATA | MBFL_ZENKAKU_KATA2HIRA) {
        if is(MBFL_ZENKAKU_HIRA2KATA)
            && ((0x3041..=0x3093).contains(&c) || c == 0x309D || c == 0x309E)
        {
            return c + 0x60;
        }
        if is(MBFL_ZENKAKU_KATA2HIRA)
            && ((0x30A1..=0x30F3).contains(&c) || c == 0x30FD || c == 0x30FE)
        {
            return c - 0x60;
        }
    }

    if is(MBFL_ZEN2HAN_SPECIAL) {
        if c == 0xFFE5 || c == 0xFF3C {
            return u32::from(b'\\');
        }
        if c == 0xFFE3 || c == 0x203E {
            return u32::from(b'~');
        }
        if c == 0x2018 || c == 0x2019 {
            return u32::from(b'\'');
        }
        if c == 0x201C || c == 0x201D {
            return u32::from(b'"');
        }
    }

    c
}

fn flag_char(c: u8) -> String {
    String::from_utf8_lossy(&[c]).into_owned()
}

/// Parse the `$mode` option string of `mb_convert_kana` into a mode bit set
/// (`PHP_FUNCTION(mb_convert_kana)`); `None` (no argument given) yields
/// [`DEFAULT_MODE`]. Invalid flags and illegal combinations produce the
/// same `ValueError`s as PHP (argument #2, `$mode`).
pub fn kana_mode_from_option(optstr: Option<&[u8]>) -> MbResult<u32> {
    let Some(optstr) = optstr else {
        return Ok(DEFAULT_MODE);
    };
    let err = |msg: String| Err(MbError::arg_value(2, "mode", msg));
    let mut opt: u32 = 0;
    for &c in optstr {
        if c == b'A' {
            opt |= MBFL_HAN2ZEN_ALL | MBFL_HAN2ZEN_ALPHA | MBFL_HAN2ZEN_NUMERIC;
        } else if c == b'a' {
            opt |= MBFL_ZEN2HAN_ALL | MBFL_ZEN2HAN_ALPHA | MBFL_ZEN2HAN_NUMERIC;
        } else if let Some(i) = MB_CONVERT_KANA_FLAGS.iter().position(|&f| f == c) {
            opt |= 1 << i;
        } else {
            return err(format!("contains invalid flag: '{}'", flag_char(c)));
        }
    }

    let badflag = ((opt & 0xFF00) >> 8) & opt;
    if badflag != 0 {
        let i = badflag.trailing_zeros() as usize;
        let mut flag1 = MB_CONVERT_KANA_FLAGS[i];
        let mut flag2 = MB_CONVERT_KANA_FLAGS[i + 8];
        if (flag1 == b'R' || flag1 == b'N') && opt & MBFL_HAN2ZEN_ALL != 0 {
            flag1 = b'A';
        }
        if (flag2 == b'r' || flag2 == b'n') && opt & MBFL_ZEN2HAN_ALL != 0 {
            flag2 = b'a';
        }
        return err(format!(
            "must not combine '{}' and '{}' flags",
            char::from(flag1),
            char::from(flag2)
        ));
    }

    if opt & MBFL_HAN2ZEN_HIRAGANA != 0 && opt & MBFL_HAN2ZEN_KATAKANA != 0 {
        return err("must not combine 'H' and 'K' flags".into());
    }

    if opt & MBFL_ZEN2HAN_HIRAGANA != 0 {
        if opt & MBFL_ZENKAKU_HIRA2KATA != 0 {
            return err("must not combine 'h' and 'C' flags".into());
        } else if opt & MBFL_ZENKAKU_KATA2HIRA != 0 {
            return err("must not combine 'h' and 'c' flags".into());
        }
    } else if opt & MBFL_ZEN2HAN_KATAKANA != 0 {
        if opt & MBFL_ZENKAKU_HIRA2KATA != 0 {
            return err("must not combine 'k' and 'C' flags".into());
        } else if opt & MBFL_ZENKAKU_KATA2HIRA != 0 {
            return err("must not combine 'k' and 'c' flags".into());
        }
    }
    Ok(opt)
}

/// `jp_kana_convert` (`mbstring.c`): decode `input` from `encoding`, apply
/// [`convert_kana_codepoint`] with `mode` to each code point (merging and
/// splitting as it directs, in 64-code-point chunks exactly like PHP), and
/// re-encode. `replacement_char` / `error_mode` are the current
/// `mb_substitute_character` settings.
pub fn jp_kana_convert(
    input: &[u8],
    encoding: &'static Encoding,
    mode: u32,
    replacement_char: u32,
    error_mode: ErrorMode,
) -> Vec<u8> {
    let mut wchar_buf = [0u32; 64];
    let mut converted: Vec<u32> = Vec::with_capacity(128);
    let mut buf_offset = 0usize;
    let mut state = 0u32;
    let mut rest = input;
    let mut buf = ConvertBuf::new(input.len(), replacement_char, error_mode);

    while !rest.is_empty() {
        converted.clear();
        let mut out_len = (encoding.to_wchar)(&mut rest, &mut wchar_buf[buf_offset..], &mut state);
        out_len += buf_offset;
        if out_len == 0 {
            continue;
        }

        let mut consumed_last = false;
        let mut i = 0;
        while i < out_len - 1 {
            let mut second = 0;
            let mut consumed = false;
            converted.push(convert_kana_codepoint(
                wchar_buf[i],
                wchar_buf[i + 1],
                Some(&mut consumed),
                &mut second,
                mode,
            ));
            if second != 0 {
                converted.push(second);
            }
            if consumed {
                i += 1;
                if i == out_len - 1 {
                    // Consumed two code points at the very end of the buffer:
                    // nothing remains to reprocess on the next iteration.
                    buf_offset = 0;
                    consumed_last = true;
                    break;
                }
            }
            i += 1;
        }

        if !consumed_last {
            if rest.is_empty() {
                let mut second = 0;
                converted.push(convert_kana_codepoint(
                    wchar_buf[out_len - 1],
                    0,
                    None,
                    &mut second,
                    mode,
                ));
                if second != 0 {
                    converted.push(second);
                }
            } else {
                wchar_buf[0] = wchar_buf[out_len - 1];
                buf_offset = 1;
            }
        }

        (encoding.from_wchar)(&converted, &mut buf, rest.is_empty());
    }

    buf.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codecs::cjk::testutil::fixture_path;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    fn kana(s: &str, mode: &str) -> String {
        let mode = kana_mode_from_option(Some(mode.as_bytes())).expect("valid mode");
        let out = jp_kana_convert(
            s.as_bytes(),
            crate::encoding::utf8(),
            mode,
            u32::from(b'?'),
            ErrorMode::Char,
        );
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn oracle_fixture() {
        let text = std::fs::read_to_string(fixture_path("kana_oracle.tsv")).expect("fixture");
        let mut failures = Vec::new();
        let mut n = 0;
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            n += 1;
            if f[1] == "ERR" {
                match kana_mode_from_option(Some(f[0].as_bytes())) {
                    Err(e) if e.message_for("mb_convert_kana") == f[2] => {}
                    other => failures.push(format!("mode {:?}: {other:?}, want {}", f[0], f[2])),
                }
                continue;
            }
            let Ok(mode) = kana_mode_from_option(Some(f[0].as_bytes())) else {
                failures.push(format!("mode {:?} rejected", f[0]));
                continue;
            };
            let out = jp_kana_convert(
                &unhex(f[1]),
                crate::encoding::utf8(),
                mode,
                u32::from(b'?'),
                ErrorMode::Char,
            );
            if out != unhex(f[2]) {
                failures.push(format!(
                    "mode {:?} in {} want {} got {}",
                    f[0],
                    f[1],
                    f[2],
                    crate::codecs::cjk::testutil::hex(&out)
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {n} failed:\n{}",
            failures.len(),
            failures[..failures.len().min(20)].join("\n")
        );
        assert!(n > 10_000);
    }

    #[test]
    fn php_test_examples() {
        // From mb_convert_kana.phpt / the PHP manual
        assert_eq!(kana("ｱｲｳｴｵ ｶﾞｷﾞｸﾞ ﾊﾟﾋﾟﾌﾟ", "KV"), "アイウエオ ガギグ パピプ");
        assert_eq!(kana("ｱｲｳｴｵ ｶﾞｷﾞｸﾞ", "HV"), "あいうえお がぎぐ");
        assert_eq!(kana("ｶﾞ", "K"), "カ゛");
        assert_eq!(kana("ｳﾞ", "KV"), "ヴ");
        assert_eq!(kana("ｳﾞ", "HV"), "う゛");
        assert_eq!(kana("ガギ", "k"), "ｶﾞｷﾞ");
        assert_eq!(kana("がぎ", "h"), "ｶﾞｷﾞ");
        assert_eq!(kana("abc 123", "AS"), "ａｂｃ　１２３");
        assert_eq!(kana("ａｂｃ　１２３－", "as"), "abc 123-");
        assert_eq!(kana("−", "a"), "-");
        assert_eq!(kana("\\~'\"", "M"), "￥￣’”");
        assert_eq!(kana("￥￣’”＼", "m"), "\\~'\"\\");
        assert_eq!(kana("ひらがな", "C"), "ヒラガナ");
        assert_eq!(kana("カタカナ", "c"), "かたかな");
        assert_eq!(kana("", "KV"), "");
        // Default mode
        assert_eq!(kana_mode_from_option(None), Ok(DEFAULT_MODE));
        assert_eq!(kana_mode_from_option(Some(b"")), Ok(0));
    }

    #[test]
    fn option_errors() {
        let msg = |m: &[u8]| kana_mode_from_option(Some(m)).map_err(|e| e.to_string());
        assert_eq!(
            msg(b"Z"),
            Err("Argument #2 ($mode) contains invalid flag: 'Z'".into())
        );
        assert_eq!(
            msg(b"Aa"),
            Err("Argument #2 ($mode) must not combine 'A' and 'a' flags".into())
        );
        assert_eq!(
            msg(b"NAn"),
            Err("Argument #2 ($mode) must not combine 'A' and 'n' flags".into())
        );
        assert_eq!(
            msg(b"HK"),
            Err("Argument #2 ($mode) must not combine 'H' and 'K' flags".into())
        );
        assert_eq!(
            msg(b"kc"),
            Err("Argument #2 ($mode) must not combine 'k' and 'c' flags".into())
        );
        assert_eq!(msg(b"A"), Ok(0x7));
        assert_eq!(msg(b"KV"), Ok(DEFAULT_MODE));
    }

    #[test]
    fn codepoint_outputs() {
        let mut consumed = false;
        let mut second = 0;
        let glue = MBFL_HAN2ZEN_KATAKANA | MBFL_HAN2ZEN_GLUE;
        assert_eq!(
            convert_kana_codepoint(0xFF76, 0xFF9E, Some(&mut consumed), &mut second, glue),
            0x30AC
        );
        assert!(consumed);
        // `None` for `consumed` is fine when merging happens
        assert_eq!(
            convert_kana_codepoint(0xFF8A, 0xFF9F, None, &mut second, glue),
            0x30D1
        );
        assert_eq!(second, 0);
        assert_eq!(
            convert_kana_codepoint(0x30AC, 0, None, &mut second, MBFL_ZEN2HAN_KATAKANA),
            0xFF76
        );
        assert_eq!(second, 0xFF9E);
        assert_eq!(
            convert_kana_codepoint(0x1F600, 0, None, &mut second, 0x1FFFF),
            0x1F600
        );
    }
}
