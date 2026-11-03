//! CSV parsing and formatting — ports of `php_fgetcsv()` and
//! `php_fputcsv()` from `ext/standard/file.c`, and the argument handling
//! of `str_getcsv()`, `fgetcsv()` and `fputcsv()`.
//!
//! The parser is locale-sensitive in C (it walks the line with `mbrlen()`).
//! PHP resets `LC_CTYPE` to `C.UTF-8` at startup, so this port uses glibc's
//! UTF-8 `mbrlen()` semantics: valid sequences of 2–6 bytes (no overlongs,
//! no UTF-16 surrogates, values up to `0x7FFFFFFF`) are stepped over as a
//! unit; any other byte counts as a single (invalid) character.

use crate::error::{Notice, Notices, StdError, StdResult};

/// Delimiter/enclosure/escape configuration of a CSV call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CsvOptions {
    /// Field separator (default `,`).
    pub delimiter: u8,
    /// Enclosure character (default `"`).
    pub enclosure: u8,
    /// Escape character; `None` is `PHP_CSV_NO_ESCAPE` (an empty `$escape`).
    pub escape: Option<u8>,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            enclosure: b'"',
            escape: Some(b'\\'),
        }
    }
}

/// Validates the `$separator`, `$enclosure` and `$escape` arguments of
/// `str_getcsv()`, `fgetcsv()`, `fputcsv()` (and `SplFileObject`'s CSV
/// methods).
///
/// * `func` — PHP function name used in messages;
/// * `first_arg` — the 1-based position of `$separator` (2 for
///   `str_getcsv`, 3 for `fgetcsv`/`fputcsv`); `$enclosure` and `$escape`
///   follow it;
/// * each argument is `None` when it was not passed.
///
/// Throws `ValueError` "Argument #N ($separator) must be a single
/// character", "... ($enclosure) must be a single character" or "...
/// ($escape) must be empty or a single character".  When `$escape` is not
/// passed, pushes php-src master's (8.4+) deprecation "the $escape
/// parameter must be provided as its default value will change" and uses
/// `\`.  (The VM must turn the deprecation into an exception-aware emit:
/// if a user error handler throws, the call must abort.)
pub fn csv_options(
    func: &'static str,
    first_arg: u32,
    delimiter: Option<&[u8]>,
    enclosure: Option<&[u8]>,
    escape: Option<&[u8]>,
    notices: &mut Notices,
) -> StdResult<CsvOptions> {
    let mut opts = CsvOptions::default();
    if let Some(d) = delimiter {
        let [c] = d else {
            return Err(StdError::arg_value(
                func,
                first_arg,
                "separator",
                "must be a single character",
            ));
        };
        opts.delimiter = *c;
    }
    if let Some(e) = enclosure {
        let [c] = e else {
            return Err(StdError::arg_value(
                func,
                first_arg + 1,
                "enclosure",
                "must be a single character",
            ));
        };
        opts.enclosure = *c;
    }
    opts.escape = csv_escape_argument(func, first_arg + 2, escape, notices)?;
    Ok(opts)
}

/// `php_csv_handle_escape_argument()`: `Ok(None)` is `PHP_CSV_NO_ESCAPE`.
pub fn csv_escape_argument(
    func: &'static str,
    arg_num: u32,
    escape: Option<&[u8]>,
    notices: &mut Notices,
) -> StdResult<Option<u8>> {
    match escape {
        Some(e) if e.len() > 1 => Err(StdError::arg_value(
            func,
            arg_num,
            "escape",
            "must be empty or a single character",
        )),
        Some(e) => Ok(e.first().copied()),
        None => {
            notices.push(Notice::deprecated(
                func,
                "the $escape parameter must be provided as its default value will change",
            ));
            Ok(Some(b'\\'))
        }
    }
}

/// `fgetcsv()`'s `$length` handling: `None`/`0` mean "whole line"
/// (`Ok(None)`); otherwise the line must be read with
/// `php_stream_get_line(stream, buf, length + 1)`, i.e. at most `length`
/// bytes.  Throws `fgetcsv(): Argument #2 ($length) must be between 0 and
/// 9223372036854775806` for negative values.  Must be called after
/// [`csv_options`] to keep PHP's error order.
pub fn fgetcsv_length(length: Option<i64>) -> StdResult<Option<u64>> {
    match length {
        None | Some(0) => Ok(None),
        Some(l) if !(0..=i64::MAX - 1).contains(&l) => Err(StdError::arg_value(
            "fgetcsv",
            2,
            "length",
            &format!("must be between 0 and {}", i64::MAX - 1),
        )),
        Some(l) => Ok(u64::try_from(l).ok()),
    }
}

