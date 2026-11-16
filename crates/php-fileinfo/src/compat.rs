// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Work-arounds for behaviour of the system libmagic (5.45) that PHP's
//! bundled, patched copy does not share.

use std::path::{Path, PathBuf};

/// php-src's `simh_parse()` (`is_simh.c`), which is stricter than the one
/// in libmagic 5.45: data that merely *starts* with a plausible record
/// length (e.g. 250000 × `0x01`) is not SIMH tape data.
pub(crate) fn is_simh(data: &[u8]) -> bool {
    const TAPEMARKS: usize = 10;
    let getlen = |pos: usize| -> Result<u32, ()> {
        let n = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        if n == 0xFFFF_FFFF {
            return Ok(n);
        }
        if (n & 0x00FF_FFFF) != (n & 0x0FFF_FFFF) {
            return Err(());
        }
        let n = n & 0x00FF_FFFF;
        Ok(if n & 1 == 1 { n + 1 } else { n })
    };
    let end = data.len();
    let mut pos = 0usize;
    let (mut nt, mut nr) = (0usize, 0usize);
    while end.saturating_sub(pos) >= 4 && pos <= end {
        let Ok(nbytes) = getlen(pos) else {
            return false;
        };
        pos += 4;
        if (nt > 0 || nr > 0) && nbytes == 0xFFFF_FFFF {
            break;
        }
        if nbytes == 0 {
            nt += 1;
            if nt == TAPEMARKS {
                break;
            }
            continue;
        }
        pos += nbytes as usize;
        if pos > end || end - pos < 4 {
            break;
        }
        let Ok(cbytes) = getlen(pos) else {
            return false;
        };
        pos += 4;
        if nbytes != cbytes {
            return false;
        }
        nr += 1;
    }
    if nt * 4 == pos {
        return false;
    }
    nr != 0
}

/// Whether `desc` is what libmagic's built-in SIMH test prints.
pub(crate) fn is_simh_description(desc: &[u8]) -> bool {
    desc == b"SIMH tape data" || desc == b"application/SIMH-tape-data"
}

/// C `isprint()` / `isspace()` in the "C" locale.
fn regex_char_ok(c: u8) -> bool {
    (0x20..0x7f).contains(&c) || matches!(c, b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | 0x08 | 0x8a)
}

/// libmagic 5.45 rejects `regex` patterns containing non-printable bytes
/// ("non-ascii characters in regex"), which PHP's PCRE-based copy accepts;
/// such bytes are rewritten to `.` (any character).  A pattern that starts
/// with a NUL is empty to POSIX `regcomp()` and would match everything,
/// while PHP hands the NULs to PCRE (bug #79283); it becomes the
/// never-matching `a^`.  Returns `None` when the line needs no change.
#[allow(clippy::many_single_char_names)]
fn sanitize_line(line: &[u8]) -> Option<Vec<u8>> {
    if line.first().is_none_or(|&c| c == b'#' || c == b'!') {
        return None;
    }
    let is_ws = |c: u8| c == b' ' || c == b'\t';
    let mut i = 0;
    // Offset (with its leading '>' continuation levels).
    while i < line.len() && !is_ws(line[i]) {
        i += 1;
    }
    while i < line.len() && is_ws(line[i]) {
        i += 1;
    }
    let type_start = i;
    while i < line.len() && !is_ws(line[i]) {
        i += 1;
    }
    if !line[type_start..i].starts_with(b"regex") {
        return None;
    }
    while i < line.len() && is_ws(line[i]) {
        i += 1;
    }
    let mut out = line[..i].to_vec();
    let mut changed = false;
    if starts_with_nul_escape(&line[i..]) {
        let mut end = i;
        while end < line.len() && !is_ws(line[end]) {
            end += if line[end] == b'\\' { 2 } else { 1 };
        }
        let end = end.min(line.len());
        out.extend_from_slice(b"a^");
        out.extend_from_slice(&line[end..]);
        return Some(out);
    }
    while i < line.len() && !is_ws(line[i]) {
        let c = line[i];
        if c != b'\\' || i + 1 >= line.len() {
            if regex_char_ok(c) {
                out.push(c);
            } else {
                out.push(b'.');
                changed = true;
            }
            i += 1;
            continue;
        }
        // An escape: decode it the way apprentice.c's getstr() does.
        let start = i;
        i += 1;
        let e = line[i];
        let value = match e {
            b'0'..=b'7' => {
                let mut v = 0u32;
                let mut n = 0;
                while n < 3 && i < line.len() && (b'0'..=b'7').contains(&line[i]) {
                    v = v * 8 + u32::from(line[i] - b'0');
                    i += 1;
                    n += 1;
                }
                i -= 1;
                Some(v & 0xff)
            }
            b'x' => {
                let mut v = 0u32;
                let mut n = 0;
                while n < 2 && i + 1 < line.len() && line[i + 1].is_ascii_hexdigit() {
                    v = v * 16 + char::from(line[i + 1]).to_digit(16).unwrap_or(0);
                    i += 1;
                    n += 1;
                }
                (n > 0).then_some(v)
            }
            b'a' => Some(7),
            b'f' => Some(0x0c),
            b'v' => Some(0x0b),
            b'e' => Some(0x1b),
            _ => None,
        };
        i += 1;
        // NUL ends the C string, so it is never inspected.
        match value {
            Some(v) if v != 0 && !regex_char_ok(u8::try_from(v).unwrap_or(0)) => {
                out.push(b'.');
                changed = true;
            }
            _ => out.extend_from_slice(&line[start..i]),
        }
    }
    out.extend_from_slice(&line[i..]);
    changed.then_some(out)
}

