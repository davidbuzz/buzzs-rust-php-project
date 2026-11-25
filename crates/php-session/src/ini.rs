// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `session.*` INI directives (`PHP_INI_BEGIN` … `PHP_INI_END` of
//! `session.c`): defaults, access levels, the on-modify validation of every
//! directive and a small store implementing `zend_alter_ini_entry_ex`,
//! `ini_get`, `ini_restore` and request deactivation for them.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::host::SessionHost;
use crate::serializer::Serializer;
use crate::state::{already_started_error, headers_sent_error, warn, SessionStatus};
use php_runtime::numeric::{is_numeric_string_ex, Number};
use php_runtime::{ErrorLevel, RuntimeError, ZStr};
use php_support::ini::{parse_bool, parse_quantity};

/// `PHP_INI_USER`.
pub const INI_USER: u8 = 1;
/// `PHP_INI_PERDIR`.
pub const INI_PERDIR: u8 = 2;
/// `PHP_INI_SYSTEM`.
pub const INI_SYSTEM: u8 = 4;
/// `PHP_INI_ALL`.
pub const INI_ALL: u8 = 7;

/// `ZEND_INI_STAGE_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IniStage {
    /// Module startup (`php.ini`, `-d`).
    Startup,
    /// Module shutdown.
    Shutdown,
    /// Request activation.
    Activate,
    /// Request deactivation (restoring modified values).
    Deactivate,
    /// `ini_set()` and the session functions.
    Runtime,
    /// `.htaccess`.
    Htaccess,
}

/// `session.cookie_lifetime` upper bound (`ZEND_LONG_MAX - INT_MAX - 1`).
pub const MAX_COOKIE_LIFETIME: i64 = i64::MAX - i32::MAX as i64 - 1;

const FORBIDDEN_NAME_CHARS: &[u8] = b"=,;.[ \t\r\n\x0b\x0c";
const FORBIDDEN_NAME_CHARS_MSG: &str = "=,;.[ \\t\\r\\n\\013\\014";
const FORBIDDEN_PATH_CHARS: &[u8] = b";\r\n";
const FORBIDDEN_PATH_CHARS_MSG: &str = ";\\r\\n";
const FORBIDDEN_DOMAIN_CHARS: &[u8] = b",; \t\r\n\x0b\x0c";
const FORBIDDEN_DOMAIN_CHARS_MSG: &str = ",; \\t\\r\\n\\013\\014";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    SaveDir,
    Name,
    SaveHandler,
    PlainBool,
    GcProbability,
    GcDivisor,
    SessionLong,
    Serializer,
    CookieLifetime,
    CookiePath,
    CookieDomain,
    SessionBool,
    SameSite,
    UseOnlyCookies,
    RefererCheck,
    SessionStr,
    UseTransSid,
    SidLength,
    SidBits,
    PlainStr,
    Rfc1867Freq,
    Real,
}

/// A `session.*` directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Directive {
    /// Full name (`"session.save_path"`).
    pub name: &'static str,
    /// Built-in default.
    pub default: &'static str,
    /// Access mask (`INI_ALL` or `INI_PERDIR`).
    pub access: u8,
    kind: Kind,
}

const fn d(name: &'static str, default: &'static str, access: u8, kind: Kind) -> Directive {
    Directive {
        name,
        default,
        access,
        kind,
    }
}

