//! Path helpers from `main/fopen_wrappers.c` and `TSRM/tsrm_virtual_cwd.c`:
//! `expand_filepath()`, `open_basedir` checks, `php_resolve_path()` and
//! `realpath()`.

use crate::diag;
use crate::state;
use crate::sys;
use crate::util::{os_path, path_bytes};
use crate::wrapper::{self, STREAM_OPEN_FOR_INCLUDE, URL_STAT_QUIET};

/// `MAXPATHLEN` on Linux.
pub const MAXPATHLEN: usize = 4096;

/// `IS_ABSOLUTE_PATH` on POSIX.
pub fn is_absolute(path: &[u8]) -> bool {
    path.first() == Some(&b'/')
}

/// Lexically normalises `path` against `cwd`: collapses repeated slashes,
/// `.` and `..` components (`virtual_file_ex` with `CWD_EXPAND`).
pub fn normalize(cwd: &[u8], path: &[u8]) -> Vec<u8> {
    let mut parts: Vec<&[u8]> = Vec::new();
    let base: &[u8] = if is_absolute(path) { b"" } else { cwd };
    for comp in base.split(|&c| c == b'/').chain(path.split(|&c| c == b'/')) {
        match comp {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    let mut out = Vec::with_capacity(path.len() + cwd.len());
    for p in parts {
        out.push(b'/');
        out.extend_from_slice(p);
    }
    if out.is_empty() {
        out.push(b'/');
    }
    out
}

/// `expand_filepath()`: makes `path` absolute relative to the current
/// directory and normalises it. `None` for an empty or too long path.
pub fn expand_filepath(path: &[u8]) -> Option<Vec<u8>> {
    expand_filepath_relative(path, None)
}

/// `expand_filepath_ex()` with an explicit base directory.
pub fn expand_filepath_relative(path: &[u8], relative_to: Option<&[u8]>) -> Option<Vec<u8>> {
    if path.is_empty() {
        return None;
    }
    let cwd = if is_absolute(path) {
        Vec::new()
    } else if let Some(r) = relative_to {
        if r.len() > MAXPATHLEN - 1 {
            return None;
        }
        r.to_vec()
    } else {
        sys::getcwd().unwrap_or_default()
    };
    let out = normalize(&cwd, path);
    if out.len() >= MAXPATHLEN {
        return None;
    }
    Some(out)
}

/// `realpath()` / `tsrm_realpath()`: canonical absolute path of an
/// existing file.
pub fn realpath(path: &[u8]) -> Option<Vec<u8>> {
    let p = if path.is_empty() { b"." as &[u8] } else { path };
    std::fs::canonicalize(os_path(p))
        .ok()
        .map(|p| path_bytes(&p))
}

fn readlink(path: &[u8]) -> Option<Vec<u8>> {
    std::fs::read_link(os_path(path))
        .ok()
        .map(|p| path_bytes(&p))
}

/// `php_check_specific_open_basedir()`: true when `path` is inside
/// `basedir`.
pub fn in_specific_basedir(basedir: &[u8], path: &[u8]) -> bool {
    let local_basedir = if basedir == b"." {
        sys::getcwd().unwrap_or_else(|| basedir.to_vec())
    } else {
        basedir.to_vec()
    };
    if path.len() > MAXPATHLEN - 1 {
        return false;
    }
    let Some(expanded) = expand_filepath(path) else {
        return false;
    };
    let mut path_tmp = expanded.clone();
    let mut nesting = 0;
    let mut resolved_name;
    let mut tail_is_slash = false;
    loop {
        if let Some(r) = realpath(&path_tmp) {
            resolved_name = r;
            tail_is_slash = nesting == 0 && path_tmp.last() == Some(&b'/');
            break;
        }
        if nesting == 0 {
            if let Some(target) = readlink(&path_tmp) {
                path_tmp = target;
            }
        }
        let Some(slash) = path_tmp.iter().rposition(|&c| c == b'/') else {
            return false;
        };
        path_tmp.truncate(slash);
        if path_tmp.is_empty() {
            resolved_name = expanded;
            break;
        }
        nesting += 1;
    }
    let Some(mut resolved_basedir) = expand_filepath(&local_basedir) else {
        return false;
    };
    if basedir.last() == Some(&b'/') {
        if resolved_basedir.last() != Some(&b'/') {
            resolved_basedir.push(b'/');
        }
    } else {
        resolved_basedir.push(b'/');
    }
    if tail_is_slash && resolved_name.last() != Some(&b'/') {
        resolved_name.push(b'/');
    }
    let blen = resolved_basedir.len();
    if resolved_name.starts_with(&resolved_basedir) {
        !(resolved_name.len() > blen && resolved_name[blen - 1] != b'/')
    } else {
        blen == resolved_name.len() + 1
            && resolved_basedir[blen - 1] == b'/'
            && resolved_basedir[..resolved_name.len()] == resolved_name[..]
    }
}

/// `php_check_open_basedir()`: true when access is allowed; emits the
/// restriction warning otherwise.
pub fn check_open_basedir(path: &[u8]) -> bool {
    check_open_basedir_ex(path, true)
}

/// `php_check_open_basedir_ex()`.
pub fn check_open_basedir_ex(path: &[u8], warn: bool) -> bool {
    let basedir = state::settings().open_basedir;
    if basedir.is_empty() {
        return true;
    }
    if path.len() > MAXPATHLEN - 1 {
        diag::warning(diag::msg(&[
            format!("File name is longer than the maximum allowed path length on this platform ({MAXPATHLEN}): ")
                .as_bytes(),
            path,
        ]));
        return false;
    }
    for dir in basedir.split(|&c| c == b':') {
        if dir.is_empty() {
            continue;
        }
        if in_specific_basedir(dir, path) {
            return true;
        }
    }
    if warn {
        diag::warning(diag::msg(&[
            b"open_basedir restriction in effect. File(",
            path,
            b") is not within the allowed path(s): (",
            &basedir,
            b")",
        ]));
    }
    false
}

fn has_url_scheme(s: &[u8]) -> Option<usize> {
    let n = wrapper::scheme_len(s);
    (s.get(n) == Some(&b':') && n > 1 && s.get(n + 1) == Some(&b'/') && s.get(n + 2) == Some(&b'/'))
        .then_some(n)
}

/// `php_resolve_path()`: resolves `filename` against `include_path` (used
/// by `include`, `fopen(..., use_include_path)` etc.). Returns the real
/// path of the first match.
pub fn resolve_path(filename: &[u8], include_path: &[u8]) -> Option<Vec<u8>> {
    if filename.contains(&0) {
        return None;
    }
    if has_url_scheme(filename).is_some() {
        let loc = wrapper::locate(filename, STREAM_OPEN_FOR_INCLUDE)?;
        if loc.wrapper.is_plain_files() {
            return realpath(&loc.path);
        }
        return None;
    }
    let is_slash = |c: Option<&u8>| c == Some(&b'/');
    if (filename.first() == Some(&b'.')
        && (is_slash(filename.get(1))
            || (filename.get(1) == Some(&b'.') && is_slash(filename.get(2)))))
        || is_absolute(filename)
        || include_path.is_empty()
    {
        return realpath(filename);
    }
    let mut rest = include_path;
    loop {
        if rest.is_empty() {
            break;
        }
        let mut is_stream_wrapper = false;
        let mut scan_from = 0;
        let n = wrapper::scheme_len(rest);
        if rest.get(n) == Some(&b':')
            && n > 1
            && rest.get(n + 1) == Some(&b'/')
            && rest.get(n + 2) == Some(&b'/')
        {
            if !(rest[n - 1] == b'.' && rest[n - 2] == b'.' && n == 2) {
                scan_from = n + 3;
                is_stream_wrapper = true;
            }
        }
        let (entry, next): (&[u8], Option<&[u8]>) = match rest[scan_from.min(rest.len())..]
            .iter()
            .position(|&c| c == b':')
        {
            Some(i) => (&rest[..scan_from + i], Some(&rest[scan_from + i + 1..])),
            None => (rest, None),
        };
        if filename.len() > MAXPATHLEN - 2 || entry.len() + 1 + filename.len() + 1 >= MAXPATHLEN {
            match next {
                Some(n) => {
                    rest = n;
                    continue;
                }
                None => break,
            }
        }
        let mut trypath = entry.to_vec();
        trypath.push(b'/');
        trypath.extend_from_slice(filename);
        rest = next.unwrap_or(b"");
        let mut actual = trypath.clone();
        if is_stream_wrapper {
            let Some(loc) = wrapper::locate(&trypath, STREAM_OPEN_FOR_INCLUDE) else {
                if next.is_none() {
                    break;
                }
                continue;
            };
            if !loc.wrapper.is_plain_files() {
                if let Some(Ok(_)) = loc.wrapper.url_stat(&trypath, URL_STAT_QUIET, None) {
                    return Some(trypath);
                }
                if next.is_none() {
                    break;
                }
                continue;
            }
            actual = loc.path;
        }
        if let Some(r) = realpath(&actual) {
            return Some(r);
        }
        if next.is_none() {
            break;
        }
    }
    let exec = state::executed_filename()?;
    let mut len = exec.len();
    while len > 0 {
        len -= 1;
        if exec[len] == b'/' {
            break;
        }
    }
    if len > 0 && filename.len() < MAXPATHLEN - 2 && len + 1 + filename.len() + 1 < MAXPATHLEN {
        let mut trypath = exec[..=len].to_vec();
        trypath.extend_from_slice(filename);
        if has_url_scheme(&trypath).is_some() {
            let loc = wrapper::locate(&trypath, STREAM_OPEN_FOR_INCLUDE)?;
            if !loc.wrapper.is_plain_files() {
                if let Some(Ok(_)) = loc.wrapper.url_stat(&trypath, URL_STAT_QUIET, None) {
                    return Some(trypath);
                }
                return None;
            }
            return realpath(&loc.path);
        }
        return realpath(&trypath);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization() {
        assert_eq!(normalize(b"/a/b", b"../c"), b"/a/c");
        assert_eq!(normalize(b"/a", b"./x//y/."), b"/a/x/y");
        assert_eq!(normalize(b"/", b"../../.."), b"/");
        assert_eq!(normalize(b"/cwd", b"/abs/./p/../q"), b"/abs/q");
        assert!(expand_filepath(b"").is_none());
        let long = vec![b'a'; 5000];
        assert!(expand_filepath(&long).is_none());
    }

    #[test]
    fn open_basedir_checks() {
        let _ = diag::take();
        let tmp = realpath(std::env::temp_dir().as_os_str().as_encoded_bytes()).unwrap();
        state::update_settings(|s| s.open_basedir = tmp.clone());
        let mut inside = tmp.clone();
        inside.extend_from_slice(b"/does-not-exist/file");
        assert!(check_open_basedir(&inside));
        assert!(check_open_basedir(&tmp));
        assert!(!check_open_basedir(b"/etc/passwd"));
        let d = diag::take();
        assert_eq!(
            d[0].text(),
            format!(
                "open_basedir restriction in effect. File(/etc/passwd) is not within the allowed path(s): ({})",
                String::from_utf8_lossy(&tmp)
            )
        );
        let mut sibling = tmp.clone();
        sibling.extend_from_slice(b"x/y");
        assert!(!check_open_basedir_ex(&sibling, false));
        state::update_settings(|s| s.open_basedir.clear());
        assert!(check_open_basedir(b"/etc/passwd"));
    }

    #[test]
    fn resolve_include_path() {
        assert_eq!(
            resolve_path(b"/etc/passwd", b".").as_deref(),
            Some(&b"/etc/passwd"[..])
        );
        assert_eq!(
            resolve_path(b"passwd", b"/nonexistent:/etc").as_deref(),
            Some(&b"/etc/passwd"[..])
        );
        assert!(resolve_path(b"no-such-file-xyz", b"/etc").is_none());
        assert_eq!(
            resolve_path(b"file:///etc/passwd", b"").as_deref(),
            Some(&b"/etc/passwd"[..])
        );
    }
}
