// Derived from php-src sapi/fpm/fpm/fpm_main.c. Similarity metric to original: 21%
//   Copyright © The PHP Group and Contributors.
//   Authors: Rasmus Lerdorf <rasmus@lerdorf.on.ca>
//            Stig Bakken <ssb@php.net>
//            Zeev Suraski <zeev@php.net>
//            FastCGI: Ben Mansell <php@slimyhorror.com>
//            Shane Caraveo <shane@caraveo.com>
//            Dmitry Stogov <dmitry@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! `init_request_info()` of `sapi/fpm/fpm/fpm_main.c`.
//!
//! It differs from php-cgi's (`cgi/env.rs`): the request method is read
//! even without a script (so such a request gets "File not found."), the
//! IIS `PATH_INFO` fix applies on every system, and Apache's
//! `mod_proxy_fcgi` / `mod_proxy_balancer` `SCRIPT_FILENAME`s
//! (`proxy:fcgi://host:port/path/info.php/extra?query`) are reduced to the
//! path, URL-decoded unless `fastcgi.script_path_encoded`, with
//! `PATH_INFO` taken from the path and removed from `SCRIPT_NAME`.

use crate::env::{self, Env, Fs, RequestInfo, Settings};

const APACHE_PROXY_FCGI_PREFIX: &[u8] = b"proxy:fcgi://";
const APACHE_PROXY_BALANCER_PREFIX: &[u8] = b"proxy:balancer://";