/// Every directive in registration order.
pub const DIRECTIVES: &[Directive] = &[
    d("session.save_path", "", INI_ALL, Kind::SaveDir),
    d("session.name", "PHPSESSID", INI_ALL, Kind::Name),
    d("session.save_handler", "files", INI_ALL, Kind::SaveHandler),
    d("session.auto_start", "0", INI_PERDIR, Kind::PlainBool),
    d("session.gc_probability", "1", INI_ALL, Kind::GcProbability),
    d("session.gc_divisor", "100", INI_ALL, Kind::GcDivisor),
    d("session.gc_maxlifetime", "1440", INI_ALL, Kind::SessionLong),
    d(
        "session.serialize_handler",
        "php",
        INI_ALL,
        Kind::Serializer,
    ),
    d(
        "session.cookie_lifetime",
        "0",
        INI_ALL,
        Kind::CookieLifetime,
    ),
    d("session.cookie_path", "/", INI_ALL, Kind::CookiePath),
    d("session.cookie_domain", "", INI_ALL, Kind::CookieDomain),
    d("session.cookie_secure", "0", INI_ALL, Kind::SessionBool),
    d(
        "session.cookie_partitioned",
        "0",
        INI_ALL,
        Kind::SessionBool,
    ),
    d("session.cookie_httponly", "1", INI_ALL, Kind::SessionBool),
    d("session.cookie_samesite", "Lax", INI_ALL, Kind::SameSite),
    d("session.use_cookies", "1", INI_ALL, Kind::SessionBool),
    d(
        "session.use_only_cookies",
        "1",
        INI_ALL,
        Kind::UseOnlyCookies,
    ),
    d("session.use_strict_mode", "1", INI_ALL, Kind::SessionBool),
    d("session.referer_check", "", INI_ALL, Kind::RefererCheck),
    d(
        "session.cache_limiter",
        "nocache",
        INI_ALL,
        Kind::SessionStr,
    ),
    d("session.cache_expire", "180", INI_ALL, Kind::SessionLong),
    d("session.use_trans_sid", "0", INI_ALL, Kind::UseTransSid),
    d("session.sid_length", "32", INI_ALL, Kind::SidLength),
    d(
        "session.sid_bits_per_character",
        "4",
        INI_ALL,
        Kind::SidBits,
    ),
    d("session.lazy_write", "1", INI_ALL, Kind::SessionBool),
    // Off by default (GHSA-658j-r8rw-q6mx, CVE-2026-115560).
    d(
        "session.upload_progress.enabled",
        "0",
        INI_PERDIR,
        Kind::PlainBool,
    ),
    d(
        "session.upload_progress.cleanup",
        "1",
        INI_PERDIR,
        Kind::PlainBool,
    ),
    d(
        "session.upload_progress.prefix",
        "upload_progress_",
        INI_PERDIR,
        Kind::PlainStr,
    ),
    d(
        "session.upload_progress.name",
        "PHP_SESSION_UPLOAD_PROGRESS",
        INI_PERDIR,
        Kind::PlainStr,
    ),
    d(
        "session.upload_progress.freq",
        "1%",
        INI_PERDIR,
        Kind::Rfc1867Freq,
    ),
    d(
        "session.upload_progress.min_freq",
        "1",
        INI_PERDIR,
        Kind::Real,
    ),
];

/// Index of a directive by exact name.
#[must_use]
pub fn directive_index(name: &str) -> Option<usize> {
    DIRECTIVES.iter().position(|d| d.name == name)
}

