//! PHP's `ext/gettext` (ref: `ext/gettext/gettext.c`, `gettext.stub.php`).
//!
//! Every PHP function is a free function over glibc's built-in libintl.
//! Arguments arrive as PHP passed them (`string` → `&[u8]`, `int` → `i64`,
//! `?string` → `Option<&[u8]>`); results are owned byte strings.
//!
//! Failures are reported as a [`GettextError`] telling the VM what PHP does.
//! Messages never include the `"func(): "` prefix — the VM adds it from the
//! active function name, exactly like `zend_argument_value_error()` does.
//!
//! The catalog lookups depend on process-global C state that lives outside
//! this crate: the C locale (`setlocale()`, ext/standard — it must call the
//! real libc `setlocale`) and the environment (`LANGUAGE`, `LC_ALL`, `LANG`,
//! read by libintl, so `putenv()` must update the real process environment).

mod sys;

use std::ffi::CString;
use std::fmt;

#[cfg(test)]
mod tests;

/// `PHP_GETTEXT_MAX_DOMAIN_LENGTH`.
pub const MAX_DOMAIN_LENGTH: usize = 1024;
/// `PHP_GETTEXT_MAX_MSGID_LENGTH`.
pub const MAX_MSGID_LENGTH: usize = 4096;
/// `LC_ALL` on Linux (rejected as a `dcgettext()` category).
pub const LC_ALL: i64 = libc::LC_ALL as i64;
/// `MAXPATHLEN` on Linux: `bindtextdomain()` resolves into a buffer this big.
const MAXPATHLEN: usize = 4096;

/// What a failing gettext function does in PHP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GettextError {
    /// Throw `ValueError` with this message (no function prefix).
    ValueError(String),
    /// Throw a plain `Error` with this message (no function prefix).
    Error(String),
}

