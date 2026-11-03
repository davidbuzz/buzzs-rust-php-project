//! Stream wrappers and the wrapper registry (`php_stream_wrapper`,
//! `url_stream_wrappers_hash`, `FG(stream_wrappers)`).

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use crate::context::ContextRef;
use crate::diag::{self, Severity, StreamErrorCode};
use crate::stat::StatBuf;
use crate::stream::Stream;
use crate::util::starts_with_ci;

/// `USE_PATH`.
pub const USE_PATH: u32 = 0x1;
/// `IGNORE_URL`.
pub const IGNORE_URL: u32 = 0x2;
/// `REPORT_ERRORS`.
pub const REPORT_ERRORS: u32 = 0x8;
/// `STREAM_MUST_SEEK`.
pub const STREAM_MUST_SEEK: u32 = 0x10;
/// `STREAM_WILL_CAST`.
pub const STREAM_WILL_CAST: u32 = 0x20;
/// `STREAM_LOCATE_WRAPPERS_ONLY`.
pub const STREAM_LOCATE_WRAPPERS_ONLY: u32 = 0x40;
/// `STREAM_OPEN_FOR_INCLUDE`.
pub const STREAM_OPEN_FOR_INCLUDE: u32 = 0x80;
/// `STREAM_USE_URL`.
pub const STREAM_USE_URL: u32 = 0x100;
/// `STREAM_DISABLE_OPEN_BASEDIR`.
pub const STREAM_DISABLE_OPEN_BASEDIR: u32 = 0x400;
/// `STREAM_OPEN_PERSISTENT`.
pub const STREAM_OPEN_PERSISTENT: u32 = 0x800;
/// `STREAM_USE_GLOB_DIR_OPEN`.
pub const STREAM_USE_GLOB_DIR_OPEN: u32 = 0x1000;
/// `STREAM_DISABLE_URL_PROTECTION`.
pub const STREAM_DISABLE_URL_PROTECTION: u32 = 0x2000;
/// `STREAM_ASSUME_REALPATH`.
pub const STREAM_ASSUME_REALPATH: u32 = 0x4000;

/// `PHP_STREAM_URL_STAT_LINK`.
pub const URL_STAT_LINK: u32 = 1;
/// `PHP_STREAM_URL_STAT_QUIET`.
pub const URL_STAT_QUIET: u32 = 2;
/// `PHP_STREAM_URL_STAT_IGNORE_OPEN_BASEDIR`.
pub const URL_STAT_IGNORE_OPEN_BASEDIR: u32 = 4;
/// `PHP_STREAM_MKDIR_RECURSIVE`.
pub const MKDIR_RECURSIVE: u32 = 1;

/// The label PHP uses when no wrapper is known.
pub const DEFAULT_WRAPPER_NAME: &str = ":na";

/// `stream_metadata()` operations (`PHP_STREAM_META_*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataOp {
    /// `touch()` with optional `(mtime, atime)`.
    Touch(Option<(i64, i64)>),
    /// `chown()` by name.
    OwnerName(Vec<u8>),
    /// `chown()` by id.
    Owner(i64),
    /// `chgrp()` by name.
    GroupName(Vec<u8>),
    /// `chgrp()` by id.
    Group(i64),
    /// `chmod()`.
    Access(i64),
}

impl MetadataOp {
    /// The `PHP_STREAM_META_*` option number.
    pub fn option(&self) -> i64 {
        match self {
            MetadataOp::Touch(_) => 1,
            MetadataOp::OwnerName(_) => 2,
            MetadataOp::Owner(_) => 3,
            MetadataOp::GroupName(_) => 4,
            MetadataOp::Group(_) => 5,
            MetadataOp::Access(_) => 6,
        }
    }
}

/// Why an opener failed. Openers run without `REPORT_ERRORS`; the errors
/// they log are joined into the final "Failed to open stream: ..." warning.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenError {
    /// Messages logged with `php_stream_wrapper_log_error()`.
    pub messages: Vec<Vec<u8>>,
    /// The `errno` at failure (used by the plain files wrapper).
    pub errno: Option<i32>,
    /// Set when an exception was thrown (no warning is displayed).
    pub exception: bool,
}

