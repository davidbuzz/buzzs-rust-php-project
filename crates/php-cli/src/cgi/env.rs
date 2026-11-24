// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The CGI/1.1 environment of `php-cgi`: `init_request_info()` (the
//! `cgi.fix_pathinfo` translation of `SCRIPT_FILENAME`, `PATH_INFO`,
//! `PATH_TRANSLATED` and `SCRIPT_NAME`), `is_valid_path()`, the `PHP_SELF`
//! rule of `sapi_cgi_register_variables()`, the CGI detection and the
//! query-string guard of `main()` (ref: `sapi/cgi/cgi_main.c`).
//!
//! The environment is modelled as an ordered list so that the translation
//! is a pure function; [`Env::changes`] records the `CGI_PUTENV()` calls
//! that the binary then applies to the process environment.

/// The process environment with the changes `CGI_PUTENV()` made.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    vars: Vec<(Vec<u8>, Vec<u8>)>,
    /// `setenv` (`Some`) / `unsetenv` (`None`) calls in order.
    pub changes: Vec<(Vec<u8>, Option<Vec<u8>>)>,
}

impl Env {
    /// An environment from `name, value` pairs.
    pub fn new(vars: impl IntoIterator<Item = (Vec<u8>, Vec<u8>)>) -> Self {
        Env {
            vars: vars.into_iter().collect(),
            changes: Vec::new(),
        }
    }

    /// The process environment.
    pub fn from_process() -> Self {
        use std::os::unix::ffi::OsStringExt;
        Env::new(std::env::vars_os().map(|(k, v)| (k.into_vec(), v.into_vec())))
    }

    /// Every variable, in the order the names were first set.
    pub fn vars(&self) -> &[(Vec<u8>, Vec<u8>)] {
        &self.vars
    }

    /// `getenv(name)`.
    pub fn get(&self, name: &str) -> Option<Vec<u8>> {
        self.vars
            .iter()
            .find(|(k, _)| k == name.as_bytes())
            .map(|(_, v)| v.clone())
    }

    /// `_sapi_cgi_putenv()`: sets or (with `None`) removes `name`, and
    /// returns the new value.
    pub fn put(&mut self, name: &str, value: Option<&[u8]>) -> Option<Vec<u8>> {
        let key = name.as_bytes();
        match value {
            Some(v) => {
                if let Some(e) = self.vars.iter_mut().find(|(k, _)| k == key) {
                    e.1 = v.to_vec();
                } else {
                    self.vars.push((key.to_vec(), v.to_vec()));
                }
            }
            None => self.vars.retain(|(k, _)| k != key),
        }
        self.changes.push((key.to_vec(), value.map(<[u8]>::to_vec)));
        value.map(<[u8]>::to_vec)
    }

    /// Applies the recorded changes to the process environment.
    pub fn apply(&self) {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        for (k, v) in &self.changes {
            match v {
                Some(v) => std::env::set_var(OsStr::from_bytes(k), OsStr::from_bytes(v)),
                None => std::env::remove_var(OsStr::from_bytes(k)),
            }
        }
    }
}

/// The `cgi.*` settings `init_request_info()` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `cgi.fix_pathinfo`.
    pub fix_pathinfo: bool,
    /// `cgi.discard_path`.
    pub discard_path: bool,
    /// `doc_root`.
    pub doc_root: Option<Vec<u8>>,
    /// `fastcgi.script_path_encoded` (FPM).
    pub script_path_encoded: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            fix_pathinfo: true,
            discard_path: false,
            doc_root: None,
            script_path_encoded: true,
        }
    }
}

/// The file system questions `init_request_info()` asks.
pub trait Fs {
    /// `tsrm_realpath(path) != NULL`.
    fn exists(&self, path: &[u8]) -> bool;
    /// `zend_stat(path) == 0 && S_ISREG`.
    fn is_regular_file(&self, path: &[u8]) -> bool;
}

/// The real file system.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealFs;

impl Fs for RealFs {
    fn exists(&self, path: &[u8]) -> bool {
        use std::os::unix::ffi::OsStrExt;
        std::fs::canonicalize(std::ffi::OsStr::from_bytes(path)).is_ok()
    }