/// The typed values the directives update (the `php_ps_globals` fields).
#[allow(clippy::struct_excessive_bools)] // mirrors the INI flags
#[derive(Debug, Clone, PartialEq)]
pub struct SessionConfig {
    /// `session.save_path`.
    pub save_path: ZStr,
    /// `session.name`.
    pub session_name: ZStr,
    /// `session.auto_start`.
    pub auto_start: bool,
    /// `session.gc_probability`.
    pub gc_probability: i64,
    /// `session.gc_divisor`.
    pub gc_divisor: i64,
    /// `session.gc_maxlifetime`.
    pub gc_maxlifetime: i64,
    /// `session.cookie_lifetime`.
    pub cookie_lifetime: i64,
    /// `session.cookie_path`.
    pub cookie_path: ZStr,
    /// `session.cookie_domain`.
    pub cookie_domain: ZStr,
    /// `session.cookie_secure`.
    pub cookie_secure: bool,
    /// `session.cookie_partitioned`.
    pub cookie_partitioned: bool,
    /// `session.cookie_httponly`.
    pub cookie_httponly: bool,
    /// `session.cookie_samesite`.
    pub cookie_samesite: ZStr,
    /// `session.use_cookies`.
    pub use_cookies: bool,
    /// `session.use_only_cookies`.
    pub use_only_cookies: bool,
    /// `session.use_strict_mode`.
    pub use_strict_mode: bool,
    /// `session.referer_check`.
    pub referer_check: ZStr,
    /// `session.cache_limiter`.
    pub cache_limiter: ZStr,
    /// `session.cache_expire` (minutes).
    pub cache_expire: i64,
    /// `session.use_trans_sid`.
    pub use_trans_sid: bool,
    /// `session.sid_length`.
    pub sid_length: usize,
    /// `session.sid_bits_per_character`.
    pub sid_bits_per_character: u8,
    /// `session.lazy_write`.
    pub lazy_write: bool,
    /// `session.upload_progress.enabled`.
    pub rfc1867_enabled: bool,
    /// `session.upload_progress.cleanup`.
    pub rfc1867_cleanup: bool,
    /// `session.upload_progress.prefix`.
    pub rfc1867_prefix: ZStr,
    /// `session.upload_progress.name`.
    pub rfc1867_name: ZStr,
    /// `session.upload_progress.freq` (negative = percent).
    pub rfc1867_freq: i64,
    /// `session.upload_progress.min_freq`.
    pub rfc1867_min_freq: f64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            save_path: ZStr::empty(),
            session_name: ZStr::from("PHPSESSID"),
            auto_start: false,
            gc_probability: 1,
            gc_divisor: 100,
            gc_maxlifetime: 1440,
            cookie_lifetime: 0,
            cookie_path: ZStr::from("/"),
            cookie_domain: ZStr::empty(),
            cookie_secure: false,
            cookie_partitioned: false,
            cookie_httponly: true,
            cookie_samesite: ZStr::from("Lax"),
            use_cookies: true,
            use_only_cookies: true,
            use_strict_mode: true,
            referer_check: ZStr::empty(),
            cache_limiter: ZStr::from("nocache"),
            cache_expire: 180,
            use_trans_sid: false,
            sid_length: 32,
            sid_bits_per_character: 4,
            lazy_write: true,
            rfc1867_enabled: false,
            rfc1867_cleanup: true,
            rfc1867_prefix: ZStr::from("upload_progress_"),
            rfc1867_name: ZStr::from("PHP_SESSION_UPLOAD_PROGRESS"),
            rfc1867_freq: -1,
            rfc1867_min_freq: 1.0,
        }
    }
}

/// The raw (string) value of one directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IniEntry {
    /// Current value.
    pub value: Vec<u8>,
    /// Value before the first runtime modification.
    pub orig: Option<Vec<u8>>,
    /// Whether the entry was modified at runtime.
    pub modified: bool,
}

/// The raw values of all directives (indexed like [`DIRECTIVES`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IniStore {
    entries: Vec<IniEntry>,
    /// Changes on every change of an entry (see [`IniStore::revision`]).
    revision: u64,
}

/// Source of process-unique [`IniStore`] revisions.
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

fn next_revision() -> u64 {
    NEXT_REVISION.fetch_add(1, Ordering::Relaxed)
}

impl Default for IniStore {
    fn default() -> Self {
        Self {
            entries: DIRECTIVES
                .iter()
                .map(|d| IniEntry {
                    value: d.default.as_bytes().to_vec(),
                    orig: None,
                    modified: false,
                })
                .collect(),
            revision: next_revision(),
        }
    }
}

impl IniStore {
    /// A process-unique stamp of the entries: it changes whenever an entry
    /// changes, and two stores share one only when one is an unmodified
    /// clone of the other. Mirrors of the values (the VM's INI registry)
    /// compare it to skip re-reading unchanged values.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The entries, indexed like [`DIRECTIVES`].
    #[must_use]
    pub fn entries(&self) -> &[IniEntry] {
        &self.entries
    }

    /// The entry `idx`, for a change: takes a new revision.
    fn entry_mut(&mut self, idx: usize) -> &mut IniEntry {
        self.revision = next_revision();
        &mut self.entries[idx]
    }