/// glibc `mbrlen()` in a UTF-8 locale, collapsed to what `php_fgetcsv`
/// distinguishes: the length of a valid character, or `-1` for an invalid
/// or incomplete sequence (`-1` and `-2` are handled identically).
fn mblen(s: &[u8]) -> isize {
    let Some(&b0) = s.first() else { return 0 };
    let (n, min) = match b0 {
        0x00..=0x7f => return 1,
        0xc2..=0xdf => (2usize, 0x80u32),
        0xe0..=0xef => (3, 0x800),
        0xf0..=0xf7 => (4, 0x1_0000),
        0xf8..=0xfb => (5, 0x20_0000),
        0xfc..=0xfd => (6, 0x400_0000),
        _ => return -1,
    };
    if s.len() < n {
        return -1;
    }
    let lead_bits = 0x7f_u32 >> n;
    let mut v = u32::from(b0) & lead_bits;
    for &c in &s[1..n] {
        if c & 0xc0 != 0x80 {
            return -1;
        }
        v = (v << 6) | u32::from(c & 0x3f);
    }
    if v < min || (0xd800..=0xdfff).contains(&v) {
        return -1;
    }
    isize::try_from(n).unwrap_or(-1)
}

/// `isspace()` in the C/C.UTF-8 locale for a single byte.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `php_fgetcsv_lookup_trailing_spaces()`: the index where a trailing
/// `"\r"`, `"\n"` or `"\r\n"` begins (despite the name it strips nothing
/// else).
fn lookup_trailing_spaces(s: &[u8]) -> usize {
    let mut last = [0u8; 2];
    let mut ptr = 0usize;
    while ptr < s.len() {
        let inc = if s[ptr] == 0 { 1 } else { mblen(&s[ptr..]) };
        let inc = if inc < 0 {
            1
        } else {
            last[0] = last[1];
            last[1] = s[ptr];
            inc.unsigned_abs()
        };
        ptr += inc;
    }
    match last[1] {
        b'\n' if last[0] == b'\r' => ptr - 2,
        b'\n' | b'\r' => ptr - 1,
        _ => ptr,
    }
}

/// The current line buffer of the parser (`buf`, NUL-terminated in C).
struct Line {
    buf: Vec<u8>,
    limit: usize,
    eol_len: usize,
}

impl Line {
    fn new(buf: Vec<u8>) -> Self {
        let limit = lookup_trailing_spaces(&buf);
        let eol_len = buf.len() - limit;
        Self {
            buf,
            limit,
            eol_len,
        }
    }

    /// `*ptr` (reads past the end see the NUL terminator).
    fn at(&self, i: usize) -> u8 {
        self.buf.get(i).copied().unwrap_or(0)
    }

    /// `bptr < limit ? (*bptr == '\0' ? 1 : php_mblen(bptr, limit - bptr)) : 0`
    fn inc_len(&self, bptr: usize) -> isize {
        if bptr < self.limit {
            if self.buf[bptr] == 0 {
                1
            } else {
                mblen(&self.buf[bptr..self.limit])
            }
        } else {
            0
        }
    }

    fn slice(&self, from: usize, to: usize) -> &[u8] {
        let to = to.min(self.buf.len());
        if from >= to {
            &[]
        } else {
            &self.buf[from..to]
        }
    }
}

fn step(inc: isize) -> usize {
    inc.unsigned_abs()
}

