//! `Phar::webPhar()` front-controller routing, the mime table and
//! `Phar::mungServer()` (`phar_object.c`).
//!
//! The SAPI side (request info, `$_SERVER`, headers, output, includes,
//! highlighting, the rewrite callback) is reached through [`WebHost`].

use crate::archive::ArchiveRef;
use crate::consts::{
    MIME_OTHER, MIME_PHP, MIME_PHPS, MUNG_PHP_SELF, MUNG_REQUEST_URI, MUNG_SCRIPT_FILENAME,
    MUNG_SCRIPT_NAME,
};
use crate::error::{PharError, PharResult};
use crate::registry as reg;
use crate::util::{find, s};

/// The default mime table (`PHAR_G(mime_types)`): extension, mime type,
/// kind (`Phar::PHP`, `Phar::PHPS` or other).
pub const MIME_TYPES: &[(&str, &str, i64)] = &[
    ("phps", "text/html", MIME_PHPS),
    ("c", "text/plain", MIME_OTHER),
    ("cc", "text/plain", MIME_OTHER),
    ("cpp", "text/plain", MIME_OTHER),
    ("c++", "text/plain", MIME_OTHER),
    ("dtd", "text/plain", MIME_OTHER),
    ("h", "text/plain", MIME_OTHER),
    ("log", "text/plain", MIME_OTHER),
    ("rng", "text/plain", MIME_OTHER),
    ("txt", "text/plain", MIME_OTHER),
    ("xsd", "text/plain", MIME_OTHER),
    ("php", "", MIME_PHP),
    ("inc", "", MIME_PHP),
    ("avi", "video/avi", MIME_OTHER),
    ("bmp", "image/bmp", MIME_OTHER),
    ("css", "text/css", MIME_OTHER),
    ("gif", "image/gif", MIME_OTHER),
    ("htm", "text/html", MIME_OTHER),
    ("html", "text/html", MIME_OTHER),
    ("htmls", "text/html", MIME_OTHER),
    ("ico", "image/x-ico", MIME_OTHER),
    ("jpe", "image/jpeg", MIME_OTHER),
    ("jpg", "image/jpeg", MIME_OTHER),
    ("jpeg", "image/jpeg", MIME_OTHER),
    ("js", "application/x-javascript", MIME_OTHER),
    ("midi", "audio/midi", MIME_OTHER),
    ("mid", "audio/midi", MIME_OTHER),
    ("mod", "audio/mod", MIME_OTHER),
    ("mov", "movie/quicktime", MIME_OTHER),
    ("mp3", "audio/mp3", MIME_OTHER),
    ("mpg", "video/mpeg", MIME_OTHER),
    ("mpeg", "video/mpeg", MIME_OTHER),
    ("pdf", "application/pdf", MIME_OTHER),
    ("png", "image/png", MIME_OTHER),
    ("swf", "application/shockwave-flash", MIME_OTHER),
    ("tif", "image/tiff", MIME_OTHER),
    ("tiff", "image/tiff", MIME_OTHER),
    ("wav", "audio/wav", MIME_OTHER),
    ("xbm", "image/xbm", MIME_OTHER),
    ("xml", "text/xml", MIME_OTHER),
];

/// `phar_file_type()`: (kind, mime type) for a file name.
pub fn file_type(file: &[u8]) -> (i64, Vec<u8>) {
    let Some(dot) = file.iter().rposition(|&c| c == b'.') else {
        return (MIME_OTHER, b"text/plain".to_vec());
    };
    let ext = crate::util::cstr(&file[dot + 1..]);
    match MIME_TYPES.iter().find(|(e, _, _)| e.as_bytes() == ext) {
        Some((_, m, k)) => (*k, m.as_bytes().to_vec()),
        None => (MIME_OTHER, b"application/octet-stream".to_vec()),
    }
}

/// A `$_SERVER` value as seen by phar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerValue {
    /// A string.
    Str(Vec<u8>),
    /// Any other type.
    Other,
}