    /// The current value of a directive.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        directive_index(name).map(|i| self.entries[i].value.as_slice())
    }

    /// The entry of a directive.
    #[must_use]
    pub fn entry(&self, name: &str) -> Option<&IniEntry> {
        directive_index(name).map(|i| &self.entries[i])
    }
}

/// `strtol(s, &end, 10)` over the C string (up to the first NUL); returns
/// the value and whether the whole string was consumed.
fn strtol_full(s: &[u8]) -> (i64, bool) {
    let s = s.split(|&c| c == 0).next().unwrap_or_default();
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut v: i64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        let digit = i64::from(s[i] - b'0');
        v = if neg {
            v.saturating_mul(10).saturating_sub(digit)
        } else {
            v.saturating_mul(10).saturating_add(digit)
        };
        i += 1;
    }
    if i == start {
        // No conversion: endptr = s.
        return (0, s.is_empty());
    }
    (v, i == s.len())
}

/// `ZEND_ATOL`.
fn atol(s: &[u8]) -> i64 {
    let s = s.split(|&c| c == 0).next().unwrap_or_default();
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    let neg = i < s.len() && s[i] == b'-';
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        i += 1;
    }
    let mut v: i64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add(i64::from(s[i] - b'0'));
        i += 1;
    }
    if neg {
        -v
    } else {
        v
    }
}

/// `zend_strtod` over the longest numeric prefix (`OnUpdateReal`).
fn strtod_prefix(s: &[u8]) -> f64 {
    php_runtime::numeric::strtod(s).0
}

/// `strpbrk`-style check (stops at the first NUL byte).
fn has_any(value: &[u8], set: &[u8]) -> bool {
    value
        .iter()
        .take_while(|&&c| c != 0)
        .any(|c| set.contains(c))
}

fn lossy(v: &[u8]) -> String {
    String::from_utf8_lossy(v).into_owned()
}

/// `zend_ini_parse_quantity_warn()`.
fn quantity_warn(h: &mut dyn SessionHost, name: &str, v: &[u8]) -> i64 {
    let (n, err) = parse_quantity(v);
    if let Some(e) = err {
        let msg = php_support::ini::quantity::quantity_warning(name, &e);
        if let Err(e) = h.diagnostic(ErrorLevel::Warning, &msg) {
            h.session().set_exception(e);
        }
    }
    n
}

/// `SESSION_CHECK_UPLOAD_PROGRESS_STATE` + `SESSION_CHECK_ACTIVE_STATE` +
/// `SESSION_CHECK_OUTPUT_STATE`. With upload progress on, no session
/// setting changes at run time (GHSA-658j-r8rw-q6mx: the progress data is
/// written with the settings the request started with).
fn check_state(h: &mut dyn SessionHost, stage: IniStage) -> bool {
    if h.session().config.rfc1867_enabled && stage == IniStage::Runtime {
        h.session().set_exception(RuntimeError::Error(
            "Session ini settings cannot be changed at runtime when session.upload_progress.enabled is On"
                .into(),
        ));
        return false;
    }
    if h.session().status == SessionStatus::Active {
        already_started_error(
            h,
            ErrorLevel::Warning,
            "Session ini settings cannot be changed when a session is active",
        );
        return false;
    }
    if h.headers_sent() && stage != IniStage::Deactivate {
        headers_sent_error(
            h,
            ErrorLevel::Warning,
            "Session ini settings cannot be changed after headers have already been sent",
        );
        return false;
    }
    true
}

/// `"%s" must not contain null bytes` (not reported while restoring).
fn reject_nul(h: &mut dyn SessionHost, name: &str, v: &[u8], stage: IniStage) -> bool {
    if v.contains(&0) {
        if stage != IniStage::Deactivate {
            warn(
                h,
                ErrorLevel::Warning,
                &format!("\"{name}\" must not contain null bytes"),
            );
        }
        return true;
    }
    false
}