impl fmt::Display for GettextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValueError(m) | Self::Error(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for GettextError {}

/// Result alias for this crate.
pub type GettextResult<T> = Result<T, GettextError>;

fn value_error(arg: u32, name: &str, what: &str) -> GettextError {
    GettextError::ValueError(format!("Argument #{arg} (${name}) {what}"))
}

/// `PHP_GETTEXT_DOMAIN_LENGTH_CHECK`.
fn check_domain(arg: u32, name: &str, domain: &[u8]) -> GettextResult<()> {
    if domain.len() > MAX_DOMAIN_LENGTH {
        Err(value_error(arg, name, "is too long"))
    } else if domain.is_empty() {
        Err(value_error(arg, name, "must not be empty"))
    } else {
        Ok(())
    }
}

/// `PHP_GETTEXT_LENGTH_CHECK`.
fn check_msgid(arg: u32, name: &str, msgid: &[u8]) -> GettextResult<()> {
    if msgid.len() > MAX_MSGID_LENGTH {
        Err(value_error(arg, name, "is too long"))
    } else {
        Ok(())
    }
}

/// `PHP_DCGETTEXT_CATEGORY_CHECK`.
fn check_category(arg: u32, category: i64) -> GettextResult<()> {
    if category == LC_ALL {
        Err(value_error(arg, "category", "cannot be LC_ALL"))
    } else {
        Ok(())
    }
}

/// `Z_PARAM_PATH_STR`: rejects embedded NUL bytes.
fn check_path(arg: u32, name: &str, s: &[u8]) -> GettextResult<()> {
    if s.contains(&0) {
        Err(value_error(arg, name, "must not contain any null bytes"))
    } else {
        Ok(())
    }
}

/// The C view of a PHP string: everything before the first NUL byte, as
/// libintl would see `ZSTR_VAL()`.
fn c(s: &[u8]) -> CString {
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    CString::new(&s[..end]).unwrap_or_default()
}

/// `zend_long` → C `int` as the implicit C conversion does (truncation).
#[allow(clippy::cast_possible_truncation)]
fn c_int(v: i64) -> libc::c_int {
    v as libc::c_int
}

/// `zend_long` → C `unsigned long` (two's-complement reinterpretation).
#[allow(clippy::cast_sign_loss)]
fn c_ulong(v: i64) -> libc::c_ulong {
    v as libc::c_ulong
}

/// `textdomain(?string $domain = null): string`.
///
/// Sets (or with `None`, queries) the default domain and returns the
/// current one.
pub fn textdomain(domain: Option<&[u8]>) -> GettextResult<Vec<u8>> {
    let cdomain = match domain {
        Some(d) => {
            check_domain(1, "domain", d)?;
            if d == b"0" {
                return Err(value_error(1, "domain", "cannot be zero"));
            }
            Some(c(d))
        }
        None => None,
    };
    sys::set_textdomain(cdomain.as_deref())
        .ok_or_else(|| GettextError::Error("Could not set text domain".into()))
}

/// `gettext(string $message): string` (also `_()`).
pub fn gettext(message: &[u8]) -> GettextResult<Vec<u8>> {
    check_msgid(1, "message", message)?;
    Ok(sys::get(&c(message)).unwrap_or_else(|| message.to_vec()))
}

/// `dgettext(string $domain, string $message): string`.
pub fn dgettext(domain: &[u8], message: &[u8]) -> GettextResult<Vec<u8>> {
    check_domain(1, "domain", domain)?;
    check_msgid(2, "message", message)?;
    Ok(sys::d_get(&c(domain), &c(message)).unwrap_or_else(|| message.to_vec()))
}

/// `dcgettext(string $domain, string $message, int $category): string`.
pub fn dcgettext(domain: &[u8], message: &[u8], category: i64) -> GettextResult<Vec<u8>> {
    check_domain(1, "domain", domain)?;
    check_msgid(2, "message", message)?;
    check_category(3, category)?;
    Ok(sys::dc_get(&c(domain), &c(message), c_int(category)).unwrap_or_else(|| message.to_vec()))
}

/// `bindtextdomain(string $domain, ?string $directory = null): string|false`.
///
/// `Ok(None)` is PHP's `false`.  A non-empty directory other than `"0"` is
/// resolved with `realpath()` (failure → `false`); `""` and `"0"` bind to
/// the current working directory; `None` queries the current binding.
pub fn bindtextdomain(domain: &[u8], directory: Option<&[u8]>) -> GettextResult<Option<Vec<u8>>> {
    check_path(1, "domain", domain)?;
    if let Some(dir) = directory {
        check_path(2, "directory", dir)?;
    }
    check_domain(1, "domain", domain)?;
    let cdomain = c(domain);
    let Some(dir) = directory else {
        return Ok(sys::bind(&cdomain, None));
    };
    let resolved = if !dir.is_empty() && dir != b"0" {
        match realpath(dir) {
            Some(p) => p,
            None => return Ok(None),
        }
    } else {
        match std::env::current_dir() {
            Ok(p) => p.into_os_string(),
            Err(_) => return Ok(None),
        }
    };
    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(resolved.as_os_str());
    if bytes.len() >= MAXPATHLEN {
        return Ok(None);
    }
    sys::bind(&cdomain, Some(&c(bytes)))
        .map(Some)
        .ok_or_else(|| GettextError::Error("Could not bind text domain".into()))
}

/// `VCWD_REALPATH`: the canonical absolute path of an existing file.
fn realpath(path: &[u8]) -> Option<std::ffi::OsString> {
    let p =
        std::path::Path::new(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(path));
    std::fs::canonicalize(p)
        .ok()
        .map(std::path::PathBuf::into_os_string)
}

/// `ngettext(string $singular, string $plural, int $count): string`.
pub fn ngettext(singular: &[u8], plural: &[u8], count: i64) -> GettextResult<Vec<u8>> {
    check_msgid(1, "singular", singular)?;
    check_msgid(2, "plural", plural)?;
    Ok(sys::n_get(&c(singular), &c(plural), c_ulong(count)))
}

/// `dngettext(string $domain, string $singular, string $plural, int $count): string`.
pub fn dngettext(
    domain: &[u8],
    singular: &[u8],
    plural: &[u8],
    count: i64,
) -> GettextResult<Vec<u8>> {
    check_domain(1, "domain", domain)?;
    check_msgid(2, "singular", singular)?;
    check_msgid(3, "plural", plural)?;
    Ok(sys::dn_get(
        &c(domain),
        &c(singular),
        &c(plural),
        c_ulong(count),
    ))
}

/// `dcngettext(string $domain, string $singular, string $plural, int $count, int $category): string`.
pub fn dcngettext(
    domain: &[u8],
    singular: &[u8],
    plural: &[u8],
    count: i64,
    category: i64,
) -> GettextResult<Vec<u8>> {
    check_domain(1, "domain", domain)?;
    check_msgid(2, "singular", singular)?;
    check_msgid(3, "plural", plural)?;
    check_category(5, category)?;
    Ok(sys::dcn_get(
        &c(domain),
        &c(singular),
        &c(plural),
        c_ulong(count),
        c_int(category),
    ))
}

/// `bind_textdomain_codeset(string $domain, ?string $codeset = null): string|false`.
///
/// `Ok(None)` is PHP's `false` (e.g. querying a domain with no codeset set).
pub fn bind_textdomain_codeset(
    domain: &[u8],
    codeset: Option<&[u8]>,
) -> GettextResult<Option<Vec<u8>>> {
    check_domain(1, "domain", domain)?;
    let cs = codeset.map(c);
    Ok(sys::bind_codeset(&c(domain), cs.as_deref()))
}