/// What a rewrite callback returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rewrite {
    /// A string: the new entry path.
    Str(Vec<u8>),
    /// `true`/`false`: deny (403).
    Bool,
    /// Anything else.
    Other,
    /// The call failed (an exception may be pending: `exception`).
    Failed {
        /// Whether an exception was thrown by the callback.
        exception: bool,
    },
}

/// A user mime override value (`$mimeTypes` element).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MimeOverride {
    /// An integer (`Phar::PHP`, `Phar::PHPS`, ...).
    Int(i64),
    /// A mime type string.
    Str(Vec<u8>),
    /// Anything else.
    Other,
}

/// The SAPI/VM services `webPhar()` needs.
pub trait WebHost {
    /// `sapi_module.name`.
    fn sapi_name(&self) -> String;
    /// `SG(request_info).request_method`.
    fn request_method(&self) -> Option<Vec<u8>>;
    /// `SG(request_info).request_uri`.
    fn request_uri(&self) -> Option<Vec<u8>>;
    /// Whether `$_SERVER` is available.
    fn has_server(&self) -> bool;
    /// `$_SERVER[$key]`.
    fn server_get(&self, key: &str) -> Option<ServerValue>;
    /// `$_SERVER[$key] = $value`.
    fn server_set(&mut self, key: &str, value: Vec<u8>);
    /// `sapi_getenv()`.
    fn getenv(&self, key: &str) -> Option<Vec<u8>>;
    /// `sapi_header_op(SAPI_HEADER_REPLACE)` with an optional response code.
    fn header(&mut self, line: &[u8], response_code: Option<u16>);
    /// `sapi_send_headers()`; `false` on failure.
    fn send_headers(&mut self) -> bool;
    /// `PHPWRITE`.
    fn write(&mut self, data: &[u8]);
    /// `highlight_file()`.
    fn highlight_file(&mut self, path: &[u8]);
    /// Compiles and runs `path` like `require`, unless it was already
    /// included (`EG(included_files)`); returns whether it ran.
    fn include(&mut self, path: &[u8]) -> bool;
    /// Calls the rewrite callback.
    fn rewrite(&mut self, entry: &[u8]) -> Rewrite;
}

/// Arguments of `Phar::webPhar()`.
#[derive(Debug, Default)]
pub struct WebPharArgs<'a> {
    /// `$alias`.
    pub alias: Option<&'a [u8]>,
    /// `$index`.
    pub index: Option<&'a [u8]>,
    /// `$fileNotFoundScript`.
    pub f404: Option<&'a [u8]>,
    /// `$mimeTypes` (in array order).
    pub mimes: Option<Vec<(Vec<u8>, MimeOverride)>>,
    /// Whether `$rewrite` was given.
    pub has_rewrite: bool,
}

/// How `webPhar()` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebResult {
    /// It returned normally (not a web request, or nothing to do).
    Return,
    /// The request was handled: the VM must `exit` (unless an exception is
    /// pending).
    Exit,
}

/// `phar_do_403()`.
pub fn do_403(host: &mut dyn WebHost) {
    host.header(b"HTTP/1.0 403 Access Denied", Some(403));
    host.send_headers();
    host.write(
        b"<html>\n <head>\n  <title>Access Denied</title>\n </head>\n <body>\n  <h1>403 - File ",
    );
    host.write(b"Access Denied</h1>\n </body>\n</html>");
}

/// `phar_do_404()`.
fn do_404(
    host: &mut dyn WebHost,
    phar: Option<&ArchiveRef>,
    fname: &[u8],
    f404: Option<&[u8]>,
) -> PharResult<()> {
    if let (Some(p), Some(f)) = (phar, f404.filter(|f| !f.is_empty())) {
        if let Ok(reg::EntryRef::Manifest(k)) = reg::get_entry_info_dir(p, f, 0, true) {
            file_action(host, p, &k, b"text/html", MIME_PHP, f, fname, None, 0)?;
            return Ok(());
        }
    }
    host.header(b"HTTP/1.0 404 Not Found", Some(404));
    host.send_headers();
    host.write(
        b"<html>\n <head>\n  <title>File Not Found</title>\n </head>\n <body>\n  <h1>404 - File ",
    );
    host.write(b"Not Found</h1>\n </body>\n</html>");
    Ok(())
}