/// Runs the on-modify handler of directive `idx` with `v`. On success the
/// typed value is stored; the caller stores the raw value.
#[allow(clippy::too_many_lines)]
pub(crate) fn on_modify(h: &mut dyn SessionHost, idx: usize, v: &[u8], stage: IniStage) -> bool {
    let dir = DIRECTIVES[idx];
    let name = dir.name;
    let runtime_err = if stage == IniStage::Runtime {
        ErrorLevel::Warning
    } else {
        ErrorLevel::Error
    };
    let modules_activated = h.session().modules_activated;
    match dir.kind {
        Kind::SaveHandler => {
            if !check_state(h, stage) {
                return false;
            }
            let found = h.session().find_module(v);
            if modules_activated && found.is_none() {
                if stage != IniStage::Deactivate {
                    warn(
                        h,
                        runtime_err,
                        &format!("Session save handler \"{}\" cannot be found", lossy(v)),
                    );
                }
                return false;
            }
            let s = h.session();
            if !s.set_handler && found.as_ref().is_some_and(crate::handler::ModRef::is_user) {
                warn(
                    h,
                    runtime_err,
                    "Session save handler \"user\" cannot be set by ini_set()",
                );
                return false;
            }
            let s = h.session();
            // ref: session.c OnUpdateSaveHandler — `default_mod` becomes the
            // previously active *native* module only on the transition into the
            // user module; native→native changes leave `PS(default_mod)` NULL.
            let to_user = found.as_ref().is_some_and(crate::handler::ModRef::is_user);
            if s.default_mod.is_none() && to_user {
                s.default_mod = s.module.take();
            }
            s.module = found;
            true
        }
        Kind::Serializer => {
            if !check_state(h, stage) {
                return false;
            }
            let found = Serializer::find(v);
            if modules_activated && found.is_none() {
                if stage != IniStage::Deactivate {
                    warn(
                        h,
                        runtime_err,
                        &format!("Serialization handler \"{}\" cannot be found", lossy(v)),
                    );
                }
                return false;
            }
            h.session().serializer = found;
            true
        }
        Kind::SaveDir => {
            if !check_state(h, stage) || reject_nul(h, name, v, stage) {
                return false;
            }
            if matches!(stage, IniStage::Runtime | IniStage::Htaccess) {
                let path = match v.iter().position(|&c| c == b';') {
                    Some(p) => {
                        let rest = &v[p + 1..];
                        match rest.iter().position(|&c| c == b';') {
                            Some(p2) => &rest[p2 + 1..],
                            None => rest,
                        }
                    }
                    None => v,
                };
                if !path.is_empty() && h.open_basedir_denied(path) {
                    return false;
                }
            }
            h.session().config.save_path = ZStr::from(v);
            true
        }
        Kind::Name => {
            if !check_state(h, stage) {
                return false;
            }
            if v.is_empty()
                || v.contains(&0)
                || is_numeric_string_ex(v, false).is_some()
                || has_any(v, FORBIDDEN_NAME_CHARS)
            {
                let level = if matches!(
                    stage,
                    IniStage::Runtime | IniStage::Activate | IniStage::Startup
                ) {
                    ErrorLevel::Warning
                } else {
                    ErrorLevel::Error
                };
                if stage != IniStage::Deactivate {
                    warn(
                        h,
                        level,
                        &format!(
                            "session.name \"{}\" must not be numeric, empty, contain null bytes \
                             or any of the following characters \"{FORBIDDEN_NAME_CHARS_MSG}\"",
                            lossy(v)
                        ),
                    );
                }
                return false;
            }
            h.session().config.session_name = ZStr::from(v);
            true
        }
        Kind::CookieLifetime => {
            if !check_state(h, stage) {
                return false;
            }
            let parsed = is_numeric_string_ex(v, false);
            let lval = match parsed {
                Some(n) if matches!(n.value, Number::Int(_)) => match n.value {
                    Number::Int(i) => i,
                    Number::Float(_) => 0,
                },
                other => {
                    let msg = if other.is_some_and(|n| n.overflow != 0) {
                        format!(
                            "session.cookie_lifetime must be between 0 and {MAX_COOKIE_LIFETIME}"
                        )
                    } else {
                        "session.cookie_lifetime must be of type int".to_string()
                    };
                    warn(h, ErrorLevel::Warning, &msg);
                    return false;
                }
            };
            if !(0..=MAX_COOKIE_LIFETIME).contains(&lval) {
                warn(
                    h,
                    ErrorLevel::Warning,
                    &format!("session.cookie_lifetime must be between 0 and {MAX_COOKIE_LIFETIME}"),
                );
                return false;
            }
            let q = quantity_warn(h, name, v);
            if q < 0 {
                return false;
            }
            h.session().config.cookie_lifetime = q;
            true
        }
        Kind::SessionLong => {
            if !check_state(h, stage) {
                return false;
            }
            let q = quantity_warn(h, name, v);
            let c = &mut h.session().config;
            if name == "session.gc_maxlifetime" {
                c.gc_maxlifetime = q;
            } else {
                c.cache_expire = q;
            }
            true
        }
        Kind::SessionStr | Kind::CookiePath | Kind::CookieDomain => {
            let (set, msg) = match dir.kind {
                Kind::CookiePath => (FORBIDDEN_PATH_CHARS, FORBIDDEN_PATH_CHARS_MSG),
                Kind::CookieDomain => (FORBIDDEN_DOMAIN_CHARS, FORBIDDEN_DOMAIN_CHARS_MSG),
                _ => (&b""[..], ""),
            };
            if has_any(v, set) {
                if stage != IniStage::Deactivate {
                    warn(
                        h,
                        ErrorLevel::Warning,
                        &format!(
                            "\"{name}\" must not contain any of the following characters \"{msg}\""
                        ),
                    );
                }
                return false;
            }
            if !check_state(h, stage) || reject_nul(h, name, v, stage) {
                return false;
            }
            let c = &mut h.session().config;
            let z = ZStr::from(v);
            match dir.kind {
                Kind::CookiePath => c.cookie_path = z,
                Kind::CookieDomain => c.cookie_domain = z,
                _ => c.cache_limiter = z,
            }
            true
        }
        Kind::SameSite => {
            if !check_state(h, stage) {
                return false;
            }
            if !v.is_empty()
                && ![&b"Strict"[..], b"Lax", b"None"]
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(v))
            {
                warn(
                    h,
                    ErrorLevel::Warning,
                    "session.cookie_samesite must be \"Strict\", \"Lax\", \"None\", or \"\"",
                );
                return false;
            }
            h.session().config.cookie_samesite = ZStr::from(v);
            true
        }
        Kind::SessionBool => {
            if !check_state(h, stage) {
                return false;
            }
            let b = parse_bool(v);
            let c = &mut h.session().config;
            match name {
                "session.cookie_secure" => c.cookie_secure = b,
                "session.cookie_partitioned" => c.cookie_partitioned = b,
                "session.cookie_httponly" => c.cookie_httponly = b,
                "session.use_cookies" => c.use_cookies = b,
                "session.use_strict_mode" => c.use_strict_mode = b,
                _ => c.lazy_write = b,
            }
            true
        }
        Kind::PlainBool => {
            let b = parse_bool(v);
            let c = &mut h.session().config;
            match name {
                "session.auto_start" => c.auto_start = b,
                "session.upload_progress.enabled" => c.rfc1867_enabled = b,
                _ => c.rfc1867_cleanup = b,
            }
            true
        }
        Kind::PlainStr => {
            let c = &mut h.session().config;
            if name == "session.upload_progress.prefix" {
                c.rfc1867_prefix = ZStr::from(v);
            } else {
                c.rfc1867_name = ZStr::from(v);
            }
            true
        }
        Kind::Real => {
            h.session().config.rfc1867_min_freq = strtod_prefix(v);
            true
        }
        Kind::SidLength | Kind::SidBits => {
            if !check_state(h, stage) {
                return false;
            }
            let (val, full) = strtol_full(v);
            let (default, lo, hi) = if dir.kind == Kind::SidLength {
                (32, 22, 256)
            } else {
                (4, 4, 6)
            };
            if val != default {
                warn(
                    h,
                    ErrorLevel::Deprecated,
                    &format!("{name} INI setting is deprecated"),
                );
            }
            if !full || val < lo || val > hi {
                warn(
                    h,
                    ErrorLevel::Warning,
                    &format!("session.configuration \"{name}\" must be between {lo} and {hi}"),
                );
                return false;
            }
            let c = &mut h.session().config;
            if dir.kind == Kind::SidLength {
                c.sid_length = usize::try_from(val).unwrap_or(32);
            } else {
                c.sid_bits_per_character = u8::try_from(val).unwrap_or(4);
            }
            true
        }
        Kind::GcProbability | Kind::GcDivisor => {
            if !check_state(h, stage) {
                return false;
            }
            let q = quantity_warn(h, name, v);
            if dir.kind == Kind::GcProbability {
                if q < 0 {
                    warn(
                        h,
                        ErrorLevel::Warning,
                        "session.gc_probability must be greater than or equal to 0",
                    );
                    return false;
                }
                h.session().config.gc_probability = q;
            } else {
                if q <= 0 {
                    warn(
                        h,
                        ErrorLevel::Warning,
                        "session.gc_divisor must be greater than 0",
                    );
                    return false;
                }
                h.session().config.gc_divisor = q;
            }
            true
        }
        Kind::Rfc1867Freq => {
            let n = atol(v);
            if n < 0 {
                warn(
                    h,
                    ErrorLevel::Warning,
                    "session.upload_progress.freq must be greater than or equal to 0",
                );
                return false;
            }
            let freq = if v.ends_with(b"%") {
                if n > 100 {
                    warn(
                        h,
                        ErrorLevel::Warning,
                        "session.upload_progress.freq must be less than or equal to 100%",
                    );
                    return false;
                }
                -n
            } else {
                n
            };
            h.session().config.rfc1867_freq = freq;
            true
        }
        Kind::UseOnlyCookies | Kind::UseTransSid => {
            if !check_state(h, stage) {
                return false;
            }
            let b = parse_bool(v);
            if dir.kind == Kind::UseOnlyCookies {
                h.session().config.use_only_cookies = b;
                if !b {
                    warn(
                        h,
                        ErrorLevel::Deprecated,
                        "Disabling session.use_only_cookies INI setting is deprecated",
                    );
                }
            } else {
                h.session().config.use_trans_sid = b;
                if b {
                    warn(
                        h,
                        ErrorLevel::Deprecated,
                        "Enabling session.use_trans_sid INI setting is deprecated",
                    );
                }
            }
            true
        }
        Kind::RefererCheck => {
            if !check_state(h, stage) || reject_nul(h, name, v, stage) {
                return false;
            }
            if !v.is_empty() {
                warn(
                    h,
                    ErrorLevel::Deprecated,
                    "Usage of session.referer_check INI setting is deprecated",
                );
            }
            h.session().config.referer_check = ZStr::from(v);
            true
        }
    }
}