    fn is_regular_file(&self, path: &[u8]) -> bool {
        use std::os::unix::ffi::OsStrExt;
        std::fs::metadata(std::ffi::OsStr::from_bytes(path)).is_ok_and(|m| m.is_file())
    }
}

/// `SG(request_info)` as `init_request_info()` leaves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestInfo {
    /// `path_translated` (the script to run).
    pub path_translated: Option<Vec<u8>>,
    /// `request_uri` (`SCRIPT_NAME` after the translation).
    pub request_uri: Option<Vec<u8>>,
    /// `request_method`.
    pub request_method: Option<Vec<u8>>,
    /// `query_string`.
    pub query_string: Option<Vec<u8>>,
    /// `content_type` (empty when unset).
    pub content_type: Vec<u8>,
    /// `content_length`.
    pub content_length: i64,
    /// `SG(sapi_headers).http_response_code`.
    pub response_code: i32,
    /// `HTTP_AUTHORIZATION` (`php_handle_auth_data`).
    pub authorization: Option<Vec<u8>>,
}

impl Default for RequestInfo {
    fn default() -> Self {
        RequestInfo {
            path_translated: None,
            request_uri: None,
            request_method: None,
            query_string: None,
            content_type: Vec::new(),
            content_length: 0,
            response_code: 200,
            authorization: None,
        }
    }
}

/// C `atol()`.
pub fn atol(s: &[u8]) -> i64 {
    let s = s.trim_ascii_start();
    let (neg, digits) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut v: i64 = 0;
    for &c in digits.iter().take_while(|c| c.is_ascii_digit()) {
        v = v.saturating_mul(10).saturating_add(i64::from(c - b'0'));
    }
    if neg {
        -v
    } else {
        v
    }
}

/// `is_valid_path()`: rejects paths with a `..` segment.
pub fn is_valid_path(path: &[u8]) -> bool {
    if path.starts_with(b"..") && matches!(path.get(2), None | Some(b'/' | b'\\')) {
        return false;
    }
    let mut i = 0;
    while i < path.len() {
        if matches!(path[i], b'/' | b'\\')
            && path.get(i + 1) == Some(&b'.')
            && path.get(i + 2) == Some(&b'.')
            && matches!(path.get(i + 3), None | Some(b'/' | b'\\'))
        {
            return false;
        }
        i += 1;
    }
    true
}

/// `strrchr(pt, '/')`, else `strrchr(pt, '\\')`.
pub fn last_slash(pt: &[u8]) -> Option<usize> {
    pt.iter()
        .rposition(|&c| c == b'/')
        .or_else(|| pt.iter().rposition(|&c| c == b'\\'))
}