/// `php_fgetcsv()`: parses one CSV record starting with `line`.
///
/// `line` is the raw line as read by `fgets` (including its line
/// terminator).  When an enclosed field spans the end of the line,
/// `next_line` is called to fetch the following raw line (return `None` at
/// EOF); `str_getcsv()` passes a closure that always returns `None`.
///
/// Returns `None` for a blank line — PHP then returns `[null]` (see
/// `php_bc_fgetcsv_empty_line()`).  Fields are returned in order.
#[allow(clippy::too_many_lines)] // mirrors the single C function
pub fn fgetcsv_parse(
    line: Vec<u8>,
    opts: &CsvOptions,
    next_line: &mut dyn FnMut() -> Option<Vec<u8>>,
) -> Option<Vec<Vec<u8>>> {
    let CsvOptions {
        delimiter,
        enclosure,
        escape,
    } = *opts;
    let mut ln = Line::new(line);
    let mut bptr = 0usize;
    let mut values = Vec::new();
    let mut first_field = true;
    loop {
        let mut temp: Vec<u8> = Vec::new();
        let mut inc_len = ln.inc_len(bptr);
        if inc_len == 1 {
            let mut ws = bptr;
            while ln.at(ws) != delimiter && is_space(ln.at(ws)) {
                ws += 1;
            }
            if ln.at(ws) == enclosure && ws < ln.limit {
                bptr = ws;
            }
        }
        if first_field && bptr == ln.limit {
            return None;
        }
        first_field = false;
        let comp_end;
        if inc_len != 0 && ln.at(bptr) == enclosure {
            // 2A. enclosure-delimited field
            let mut state = 0u8;
            bptr += 1;
            let mut hunk = bptr;
            loop {
                match inc_len {
                    0 => {
                        if state == 2 {
                            temp.extend_from_slice(ln.slice(hunk, bptr - 1));
                            hunk = bptr;
                            break;
                        }
                        if state == 1 {
                            temp.extend_from_slice(ln.slice(hunk, bptr));
                            hunk = bptr;
                        }
                        if hunk != ln.limit {
                            temp.extend_from_slice(ln.slice(hunk, bptr));
                            hunk = bptr;
                        }
                        // add the embedded line end to the field
                        temp.extend_from_slice(ln.slice(ln.limit, ln.limit + ln.eol_len));
                        if let Some(nb) = next_line() {
                            ln = Line::new(nb);
                            bptr = 0;
                            hunk = 0;
                            state = 0;
                        } else {
                            // unterminated enclosure: the rest is the field
                            if bptr > ln.limit {
                                if hunk == bptr {
                                    hunk -= 1;
                                }
                                bptr -= 1;
                            }
                            break;
                        }
                    }
                    1 | -1 | -2 => match state {
                        1 => {
                            // escaped
                            bptr += 1;
                            state = 0;
                        }
                        2 => {
                            if ln.at(bptr) != enclosure {
                                // real enclosure
                                temp.extend_from_slice(ln.slice(hunk, bptr - 1));
                                hunk = bptr;
                                break;
                            }
                            // embedded (doubled) enclosure
                            temp.extend_from_slice(ln.slice(hunk, bptr));
                            bptr += 1;
                            hunk = bptr;
                            state = 0;
                        }
                        _ => {
                            let c = ln.at(bptr);
                            if c == enclosure {
                                state = 2;
                            } else if escape == Some(c) {
                                state = 1;
                            }
                            bptr += 1;
                        }
                    },
                    n => match state {
                        2 => {
                            temp.extend_from_slice(ln.slice(hunk, bptr - 1));
                            hunk = bptr;
                            break;
                        }
                        1 => {
                            bptr += step(n);
                            temp.extend_from_slice(ln.slice(hunk, bptr));
                            hunk = bptr;
                            state = 0;
                        }
                        _ => bptr += step(n),
                    },
                }
                inc_len = ln.inc_len(bptr);
            }
            // look for the delimiter
            inc_len = seek_delimiter(&ln, &mut bptr, inc_len, delimiter);
            temp.extend_from_slice(ln.slice(hunk, bptr));
            bptr += step(inc_len);
            comp_end = temp.len();
        } else {
            // 2B. non-enclosed field
            let hunk = bptr;
            inc_len = seek_delimiter(&ln, &mut bptr, inc_len, delimiter);
            temp.extend_from_slice(ln.slice(hunk, bptr));
            comp_end = lookup_trailing_spaces(&temp);
            if ln.at(bptr) == delimiter {
                bptr += 1;
            }
        }
        temp.truncate(comp_end);
        values.push(temp);
        if inc_len <= 0 {
            break;
        }
    }
    Some(values)
}

/// The "look up for a delimiter" loops (`quit_loop_3` / `quit_loop_4`):
/// advances `bptr` to the delimiter or the end and returns the final
/// `inc_len` (1 at a delimiter, 0 at the end).
fn seek_delimiter(ln: &Line, bptr: &mut usize, mut inc_len: isize, delimiter: u8) -> isize {
    loop {
        match inc_len {
            0 => return 0,
            1 | -1 | -2 => {
                inc_len = 1;
                if ln.at(*bptr) == delimiter {
                    return 1;
                }
            }
            _ => {}
        }
        *bptr += step(inc_len);
        inc_len = ln.inc_len(*bptr);
    }
}

/// `str_getcsv(string $string, string $separator = ",", string $enclosure =
/// "\"", string $escape = "\\"): array`.
///
/// Parses `s` as a single record (embedded newlines inside enclosures are
/// kept).  `None` means PHP's `[null]` (blank input).  Validate the
/// arguments first with [`csv_options`]`("str_getcsv", 2, …)`.
pub fn str_getcsv(s: &[u8], opts: &CsvOptions) -> Option<Vec<Vec<u8>>> {
    fgetcsv_parse(s.to_vec(), opts, &mut || None)
}