/// `zend_alter_ini_entry_ex(name, value, modify_type, stage, false)` for a
/// `session.*` directive. Returns `None` when `name` is not one of ours,
/// otherwise whether the change was accepted.
pub fn alter(
    h: &mut dyn SessionHost,
    name: &str,
    value: &[u8],
    modify_type: u8,
    stage: IniStage,
) -> Option<bool> {
    let idx = directive_index(name)?;
    if DIRECTIVES[idx].access & modify_type == 0 {
        return Some(false);
    }
    {
        let e = h.session().ini.entry_mut(idx);
        if !e.modified {
            e.orig = Some(e.value.clone());
            e.modified = true;
        }
    }
    if on_modify(h, idx, value, stage) {
        h.session().ini.entry_mut(idx).value = value.to_vec();
        Some(true)
    } else {
        Some(false)
    }
}

/// `ini_set(name, value)` from user code (`PHP_INI_USER`, runtime stage).
pub fn ini_set(h: &mut dyn SessionHost, name: &str, value: &[u8]) -> Option<bool> {
    alter(h, name, value, INI_USER, IniStage::Runtime)
}

/// `ini_get(name)`.
pub fn ini_get(h: &mut dyn SessionHost, name: &str) -> Option<Vec<u8>> {
    h.session().ini.get(name).map(<[u8]>::to_vec)
}

