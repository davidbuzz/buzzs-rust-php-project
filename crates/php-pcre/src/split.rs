//! `preg_split()` (`php_pcre_split_impl`), `preg_grep()` and `preg_quote()`.

use crate::cache::MatchConfig;
use crate::error::PregError;
use crate::exec::{off, pairs, Exec};
use crate::ffi;
use crate::regex::Regex;
use crate::value::{MatchArray, Value};
use crate::{PREG_SPLIT_DELIM_CAPTURE, PREG_SPLIT_NO_EMPTY, PREG_SPLIT_OFFSET_CAPTURE};

fn piece(subject: &[u8], start: usize, end: usize, offset_capture: bool) -> Value {
    let s = Value::Str(subject[start..end].to_vec());
    if offset_capture {
        Value::pair(s, off(start))
    } else {
        s
    }
}

/// `preg_split($pattern, $subject, $limit, $flags)`.
///
/// Returns the list PHP returns, or the error that makes it return `false`
/// (`preg_last_error()` is `NoError` on success, the error otherwise).
#[allow(clippy::too_many_lines)]
pub fn split(
    re: &Regex,
    cfg: &MatchConfig,
    subject: &[u8],
    limit: i64,
    flags: i64,
) -> Result<MatchArray, PregError> {
    let no_empty = flags & PREG_SPLIT_NO_EMPTY != 0;
    let delim_capture = flags & PREG_SPLIT_DELIM_CAPTURE != 0;
    let offset_capture = flags & PREG_SPLIT_OFFSET_CAPTURE != 0;
    let len = subject.len();
    let num_subpats = re.capture_count() + 1;
    let mut out = MatchArray::new();
    let mut last_match = 0usize;

    let mut limit_val = limit;
    let run = if limit_val == -1 {
        true
    } else if limit_val == 0 {
        limit_val = -1;
        true
    } else {
        limit_val > 1
    };

    if run {
        let mut ex = Exec::new(re, cfg, subject).ok_or(PregError::Internal)?;
        let mut start = 0usize;
        let mut count = ex.run(start, ex.first_options());
        'outer: loop {
            if count >= 0 {
                let mut n = pairs(count, num_subpats);
                loop {
                    let (s, e) = ex.span();
                    if e < s {
                        return Err(PregError::Internal);
                    }
                    if !no_empty || s != last_match {
                        out.push(piece(subject, last_match, s, offset_capture));
                        if limit_val != -1 {
                            limit_val -= 1;
                        }
                    }
                    if delim_capture {
                        let ov = ex.ov();
                        for i in 1..n {
                            let (gs, ge) = (ov[2 * i], ov[2 * i + 1]);
                            if !no_empty || gs != ge {
                                // An unset group has both offsets PCRE2_UNSET.
                                if gs == ffi::PCRE2_UNSET {
                                    out.push(if offset_capture {
                                        Value::pair(Value::Str(Vec::new()), -1)
                                    } else {
                                        Value::Str(Vec::new())
                                    });
                                } else {
                                    out.push(piece(subject, gs, ge, offset_capture));
                                }
                            }
                        }
                    }
                    start = e;
                    last_match = e;
                    if start == s {
                        if limit_val != -1 && limit_val <= 1 {
                            break 'outer;
                        }
                        count = ex.retry_not_empty(start);
                        if count >= 0 {
                            n = pairs(count, num_subpats);
                            continue;
                        } else if count == ffi::PCRE2_ERROR_NOMATCH {
                            if start < len {
                                start += ex.unit_length(start);
                            } else {
                                break 'outer;
                            }
                        } else {
                            return Err(PregError::from_pcre2(count));
                        }
                    }
                    break;
                }
            } else if count == ffi::PCRE2_ERROR_NOMATCH {
                break;
            } else {
                return Err(PregError::from_pcre2(count));
            }
            if limit_val != -1 && limit_val <= 1 {
                break;
            }
            count = ex.next(start);
        }
    }

    // `last:`
    let start = last_match;
    if !no_empty || start < len {
        out.push(piece(subject, start, len, offset_capture));
    }
    Ok(out)
}

/// `preg_grep($pattern, $array, $flags)`: returns the positions (into
/// `items`) of the entries to keep — the VM copies those entries with their
/// original keys.  `invert` is `$flags & PREG_GREP_INVERT`.
pub fn grep<'s, I>(
    re: &Regex,
    cfg: &MatchConfig,
    items: I,
    invert: bool,
) -> Result<Vec<usize>, PregError>
where
    I: IntoIterator<Item = &'s [u8]>,
{
    let mut kept = Vec::new();
    for (idx, subject) in items.into_iter().enumerate() {
        let mut ex = Exec::new(re, cfg, subject).ok_or(PregError::Internal)?;
        let count = ex.run(0, ex.first_options());
        if count >= 0 {
            if !invert {
                kept.push(idx);
            }
        } else if count == ffi::PCRE2_ERROR_NOMATCH {
            if invert {
                kept.push(idx);
            }
        } else {
            return Err(PregError::from_pcre2(count));
        }
    }
    Ok(kept)
}

