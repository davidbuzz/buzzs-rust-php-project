//! `php_zip_extract_file()`: extraction of one entry below a directory,
//! with PHP's path sanitising (zip-slip protection) and error reporting.

use php_streams::wrapper::{MetadataOp, MKDIR_RECURSIVE, REPORT_ERRORS, URL_STAT_QUIET};

use crate::archive::cstr;
use crate::error::warning;
use crate::ffi::{self, zip_t};
use crate::paths::{self, MAXPATHLEN};
use crate::stream::ZipFile;

/// Extracts the entry `file` (looked up by name when `idx < 0`) into
/// `dest`. Returns `false` on any failure (some emit warnings).
#[allow(clippy::cast_sign_loss)]
pub(crate) fn extract_file(za: *mut zip_t, dest: &[u8], file: &[u8], idx: i64) -> bool {
    let idx = if idx < 0 {
        let c = cstr(file);
        // SAFETY: open archive and C string.
        let i = unsafe { ffi::zip_name_locate(za, c.as_ptr(), 0) };
        if i < 0 {
            return false;
        }
        i
    } else {
        idx
    };
    // virtual_file_ex() works on the C string.
    let file_c = cstr(file);
    let expanded = paths::expand_lexically(file_c.as_bytes()).unwrap_or_default();
    let Some(cleaned) = paths::make_relative(&expanded) else {
        return false;
    };
    let mut sb = std::mem::MaybeUninit::<ffi::zip_stat_t>::zeroed();
    // SAFETY: open archive and zeroed output struct.
    if cleaned.len() >= MAXPATHLEN
        || unsafe { ffi::zip_stat_index(za, idx as u64, 0, sb.as_mut_ptr()) } != 0
    {
        return false;
    }
    // SAFETY: zip_stat_index() succeeded.
    let sb = unsafe { sb.assume_init() };

    let is_dir_only = cleaned.len() > 1 && cleaned.last() == Some(&b'/');
    let (dir_full, base) = if is_dir_only {
        ([dest, b"/", cleaned].concat(), Vec::new())
    } else {
        let dir = paths::dirname(cleaned);
        let dir_full = if dir.is_empty() || dir == b"." {
            dest.to_vec()
        } else {
            [dest, b"/", &dir].concat()
        };
        let base = paths::basename(cleaned).to_vec();
        if !php_streams::paths::check_open_basedir(&dir_full) {
            return false;
        }
        (dir_full, base)
    };

    if php_streams::open::stat_path(&dir_full, URL_STAT_QUIET, None).is_none()
        && !php_streams::open::mkdir(&dir_full, 0o777, MKDIR_RECURSIVE | REPORT_ERRORS, None)
    {
        return false;
    }
    if is_dir_only {
        return true;
    }

    let fullpath = [&dir_full[..], b"/", &base].concat();
    if fullpath.len() > MAXPATHLEN {
        warning(format!(
            "Full extraction path exceed MAXPATHLEN ({MAXPATHLEN})"
        ));
        return false;
    }
    if !php_streams::paths::check_open_basedir(&fullpath) {
        return false;
    }

    // SAFETY: open archive.
    let zf = unsafe { ffi::zip_fopen_index(za, idx as u64, 0) };
    if zf.is_null() {
        return false;
    }
    let zf = ZipFile(zf);
    let mut opened = None;
    let Some(mut stream) =
        php_streams::open::open_wrapper(&fullpath, "w+b", REPORT_ERRORS, &mut opened, None)
    else {
        return false;
    };
    let mut buf = vec![0u8; 8192];
    let mut failed = false;
    loop {
        match zf.read(&mut buf) {
            None => {
                failed = true;
                break;
            }
            Some(0) => break,
            Some(n) => {
                if stream.write(&buf[..n]) != Ok(n) {
                    failed = true;
                    break;
                }
            }
        }
    }
    if failed {
        if zf.error_code() != crate::consts::er::OK {
            let mut m = b"Cannot extract \"".to_vec();
            m.extend_from_slice(file);
            m.extend_from_slice(b"\": \"");
            m.extend_from_slice(zf.error_message().as_bytes());
            m.push(b'"');
            warning(m);
        }
        stream.close();
        return false;
    }
    let _ = php_streams::open::metadata(
        &fullpath,
        &MetadataOp::Touch(Some((sb.mtime, sb.mtime))),
        None,
    );
    stream.close();
    zf.close() == 0
}