/// `init_request_info()`: `env` is the process environment of a CGI
/// request, or the environment of a `FastCGI` request (`CGI_GETENV` and
/// `CGI_PUTENV` then use `request->env`).
#[allow(clippy::too_many_lines)]
pub fn init_request_info(env: &mut Env, settings: &Settings, fs: &dyn Fs) -> RequestInfo {
    let mut info = RequestInfo::default();
    let env_script_filename = env.get("SCRIPT_FILENAME");
    let env_path_translated = env.get("PATH_TRANSLATED");
    let mut script_path_translated = env_script_filename
        .clone()
        .or_else(|| env_path_translated.clone());
    // Without SCRIPT_FILENAME/PATH_TRANSLATED this is not a CGI request:
    // the script comes from the command line.
    if script_path_translated.is_none() {
        return info;
    }
    let content_length = env.get("CONTENT_LENGTH");
    let content_type = env.get("CONTENT_TYPE");
    let mut env_path_info = env.get("PATH_INFO");
    let mut env_script_name = env.get("SCRIPT_NAME");

    if settings.fix_pathinfo {
        let env_redirect_url = env.get("REDIRECT_URL");
        let mut env_document_root = env.get("DOCUMENT_ROOT");
        let orig_path_translated = env_path_translated.clone();
        let orig_path_info = env_path_info.clone();
        let orig_script_name = env_script_name.clone();
        let orig_script_filename = env_script_filename.clone();
        let mut script_name_redirected = false;

        if env_document_root.is_none() {
            if let Some(dr) = &settings.doc_root {
                env_document_root = env.put("DOCUMENT_ROOT", Some(dr));
            }
        }
        if let (Some(pt), Some(url)) = (&env_path_translated, &env_redirect_url) {
            if Some(pt) != script_path_translated.as_ref() {
                // Apache's Action: SCRIPT_FILENAME/SCRIPT_NAME point at
                // the PHP binary.
                script_path_translated = Some(pt.clone());
                env_script_name = Some(url.clone());
                script_name_redirected = true;
            }
        }
        let spt = script_path_translated.clone().unwrap_or_default();
        if !spt.is_empty() && (spt.ends_with(b"/") || !fs.exists(&spt)) {
            // Look for the script by stripping path segments: the rest
            // becomes PATH_INFO (`/info.php/test`).
            let mut pt = spt.clone();
            let len = spt.len();
            let mut found = false;
            while let Some(pos) = last_slash(&pt) {
                pt.truncate(pos);
                if !fs.is_regular_file(&pt) {
                    continue;
                }
                found = true;
                let slen = len - pt.len();
                // Only a PATH_INFO longer than what was stripped ends in it
                // (GHSA-5j3f-64r7-f564, CVE-2026-129538: C read and wrote
                // before the value otherwise; it now leaves PATH_INFO and
                // SCRIPT_NAME alone then).
                if let Some(pi) = env_path_info.clone().filter(|pi| pi.len() > slen) {
                    let off = pi.len() - slen;
                    env.put("ORIG_PATH_INFO", Some(&pi));
                    let prefix = &pi[..off];
                    if orig_script_name.as_deref() == Some(prefix) {
                        info.request_uri.clone_from(&orig_script_name);
                    } else {
                        if let Some(o) = &orig_script_name {
                            env.put("ORIG_SCRIPT_NAME", Some(o));
                        }
                        info.request_uri = env.put("SCRIPT_NAME", Some(prefix));
                    }
                    env_path_info = env.put("PATH_INFO", Some(&pi[off..]));
                }
                if orig_script_filename.as_deref() != Some(pt.as_slice()) {
                    if let Some(o) = &orig_script_filename {
                        env.put("ORIG_SCRIPT_FILENAME", Some(o));
                    }
                    script_path_translated = env.put("SCRIPT_FILENAME", Some(&pt));
                }
                let path_info = env_path_info.clone().unwrap_or_default();
                if let Some(dr) = &env_document_root {
                    // PATH_TRANSLATED = DOCUMENT_ROOT + PATH_INFO
                    let l = if dr.ends_with(b"/") {
                        dr.len() - 1
                    } else {
                        dr.len()
                    };
                    let mut translated = dr[..l].to_vec();
                    translated.extend_from_slice(&path_info);
                    if let Some(o) = &orig_path_translated {
                        env.put("ORIG_PATH_TRANSLATED", Some(o));
                    }
                    env.put("PATH_TRANSLATED", Some(&translated));
                } else if let Some(sn) = env_script_name
                    .as_ref()
                    .filter(|sn| sn.is_empty() || pt.windows(sn.len()).any(|w| w == sn.as_slice()))
                {
                    // PATH_TRANSLATED = SCRIPT_FILENAME - SCRIPT_NAME + PATH_INFO
                    let ptlen = pt.len().saturating_sub(sn.len());
                    let mut translated = pt[..ptlen].to_vec();
                    translated.extend_from_slice(&path_info);
                    if let Some(o) = &orig_path_translated {
                        env.put("ORIG_PATH_TRANSLATED", Some(o));
                    }
                    env.put("PATH_TRANSLATED", Some(&translated));
                }
                break;
            }
            if !found {
                if let Some(o) = &orig_script_filename {
                    env.put("ORIG_SCRIPT_FILENAME", Some(o));
                }
                script_path_translated = env.put("SCRIPT_FILENAME", None);
                info.response_code = 404;
            }
            if info.request_uri.is_none() {
                if orig_script_name.is_some() && orig_script_name == env_script_name {
                    info.request_uri.clone_from(&orig_script_name);
                } else {
                    if let Some(o) = &orig_script_name {
                        env.put("ORIG_SCRIPT_NAME", Some(o));
                    }
                    info.request_uri = env.put("SCRIPT_NAME", env_script_name.as_deref());
                }
            }
        } else {
            // The script exists: PATH_INFO/PATH_TRANSLATED stay as given.
            if orig_script_filename.as_ref() != script_path_translated.as_ref() {
                if let Some(o) = &orig_script_filename {
                    env.put("ORIG_SCRIPT_FILENAME", Some(o));
                }
                script_path_translated =
                    env.put("SCRIPT_FILENAME", script_path_translated.as_deref());
            }
            if env_redirect_url.is_some() {
                if let Some(o) = &orig_path_info {
                    env.put("ORIG_PATH_INFO", Some(o));
                    env.put("PATH_INFO", None);
                }
                if let Some(o) = &orig_path_translated {
                    env.put("ORIG_PATH_TRANSLATED", Some(o));
                    env.put("PATH_TRANSLATED", None);
                }
            }
            if script_name_redirected {
                if let Some(o) = &orig_script_name {
                    env.put("ORIG_SCRIPT_NAME", Some(o));
                }
                info.request_uri = env.put("SCRIPT_NAME", env_script_name.as_deref());
            } else {
                info.request_uri = env_script_name;
            }
        }
    } else {
        // Pre-4.3 behaviour.
        info.request_uri = env_path_info.or(env_script_name);
        if !settings.discard_path && env_path_translated.is_some() {
            script_path_translated = env_path_translated;
        }
    }

    info.path_translated = script_path_translated.filter(|p| is_valid_path(p));
    info.request_method = env.get("REQUEST_METHOD");
    info.query_string = env.get("QUERY_STRING");
    info.content_type = content_type.unwrap_or_default();
    info.content_length = content_length.as_deref().map_or(0, atol);
    info.authorization = env.get("HTTP_AUTHORIZATION");
    info
}