/// `preg_quote($str, $delimiter)`: backslash-escapes
/// `. \ + * ? [ ^ ] $ ( ) { } = ! > < | : - #`, turns NUL into `\000` and
/// also escapes the first byte of `delimiter` when given.
pub fn quote(input: &[u8], delimiter: Option<&[u8]>) -> Vec<u8> {
    let delim = delimiter.map_or(0, |d| d.first().copied().unwrap_or(0));
    let mut out = Vec::with_capacity(input.len() + input.len() / 4);
    for &c in input {
        match c {
            b'.' | b'\\' | b'+' | b'*' | b'?' | b'[' | b'^' | b']' | b'$' | b'(' | b')' | b'{'
            | b'}' | b'=' | b'!' | b'>' | b'<' | b'|' | b':' | b'-' | b'#' => {
                out.push(b'\\');
                out.push(c);
            }
            0 => out.extend_from_slice(b"\\000"),
            _ => {
                if c == delim {
                    out.push(b'\\');
                }
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(a: &MatchArray) -> Vec<String> {
        a.values()
            .map(|v| String::from_utf8_lossy(v.as_bytes().unwrap()).into_owned())
            .collect()
    }

    #[test]
    fn basic_split() {
        let cfg = MatchConfig::default();
        let re = Regex::compile(b"/[\\s,]+/").unwrap();
        let r = split(&re, &cfg, b"hypertext language, programming", -1, 0).unwrap();
        assert_eq!(strs(&r), ["hypertext", "language", "programming"]);
        let r = split(&re, &cfg, b"a b c", 2, 0).unwrap();
        assert_eq!(strs(&r), ["a", "b c"]);
        let r = split(&re, &cfg, b"a b c", 1, 0).unwrap();
        assert_eq!(strs(&r), ["a b c"]);
    }

    #[test]
    fn split_chars_no_empty() {
        let cfg = MatchConfig::default();
        let re = Regex::compile(b"//").unwrap();
        let r = split(&re, &cfg, b"string", -1, PREG_SPLIT_NO_EMPTY).unwrap();
        assert_eq!(strs(&r), ["s", "t", "r", "i", "n", "g"]);
        let r = split(&re, &cfg, b"ab", -1, 0).unwrap();
        assert_eq!(strs(&r), ["", "a", "b", ""]);
        let re = Regex::compile(b"//u").unwrap();
        let r = split(&re, &cfg, "é€".as_bytes(), -1, PREG_SPLIT_NO_EMPTY).unwrap();
        assert_eq!(strs(&r), ["é", "€"]);
    }

    #[test]
    fn split_delims_offsets() {
        let cfg = MatchConfig::default();
        let re = Regex::compile(b"/(-)|(\\+)/").unwrap();
        let r = split(&re, &cfg, b"a-b+c", -1, PREG_SPLIT_DELIM_CAPTURE).unwrap();
        assert_eq!(strs(&r), ["a", "-", "b", "", "+", "c"]);
        let r = split(&re, &cfg, b"a-b", -1, PREG_SPLIT_OFFSET_CAPTURE).unwrap();
        let second = r.get_index(1).unwrap().as_array().unwrap();
        assert_eq!(second.get_index(1), Some(&Value::Int(2)));
    }

    #[test]
    fn grep_and_quote() {
        let cfg = MatchConfig::default();
        let re = Regex::compile(b"/^\\d+$/").unwrap();
        let items: [&[u8]; 3] = [b"1", b"a", b"22"];
        assert_eq!(grep(&re, &cfg, items, false).unwrap(), vec![0, 2]);
        assert_eq!(grep(&re, &cfg, items, true).unwrap(), vec![1]);
        let re = Regex::compile(b"/./u").unwrap();
        let items: [&[u8]; 2] = [b"a", b"\xff"];
        assert_eq!(grep(&re, &cfg, items, false), Err(PregError::BadUtf8));

        assert_eq!(
            quote(b"Hello $world [*]", None),
            b"Hello \\$world \\[\\*\\]"
        );
        assert_eq!(quote(b"a/b#c\0", Some(b"/x")), b"a\\/b\\#c\\000");
        assert_eq!(quote(b"", Some(b"/")), b"");
        assert_eq!(quote(b"a\0", Some(b"")), b"a\\000");
    }
}
