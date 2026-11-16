// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `fopen()` mode parsing (`php_stream_parse_fopen_modes`,
//! `php_stream_mode_from_str`).

/// Parsed `open()` flags of an fopen mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct FopenFlags {
    /// Readable.
    pub read: bool,
    /// Writable.
    pub write: bool,
    /// `O_APPEND`.
    pub append: bool,
    /// `O_CREAT`.
    pub create: bool,
    /// `O_TRUNC`.
    pub truncate: bool,
    /// `O_EXCL`.
    pub exclusive: bool,
    /// `O_NONBLOCK` (`n` flag).
    pub nonblock: bool,
    /// `O_CLOEXEC` (`e` flag).
    pub cloexec: bool,
}

/// `php_stream_parse_fopen_modes()`: `None` for an unknown mode letter.
///
/// Only the first character selects the base mode; `+` anywhere adds
/// read/write, `e` and `n` anywhere add `O_CLOEXEC` / `O_NONBLOCK`; `b` and
/// `t` are ignored on POSIX.
pub fn parse_fopen_mode(mode: &str) -> Option<FopenFlags> {
    let m = mode.as_bytes();
    let mut f = FopenFlags::default();
    match m.first()? {
        b'r' => {}
        b'w' => {
            f.truncate = true;
            f.create = true;
        }
        b'a' => {
            f.create = true;
            f.append = true;
        }
        b'x' => {
            f.create = true;
            f.exclusive = true;
        }
        b'c' => f.create = true,
        _ => return None,
    }
    let base_writes = m[0] != b'r';
    if m.contains(&b'+') {
        f.read = true;
        f.write = true;
    } else if base_writes {
        f.write = true;
    } else {
        f.read = true;
    }
    f.cloexec = m.contains(&b'e');
    f.nonblock = m.contains(&b'n');
    Some(f)
}

/// Memory/temp stream modes (`TEMP_STREAM_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempMode {
    /// `TEMP_STREAM_DEFAULT`.
    Default,
    /// `TEMP_STREAM_READONLY`.
    ReadOnly,
    /// `TEMP_STREAM_APPEND`.
    Append,
}

impl TempMode {
    /// `php_stream_mode_from_str()`.
    pub fn from_mode(mode: &str) -> Self {
        if mode.contains('a') {
            TempMode::Append
        } else if mode.contains(['w', '+']) {
            TempMode::Default
        } else {
            TempMode::ReadOnly
        }
    }

    /// `_php_stream_mode_to_str()`.
    pub fn as_mode(self) -> &'static str {
        match self {
            TempMode::ReadOnly => "rb",
            TempMode::Append => "a+b",
            TempMode::Default => "w+b",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn modes() {
        let r = parse_fopen_mode("r").unwrap();
        assert!(r.read && !r.write && !r.create);
        let w = parse_fopen_mode("wb").unwrap();
        assert!(!w.read && w.write && w.truncate && w.create);
        let a = parse_fopen_mode("a+").unwrap();
        assert!(a.read && a.write && a.append);
        let x = parse_fopen_mode("x+e").unwrap();
        assert!(x.exclusive && x.cloexec && x.read);
        let c = parse_fopen_mode("ct").unwrap();
        assert!(c.create && !c.truncate && c.write);
        assert!(parse_fopen_mode("Q").is_none());
        assert!(parse_fopen_mode("").is_none());
        assert!(parse_fopen_mode("rw").unwrap().read);
    }

    #[test]
    fn temp_modes() {
        assert_eq!(TempMode::from_mode("r"), TempMode::ReadOnly);
        assert_eq!(TempMode::from_mode("r+"), TempMode::Default);
        assert_eq!(TempMode::from_mode("wb"), TempMode::Default);
        assert_eq!(TempMode::from_mode("a"), TempMode::Append);
        assert_eq!(TempMode::Default.as_mode(), "w+b");
    }
}