/// Whether a magic test value begins with an escape decoding to NUL
/// (`\0`, `\00`, `\000`, `\x0`, `\x00`).
fn starts_with_nul_escape(value: &[u8]) -> bool {
    let digits = |rest: &[u8], max: usize, ok: fn(&u8) -> bool| -> Option<bool> {
        let run: Vec<u8> = rest
            .iter()
            .take(max)
            .take_while(|c| ok(c))
            .copied()
            .collect();
        (!run.is_empty()).then(|| run.iter().all(|&c| c == b'0'))
    };
    match value {
        [b'\\', b'x', rest @ ..] => digits(rest, 2, u8::is_ascii_hexdigit) == Some(true),
        [b'\\', rest @ ..] => digits(rest, 3, |c| (b'0'..=b'7').contains(c)) == Some(true),
        _ => false,
    }
}

/// The sanitized text of a magic file, if anything needed rewriting.
fn sanitize(text: &[u8]) -> Option<Vec<u8>> {
    let mut changed = false;
    let mut out = Vec::with_capacity(text.len());
    for line in text.split_inclusive(|&b| b == b'\n') {
        let (body, nl) = match line.strip_suffix(b"\n") {
            Some(b) => (b, &b"\n"[..]),
            None => (line, &b""[..]),
        };
        if let Some(fixed) = sanitize_line(body) {
            out.extend_from_slice(&fixed);
            changed = true;
        } else {
            out.extend_from_slice(body);
        }
        out.extend_from_slice(nl);
    }
    changed.then_some(out)
}

/// C `text_chars[c] == T` of php-src's libmagic 5.48 (`encoding.c`) for the
/// ASCII range: BEL..CR, SUB, ESC and the printable characters.  5.48 added
/// SUB (`^Z`, the DOS end-of-file mark); 5.45 treats it as binary.
fn is_text_ascii(c: u8) -> bool {
    matches!(c, 0x07..=0x0d | 0x1a | 0x1b | 0x20..=0x7e)
}

/// libmagic 5.48 counts SUB (0x1a) as a text character, so data that is
/// text apart from SUB bytes takes the text path (binary-only `/b` tests
/// are skipped, `ascmagic` describes it).  The system 5.45 would call it
/// binary; for such data the SUB bytes within the encoding window
/// (`encoding_max`, what `file_encoding()` inspects) are replaced by spaces,
/// which 5.45 classifies the same way 5.48 classifies SUB.  UTF-16/32 data
/// (byte-order mark) is left alone: there 0x1a is half of a character.
/// Returns `None` when nothing needs rewriting.
pub(crate) fn sub_as_text(data: &[u8], encoding_max: usize) -> Option<Vec<u8>> {
    let n = data.len().min(encoding_max);
    let head = &data[..n];
    if !head.contains(&0x1a) || head.starts_with(&[0xff, 0xfe]) || head.starts_with(&[0xfe, 0xff]) {
        return None;
    }
    if !head.iter().all(|&c| c >= 0x80 || is_text_ascii(c)) {
        return None;
    }
    let mut out = data.to_vec();
    for c in &mut out[..n] {
        if *c == 0x1a {
            *c = b' ';
        }
    }
    Some(out)
}

/// Magic entries of php-src's bundled libmagic 5.48 database that the
/// system 5.45 database describes differently (see the file's header);
/// loaded in front of the default database.
pub(crate) const OVERLAY: &[u8] = include_bytes!("overlay.magic");

/// Writes [`OVERLAY`] to a temporary file for `magic_load()` (which only
/// parses text databases from paths); returns its path plus the guard
/// removing it, which may be dropped once the database is loaded.
pub(crate) fn overlay_source() -> Option<(PathBuf, TempDir)> {
    let tmp = TempDir::new()?;
    let file = tmp.0.join("php-overlay.magic");
    std::fs::write(&file, OVERLAY).ok()?;
    Some((file, tmp))
}