/// One row of `ini_get_all("session")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IniInfo {
    /// Directive name.
    pub name: &'static str,
    /// `global_value` (the value before runtime changes).
    pub global_value: Vec<u8>,
    /// `local_value` (the current value).
    pub local_value: Vec<u8>,
    /// `access` mask.
    pub access: u8,
}

/// Every directive with its values, sorted by name like `ini_get_all()`.
pub fn ini_get_all(h: &mut dyn SessionHost) -> Vec<IniInfo> {
    let store = &h.session().ini;
    let mut v: Vec<IniInfo> = DIRECTIVES
        .iter()
        .zip(&store.entries)
        .map(|(d, e)| IniInfo {
            name: d.name,
            global_value: e.orig.clone().unwrap_or_else(|| e.value.clone()),
            local_value: e.value.clone(),
            access: d.access,
        })
        .collect();
    v.sort_by(|a, b| a.name.cmp(b.name));
    v
}

/// `ini_restore(name)` (`zend_restore_ini_entry`, runtime stage). Returns
/// `None` when `name` is not a session directive.
pub fn ini_restore(h: &mut dyn SessionHost, name: &str) -> Option<()> {
    let idx = directive_index(name)?;
    restore_entry(h, idx, IniStage::Runtime);
    Some(())
}

fn restore_entry(h: &mut dyn SessionHost, idx: usize, stage: IniStage) {
    let e = &h.session().ini.entries[idx];
    if !e.modified {
        return;
    }
    let orig = e.orig.clone().unwrap_or_default();
    let ok = on_modify(h, idx, &orig, stage);
    if stage == IniStage::Runtime && !ok {
        return;
    }
    let e = h.session().ini.entry_mut(idx);
    e.value = orig;
    e.orig = None;
    e.modified = false;
}