/// `phar_mung_server_vars()`.
fn mung_server_vars(
    host: &mut dyn WebHost,
    fname: &[u8],
    entry: &[u8],
    basename: &[u8],
    request_uri_len: usize,
) {
    if !host.has_server() {
        return;
    }
    if let Some(ServerValue::Str(pi)) = host.server_get("PATH_INFO") {
        if pi.len() > entry.len() && pi.starts_with(entry) {
            let end = (entry.len() + request_uri_len).min(pi.len());
            host.server_set("PATH_INFO", pi[entry.len()..end].to_vec());
            host.server_set("PHAR_PATH_INFO", pi);
        }
    }
    if let Some(ServerValue::Str(pt)) = host.server_get("PATH_TRANSLATED") {
        let mut v = b"phar://".to_vec();
        v.extend_from_slice(fname);
        v.extend_from_slice(entry);
        host.server_set("PATH_TRANSLATED", v);
        host.server_set("PHAR_PATH_TRANSLATED", pt);
    }
    let mung = reg::mung_list();
    if mung == 0 {
        return;
    }
    for (bit, key, saved) in [
        (MUNG_REQUEST_URI, "REQUEST_URI", "PHAR_REQUEST_URI"),
        (MUNG_PHP_SELF, "PHP_SELF", "PHAR_PHP_SELF"),
    ] {
        if mung & bit != 0 {
            if let Some(ServerValue::Str(v)) = host.server_get(key) {
                if v.len() > basename.len() && v.starts_with(basename) {
                    host.server_set(key, v[basename.len()..].to_vec());
                    host.server_set(saved, v);
                }
            }
        }
    }
    if mung & MUNG_SCRIPT_NAME != 0 {
        if let Some(ServerValue::Str(v)) = host.server_get("SCRIPT_NAME") {
            host.server_set("SCRIPT_NAME", entry.to_vec());
            host.server_set("PHAR_SCRIPT_NAME", v);
        }
    }
    if mung & MUNG_SCRIPT_FILENAME != 0 {
        if let Some(ServerValue::Str(v)) = host.server_get("SCRIPT_FILENAME") {
            let mut n = b"phar://".to_vec();
            n.extend_from_slice(fname);
            n.extend_from_slice(entry);
            host.server_set("SCRIPT_FILENAME", n);
            host.server_set("PHAR_SCRIPT_FILENAME", v);
        }
    }
}

/// `phar_file_action()`; returns whether the request was handled (exit).
#[allow(clippy::too_many_arguments)]
fn file_action(
    host: &mut dyn WebHost,
    phar: &ArchiveRef,
    key: &[u8],
    mime_type: &[u8],
    code: i64,
    entry: &[u8],
    arch: &[u8],
    basename: Option<&[u8]>,
    ru_len: usize,
) -> PharResult<bool> {
    let name = || {
        let mut n = b"phar://".to_vec();
        n.extend_from_slice(arch);
        if entry.first() != Some(&b'/') {
            n.push(b'/');
        }
        n.extend_from_slice(entry);
        n
    };
    match code {
        MIME_PHPS => {
            host.highlight_file(&name());
            Ok(true)
        }
        MIME_OTHER => {
            let size = phar
                .borrow()
                .manifest
                .get(key)
                .map_or(0, |e| e.uncompressed_filesize);
            let mut l = b"Content-type: ".to_vec();
            l.extend_from_slice(mime_type);
            host.header(&l, None);
            host.header(format!("Content-length: {size}").as_bytes(), None);
            if !host.send_headers() {
                return Ok(true);
            }
            let data = phar.borrow_mut().read_entry(key).map_err(PharError::phar)?;
            host.write(&data[..(size as usize).min(data.len())]);
            Ok(true)
        }
        MIME_PHP => {
            if let Some(b) = basename {
                mung_server_vars(host, arch, entry, b, ru_len);
            }
            let path = name();
            reg::set_cwd(None, reg::cwd_init());
            if let Some(slash) = entry.iter().rposition(|&c| c == b'/') {
                if slash == 0 {
                    reg::set_cwd(None, true);
                } else if entry[0] == b'/' {
                    reg::set_cwd(Some(entry[1..slash].to_vec()), true);
                } else {
                    reg::set_cwd(Some(entry[..slash].to_vec()), true);
                }
            }
            let ran = host.include(&path);
            reg::set_cwd(None, false);
            Ok(ran)
        }
        _ => Ok(false),
    }
}