/// A temporary directory removed on drop.
#[derive(Debug)]
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Option<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        for _ in 0..16 {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir()
                .join(format!(".php-fileinfo-magic-{}-{n}", std::process::id()));
            if std::fs::create_dir(&p).is_ok() {
                return Some(Self(p));
            }
        }
        None
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Rewrites the text magic sources `path` (a file or a directory of
/// files) into a temporary location when libmagic 5.45 would reject them;
/// returns the path to load instead plus the guard keeping it alive.
pub(crate) fn sanitized_source(path: &Path) -> Option<(PathBuf, TempDir)> {
    if path.is_dir() {
        let mut files: Vec<(std::ffi::OsString, Vec<u8>, bool)> = Vec::new();
        let mut any = false;
        for entry in std::fs::read_dir(path).ok()?.flatten() {
            let p = entry.path();
            if !p.metadata().is_ok_and(|m| m.is_file()) {
                continue;
            }
            let text = std::fs::read(&p).ok()?;
            match sanitize(&text) {
                Some(fixed) => {
                    any = true;
                    files.push((entry.file_name(), fixed, true));
                }
                None => files.push((entry.file_name(), text, false)),
            }
        }
        if !any {
            return None;
        }
        let tmp = TempDir::new()?;
        for (name, data, _) in &files {
            std::fs::write(tmp.0.join(name), data).ok()?;
        }
        let dir = tmp.0.clone();
        Some((dir, tmp))
    } else {
        let fixed = sanitize(&std::fs::read(path).ok()?)?;
        let tmp = TempDir::new()?;
        let name = path
            .file_name()
            .map_or_else(|| "magic".into(), ToOwned::to_owned);
        let file = tmp.0.join(name);
        std::fs::write(&file, fixed).ok()?;
        Some((file, tmp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simh_rejects_unterminated_records() {
        assert!(!is_simh(&[1u8; 1000]));
        assert!(!is_simh(&[0u8; 64]));
        // One 2-byte record framed by its length twice.
        let rec = [2, 0, 0, 0, b'h', b'i', 2, 0, 0, 0, 0xff, 0xff, 0xff, 0xff];
        assert!(is_simh(&rec));
        // Mismatched trailer.
        let bad = [2, 0, 0, 0, b'h', b'i', 4, 0, 0, 0];
        assert!(!is_simh(&bad));
    }

    #[test]
    fn sub_counts_as_text() {
        // finfo_buffer_basic.phpt's MIFF lookalike.
        assert_eq!(
            sub_as_text(b"id=ImageMagick\x0a\x0c\x0a:\x1a", 65536).unwrap(),
            b"id=ImageMagick\x0a\x0c\x0a: "
        );
        assert_eq!(sub_as_text(b"a\x1a\xe9b", 65536).unwrap(), b"a \xe9b");
        // Only the encoding window is inspected and rewritten.
        assert_eq!(sub_as_text(b"ab\x1a\x1a", 3).unwrap(), b"ab \x1a");
        assert!(sub_as_text(b"ab\x00\x1a", 65536).is_none());
        assert!(sub_as_text(b"plain text\n", 65536).is_none());
        assert!(sub_as_text(b"\x89PNG\r\n\x1a\n\x00\x00", 65536).is_none());
        assert!(sub_as_text(b"\xff\xfea\x00\x1a\x00", 65536).is_none());
        assert!(sub_as_text(b"ab\x7f\x1a", 65536).is_none());
    }

    #[test]
    fn overlay_is_written() {
        let (path, guard) = overlay_source().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), OVERLAY);
        drop(guard);
        assert!(!path.exists());
        assert!(OVERLAY.starts_with(b"# php-fileinfo overlay"));
    }

    #[test]
    fn regex_lines_are_sanitized() {
        assert_eq!(
            sanitize_line(b">7\tregex/9 V[.0-9]{4,5}\\020\t%s").unwrap(),
            b">7\tregex/9 V[.0-9]{4,5}.\t%s"
        );
        assert!(sanitize_line(b"0\tregex\t\\^[\\040\\t]{0,50}\\\\.byte\t\tasm").is_none());
        assert!(sanitize_line(b">0\tregex\tdex\\n[0-9]{2}\\0\tDalvik").is_none());
        assert!(sanitize_line(b">>5\tregex/s\t[=.<>|!^\\x8a]{79}\tAW").is_none());
        assert!(sanitize_line(b"0\tstring\t\\020\tx").is_none());
        assert!(sanitize_line(b"# regex \\020").is_none());
        assert_eq!(
            sanitize(b"a\n0 regex \\x01b x\n").unwrap(),
            b"a\n0 regex .b x\n"
        );
        assert!(sanitize(b"a\nb").is_none());
        assert_eq!(
            sanitize_line(b"0\tregex\t\\0\\0\\0\\0\tTest").unwrap(),
            b"0\tregex\ta^\tTest"
        );
        assert!(sanitize_line(b"0\tregex\t\\040x\tT").is_none());
        assert!(starts_with_nul_escape(b"\\x00a"));
        assert!(starts_with_nul_escape(b"\\000"));
        assert!(!starts_with_nul_escape(b"\\01"));
        assert!(!starts_with_nul_escape(b"\\x"));
    }
}