/// `php_raw_url_decode()`: `%XX` escapes (`+` stays).
pub fn raw_url_decode(s: &[u8]) -> Vec<u8> {
    let hex = |c: u8| {
        char::from(c)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    };
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 2 < s.len() {
            if let (Some(h), Some(l)) = (hex(s[i + 1]), hex(s[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// `strstr(haystack, needle) != NULL`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

/// `init_request_info()` of FPM on a request's environment.
#[allow(clippy::too_many_lines)]
pub fn init_request_info(env: &mut Env, settings: &Settings, fs: &dyn Fs) -> RequestInfo {
    let script_path_encoded = settings.script_path_encoded;
    let mut info = RequestInfo::default();
    let mut env_script_filename = env.get("SCRIPT_FILENAME");
    let env_path_translated = env.get("PATH_TRANSLATED");
    // "some broken servers do not have script_filename or argv0".
    let mut script_path_translated = env_script_filename
        .clone()
        .or_else(|| env_path_translated.clone());
    let mut apache_was_here = false;
    info.request_method = env.get("REQUEST_METHOD");
    // "if script_path_translated is not set, then there is no point to
    // carry on as the response is 404 and there is no further processing".
    if script_path_translated.is_none() {
        return info;
    }
    let content_length = env.get("CONTENT_LENGTH");
    let content_type = env.get("CONTENT_TYPE");
    let mut env_path_info = env.get("PATH_INFO");
    let mut env_script_name = env.get("SCRIPT_NAME");

    // "Hack for buggy IIS that sets incorrect PATH_INFO".
    if let (Some(sw), Some(sn), Some(pi)) = (
        env.get("SERVER_SOFTWARE"),
        env_script_name.clone(),
        env_path_info.clone(),
    ) {
        if sw.starts_with(b"Microsoft-IIS") && pi.starts_with(&sn) {
            env.put("ORIG_PATH_INFO", Some(&pi));
            let rest = &pi[sn.len()..];
            env_path_info = env.put("PATH_INFO", (!rest.is_empty()).then_some(rest));
        }
    }

    // Apache's `proxy:fcgi://localhost:9000/some-dir/info.php/test?foo=bar`
    // becomes `/some-dir/info.php/test` (in place: `SCRIPT_FILENAME`
    // itself changes).
    for prefix in [APACHE_PROXY_FCGI_PREFIX, APACHE_PROXY_BALANCER_PREFIX] {
        let Some(sf) = env_script_filename.clone() else {
            break;
        };
        if sf.len() < prefix.len() || !sf[..prefix.len()].eq_ignore_ascii_case(prefix) {
            continue;
        }
        let rest = &sf[prefix.len()..];
        let mut new = sf.clone();
        if let Some(slash) = rest.iter().position(|&c| c == b'/') {
            new = rest[slash..].to_vec();
            apache_was_here = true;
        }
        // "ignore query string if sent by Apache (RewriteRule)".
        if let Some(q) = new.iter().position(|&c| c == b'?') {
            new.truncate(q);
        }
        env_script_filename = env.put("SCRIPT_FILENAME", Some(&new));
    }
    if apache_was_here && !script_path_encoded {
        if let Some(sf) = &env_script_filename {
            let decoded = raw_url_decode(sf);
            env_script_filename = env.put("SCRIPT_FILENAME", Some(&decoded));
        }
    }
    if env_script_filename.is_some() {
        script_path_translated.clone_from(&env_script_filename);
    }

    if settings.fix_pathinfo {
        let env_redirect_url = env.get("REDIRECT_URL");
        let mut env_document_root = env.get("DOCUMENT_ROOT");
        let orig_path_translated = env_path_translated.clone();
        let orig_path_info = env_path_info.clone();
        let orig_script_name = env_script_name.clone();
        let orig_script_filename = env_script_filename.clone();
        // `env_script_name != orig_script_name` (a pointer comparison).
        let mut script_name_changed = false;

        if env_document_root.is_none() {
            if let Some(dr) = &settings.doc_root {
                env_document_root = env.put("DOCUMENT_ROOT", Some(dr));
            }
        }
        if !apache_was_here {
            if let (Some(pt), Some(url)) = (&env_path_translated, &env_redirect_url) {
                if script_path_translated.as_ref() != Some(pt) {
                    // Apache's mod_fastcgi: SCRIPT_FILENAME and SCRIPT_NAME
                    // point at the PHP binary.
                    script_path_translated = Some(pt.clone());
                    env_script_name = Some(url.clone());
                    script_name_changed = true;
                }
            }
        }

        let search = script_path_translated
            .as_ref()
            .is_some_and(|s| !s.is_empty() && (s.ends_with(b"/") || !fs.exists(s)));
        if search {
            // Strip path segments until a regular file is found; what was
            // stripped becomes PATH_INFO (`/info.php/test`).
            let spt = script_path_translated.clone().unwrap_or_default();
            let len = spt.len();
            let mut pt = spt.clone();
            let mut found = false;
            while let Some(pos) = env::last_slash(&pt) {
                pt.truncate(pos);
                if !fs.is_regular_file(&pt) {
                    continue;
                }
                found = true;
                let ptlen = pt.len();
                let slen = len - ptlen;
                let info_len = env_path_info.as_ref().map_or(0, Vec::len);
                let (path_info, tflag) = if apache_was_here {
                    // "recall that PATH_INFO won't exist".
                    let pi = spt[ptlen..].to_vec();
                    let t = slen != 0 && orig_path_info.as_ref() != Some(&pi);
                    (pi, t)
                } else {
                    match &env_path_info {
                        Some(epi) if info_len > slen => (epi[info_len - slen..].to_vec(), true),
                        _ => (Vec::new(), false),
                    }
                };
                if tflag {
                    let mut decoded: Option<Vec<u8>> = None;
                    if let Some(opi) = &orig_path_info {
                        env.put("ORIG_PATH_INFO", Some(opi));
                        // `path_info[0] = 0`: what precedes PATH_INFO in the
                        // buffer it points into.
                        let prefix = if apache_was_here {
                            env_path_info.clone().unwrap_or_default()
                        } else {
                            env_path_info
                                .as_ref()
                                .map(|e| e[..info_len - slen].to_vec())
                                .unwrap_or_default()
                        };
                        if orig_script_name.as_deref() == Some(prefix.as_slice()) {
                            info.request_uri.clone_from(&orig_script_name);
                        } else {
                            if let Some(o) = &orig_script_name {
                                env.put("ORIG_SCRIPT_NAME", Some(o));
                            }
                            info.request_uri = env.put("SCRIPT_NAME", Some(&prefix));
                        }
                    } else if apache_was_here {
                        if let Some(sn) = env_script_name.clone() {
                            // "Using mod_proxy_fcgi and ProxyPass, apache cannot
                            // set PATH_INFO": it is probably at the end of
                            // SCRIPT_NAME too, and is removed from there.
                            if script_path_encoded && path_info.contains(&b'%') {
                                decoded = Some(raw_url_decode(&path_info));
                            }
                            let name_len = sn.len();
                            let mut start = 0;
                            let matched = (name_len > slen && {
                                start = name_len - slen;
                                sn[start..] == path_info[..]
                            }) || decoded.as_ref().is_some_and(|d| {
                                name_len > d.len() && {
                                    start = name_len - d.len();
                                    sn[start..] == d[..]
                                }
                            });
                            if matched {
                                env.put("ORIG_SCRIPT_NAME", orig_script_name.as_deref());
                                let new_sn = sn[..start].to_vec();
                                info.request_uri = env.put("SCRIPT_NAME", Some(&new_sn));
                                env_script_name = Some(new_sn);
                            }
                        }
                    }
                    env_path_info =
                        env.put("PATH_INFO", Some(decoded.as_ref().unwrap_or(&path_info)));
                }
                if orig_script_filename.as_deref() != Some(pt.as_slice()) {
                    if let Some(o) = &orig_script_filename {
                        env.put("ORIG_SCRIPT_FILENAME", Some(o));
                    }
                    script_path_translated = env.put("SCRIPT_FILENAME", Some(&pt));
                }
                // The document root: DOCUMENT_ROOT, else SCRIPT_FILENAME
                // minus SCRIPT_NAME.
                if let Some(dr) = &env_document_root {
                    let mut l = dr.len();
                    if l > 0 && dr[l - 1] == b'/' {
                        l -= 1;
                    }
                    let mut translated = dr[..l].to_vec();
                    translated.extend_from_slice(env_path_info.as_deref().unwrap_or_default());
                    if let Some(o) = &orig_path_translated {
                        env.put("ORIG_PATH_TRANSLATED", Some(o));
                    }
                    env.put("PATH_TRANSLATED", Some(&translated));
                } else if let Some(sn) = env_script_name.as_ref().filter(|sn| contains(&pt, sn)) {
                    let ptlen = pt.len() - sn.len();
                    let mut translated = pt[..ptlen].to_vec();
                    translated.extend_from_slice(env_path_info.as_deref().unwrap_or_default());
                    if let Some(o) = &orig_path_translated {
                        env.put("ORIG_PATH_TRANSLATED", Some(o));
                    }
                    env.put("PATH_TRANSLATED", Some(&translated));
                }
                break;
            }
            if !found {
                // "we output 'no input file' now".
                if let Some(o) = &orig_script_filename {
                    env.put("ORIG_SCRIPT_FILENAME", Some(o));
                }
                script_path_translated = env.put("SCRIPT_FILENAME", None);
                info.response_code = 404;
            }
            if info.request_uri.is_none() {
                if orig_script_name.is_none() || orig_script_name != env_script_name {
                    if let Some(o) = &orig_script_name {
                        env.put("ORIG_SCRIPT_NAME", Some(o));
                    }
                    info.request_uri = env.put("SCRIPT_NAME", env_script_name.as_deref());
                } else {
                    info.request_uri.clone_from(&orig_script_name);
                }
            }
        } else {
            // "make sure original values are remembered in ORIG_ copies if
            // we've changed them".
            if orig_script_filename.is_none() || orig_script_filename != script_path_translated {
                if let Some(o) = &orig_script_filename {
                    env.put("ORIG_SCRIPT_FILENAME", Some(o));
                }
                script_path_translated =
                    env.put("SCRIPT_FILENAME", script_path_translated.as_deref());
            }
            if !apache_was_here && env_redirect_url.is_some() {
                if let Some(o) = &orig_path_info {
                    env.put("ORIG_PATH_INFO", Some(o));
                    env.put("PATH_INFO", None);
                }
                if let Some(o) = &orig_path_translated {
                    env.put("ORIG_PATH_TRANSLATED", Some(o));
                    env.put("PATH_TRANSLATED", None);
                }
            }
            if script_name_changed {
                if let Some(o) = &orig_script_name {
                    env.put("ORIG_SCRIPT_NAME", Some(o));
                }
                info.request_uri = env.put("SCRIPT_NAME", env_script_name.as_deref());
            } else {
                info.request_uri = env_script_name;
            }
        }
    } else {
        // "pre 4.3 behaviour, shouldn't be used but provides BC".
        info.request_uri = env_path_info.or(env_script_name);
        if !settings.discard_path && env_path_translated.is_some() {
            script_path_translated = env_path_translated;
        }
    }

    info.path_translated = script_path_translated.filter(|p| env::is_valid_path(p));
    info.query_string = env.get("QUERY_STRING");
    info.content_type = content_type.unwrap_or_default();
    info.content_length = content_length.as_deref().map_or(0, env::atol);
    info.authorization = env.get("HTTP_AUTHORIZATION");
    info
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

    #[test]
    fn method_without_script() {
        let mut e = env(&[("REQUEST_METHOD", "GET")]);
        let info = init_request_info(&mut e, &Settings::default(), &FakeFs(&[]));
        assert_eq!(info.request_method.as_deref(), Some(&b"GET"[..]));
        assert_eq!(info.path_translated, None);
    }

    #[test]
    fn apache_proxy_fcgi() {
        let mut e = env(&[
            (
                "SCRIPT_FILENAME",
                "proxy:fcgi://127.0.0.1:9000/srv/www/t.php/pinfo?x=1",
            ),
            ("SCRIPT_NAME", "/t.php/pinfo"),
            ("REQUEST_METHOD", "GET"),
        ]);
        let info = init_request_info(&mut e, &Settings::default(), &FakeFs(&["/srv/www/t.php"]));
        assert_eq!(
            info.path_translated.as_deref(),
            Some(&b"/srv/www/t.php"[..])
        );
        assert_eq!(e.get("PATH_INFO").as_deref(), Some(&b"/pinfo"[..]));
        assert_eq!(e.get("SCRIPT_NAME").as_deref(), Some(&b"/t.php"[..]));
        assert_eq!(
            e.get("ORIG_SCRIPT_NAME").as_deref(),
            Some(&b"/t.php/pinfo"[..])
        );
        assert_eq!(info.request_uri.as_deref(), Some(&b"/t.php"[..]));
    }

    #[test]
    fn apache_proxy_decodes_unless_encoded() {
        let mut e = env(&[("SCRIPT_FILENAME", "proxy:fcgi://h/srv/a%20b.php")]);
        let raw = Settings {
            script_path_encoded: false,
            ..Settings::default()
        };
        init_request_info(&mut e, &raw, &FakeFs(&["/srv/a b.php"]));
        assert_eq!(
            e.get("SCRIPT_FILENAME").as_deref(),
            Some(&b"/srv/a b.php"[..])
        );
        let mut e = env(&[("SCRIPT_FILENAME", "proxy:fcgi://h/srv/a%20b.php")]);
        init_request_info(&mut e, &Settings::default(), &FakeFs(&["/srv/a%20b.php"]));
        assert_eq!(
            e.get("SCRIPT_FILENAME").as_deref(),
            Some(&b"/srv/a%20b.php"[..])
        );
    }

    #[test]
    fn raw_decode() {
        assert_eq!(raw_url_decode(b"a%2Fb+c%"), b"a/b+c%");
        assert_eq!(raw_url_decode(b"%41%4"), b"A%4");
    }
}