impl OpenError {
    /// A failure with a logged message.
    pub fn msg(m: impl Into<Vec<u8>>) -> Self {
        OpenError {
            messages: vec![m.into()],
            ..OpenError::default()
        }
    }

    /// A failure with an errno.
    pub fn errno(e: i32) -> Self {
        OpenError {
            errno: Some(e),
            ..OpenError::default()
        }
    }

    /// A silent failure (warnings already emitted or none wanted).
    pub fn silent() -> Self {
        OpenError::default()
    }

    /// A failure caused by an exception: no warning is displayed
    /// (`php_stream_display_wrapper_errors` returns early when
    /// `EG(exception)` is set).
    pub fn exception() -> Self {
        OpenError {
            exception: true,
            ..OpenError::default()
        }
    }
}

/// A stream wrapper (`php_stream_wrapper` with its `wops`).
///
/// Optional operations return `None` when the wrapper does not implement
/// them (a NULL `wops` slot).
pub trait Wrapper: fmt::Debug {
    /// `wops->label`.
    fn label(&self) -> &'static str;

    /// `is_url`.
    fn is_url(&self) -> bool {
        false
    }

    /// Whether this is the plain files wrapper.
    fn is_plain_files(&self) -> bool {
        false
    }

    /// Whether `stream_opener` exists.
    fn has_opener(&self) -> bool {
        true
    }

    /// `stream_opener`.
    fn open(
        &self,
        path: &[u8],
        mode: &str,
        options: u32,
        opened_path: &mut Option<Vec<u8>>,
        context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError>;

    /// `url_stat`; `None` if unsupported, `Some(Err(()))` on failure.
    fn url_stat(
        &self,
        _path: &[u8],
        _flags: u32,
        _context: Option<&ContextRef>,
    ) -> Option<Result<StatBuf, ()>> {
        None
    }

    /// Whether `dir_opener` exists.
    fn has_dir_opener(&self) -> bool {
        false
    }

    /// `dir_opener`.
    fn opendir(
        &self,
        _path: &[u8],
        _options: u32,
        _context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        Err(OpenError::silent())
    }

    /// Whether `unlink` exists.
    fn has_unlink(&self) -> bool {
        false
    }

    /// Whether `rename` exists.
    fn has_rename(&self) -> bool {
        false
    }

    /// `unlink`.
    fn unlink(&self, _path: &[u8], _options: u32, _context: Option<&ContextRef>) -> Option<bool> {
        None
    }

    /// `rename`.
    fn rename(
        &self,
        _from: &[u8],
        _to: &[u8],
        _options: u32,
        _context: Option<&ContextRef>,
    ) -> Option<bool> {
        None
    }

    /// `stream_mkdir`.
    fn mkdir(
        &self,
        _path: &[u8],
        _mode: i64,
        _options: u32,
        _context: Option<&ContextRef>,
    ) -> Option<bool> {
        None
    }

    /// `stream_rmdir`.
    fn rmdir(&self, _path: &[u8], _options: u32, _context: Option<&ContextRef>) -> Option<bool> {
        None
    }

    /// `stream_metadata`.
    fn metadata(
        &self,
        _path: &[u8],
        _op: &MetadataOp,
        _context: Option<&ContextRef>,
    ) -> Option<bool> {
        None
    }
}

/// Reports a wrapper error (`php_stream_wrapper_warn` and friends) when
/// `options` includes `REPORT_ERRORS`.
pub fn wrapper_error(
    label: &str,
    options: u32,
    severity: Severity,
    terminating: bool,
    code: StreamErrorCode,
    msg: impl Into<Vec<u8>>,
) {
    if options & REPORT_ERRORS != 0 {
        diag::stream_error(severity, label, code, terminating, msg);
    }
}

/// `php_stream_wrapper_warn`.
pub fn wrapper_warn(label: &str, options: u32, code: StreamErrorCode, msg: impl Into<Vec<u8>>) {
    wrapper_error(label, options, Severity::Warning, true, code, msg);
}

/// An ordered wrapper table.
#[derive(Debug, Clone, Default)]
pub struct WrapperTable {
    entries: Vec<(Vec<u8>, Rc<dyn Wrapper>)>,
}

impl WrapperTable {
    /// Finds a wrapper by exact protocol.
    pub fn get(&self, protocol: &[u8]) -> Option<Rc<dyn Wrapper>> {
        self.entries
            .iter()
            .find(|(p, _)| p == protocol)
            .map(|(_, w)| Rc::clone(w))
    }

