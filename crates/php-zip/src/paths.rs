// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Byte-level path helpers used by `ZipArchive::extractTo()` and the glob
//! options: a port of `virtual_file_ex(…, CWD_EXPAND)` (purely textual
//! `tsrm_realpath_r`), `php_zip_make_relative_path()`, `php_basename()`
//! and `zend_dirname()`.

/// `MAXPATHLEN` on Linux.
pub const MAXPATHLEN: usize = 4096;

fn is_slash(c: u8) -> bool {
    c == b'/'
}

/// `tsrm_realpath_r()` with `use_realpath == CWD_EXPAND` (no filesystem
/// access). `path` is a zero-padded C buffer. Returns the new length or
/// `None` for `(size_t)-1`.
fn realpath_r(path: &mut [u8], start: usize, mut len: usize) -> Option<usize> {
    loop {
        if len <= start {
            return Some(start);
        }
        let mut i = len;
        while i > start && !is_slash(path[i - 1]) {
            i -= 1;
        }
        if i == len || (i + 1 == len && path[i] == b'.') {
            // Remove double slashes and '.'.
            len = i.saturating_sub(1);
            continue;
        } else if i + 2 == len && path[i] == b'.' && path[i + 1] == b'.' {
            // Remove '..' and the previous directory.
            if i <= start + 1 {
                return Some(if start > 0 { start } else { len });
            }
            let r = realpath_r(path, start, i - 1);
            let mut j;
            match r {
                Some(jj) if jj > start => {
                    j = jj - 1;
                    while j > start && !is_slash(path[j]) {
                        j -= 1;
                    }
                    if start == 0 {
                        // Leading '..' must not be removed for relative paths.
                        if j == 0 && path[0] == b'.' && path[1] == b'.' && is_slash(path[2]) {
                            path[3] = b'.';
                            path[4] = b'.';
                            path[5] = b'/';
                            j = 5;
                        } else if j > 0
                            && path[j + 1] == b'.'
                            && path[j + 2] == b'.'
                            && is_slash(path[j + 3])
                        {
                            j += 4;
                            path[j] = b'.';
                            path[j + 1] = b'.';
                            path[j + 2] = b'/';
                            j += 2;
                        }
                    }
                }
                Some(0) if start == 0 => {
                    path[0] = b'.';
                    path[1] = b'.';
                    path[2] = b'/';
                    j = 2;
                }
                other => return other,
            }
            return Some(j);
        }
        path[len] = 0;
        let tmp: Vec<u8> = path[..=len].to_vec();
        let mut j = if i <= start + 1 {
            start
        } else {
            match realpath_r(path, start, i - 1) {
                Some(jj) => {
                    if jj > start {
                        path[jj] = b'/';
                        jj + 1
                    } else {
                        jj
                    }
                }
                None => return None,
            }
        };
        if j + len >= MAXPATHLEN - 1 + i {
            return None;
        }
        path[j..=j + (len - i)].copy_from_slice(&tmp[i..=len]);
        j += len - i;
        return Some(j);
    }
}

/// `virtual_file_ex()` with an empty working directory and `CWD_EXPAND`, as
/// `php_zip_extract_file()` uses it: collapses `.`, `..` and repeated
/// slashes textually. Returns `None` where the C function fails (the state
/// then keeps its empty path).
pub fn expand_lexically(path: &[u8]) -> Option<Vec<u8>> {
    let path_len = path.len();
    if path_len == 0 || path_len >= MAXPATHLEN - 1 {
        return None;
    }
    let mut buf = vec![0u8; MAXPATHLEN + 8];
    buf[..path_len].copy_from_slice(path);
    let start = usize::from(is_slash(path[0]));
    let add_slash = is_slash(buf[path_len - 1]);
    let mut len = realpath_r(&mut buf, start, path_len)?;
    if start == 0 && len == 0 {
        buf[0] = b'.';
        len = 1;
    }
    if add_slash && len > 0 && !is_slash(buf[len - 1]) {
        if len >= MAXPATHLEN - 1 {
            return None;
        }
        buf[len] = b'/';
        len += 1;
    }
    buf.truncate(len);
    Some(buf)
}

