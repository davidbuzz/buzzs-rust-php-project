// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Directory listing post-processing (`ftp_genlist`, `ftp_mlsd_parse_line`)
//! and the ASCII-mode line-ending conversions of transfers.

use crate::text::cstr;

/// Splits a listing received on the data connection into entries the way
/// `ftp_genlist` does: entries end at `"\r\n"`, a trailing piece without
/// one is dropped, and each entry is a C string (cut at the first NUL).
pub fn split_listing(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut lastch = 0u8;
    for &ch in data {
        if ch == b'\n' && lastch == b'\r' {
            cur.pop();
            out.push(cstr(&cur).to_vec());
            cur.clear();
        } else {
            cur.push(ch);
        }
        lastch = ch;
    }
    out
}

/// Why an `MLSD` line was rejected (the warning text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MlsdError {
    /// No space separating the facts from the pathname.
    MissingPathname,
    /// A fact without `;` terminator or without `=`.
    MalformedFact,
}

impl MlsdError {
    /// The warning PHP emits.
    pub fn message(self) -> &'static str {
        match self {
            MlsdError::MissingPathname => "Missing pathname in MLSD response",
            MlsdError::MalformedFact => "Malformed fact in MLSD response",
        }
    }
}

/// One parsed `MLSD` entry: `("name", pathname)` first, then each
/// `fact=value` in order. A repeated key keeps its first position and
/// takes the last value (hash-update semantics).
pub type MlsdEntry = Vec<(Vec<u8>, Vec<u8>)>;

/// `ftp_mlsd_parse_line`.
pub fn parse_mlsd_line(line: &[u8]) -> Result<MlsdEntry, MlsdError> {
    let line = cstr(line);
    let sp = line
        .iter()
        .position(|&c| c == b' ')
        .ok_or(MlsdError::MissingPathname)?;
    let mut entry: MlsdEntry = Vec::new();
    let mut update = |k: &[u8], v: &[u8]| {
        if let Some(slot) = entry.iter_mut().find(|(ek, _)| ek == k) {
            slot.1 = v.to_vec();
        } else {
            entry.push((k.to_vec(), v.to_vec()));
        }
    };
    update(b"name", &line[sp + 1..]);
    let mut facts = &line[..sp];
    while !facts.is_empty() {
        let semi = facts
            .iter()
            .position(|&c| c == b';')
            .ok_or(MlsdError::MalformedFact)?;
        let eq = facts[..semi]
            .iter()
            .position(|&c| c == b'=')
            .ok_or(MlsdError::MalformedFact)?;
        update(&facts[..eq], &facts[eq + 1..semi]);
        facts = &facts[semi + 1..];
    }
    Ok(entry)
}

/// The receive-side ASCII conversion of a blocking `ftp_get`: `"\r\n"`
/// becomes `"\n"`, bare `"\r"` is kept; a `"\r"` ending a chunk is held in
/// `pending_cr` until the next chunk (or the end of the transfer) decides.
pub fn ascii_get_chunk(chunk: &[u8], pending_cr: &mut bool, out: &mut Vec<u8>) {
    let mut rest = chunk;
    if *pending_cr && !rest.is_empty() {
        *pending_cr = false;
        if rest[0] == b'\n' {
            out.push(b'\n');
            rest = &rest[1..];
        } else {
            out.push(b'\r');
        }
    }
    while let Some(p) = rest.iter().position(|&c| c == b'\r') {
        if p + 1 < rest.len() {
            if rest[p + 1] == b'\n' {
                out.extend_from_slice(&rest[..p]);
                out.push(b'\n');
                rest = &rest[p + 2..];
            } else {
                out.extend_from_slice(&rest[..=p]);
                rest = &rest[p + 1..];
            }
        } else {
            out.extend_from_slice(&rest[..p]);
            *pending_cr = true;
            rest = &[];
        }
    }
    out.extend_from_slice(rest);
}

/// The receive-side ASCII conversion of `ftp_nb_continue` (byte by byte
/// with the last character carried between calls in `lastch`).
pub fn ascii_nb_chunk(chunk: &[u8], lastch: &mut u8, out: &mut Vec<u8>) {
    for &c in chunk {
        if *lastch == b'\r' && c != b'\n' {
            out.push(b'\r');
        }
        if c != b'\r' {
            out.push(c);
        }
        *lastch = c;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_split() {
        assert_eq!(
            split_listing(b"file1\r\nfile1\r\nfile\nb0rk\r\n"),
            vec![b"file1".to_vec(), b"file1".to_vec(), b"file\nb0rk".to_vec()]
        );
        assert_eq!(split_listing(b"a\r\npartial"), vec![b"a".to_vec()]);
        assert_eq!(split_listing(b"a\0b\r\n"), vec![b"a".to_vec()]);
        assert!(split_listing(b"").is_empty());
    }

    #[test]
    fn mlsd_lines() {
        let e = parse_mlsd_line(b"type=file;size=12; foo bar").unwrap();
        assert_eq!(
            e,
            vec![
                (b"name".to_vec(), b"foo bar".to_vec()),
                (b"type".to_vec(), b"file".to_vec()),
                (b"size".to_vec(), b"12".to_vec()),
            ]
        );
        let e = parse_mlsd_line(b"fact=val=ue;empty=; path;name").unwrap();
        assert_eq!(e[0].1, b"path;name");
        assert_eq!(e[1], (b"fact".to_vec(), b"val=ue".to_vec()));
        assert_eq!(e[2], (b"empty".to_vec(), Vec::new()));
        assert_eq!(
            parse_mlsd_line(b"no_space"),
            Err(MlsdError::MissingPathname)
        );
        assert_eq!(parse_mlsd_line(b"no_semi x"), Err(MlsdError::MalformedFact));
        assert_eq!(parse_mlsd_line(b"no_eq; x"), Err(MlsdError::MalformedFact));
        let e = parse_mlsd_line(b"name=x; y").unwrap();
        assert_eq!(e, vec![(b"name".to_vec(), b"x".to_vec())]);
    }

    #[test]
    fn ascii_blocking() {
        let mut out = Vec::new();
        let mut cr = false;
        ascii_get_chunk(b"a\r\nb\rc\r", &mut cr, &mut out);
        assert!(cr);
        ascii_get_chunk(b"\nd\r", &mut cr, &mut out);
        ascii_get_chunk(b"e", &mut cr, &mut out);
        assert_eq!(out, b"a\nb\rc\nd\re");
    }

    #[test]
    fn ascii_nonblocking() {
        let mut out = Vec::new();
        let mut last = 0;
        ascii_nb_chunk(b"a\r\nb\rc\r", &mut last, &mut out);
        ascii_nb_chunk(b"\nd", &mut last, &mut out);
        assert_eq!(out, b"a\nb\rc\nd");
    }
}