/// `phar_postprocess_ru_web()`: splits `/path/to/file.php/extra` into
/// the entry and the trailing request URI part.
fn postprocess_ru_web(phar: &ArchiveRef, entry: &mut Vec<u8>) -> Option<Vec<u8>> {
    let a = phar.borrow();
    let e = &entry[1..];
    let mut cut = e.len();
    loop {
        if a.manifest.contains_key(&e[..cut]) {
            let ru = (cut < e.len()).then(|| e[cut..].to_vec());
            entry.truncate(cut + 1);
            return ru;
        }
        match e[..cut].iter().rposition(|&c| c == b'/') {
            Some(p) => cut = p,
            None => return None,
        }
    }
}

/// `Phar::webPhar()`. `executed` is the executing script,
/// `halt_defined` whether `__COMPILER_HALT_OFFSET__` exists.
#[allow(clippy::too_many_lines)]
pub fn web_phar(
    host: &mut dyn WebHost,
    executed: Option<&[u8]>,
    halt_defined: bool,
    args: &WebPharArgs<'_>,
) -> PharResult<WebResult> {
    match reg::open_executed_filename(executed, args.alias, halt_defined) {
        Ok(()) => {}
        Err(Some(e)) => return Err(PharError::phar(e)),
        Err(None) => return Ok(WebResult::Return),
    }
    let method = host.request_method();
    let uri = host.request_uri();
    let web_methods: [&[u8]; 7] = [
        b"GET", b"POST", b"DELETE", b"HEAD", b"OPTIONS", b"PATCH", b"PUT",
    ];
    match (&method, &uri) {
        (Some(m), Some(_)) if web_methods.contains(&m.as_slice()) => {}
        _ => return Ok(WebResult::Return),
    }
    let Some(fname) = executed else {
        return Ok(WebResult::Return);
    };
    let basename = match fname.iter().rposition(|&c| c == b'/') {
        Some(p) => &fname[p + 1..],
        None => fname,
    };
    let sapi = host.sapi_name();
    let is_cgi = matches!(sapi.as_str(), "cgi-fcgi" | "fpm-fcgi" | "cgi" | "litespeed");
    let tail_len = fname.len() - (fname.len() - basename.len());
    let (mut entry, path_info, pt): (Vec<u8>, Vec<u8>, Vec<u8>);
    let not_cgi;
    if is_cgi {
        if host.has_server() {
            let Some(ServerValue::Str(script)) = host.server_get("SCRIPT_NAME") else {
                return Ok(WebResult::Return);
            };
            if find(&script, basename).is_none() {
                return Ok(WebResult::Return);
            }
            if let Some(ServerValue::Str(pi)) = host.server_get("PATH_INFO") {
                entry = pi.clone();
                let mut p = script.clone();
                p.extend_from_slice(&pi);
                path_info = p;
            } else {
                entry = Vec::new();
                path_info = script.clone();
            }
            pt = script;
        } else {
            let Some(testit) = host.getenv("SCRIPT_NAME") else {
                return Ok(WebResult::Return);
            };
            let Some(pos) = find(&testit, basename) else {
                return Ok(WebResult::Return);
            };
            if let Some(pi) = host.getenv("PATH_INFO") {
                entry = pi.clone();
                let mut p = testit.clone();
                p.extend_from_slice(&pi);
                path_info = p;
            } else {
                entry = Vec::new();
                path_info = testit.clone();
            }
            pt = testit[..(pos + tail_len).min(testit.len())].to_vec();
        }
        not_cgi = false;
    } else {
        let pi = uri.clone().unwrap_or_default();
        let Some(pos) = find(&pi, basename) else {
            return Ok(WebResult::Return);
        };
        let start = (pos + tail_len).min(pi.len());
        entry = pi[start..].to_vec();
        pt = pi[..start].to_vec();
        path_info = pi;
        not_cgi = true;
    }

    if args.has_rewrite {
        match host.rewrite(&entry) {
            Rewrite::Str(e) => entry = e,
            Rewrite::Bool => {
                do_403(host);
                return Ok(WebResult::Exit);
            }
            Rewrite::Other => {
                return Err(PharError::phar(
                    "phar error: rewrite callback must return a string or false",
                ))
            }
            Rewrite::Failed { exception } => {
                if exception {
                    return Ok(WebResult::Return);
                }
                return Err(PharError::phar(
                    "phar error: failed to call rewrite callback",
                ));
            }
        }
    }

    // `phar_fname_map`, then the `phar.cache_list` cache.
    let phar_now = reg::find_by_fname(fname).or_else(|| reg::cached(fname));
    let mut ru: Option<Vec<u8>> = None;
    if !entry.is_empty() {
        if let Some(p) = &phar_now {
            ru = postprocess_ru_web(p, &mut entry);
        }
    }
    let ru_len = ru.as_ref().map_or(0, Vec::len);

    if entry.is_empty() || entry == b"/" {
        let target: Vec<u8> = match args.index.filter(|i| !i.is_empty()) {
            Some(i) if i.first() == Some(&b'/') => i.to_vec(),
            Some(i) => {
                let mut v = b"/".to_vec();
                v.extend_from_slice(i);
                v
            }
            None => b"/index.php".to_vec(),
        };
        let phar = reg::get_archive(fname, None).ok().flatten();
        let found = phar
            .as_ref()
            .is_some_and(|p| reg::get_entry_info_dir(p, &target, 0, false).is_ok());
        if found {
            host.header(b"HTTP/1.1 301 Moved Permanently", Some(301));
            let base: Vec<u8> = if not_cgi {
                let p = find(&path_info, basename).unwrap_or(0) + tail_len;
                path_info[..p.min(path_info.len())].to_vec()
            } else {
                path_info.clone()
            };
            let mut loc = b"Location: ".to_vec();
            loc.extend_from_slice(&base);
            if base.last() == Some(&b'/') {
                loc.extend_from_slice(&target[1..]);
            } else {
                loc.extend_from_slice(&target);
            }
            host.header(&loc, None);
            host.send_headers();
        } else {
            do_404(host, phar.as_ref(), fname, args.f404)?;
        }
        return Ok(WebResult::Exit);
    }

    let phar = reg::get_archive(fname, None).ok().flatten();
    let key = match &phar {
        Some(p) => match reg::get_entry_info_dir(p, &entry, 0, false) {
            Ok(reg::EntryRef::Manifest(k)) => Some(k),
            _ => None,
        },
        None => None,
    };
    let (Some(phar), Some(key)) = (phar, key) else {
        do_404(
            host,
            reg::get_archive(fname, None).ok().flatten().as_ref(),
            fname,
            args.f404,
        )?;
        return Ok(WebResult::Exit);
    };
    let mut mime: Option<(i64, Vec<u8>)> = None;
    if let Some(mimes) = args.mimes.as_ref().filter(|m| !m.is_empty()) {
        if let Some(dot) = entry.iter().rposition(|&c| c == b'.') {
            let ext = crate::util::cstr(&entry[dot + 1..]);
            if let Some((_, v)) = mimes.iter().find(|(k, _)| k == ext) {
                match v {
                    MimeOverride::Int(i) if *i == MIME_PHP || *i == MIME_PHPS => {
                        mime = Some((*i, Vec::new()));
                    }
                    MimeOverride::Int(_) => {
                        return Err(PharError::phar("Unknown mime type specifier used, only Phar::PHP, Phar::PHPS and a mime type string are allowed"));
                    }
                    MimeOverride::Str(m) => mime = Some((MIME_OTHER, m.clone())),
                    MimeOverride::Other => {
                        return Err(PharError::phar("Unknown mime type specifier used (not a string or int), only Phar::PHP, Phar::PHPS and a mime type string are allowed"));
                    }
                }
            }
        }
    }
    let (code, mime_type) = mime.unwrap_or_else(|| file_type(&entry));
    let _ = s(&pt);
    let handled = file_action(
        host,
        &phar,
        &key,
        &mime_type,
        code,
        &entry,
        fname,
        Some(&pt),
        ru_len,
    )?;
    Ok(if handled {
        WebResult::Exit
    } else {
        WebResult::Return
    })
}