/// Request-end restoration of every modified directive
/// (`zend_ini_deactivate`).
pub fn deactivate(h: &mut dyn SessionHost) {
    for idx in 0..DIRECTIVES.len() {
        restore_entry(h, idx, IniStage::Deactivate);
    }
}

/// `REGISTER_INI_ENTRIES()`: applies configuration values (`php.ini`,
/// `-d`) at startup, falling back to the default when a value is rejected.
pub fn register(h: &mut dyn SessionHost, config: &dyn Fn(&str) -> Option<Vec<u8>>) {
    h.session().modules_activated = false;
    for (idx, dir) in DIRECTIVES.iter().enumerate() {
        let value = match config(dir.name) {
            Some(v) if on_modify(h, idx, &v, IniStage::Startup) => v,
            _ => {
                let v = dir.default.as_bytes().to_vec();
                on_modify(h, idx, &v, IniStage::Startup);
                v
            }
        };
        let e = h.session().ini.entry_mut(idx);
        e.value = value;
        e.orig = None;
        e.modified = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strtol_semantics() {
        assert_eq!(strtol_full(b"32"), (32, true));
        assert_eq!(strtol_full(b" -5"), (-5, true));
        assert_eq!(strtol_full(b"40x"), (40, false));
        assert_eq!(strtol_full(b"abc"), (0, false));
        assert_eq!(strtol_full(b""), (0, true));
        assert_eq!(strtol_full(b"40\0x"), (40, true));
        assert_eq!(atol(b"10%"), 10);
        assert_eq!(atol(b"-3"), -3);
    }

    #[test]
    fn table_is_consistent() {
        assert_eq!(DIRECTIVES.len(), 31);
        assert_eq!(directive_index("session.name"), Some(1));
        assert!(directive_index("session.nope").is_none());
        let store = IniStore::default();
        assert_eq!(store.get("session.cache_limiter"), Some(&b"nocache"[..]));
        assert_eq!(MAX_COOKIE_LIFETIME, 9_223_372_034_707_292_159);
        let mut names: Vec<&str> = DIRECTIVES.iter().map(|d| d.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), DIRECTIVES.len());
    }
}