/// `php_zip_make_relative_path()`: strips a leading `/` and everything up
/// to the last `./` or `../` component. `None` for an empty path.
pub fn make_relative(path: &[u8]) -> Option<&[u8]> {
    if path.is_empty() {
        return None;
    }
    if is_slash(path[0]) {
        return Some(&path[1..]);
    }
    let at = |i: usize| path.get(i).copied().unwrap_or(0);
    let mut i = path.len();
    loop {
        while i > 0 && !is_slash(at(i)) {
            i -= 1;
        }
        if i == 0 {
            return Some(path);
        }
        if i >= 2 && at(i - 1) == b'.' {
            return Some(&path[(i + 1).min(path.len())..]);
        }
        i -= 1;
    }
}

/// `php_basename()` without a suffix (C locale).
pub fn basename(s: &[u8]) -> &[u8] {
    let mut end = s.len();
    while end > 0 && is_slash(s[end - 1]) {
        end -= 1;
    }
    if end == 0 {
        return b"";
    }
    let mut start = end - 1;
    while start > 0 && !is_slash(s[start - 1]) {
        start -= 1;
    }
    &s[start..end]
}

/// `zend_dirname()`.
pub fn dirname(path: &[u8]) -> Vec<u8> {
    if path.is_empty() {
        return Vec::new();
    }
    let mut end = path.len();
    while end > 0 && is_slash(path[end - 1]) {
        end -= 1;
    }
    if end == 0 {
        return b"/".to_vec();
    }
    while end > 0 && !is_slash(path[end - 1]) {
        end -= 1;
    }
    if end == 0 {
        return b".".to_vec();
    }
    while end > 0 && is_slash(path[end - 1]) {
        end -= 1;
    }
    if end == 0 {
        return b"/".to_vec();
    }
    path[..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exp(p: &str) -> Option<String> {
        expand_lexically(p.as_bytes()).map(|v| String::from_utf8(v).unwrap())
    }

    #[test]
    fn expand() {
        assert_eq!(exp("a/b/../c").as_deref(), Some("a/c"));
        assert_eq!(exp("../../a").as_deref(), Some("../../a"));
        assert_eq!(exp("./a//b/").as_deref(), Some("a/b/"));
        assert_eq!(exp("/x/../y").as_deref(), Some("/y"));
        assert_eq!(exp("/../../etc/passwd").as_deref(), Some("/etc/passwd"));
        assert_eq!(exp(".").as_deref(), Some("."));
        assert_eq!(exp("a/..").as_deref(), Some("."));
        assert_eq!(exp("").as_deref(), None);
        assert_eq!(exp("..").as_deref(), Some(".."));
        assert_eq!(exp("a/../../b").as_deref(), Some("../b"));
    }

    #[test]
    fn relative() {
        assert_eq!(make_relative(b"/etc/passwd"), Some(&b"etc/passwd"[..]));
        assert_eq!(make_relative(b"../../a/b"), Some(&b"a/b"[..]));
        assert_eq!(make_relative(b"a/b"), Some(&b"a/b"[..]));
        assert_eq!(make_relative(b""), None);
        assert_eq!(make_relative(b".."), Some(&b".."[..]));
    }

    #[test]
    fn base_and_dir() {
        assert_eq!(basename(b"a/b/c.txt"), b"c.txt");
        assert_eq!(basename(b"c.txt"), b"c.txt");
        assert_eq!(basename(b"a/b/"), b"b");
        assert_eq!(basename(b"///"), b"");
        assert_eq!(dirname(b"a/b/c.txt"), b"a/b");
        assert_eq!(dirname(b"c.txt"), b".");
        assert_eq!(dirname(b"/c.txt"), b"/");
        assert_eq!(dirname(b"a//b"), b"a");
    }
}