    /// Adds a wrapper; false when the protocol exists.
    pub fn add(&mut self, protocol: &[u8], wrapper: Rc<dyn Wrapper>) -> bool {
        if self.get(protocol).is_some() {
            return false;
        }
        self.entries.push((protocol.to_vec(), wrapper));
        true
    }

    /// Removes a wrapper; false when absent.
    pub fn remove(&mut self, protocol: &[u8]) -> bool {
        let n = self.entries.len();
        self.entries.retain(|(p, _)| p != protocol);
        n != self.entries.len()
    }

    /// The protocols in order.
    pub fn protocols(&self) -> Vec<Vec<u8>> {
        self.entries.iter().map(|(p, _)| p.clone()).collect()
    }
}

#[derive(Debug, Default)]
struct Registry {
    global: WrapperTable,
    volatile: Option<WrapperTable>,
    filters_global: crate::filter::FilterRegistry,
    filters_volatile: Option<crate::filter::FilterRegistry>,
    initialised: bool,
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        if !r.initialised {
            r.initialised = true;
            let reg = &mut *r;
            crate::builtin::register_builtin(&mut reg.global, &mut reg.filters_global);
        }
        f(&mut r)
    })
}

/// `php_stream_wrapper_scheme_validate()`.
pub fn scheme_valid(protocol: &[u8]) -> bool {
    protocol
        .iter()
        .all(|&c| c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.')
}

/// `php_register_url_stream_wrapper()` (module-startup registration).
///
/// Built-in wrappers (`php`, `file`, `glob`, `data`) are registered on first
/// use; extensions registered before that point (e.g. `compress.zlib`) come
/// first in `stream_get_wrappers()`, mirroring PHP's MINIT order.
pub fn register_global(protocol: &[u8], wrapper: Rc<dyn Wrapper>) -> bool {
    if !scheme_valid(protocol) {
        return false;
    }
    with_registry(|r| r.global.add(protocol, wrapper))
}

/// `php_register_url_stream_wrapper_volatile()` (`stream_wrapper_register`).
pub fn register_volatile(protocol: &[u8], wrapper: Rc<dyn Wrapper>) -> bool {
    if !scheme_valid(protocol) {
        return false;
    }
    with_registry(|r| {
        let table = r.volatile.get_or_insert_with(|| r.global.clone());
        table.add(protocol, wrapper)
    })
}

/// `php_unregister_url_stream_wrapper_volatile()`.
pub fn unregister_volatile(protocol: &[u8]) -> bool {
    with_registry(|r| {
        let table = r.volatile.get_or_insert_with(|| r.global.clone());
        table.remove(protocol)
    })
}

/// Looks up a wrapper in the active table by exact protocol.
pub fn find(protocol: &[u8]) -> Option<Rc<dyn Wrapper>> {
    with_registry(|r| r.volatile.as_ref().unwrap_or(&r.global).get(protocol))
}

/// Looks up a wrapper in the global (startup) table.
pub fn find_global(protocol: &[u8]) -> Option<Rc<dyn Wrapper>> {
    with_registry(|r| r.global.get(protocol))
}

/// Whether the per-request table differs from the global one.
pub fn has_volatile() -> bool {
    with_registry(|r| r.volatile.is_some())
}

/// `stream_get_wrappers()`.
pub fn protocols() -> Vec<Vec<u8>> {
    with_registry(|r| r.volatile.as_ref().unwrap_or(&r.global).protocols())
}

/// Drops per-request registrations (wrappers and filters).
pub fn reset_volatile() {
    REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.volatile = None;
        r.filters_volatile = None;
    });
}

/// Runs `f` with the active filter registry.
pub fn with_filters<R>(f: impl FnOnce(&crate::filter::FilterRegistry) -> R) -> R {
    let reg = with_registry(|r| {
        r.filters_volatile
            .as_ref()
            .unwrap_or(&r.filters_global)
            .clone()
    });
    f(&reg)
}

/// `php_stream_filter_register_factory()` (startup).
pub fn register_filter_global(
    pattern: &str,
    factory: Rc<dyn crate::filter::FilterFactory>,
) -> bool {
    with_registry(|r| r.filters_global.register(pattern, factory))
}

