//! File discovery for `ZipArchive::addGlob()` (`php_zip_glob()`) and
//! `addPattern()` (`php_zip_pcre()`).

use php_streams::glob::{php_glob, GLOB_AVAILABLE_FLAGS, GLOB_ONLYDIR};
use php_streams::open::{scandir, ScandirSort};

use crate::error::warning;
use crate::host::ZipHost;
use crate::paths::MAXPATHLEN;

fn is_dir(path: &[u8]) -> Option<bool> {
    let p: &std::ffi::OsStr = std::os::unix::ffi::OsStrExt::from_bytes(path);
    std::fs::metadata(p).ok().map(|m| m.is_dir())
}

/// `php_zip_glob()`: `None` for the `-1` (false) result.
pub(crate) fn glob(pattern: &[u8], flags: i64) -> Option<Vec<Vec<u8>>> {
    if pattern.len() >= MAXPATHLEN {
        warning(format!(
            "Pattern exceeds the maximum allowed length of {MAXPATHLEN} characters"
        ));
        return None;
    }
    if (i64::from(GLOB_AVAILABLE_FLAGS) & flags) != flags {
        warning("At least one of the passed flags is invalid or not supported on this platform");
        return None;
    }
    // The check above guarantees `flags` fits.
    let flags = i32::try_from(flags).unwrap_or(0);
    let Ok(found) = php_glob(pattern, flags & !GLOB_ONLYDIR) else {
        return Some(Vec::new());
    };
    if found.is_empty() {
        return Some(Vec::new());
    }
    if !php_streams::paths::check_open_basedir(&found[0]) {
        return None;
    }
    Some(
        found
            .into_iter()
            .filter(|p| flags & GLOB_ONLYDIR == 0 || is_dir(p) == Some(true))
            .collect(),
    )
}

/// `php_zip_pcre()`: `None` for the `-1` (false) result.
pub(crate) fn pcre(host: &mut dyn ZipHost, regex: &[u8], path: &[u8]) -> Option<Vec<Vec<u8>>> {
    if !php_streams::paths::check_open_basedir(path) {
        return None;
    }
    let names = scandir(path, ScandirSort::Ascending, None)?;
    let mut out = Vec::new();
    if names.is_empty() {
        return Some(out);
    }
    let Some(mut matcher) = host.compile_regex(regex) else {
        warning("Invalid expression");
        return None;
    };
    for name in names {
        if name == b"." || name == b".." {
            continue;
        }
        if path.len() + name.len() + 1 >= MAXPATHLEN {
            warning(format!(
                "add_path string too long (max: {}, {} given)",
                MAXPATHLEN - 1,
                path.len() + name.len() + 1
            ));
            break;
        }
        if !matcher(&name) {
            continue;
        }
        let fullpath = [path, b"/", &name].concat();
        match is_dir(&fullpath) {
            None => {
                let mut m = b"Cannot read <".to_vec();
                m.extend_from_slice(&fullpath);
                m.push(b'>');
                warning(m);
            }
            Some(true) => {}
            Some(false) => out.push(fullpath),
        }
    }
    Some(out)
}