/// `Phar::mungServer()`: `values` are the array elements (`None` for
/// non-strings).
pub fn mung_server(values: &[Option<Vec<u8>>]) -> PharResult<()> {
    const LIST: &str = "expecting an array of any of these strings: PHP_SELF, REQUEST_URI, SCRIPT_FILENAME, SCRIPT_NAME";
    if values.is_empty() {
        return Err(PharError::phar(format!(
            "No values passed to Phar::mungServer(), {LIST}"
        )));
    }
    if values.len() > 4 {
        return Err(PharError::phar(format!(
            "Too many values passed to Phar::mungServer(), {LIST}"
        )));
    }
    for v in values {
        let Some(v) = v else {
            return Err(PharError::phar(format!(
                "Non-string value passed to Phar::mungServer(), {LIST}"
            )));
        };
        let bit = match v.as_slice() {
            b"PHP_SELF" => MUNG_PHP_SELF,
            b"REQUEST_URI" => MUNG_REQUEST_URI,
            b"SCRIPT_NAME" => MUNG_SCRIPT_NAME,
            b"SCRIPT_FILENAME" => MUNG_SCRIPT_FILENAME,
            _ => {
                return Err(PharError::phar(format!(
                    "Invalid value passed to Phar::mungServer(), {LIST}"
                )))
            }
        };
        reg::add_mung(bit);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_lookup() {
        assert_eq!(file_type(b"/a.php"), (MIME_PHP, Vec::new()));
        assert_eq!(file_type(b"/a.phps").0, MIME_PHPS);
        assert_eq!(file_type(b"/a.jpg"), (MIME_OTHER, b"image/jpeg".to_vec()));
        assert_eq!(
            file_type(b"/a.gronk"),
            (MIME_OTHER, b"application/octet-stream".to_vec())
        );
        assert_eq!(file_type(b"/noext"), (MIME_OTHER, b"text/plain".to_vec()));
    }

    #[test]
    fn mung_errors() {
        assert!(mung_server(&[])
            .unwrap_err()
            .message
            .starts_with("No values"));
        assert!(mung_server(&[None])
            .unwrap_err()
            .message
            .starts_with("Non-string"));
        assert!(mung_server(&[Some(b"FOO".to_vec())])
            .unwrap_err()
            .message
            .starts_with("Invalid value"));
        assert!(mung_server(&vec![Some(b"PHP_SELF".to_vec()); 5])
            .unwrap_err()
            .message
            .starts_with("Too many"));
        assert!(mung_server(&[Some(b"PHP_SELF".to_vec())]).is_ok());
    }
}