/// `php_stream_filter_register_factory_volatile()` (`stream_filter_register`).
pub fn register_filter_volatile(
    pattern: &str,
    factory: Rc<dyn crate::filter::FilterFactory>,
) -> bool {
    with_registry(|r| {
        let reg = r
            .filters_volatile
            .get_or_insert_with(|| r.filters_global.clone());
        reg.register(pattern, factory)
    })
}

/// `stream_get_filters()`.
pub fn filter_names() -> Vec<String> {
    with_filters(crate::filter::FilterRegistry::names)
}

/// `php_stream_filter_create()` against the active registry.
pub fn create_filter(
    name: &str,
    params: Option<&crate::value::Value>,
    persistent: bool,
) -> Option<Box<dyn crate::filter::StreamFilter>> {
    with_filters(|r| r.create(name, params, persistent))
}

/// Length of the scheme prefix (`[A-Za-z0-9+.-]*`).
pub fn scheme_len(path: &[u8]) -> usize {
    path.iter()
        .take_while(|&&c| c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.')
        .count()
}

/// The result of [`locate`].
#[derive(Debug, Clone)]
pub struct Located {
    /// The wrapper.
    pub wrapper: Rc<dyn Wrapper>,
    /// The path to pass to the wrapper (`path_for_open`).
    pub path: Vec<u8>,
}

/// `php_stream_locate_url_wrapper()`. Emits PHP's warnings; returns
/// `None` when no wrapper may be used.
#[allow(clippy::too_many_lines)] // a direct port of the C function
pub fn locate(path: &[u8], options: u32) -> Option<Located> {
    let plain = || crate::builtin::plain_wrapper();
    if options & IGNORE_URL != 0 {
        if options & STREAM_LOCATE_WRAPPERS_ONLY != 0 {
            return None;
        }
        return Some(Located {
            wrapper: plain(),
            path: path.to_vec(),
        });
    }
    let n = scheme_len(path);
    let mut protocol: Option<&[u8]> = None;
    if path.get(n) == Some(&b':')
        && n > 1
        && (path[n + 1..].starts_with(b"//") || (n == 4 && path.starts_with(b"data:")))
    {
        protocol = Some(&path[..n]);
    }
    let mut wrapper = None;
    if let Some(proto) = protocol {
        wrapper = find(proto).or_else(|| find(&proto.to_ascii_lowercase()));
        if wrapper.is_none() {
            let shown = &proto[..proto.len().min(31)];
            diag::warning(diag::msg(&[
                b"Unable to find the wrapper \"",
                shown,
                b"\" - did you forget to enable it when you configured PHP?",
            ]));
            protocol = None;
        }
    }
    if protocol.is_none_or(|p| p.eq_ignore_ascii_case(b"file")) {
        let mut path_for_open = path.to_vec();
        if protocol.is_some() {
            let localhost = starts_with_ci(path, b"file://localhost/");
            if !localhost && path.len() > n + 3 && path[n + 3] != b'/' {
                wrapper_warn(
                    "plainfile",
                    options,
                    StreamErrorCode::ProtocolUnsupported,
                    diag::msg(&[b"Remote host file access not supported, ", path]),
                );
                return None;
            }
            // skip past protocol and :/, keeping one leading slash
            let mut i = n + 1;
            if localhost {
                i += 11;
            }
            loop {
                i += 1;
                if path.get(i) != Some(&b'/') {
                    break;
                }
            }
            i -= 1;
            path_for_open = path[i.min(path.len())..].to_vec();
        }
        if options & STREAM_LOCATE_WRAPPERS_ONLY != 0 {
            return None;
        }
        if has_volatile() {
            if let Some(w) = wrapper {
                return Some(Located {
                    wrapper: w,
                    path: path_for_open,
                });
            }
            if let Some(w) = find(b"file") {
                return Some(Located {
                    wrapper: w,
                    path: path_for_open,
                });
            }
            wrapper_warn(
                "plainfile",
                options,
                StreamErrorCode::Disabled,
                "file:// wrapper is disabled in the server configuration",
            );
            return None;
        }
        return Some(Located {
            wrapper: plain(),
            path: path_for_open,
        });
    }
    let wrapper = wrapper?;
    let proto = protocol.unwrap_or_default();
    if wrapper.is_url() && options & STREAM_DISABLE_URL_PROTECTION == 0 {
        let s = crate::state::settings();
        if !s.allow_url_fopen
            || ((options & STREAM_OPEN_FOR_INCLUDE != 0 || crate::state::in_user_include())
                && !s.allow_url_include)
        {
            if options & REPORT_ERRORS != 0 {
                let which = if s.allow_url_fopen {
                    "allow_url_include=0"
                } else {
                    "allow_url_fopen=0"
                };
                diag::warning(diag::msg(&[
                    proto,
                    b":// wrapper is disabled in the server configuration by ",
                    which.as_bytes(),
                ]));
            }
            return None;
        }
    }
    Some(Located {
        wrapper,
        path: path.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_order() {
        reset_volatile();
        let p = protocols();
        let names: Vec<String> = p
            .iter()
            .map(|x| String::from_utf8_lossy(x).into_owned())
            .collect();
        assert_eq!(names, vec!["php", "file", "glob", "data"]);
    }

    #[test]
    fn locate_file_urls() {
        let l = locate(b"file:///etc/passwd", 0).unwrap();
        assert_eq!(l.path, b"/etc/passwd");
        assert!(l.wrapper.is_plain_files());
        let l = locate(b"file://localhost/etc/passwd", 0).unwrap();
        assert_eq!(l.path, b"/etc/passwd");
        let l = locate(b"/tmp/x", 0).unwrap();
        assert_eq!(l.path, b"/tmp/x");
        let l = locate(b"c:/x", 0).unwrap();
        assert!(l.wrapper.is_plain_files());
        let _ = diag::take();
        assert!(locate(b"file://host/x", REPORT_ERRORS).is_none());
        assert_eq!(
            diag::take()[0].text(),
            "Remote host file access not supported, file://host/x"
        );
        assert!(locate(b"foo://bar", 0).unwrap().wrapper.is_plain_files());
        assert_eq!(
            diag::take()[0].text(),
            "Unable to find the wrapper \"foo\" - did you forget to enable it when you configured PHP?"
        );
        // "data:" without "//" is only recognised in lower case.
        let l = locate(b"DATA:,x", 0).unwrap();
        assert_eq!(l.wrapper.label(), "plainfile");
        let l = locate(b"data:,x", 0).unwrap();
        assert_eq!(l.wrapper.label(), "RFC2397");
        let l = locate(b"PHP://memory", 0).unwrap();
        assert_eq!(l.wrapper.label(), "PHP");
    }

    #[test]
    fn filter_order() {
        reset_volatile();
        assert_eq!(
            filter_names(),
            [
                "string.rot13",
                "string.toupper",
                "string.tolower",
                "convert.*",
                "consumed",
                "dechunk"
            ]
        );
    }

    #[test]
    fn open_basedir_on_open() {
        let _ = diag::take();
        let d = crate::test_util::TempDir::new();
        crate::state::update_settings(|s| s.open_basedir = d.path());
        assert!(crate::open::fopen(b"/etc/passwd", "r", false, None).is_none());
        let msgs: Vec<String> = diag::take().iter().map(diag::Diagnostic::text).collect();
        assert_eq!(
            msgs,
            [
                format!(
                    "open_basedir restriction in effect. File(/etc/passwd) is not within the allowed path(s): ({})",
                    String::from_utf8_lossy(&d.path())
                ),
                "Failed to open stream: Operation not permitted".to_string(),
            ]
        );
        assert!(crate::open::fopen(&d.file("new"), "w", false, None).is_some());
        crate::state::update_settings(|s| s.open_basedir.clear());
    }

    #[test]
    fn volatile_registration() {
        reset_volatile();
        assert!(!register_volatile(
            b"bad scheme",
            crate::builtin::plain_wrapper()
        ));
        assert!(!register_volatile(b"file", crate::builtin::plain_wrapper()));
        assert!(register_volatile(b"mine", crate::builtin::plain_wrapper()));
        assert!(protocols().iter().any(|p| p == b"mine"));
        assert!(unregister_volatile(b"file"));
        assert!(locate(b"/x", REPORT_ERRORS).is_none());
        let _ = diag::take();
        reset_volatile();
        assert!(locate(b"/x", 0).is_some());
    }
}