/// `php_fputcsv()` line formatting — backs `fputcsv()` (and
/// `SplFileObject::fputcsv()`).
///
/// The VM converts every array element to string first
/// (`zval_get_string`, which may warn/throw).  A field is enclosed when it
/// contains the delimiter, the enclosure, the escape character, `\n`,
/// `\r`, `\t` or a space; inside it, enclosure characters are doubled
/// unless they directly follow the escape character.  The line ends with
/// `eol` (default `"\n"`).  `fputcsv()` returns the number of bytes
/// written.
pub fn fputcsv_line<F: AsRef<[u8]>>(
    fields: &[F],
    opts: &CsvOptions,
    eol: Option<&[u8]>,
) -> Vec<u8> {
    let CsvOptions {
        delimiter,
        enclosure,
        escape,
    } = *opts;
    let mut out = Vec::new();
    for (i, f) in fields.iter().enumerate() {
        let f = f.as_ref();
        let needs = f.iter().any(|&c| {
            c == delimiter
                || c == enclosure
                || escape == Some(c)
                || matches!(c, b'\n' | b'\r' | b'\t' | b' ')
        });
        if needs {
            out.push(enclosure);
            let mut escaped = false;
            for &c in f {
                if escape == Some(c) {
                    escaped = true;
                } else if !escaped && c == enclosure {
                    out.push(enclosure);
                } else {
                    escaped = false;
                }
                out.push(c);
            }
            out.push(enclosure);
        } else {
            out.extend_from_slice(f);
        }
        if i + 1 != fields.len() {
            out.push(delimiter);
        }
    }
    out.extend_from_slice(eol.unwrap_or(b"\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> CsvOptions {
        CsvOptions::default()
    }

    fn strs(v: &[&str]) -> Vec<Vec<u8>> {
        v.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    #[test]
    fn basic_parse() {
        assert_eq!(str_getcsv(b"a,b,c", &opts()), Some(strs(&["a", "b", "c"])));
        assert_eq!(str_getcsv(b"", &opts()), None);
        assert_eq!(
            str_getcsv(b"\"a,b\",\"c\"\"d\",e\n", &opts()),
            Some(strs(&["a,b", "c\"d", "e"]))
        );
        assert_eq!(str_getcsv(b"\"a\nb\"", &opts()), Some(strs(&["a\nb"])));
        assert_eq!(str_getcsv(b"a,", &opts()), Some(strs(&["a", ""])));
    }

    #[test]
    fn multi_line() {
        let mut lines = vec![b"c\"\n".to_vec(), b"x\n".to_vec()].into_iter();
        let r = fgetcsv_parse(b"a,\"b\n".to_vec(), &opts(), &mut || lines.next());
        assert_eq!(r, Some(strs(&["a", "b\nc"])));
        assert_eq!(lines.next(), Some(b"x\n".to_vec()));
    }

    #[test]
    fn put() {
        assert_eq!(
            fputcsv_line(&["a", "b c", "d\"e", "f\\\"g"], &opts(), None),
            b"a,\"b c\",\"d\"\"e\",\"f\\\"g\"\n"
        );
        assert_eq!(fputcsv_line::<&str>(&[], &opts(), Some(b"\r\n")), b"\r\n");
    }

    #[test]
    fn options() {
        let mut n = Vec::new();
        let e = csv_options("str_getcsv", 2, Some(b""), None, Some(b""), &mut n).unwrap_err();
        assert_eq!(
            e.to_string(),
            "str_getcsv(): Argument #2 ($separator) must be a single character"
        );
        let e = csv_options("fputcsv", 3, None, None, Some(b"ab"), &mut n).unwrap_err();
        assert_eq!(
            e.to_string(),
            "fputcsv(): Argument #5 ($escape) must be empty or a single character"
        );
        assert!(n.is_empty());
        let o = csv_options("str_getcsv", 2, None, None, None, &mut n).unwrap_or_default();
        assert_eq!(o.escape, Some(b'\\'));
        assert_eq!(
            n[0].to_string(),
            "str_getcsv(): the $escape parameter must be provided as its default value will change"
        );
        assert!(fgetcsv_length(Some(-1)).is_err());
        assert_eq!(fgetcsv_length(Some(0)), Ok(None));
    }

    #[test]
    fn mb() {
        assert_eq!(mblen(b"\xc3\xa9"), 2);
        assert_eq!(mblen(b"\xed\xa0\x80"), -1);
        assert_eq!(mblen(b"\xf4\x90\x80\x80"), 4);
        assert_eq!(mblen(b"\xf8\x88\x80\x80\x80"), 5);
        assert_eq!(mblen(b"\xe0\x80\x80"), -1);
        assert_eq!(mblen(b"\xc3"), -1);
    }
}