/// `$_SERVER['PHP_SELF']` (`sapi_cgi_register_variables`): the script name
/// plus `PATH_INFO` with `cgi.fix_pathinfo`, else the request URI.
pub fn php_self(
    request_uri: Option<&[u8]>,
    path_info: Option<&[u8]>,
    fix_pathinfo: bool,
) -> Vec<u8> {
    let mut out = request_uri.unwrap_or_default().to_vec();
    if fix_pathinfo {
        if let Some(pi) = path_info {
            out.extend_from_slice(pi);
        }
    }
    out
}

/// `main()`: started by a web server (any of these variables is set, even
/// empty).
pub fn is_cgi(env: &Env) -> bool {
    [
        "SERVER_SOFTWARE",
        "SERVER_NAME",
        "GATEWAY_INTERFACE",
        "REQUEST_METHOD",
    ]
    .iter()
    .any(|n| env.get(n).is_some())
}

/// `php_url_decode()`.
fn url_decode(s: &[u8]) -> Vec<u8> {
    let hex = |c: u8| {
        char::from(c)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    };
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < s.len() => {
                if let (Some(h), Some(l)) = (hex(s[i + 1]), hex(s[i + 2])) {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

/// `main()`: a query string without `=` whose decoded form starts (after
/// blanks and control characters) with `-` is not parsed as options
/// (CVE-2012-1823).
pub fn skip_getopt(query_string: Option<&[u8]>) -> bool {
    let Some(q) = query_string else {
        return false;
    };
    if q.contains(&b'=') {
        return false;
    }
    let decoded = url_decode(q);
    decoded
        .iter()
        .take_while(|&&c| c != 0)
        .find(|&&c| c > b' ')
        .is_some_and(|&c| c == b'-')
}

/// `apache_request_headers()` of the CGI SAPI: `HTTP_FOO_BAR` becomes
/// `Foo-Bar`, plus `Content-Type` and `Content-Length`.
pub fn request_header_name(var: &[u8]) -> Option<Vec<u8>> {
    if var.len() > 5 && var.starts_with(b"HTTP_") {
        let p = &var[5..];
        let mut out = Vec::with_capacity(p.len());
        out.push(p[0]);
        let mut i = 1;
        while i < p.len() {
            let c = p[i];
            if c == b'_' {
                out.push(b'-');
                i += 1;
                if let Some(&n) = p.get(i) {
                    out.push(n);
                    i += 1;
                }
                continue;
            }
            out.push(c.to_ascii_lowercase());
            i += 1;
        }
        return Some(out);
    }
    match var {
        b"CONTENT_TYPE" => Some(b"Content-Type".to_vec()),
        b"CONTENT_LENGTH" => Some(b"Content-Length".to_vec()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeFs(&'static [&'static str]);

    impl Fs for FakeFs {
        fn exists(&self, path: &[u8]) -> bool {
            self.0.iter().any(|f| {
                f.as_bytes() == path || f.as_bytes().starts_with(&[path, &b"/"[..]].concat())
            })
        }
        fn is_regular_file(&self, path: &[u8]) -> bool {
            self.0.iter().any(|f| f.as_bytes() == path)
        }
    }

    fn env(vars: &[(&str, &str)]) -> Env {
        Env::new(
            vars.iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec())),
        )
    }

    /// What run-tests.php passes: `SCRIPT_FILENAME` = `PATH_TRANSLATED` = the
    /// test file, no `SCRIPT_NAME` (so `PHP_SELF` is empty, as the oracle
    /// prints).
    #[test]
    fn run_tests_environment() {
        let mut e = env(&[
            ("REDIRECT_STATUS", "1"),
            ("REQUEST_METHOD", "POST"),
            ("QUERY_STRING", "a=1"),
            ("SCRIPT_FILENAME", "/t/x.php"),
            ("PATH_TRANSLATED", "/t/x.php"),
            ("CONTENT_TYPE", "application/x-www-form-urlencoded"),
            ("CONTENT_LENGTH", "3"),
            ("HTTP_COOKIE", "c=2"),
        ]);
        let info = init_request_info(&mut e, &Settings::default(), &FakeFs(&["/t/x.php"]));
        assert_eq!(info.path_translated.as_deref(), Some(&b"/t/x.php"[..]));
        assert_eq!(info.request_uri, None);
        assert_eq!(info.request_method.as_deref(), Some(&b"POST"[..]));
        assert_eq!(info.query_string.as_deref(), Some(&b"a=1"[..]));
        assert_eq!(info.content_length, 3);
        assert_eq!(info.response_code, 200);
        assert!(e.changes.is_empty());
        assert_eq!(php_self(None, None, true), b"");
    }

    /// `/info.php/test`: `PATH_INFO` is split off the script path.
    #[test]
    fn path_info_split() {
        let mut e = env(&[
            ("SCRIPT_FILENAME", "/docroot/info.php/test"),
            ("SCRIPT_NAME", "/info.php/test"),
            ("PATH_INFO", "/info.php/test"),
            ("DOCUMENT_ROOT", "/docroot/"),
        ]);
        let info = init_request_info(
            &mut e,
            &Settings::default(),
            &FakeFs(&["/docroot/info.php"]),
        );
        assert_eq!(
            info.path_translated.as_deref(),
            Some(&b"/docroot/info.php"[..])
        );
        assert_eq!(info.request_uri.as_deref(), Some(&b"/info.php"[..]));
        assert_eq!(e.get("PATH_INFO").as_deref(), Some(&b"/test"[..]));
        assert_eq!(
            e.get("ORIG_PATH_INFO").as_deref(),
            Some(&b"/info.php/test"[..])
        );
        assert_eq!(
            e.get("PATH_TRANSLATED").as_deref(),
            Some(&b"/docroot/test"[..])
        );
        assert_eq!(
            e.get("ORIG_SCRIPT_FILENAME").as_deref(),
            Some(&b"/docroot/info.php/test"[..])
        );
        assert_eq!(
            php_self(
                info.request_uri.as_deref(),
                e.get("PATH_INFO").as_deref(),
                true
            ),
            b"/info.php/test"
        );
    }

    /// GHSA-5j3f-64r7-f564 (CVE-2026-129538): a stripped suffix longer
    /// than `PATH_INFO` leaves `PATH_INFO` (and `SCRIPT_NAME`) as they are.
    #[test]
    fn path_info_shorter_than_stripped_suffix() {
        let fs = FakeFs(&["/t/s.php"]);
        let mut e = env(&[
            ("SCRIPT_FILENAME", "/t/s.php/b"),
            ("PATH_INFO", "/a/b"),
            ("SCRIPT_NAME", "/s.php"),
        ]);
        init_request_info(&mut e, &Settings::default(), &fs);
        assert_eq!(e.get("PATH_INFO").as_deref(), Some(&b"/b"[..]));
        assert_eq!(e.get("ORIG_PATH_INFO").as_deref(), Some(&b"/a/b"[..]));
        let mut e = env(&[
            ("SCRIPT_FILENAME", "/t/s.php/xxxxxxxxxxxxxxxx"),
            ("PATH_INFO", "/x"),
            ("SCRIPT_NAME", "/s.php"),
        ]);
        let info = init_request_info(&mut e, &Settings::default(), &fs);
        assert_eq!(info.path_translated.as_deref(), Some(&b"/t/s.php"[..]));
        assert_eq!(e.get("PATH_INFO").as_deref(), Some(&b"/x"[..]));
        assert_eq!(e.get("ORIG_PATH_INFO"), None);
        assert_eq!(e.get("SCRIPT_NAME").as_deref(), Some(&b"/s.php"[..]));
    }

    /// No file anywhere along the path: 404 and no script.
    #[test]
    fn missing_script_is_404() {
        let mut e = env(&[("SCRIPT_FILENAME", "/nonexist.php")]);
        let info = init_request_info(&mut e, &Settings::default(), &FakeFs(&[]));
        assert_eq!(info.response_code, 404);
        assert_eq!(info.path_translated, None);
        assert_eq!(e.get("SCRIPT_FILENAME"), None);
        assert_eq!(
            e.get("ORIG_SCRIPT_FILENAME").as_deref(),
            Some(&b"/nonexist.php"[..])
        );
    }

    #[test]
    fn without_fix_pathinfo() {
        let mut e = env(&[
            ("SCRIPT_FILENAME", "/a/php-cgi"),
            ("PATH_TRANSLATED", "/t/x.php"),
            ("PATH_INFO", "/x.php"),
        ]);
        let s = Settings {
            fix_pathinfo: false,
            ..Settings::default()
        };
        let info = init_request_info(&mut e, &s, &FakeFs(&[]));
        assert_eq!(info.path_translated.as_deref(), Some(&b"/t/x.php"[..]));
        assert_eq!(info.request_uri.as_deref(), Some(&b"/x.php"[..]));
        let s = Settings {
            fix_pathinfo: false,
            discard_path: true,
            ..Settings::default()
        };
        let info = init_request_info(&mut e, &s, &FakeFs(&[]));
        assert_eq!(info.path_translated.as_deref(), Some(&b"/a/php-cgi"[..]));
        assert_eq!(php_self(Some(b"/x.php"), Some(b"/y"), false), b"/x.php");
    }

    #[test]
    fn command_line_has_no_request_info() {
        let mut e = env(&[("REQUEST_METHOD", "GET")]);
        let info = init_request_info(&mut e, &Settings::default(), &FakeFs(&[]));
        assert_eq!(info, RequestInfo::default());
        assert!(is_cgi(&e));
        assert!(!is_cgi(&env(&[("PATH", "/bin")])));
        assert!(is_cgi(&env(&[("SERVER_NAME", "")])));
    }

    #[test]
    fn valid_paths() {
        assert!(is_valid_path(b"/t/x.php"));
        assert!(is_valid_path(b"/t/..x/y"));
        assert!(!is_valid_path(b"/t/../x"));
        assert!(!is_valid_path(b"/t/.."));
        assert!(!is_valid_path(b".."));
        assert!(!is_valid_path(b"../x"));
    }

    #[test]
    fn query_string_guard() {
        assert!(skip_getopt(Some(b"-s")));
        assert!(skip_getopt(Some(b"%2ds")));
        assert!(skip_getopt(Some(b"+-d+x")));
        assert!(!skip_getopt(Some(b"-d=1")));
        assert!(!skip_getopt(Some(b"a")));
        assert!(!skip_getopt(None));
    }

    #[test]
    fn request_header_names() {
        assert_eq!(
            request_header_name(b"HTTP_X_FOO_BAR").as_deref(),
            Some(&b"X-Foo-Bar"[..])
        );
        assert_eq!(
            request_header_name(b"HTTP_COOKIE").as_deref(),
            Some(&b"Cookie"[..])
        );
        assert_eq!(
            request_header_name(b"CONTENT_TYPE").as_deref(),
            Some(&b"Content-Type"[..])
        );
        assert_eq!(request_header_name(b"PATH"), None);
    }

    #[test]
    fn atol_prefix() {
        assert_eq!(atol(b" 12abc"), 12);
        assert_eq!(atol(b"-3"), -3);
        assert_eq!(atol(b""), 0);
    }
}
