// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `php-fpm.conf` and pool files (ref: `sapi/fpm/fpm/fpm_conf.c`): the
//! `[global]` section and one section per pool, read line by line through
//! the INI parser (`include=` globs, `${ENV}`, constants, `$pool`), then
//! checked and completed as `fpm_conf_post_process()` does.

use super::log::{self, Level};
use php_support::ini::{self, IniHooks, IniParserCallback, IniScalar, ScannerMode};

/// `PHP_PREFIX`: where relative paths go when no prefix is given.
pub const PHP_PREFIX: &str = "/usr/local";
/// `PHP_SYSCONFDIR`.
pub const PHP_SYSCONFDIR: &str = "/usr/local/etc";
/// `PHP_LOCALSTATEDIR`.
pub const PHP_LOCALSTATEDIR: &str = "/usr/local/var";
/// `FPM_BACKLOG_DEFAULT` on Linux (`-1`: the system's maximum).
pub const BACKLOG_DEFAULT: i32 = -1;

/// `pm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pm {
    /// Not set (an error).
    #[default]
    None,
    /// `static`: `pm.max_children` workers all the time.
    Static,
    /// `dynamic`: between the spare limits.
    Dynamic,
    /// `ondemand`: forked when a connection waits.
    Ondemand,
}

impl Pm {
    /// The name in the status page and `-tt`.
    pub fn name(self) -> &'static str {
        match self {
            Pm::None => "",
            Pm::Static => "static",
            Pm::Dynamic => "dynamic",
            Pm::Ondemand => "ondemand",
        }
    }
}

/// `struct fpm_global_config_s`.
#[derive(Debug, Clone)]
pub struct Global {
    pub pid_file: Option<String>,
    pub error_log: Option<String>,
    pub syslog_ident: Option<String>,
    pub syslog_facility: i32,
    pub log_buffering: bool,
    pub log_level: Level,
    pub log_limit: i32,
    pub emergency_restart_threshold: i32,
    pub emergency_restart_interval: i32,
    pub process_control_timeout: i32,
    pub process_max: i32,
    pub process_priority: i32,
    pub daemonize: bool,
    pub rlimit_files: i32,
    pub rlimit_core: i32,
    pub events_mechanism: Option<String>,
    pub systemd_interval: i32,
}

impl Default for Global {
    fn default() -> Self {
        Global {
            pid_file: None,
            error_log: None,
            syslog_ident: None,
            syslog_facility: -1,
            log_buffering: true,
            log_level: Level::Notice,
            log_limit: i32::try_from(log::DEFAULT_LIMIT).unwrap_or(1024),
            emergency_restart_threshold: 0,
            emergency_restart_interval: 0,
            process_control_timeout: 0,
            process_max: 0,
            process_priority: 64,
            daemonize: true,
            rlimit_files: 0,
            rlimit_core: 0,
            events_mechanism: None,
            systemd_interval: -1,
        }
    }
}

/// `struct fpm_worker_pool_config_s`.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // the C configuration's flags
pub struct Pool {
    pub name: String,
    pub prefix: Option<String>,
    pub user: Option<String>,
    pub group: Option<String>,
    pub listen_address: Option<String>,
    pub listen_backlog: i32,
    pub listen_acl_users: Option<String>,
    pub listen_acl_groups: Option<String>,
    pub listen_owner: Option<String>,
    pub listen_group: Option<String>,
    pub listen_mode: Option<String>,
    pub listen_allowed_clients: Option<String>,
    pub listen_setfib: i32,
    pub process_priority: i32,
    pub process_dumpable: bool,
    pub pm: Pm,
    pub pm_max_children: i32,
    pub pm_start_servers: i32,
    pub pm_min_spare_servers: i32,
    pub pm_max_spare_servers: i32,
    pub pm_max_spawn_rate: i32,
    pub pm_process_idle_timeout: i32,
    pub pm_max_requests: i32,
    pub pm_status_path: Option<String>,
    pub pm_status_listen: Option<String>,
    pub ping_path: Option<String>,
    pub ping_response: Option<String>,
    pub access_log: Option<String>,
    pub access_format: Option<String>,
    pub access_suppress_paths: Vec<String>,
    pub slowlog: Option<String>,
    pub request_slowlog_timeout: i32,
    pub request_slowlog_trace_depth: i32,
    pub request_terminate_timeout: i32,
    pub request_terminate_timeout_track_finished: bool,
    pub rlimit_files: i32,
    pub rlimit_core: i32,
    pub chroot: Option<String>,
    pub chdir: Option<String>,
    pub catch_workers_output: bool,
    pub decorate_workers_output: bool,
    pub clear_env: bool,
    pub security_limit_extensions: Option<String>,
    /// The array options are kept as `fpm_conf_set_array()` builds its
    /// lists: each entry before the ones already given.
    pub env: Vec<(String, String)>,
    pub php_values: Vec<(String, String)>,
    pub php_admin_values: Vec<(String, String)>,
    /// `wp->limit_extensions`.
    pub limit_extensions: Vec<String>,
    /// `wp->shared`: for a `pm.status_listen` pool, the pool whose status
    /// it shows.
    pub shared: Option<usize>,
}

impl Pool {
    /// `fpm_worker_pool_config_alloc()`'s defaults.
    fn new(name: &str) -> Pool {
        Pool {
            name: name.to_string(),
            shared: None,
            prefix: None,
            user: None,
            group: None,
            listen_address: None,
            listen_backlog: BACKLOG_DEFAULT,
            listen_acl_users: None,
            listen_acl_groups: None,
            listen_owner: None,
            listen_group: None,
            listen_mode: None,
            listen_allowed_clients: None,
            listen_setfib: -1,
            process_priority: 64,
            process_dumpable: false,
            pm: Pm::None,
            pm_max_children: 0,
            pm_start_servers: 0,
            pm_min_spare_servers: 0,
            pm_max_spare_servers: 0,
            pm_max_spawn_rate: 32,
            pm_process_idle_timeout: 10,
            pm_max_requests: 0,
            pm_status_path: None,
            pm_status_listen: None,
            ping_path: None,
            ping_response: None,
            access_log: None,
            access_format: None,
            access_suppress_paths: Vec::new(),
            slowlog: None,
            request_slowlog_timeout: 0,
            request_slowlog_trace_depth: 0,
            request_terminate_timeout: 0,
            request_terminate_timeout_track_finished: false,
            rlimit_files: 0,
            rlimit_core: 0,
            chroot: None,
            chdir: None,
            catch_workers_output: false,
            decorate_workers_output: true,
            clear_env: true,
            security_limit_extensions: None,
            env: Vec::new(),
            php_values: Vec::new(),
            php_admin_values: Vec::new(),
            limit_extensions: Vec::new(),
        }
    }
}

/// The whole configuration.
#[derive(Debug, Clone, Default)]
pub struct Conf {
    pub global: Global,
    pub pools: Vec<Pool>,
    /// The configuration file (`fpm_globals.config`).
    pub file: String,
    /// `-p` (`fpm_globals.prefix`).
    pub prefix: Option<String>,
}

/// `fpm_conf_set_boolean()`.
fn set_boolean(v: &str) -> Result<bool, String> {
    let yes = matches!(v, "1" | "yes" | "true" | "on");
    let no = yes || v.is_empty() || matches!(v, "0" | "no" | "none" | "false" | "off");
    if !no {
        return Err("invalid boolean value".into());
    }
    Ok(yes)
}

/// `fpm_conf_set_integer()`: digits, a leading `-` allowed.
fn set_integer(v: &str) -> Result<i32, String> {
    for (i, c) in v.bytes().enumerate() {
        if i == 0 && c == b'-' {
            continue;
        }
        if !c.is_ascii_digit() {
            return Err("is not a valid number (greater or equal than zero)".into());
        }
    }
    Ok(php_support::atoi(v.as_bytes()))
}

/// `fpm_conf_set_time()`: seconds, with an `s`, `m`, `h` or `d` suffix.
fn set_time(v: &str) -> Result<i32, String> {
    let Some(&suffix) = v.as_bytes().last() else {
        return Err("invalid time value".into());
    };
    let body = &v[..v.len() - 1];
    Ok(match suffix {
        b'm' => php_support::atoi(body.as_bytes()).saturating_mul(60),
        b'h' => php_support::atoi(body.as_bytes()).saturating_mul(3600),
        b'd' => php_support::atoi(body.as_bytes()).saturating_mul(86400),
        b's' => php_support::atoi(body.as_bytes()),
        b'0'..=b'9' => php_support::atoi(v.as_bytes()),
        _ => return Err("unknown suffix used in time value".into()),
    })
}

/// `fpm_conf_set_rlimit_core()`.
fn set_rlimit_core(v: &str) -> Result<i32, String> {
    if v.eq_ignore_ascii_case("unlimited") {
        return Ok(-1);
    }
    let n = set_integer(v)?;
    if n < 0 {
        return Err("must be greater than zero or 'unlimited'".into());
    }
    Ok(n)
}

/// `fpm_conf_set_pm()`.
fn set_pm(v: &str) -> Result<Pm, String> {
    Ok(match v.to_ascii_lowercase().as_str() {
        "static" => Pm::Static,
        "dynamic" => Pm::Dynamic,
        "ondemand" => Pm::Ondemand,
        _ => return Err("invalid process manager (static, dynamic or ondemand)".into()),
    })
}

/// `fpm_conf_set_syslog_facility()`.
fn set_syslog_facility(v: &str) -> Result<i32, String> {
    let f = match v.to_ascii_uppercase().as_str() {
        "AUTH" => libc::LOG_AUTH,
        "AUTHPRIV" => libc::LOG_AUTHPRIV,
        "CRON" => libc::LOG_CRON,
        "DAEMON" => libc::LOG_DAEMON,
        "FTP" => libc::LOG_FTP,
        "KERN" => libc::LOG_KERN,
        "LPR" => libc::LOG_LPR,
        "MAIL" => libc::LOG_MAIL,
        "NEWS" => libc::LOG_NEWS,
        "SYSLOG" => libc::LOG_SYSLOG,
        "USER" => libc::LOG_USER,
        "UUCP" => libc::LOG_UUCP,
        "LOCAL0" => libc::LOG_LOCAL0,
        "LOCAL1" => libc::LOG_LOCAL1,
        "LOCAL2" => libc::LOG_LOCAL2,
        "LOCAL3" => libc::LOG_LOCAL3,
        "LOCAL4" => libc::LOG_LOCAL4,
        "LOCAL5" => libc::LOG_LOCAL5,
        "LOCAL6" => libc::LOG_LOCAL6,
        "LOCAL7" => libc::LOG_LOCAL7,
        _ => return Err("invalid value for 'syslog.facility'".into()),
    };
    Ok(f)
}

/// `fpm_conf_expand_pool_name()`: `$pool` becomes the pool's name.
fn expand_pool(v: &str, pool: Option<&str>) -> Result<String, String> {
    if !v.contains("$pool") {
        return Ok(v.to_string());
    }
    match pool {
        Some(p) => Ok(v.replace("$pool", p)),
        None => Err("Can't use '$pool' when the pool is not defined".into()),
    }
}

/// The constants a value may name (`zend_get_constant()` in the INI
/// scanner): the error levels (`php_value[error_reporting] = E_ALL`).
pub struct Hooks;

impl IniHooks for Hooks {
    fn constant(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        let v: i64 = match name {
            b"E_ERROR" => 1,
            b"E_WARNING" => 2,
            b"E_PARSE" => 4,
            b"E_NOTICE" => 8,
            b"E_CORE_ERROR" => 16,
            b"E_CORE_WARNING" => 32,
            b"E_COMPILE_ERROR" => 64,
            b"E_COMPILE_WARNING" => 128,
            b"E_USER_ERROR" => 256,
            b"E_USER_WARNING" => 512,
            b"E_USER_NOTICE" => 1024,
            b"E_STRICT" => 2048,
            b"E_RECOVERABLE_ERROR" => 4096,
            b"E_DEPRECATED" => 8192,
            b"E_USER_DEPRECATED" => 16384,
            b"E_ALL" => 30719,
            _ => return None,
        };
        Some(v.to_string().into_bytes())
    }
}

/// Where entries go while one file is read.
struct Loader<'a> {
    conf: &'a mut Conf,
    /// Index of the pool the entries belong to (`current_wp`), `None` in
    /// `[global]`.
    current: Option<usize>,
    file: String,
    line: u32,
    error: bool,
    include: Option<String>,
}

pub fn scalar(v: IniScalar) -> String {
    match v {
        IniScalar::Str(s) => String::from_utf8_lossy(&s).into_owned(),
        IniScalar::Long(n) => n.to_string(),
        IniScalar::Double(d) => d.to_string(),
        other => String::from_utf8_lossy(&other.to_bytes()).into_owned(),
    }
}

impl Loader<'_> {
    fn fail(&mut self, msg: &str) {
        log::error(
            "fpm_conf_ini_parser",
            0,
            &format!("[{}:{}] {msg}", self.file, self.line),
        );
        self.error = true;
    }

    /// `ini_fpm_global_options[]`.
    fn global_entry(&mut self, name: &str, v: &str) -> Option<Result<(), String>> {
        let g = &mut self.conf.global;
        let s = || expand_pool(v, None);
        Some((|| {
            match name.to_ascii_lowercase().as_str() {
                "pid" => g.pid_file = Some(s()?),
                "error_log" => g.error_log = Some(s()?),
                "syslog.ident" => g.syslog_ident = Some(s()?),
                "syslog.facility" => g.syslog_facility = set_syslog_facility(v)?,
                "log_buffering" => g.log_buffering = set_boolean(v)?,
                "log_level" => {
                    g.log_level = Level::parse(v)
                        .ok_or_else(|| "invalid value for 'log_level'".to_string())?;
                }
                "log_limit" => g.log_limit = set_integer(v)?,
                "emergency_restart_threshold" => g.emergency_restart_threshold = set_integer(v)?,
                "emergency_restart_interval" => g.emergency_restart_interval = set_time(v)?,
                "process_control_timeout" => g.process_control_timeout = set_time(v)?,
                "process.max" => g.process_max = set_integer(v)?,
                "process.priority" => g.process_priority = set_integer(v)?,
                "daemonize" => g.daemonize = set_boolean(v)?,
                "rlimit_files" => g.rlimit_files = set_integer(v)?,
                "rlimit_core" => g.rlimit_core = set_rlimit_core(v)?,
                "events.mechanism" => g.events_mechanism = Some(s()?),
                "systemd_interval" => g.systemd_interval = set_time(v)?,
                _ => return Err(String::new()),
            }
            Ok(())
        })())
        .filter(|r| r.as_ref().err().is_none_or(|e| !e.is_empty()))
    }

    /// `ini_fpm_pool_options[]`.
    fn pool_entry(&mut self, idx: usize, name: &str, v: &str) -> Option<Result<(), String>> {
        let p = &mut self.conf.pools[idx];
        let pool_name = p.name.clone();
        let s = || expand_pool(v, Some(&pool_name));
        Some((|| {
            match name.to_ascii_lowercase().as_str() {
                "prefix" => p.prefix = Some(s()?),
                "user" => p.user = Some(s()?),
                "group" => p.group = Some(s()?),
                "listen" => p.listen_address = Some(s()?),
                "listen.backlog" => p.listen_backlog = set_integer(v)?,
                "listen.owner" => p.listen_owner = Some(s()?),
                "listen.group" => p.listen_group = Some(s()?),
                "listen.mode" => p.listen_mode = Some(s()?),
                "listen.allowed_clients" => p.listen_allowed_clients = Some(s()?),
                "listen.setfib" => p.listen_setfib = set_integer(v)?,
                "process.priority" => p.process_priority = set_integer(v)?,
                "process.dumpable" => p.process_dumpable = set_boolean(v)?,
                "pm" => p.pm = set_pm(v)?,
                "pm.max_children" => p.pm_max_children = set_integer(v)?,
                "pm.start_servers" => p.pm_start_servers = set_integer(v)?,
                "pm.min_spare_servers" => p.pm_min_spare_servers = set_integer(v)?,
                "pm.max_spare_servers" => p.pm_max_spare_servers = set_integer(v)?,
                "pm.max_spawn_rate" => p.pm_max_spawn_rate = set_integer(v)?,
                "pm.process_idle_timeout" => p.pm_process_idle_timeout = set_time(v)?,
                "pm.max_requests" => p.pm_max_requests = set_integer(v)?,
                // `listen.acl_users`, `listen.acl_groups` (`--with-fpm-acl`)
                // and `apparmor_hat` (`--with-fpm-apparmor`) are not built
                // in, as in a default `./configure --enable-fpm`.
                "pm.status_path" => p.pm_status_path = Some(s()?),
                "pm.status_listen" => p.pm_status_listen = Some(s()?),
                "ping.path" => p.ping_path = Some(s()?),
                "ping.response" => p.ping_response = Some(s()?),
                "access.log" => p.access_log = Some(s()?),
                "access.format" => p.access_format = Some(s()?),
                "slowlog" => p.slowlog = Some(s()?),
                "request_slowlog_timeout" => p.request_slowlog_timeout = set_time(v)?,
                "request_slowlog_trace_depth" => p.request_slowlog_trace_depth = set_integer(v)?,
                "request_terminate_timeout" => p.request_terminate_timeout = set_time(v)?,
                "request_terminate_timeout_track_finished" => {
                    p.request_terminate_timeout_track_finished = set_boolean(v)?;
                }
                "rlimit_files" => p.rlimit_files = set_integer(v)?,
                "rlimit_core" => p.rlimit_core = set_rlimit_core(v)?,
                "chroot" => p.chroot = Some(s()?),
                "chdir" => p.chdir = Some(s()?),
                "catch_workers_output" => p.catch_workers_output = set_boolean(v)?,
                "decorate_workers_output" => p.decorate_workers_output = set_boolean(v)?,
                "clear_env" => p.clear_env = set_boolean(v)?,
                "security.limit_extensions" => p.security_limit_extensions = Some(s()?),
                _ => return Err(String::new()),
            }
            Ok(())
        })())
        .filter(|r| r.as_ref().err().is_none_or(|e| !e.is_empty()))
    }
}

/// The C option name as written in the table (for error messages).
fn canonical(name: &str) -> String {
    name.to_ascii_lowercase()
}

impl IniParserCallback for Loader<'_> {
    /// `fpm_conf_ini_parser_section()`.
    fn section(&mut self, name: &[u8]) {
        if self.error {
            return;
        }
        let name = String::from_utf8_lossy(name).into_owned();
        if name.eq_ignore_ascii_case("global") {
            self.current = None;
            return;
        }
        if let Some(i) = self
            .conf
            .pools
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(&name))
        {
            self.current = Some(i);
            return;
        }
        self.conf.pools.push(Pool::new(&name));
        self.current = Some(self.conf.pools.len() - 1);
    }

    /// `fpm_conf_ini_parser_entry()`.
    fn entry(&mut self, key: &[u8], value: Option<IniScalar>) {
        if self.error {
            return;
        }
        let name = String::from_utf8_lossy(key).into_owned();
        let Some(value) = value else {
            self.fail("value is NULL for a ZEND_INI_PARSER_ENTRY");
            return;
        };
        let value = scalar(value);
        if name == "include" {
            if self.include.is_some() {
                self.fail("two includes at the same time !");
                return;
            }
            self.include = Some(value);
            return;
        }
        let r = match self.current {
            None => self.global_entry(&name, &value),
            Some(i) => self.pool_entry(i, &name, &value),
        };
        match r {
            None => self.fail(&format!("unknown entry '{name}'")),
            Some(Err(e)) => self.fail(&format!(
                "unable to parse value for entry '{}': {e}",
                canonical(&name)
            )),
            Some(Ok(())) => {}
        }
    }

    /// `fpm_conf_ini_parser_array()`.
    fn pop_entry(&mut self, key: &[u8], value: IniScalar, offset: &[u8]) {
        if self.error {
            return;
        }
        let name = String::from_utf8_lossy(key).into_owned();
        let key = String::from_utf8_lossy(offset).into_owned();
        let value = scalar(value);
        if name == "access.suppress_path" {
            if !key.is_empty() {
                self.fail("Keys provided to field 'access.suppress_path' are ignored");
            }
            if !value.starts_with('/') {
                self.fail("Values provided to field 'access.suppress_path' must begin with '/'");
            }
            if self.error {
                return;
            }
        } else if key.is_empty() {
            self.fail(&format!("You must provide a key for field '{name}'"));
            return;
        }
        let Some(idx) = self.current else {
            self.fail("Array are not allowed in the global section");
            return;
        };
        let pool_name = self.conf.pools[idx].name.clone();
        let p = &mut self.conf.pools[idx];
        let r: Result<(), String> = match name.as_str() {
            "env" => {
                if value.is_empty() {
                    self.fail("empty value");
                    return;
                }
                expand_pool(&value, Some(&pool_name)).map(|v| p.env.insert(0, (key.clone(), v)))
            }
            "php_value" => expand_pool(&value, Some(&pool_name))
                .map(|v| p.php_values.insert(0, (key.clone(), v))),
            "php_admin_value" => expand_pool(&value, Some(&pool_name))
                .map(|v| p.php_admin_values.insert(0, (key.clone(), v))),
            "php_flag" => set_boolean(&value).map(|b| {
                p.php_values
                    .insert(0, (key.clone(), if b { "1" } else { "0" }.into()));
            }),
            "php_admin_flag" => set_boolean(&value).map(|b| {
                p.php_admin_values
                    .insert(0, (key.clone(), if b { "1" } else { "0" }.into()));
            }),
            "access.suppress_path" => {
                p.access_suppress_paths.insert(0, value.clone());
                Ok(())
            }
            _ => {
                self.fail(&format!("unknown directive '{name}'"));
                return;
            }
        };
        if let Err(e) = r {
            self.fail(&format!("error while parsing '{name}[{key}]' : {e}"));
        }
    }
}

/// `fpm_conf_load_ini_file()`: the file one line at a time (`include`
/// is followed at the end of its line), at most five levels deep.
fn load_file(conf: &mut Conf, current: &mut Option<usize>, path: &str, depth: u32) -> bool {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            log::syserror(
                "fpm_conf_load_ini_file",
                0,
                &format!("failed to open configuration file '{path}'"),
                &e,
            );
            return false;
        }
    };
    if depth > 4 {
        log::error(
            "fpm_conf_load_ini_file",
            0,
            "failed to include more than 5 files recursively",
        );
        return false;
    }
    let mut lineno = 0u32;
    for raw in data.split(|&b| b == b'\n') {
        lineno += 1;
        if raw.is_empty() {
            continue;
        }
        let mut buf = raw.to_vec();
        buf.push(b'\n');
        let mut loader = Loader {
            conf,
            current: *current,
            file: path.to_string(),
            line: lineno,
            error: false,
            include: None,
        };
        let parsed = ini::parse_ini(&buf, path, ScannerMode::Normal, &mut Hooks, &mut loader);
        let (error, include) = (loader.error, loader.include.take());
        *current = loader.current;
        if let Err(d) = parsed {
            // `zend_ini_scanner`'s syntax error is reported by the parser
            // as a PHP warning; FPM shows it as its own line.
            log::error(
                "fpm_conf_load_ini_file",
                0,
                &format!(
                    "[{path}:{lineno}] {}",
                    d.message
                        .trim_end()
                        .replace(&format!(" in {path} on line 1"), "")
                ),
            );
            return false;
        }
        if error {
            return false;
        }
        if let Some(inc) = include {
            let inc = evaluate_full_path(&inc, None, conf.prefix.as_deref(), None, false);
            if !load_include(conf, current, &inc, path, lineno, depth) {
                return false;
            }
        }
    }
    true
}

/// `fpm_conf_ini_parser_include()`: every file the glob matches, in order.
fn load_include(
    conf: &mut Conf,
    current: &mut Option<usize>,
    pattern: &str,
    file: &str,
    line: u32,
    depth: u32,
) -> bool {
    let matches = super::os::glob(pattern);
    match matches {
        Err(()) => {
            log::error(
                "fpm_conf_ini_parser_include",
                0,
                &format!("[{file}:{line}] Unable to globalize '{pattern}' (ret = 0) from {file} at line {line}."),
            );
            false
        }
        Ok(list) if list.is_empty() => {
            log::warning(
                "fpm_conf_ini_parser_include",
                0,
                &format!(
                    "Nothing matches the include pattern '{pattern}' from {file} at line {line}."
                ),
            );
            true
        }
        Ok(list) => {
            for f in list {
                if !load_file(conf, current, &f, depth + 1) {
                    log::error(
                        "fpm_conf_ini_parser_include",
                        0,
                        &format!("Unable to include {f} from {file} at line {line}"),
                    );
                    return false;
                }
            }
            true
        }
    }
}

/// `fpm_evaluate_full_path()`: a relative path below the pool's prefix,
/// else `-p`'s, else `default_prefix`, else `PHP_PREFIX`; with `expand`
/// a leading `$prefix` is dropped first.
pub fn evaluate_full_path(
    path: &str,
    pool_prefix: Option<&str>,
    global_prefix: Option<&str>,
    default_prefix: Option<&str>,
    expand: bool,
) -> String {
    if path.starts_with('/') {
        return path.to_string();
    }
    let prefix = pool_prefix
        .or(global_prefix)
        .or(default_prefix)
        .unwrap_or(PHP_PREFIX);
    let mut p = path;
    if expand {
        if let Some(rest) = p.strip_prefix("$prefix") {
            p = rest.trim_start_matches('/');
            if p.is_empty() {
                return prefix.to_string();
            }
        }
    }
    let full = format!("{prefix}/{p}");
    if !full.starts_with('/') && pool_prefix.is_some() {
        return evaluate_full_path(&full, None, global_prefix, default_prefix, expand);
    }
    full
}

fn is_dir(p: &str) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_dir())
}

fn path_chars_ok(s: &str) -> bool {
    s.bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'-' | b'_' | b'.' | b'~'))
}

/// `fpm_conf_process_all_pools()`: checks and defaults of each pool.
#[allow(clippy::too_many_lines)]
fn process_all_pools(conf: &mut Conf, run_as_root: bool) -> bool {
    const F: &str = "fpm_conf_process_all_pools";
    if conf.pools.is_empty() {
        log::error(
            F,
            0,
            "No pool defined. at least one pool section must be specified in config file",
        );
        return false;
    }
    let global_prefix = conf.prefix.clone();
    let mut idx = 0;
    while idx < conf.pools.len() {
        let mut status_pool = None;
        let p = &mut conf.pools[idx];
        let name = p.name.clone();
        if let Some(prefix) = p.prefix.clone().filter(|s| !s.is_empty()) {
            let full = evaluate_full_path(&prefix, None, global_prefix.as_deref(), None, false);
            if !is_dir(&full) {
                log::error(
                    F,
                    0,
                    &format!(
                        "[pool {name}] the prefix '{full}' does not exist or is not a directory"
                    ),
                );
                return false;
            }
            p.prefix = Some(full);
        }
        let pool_prefix = p.prefix.clone();
        let full = |s: &str, expand: bool| {
            evaluate_full_path(
                s,
                pool_prefix.as_deref(),
                global_prefix.as_deref(),
                None,
                expand,
            )
        };
        if p.user.is_none() && super::os::geteuid() == 0 && !run_as_root {
            log::alert(F, 0, &format!("[pool {name}] 'user' directive has not been specified when running as a root without --allow-to-run-as-root"));
            return false;
        }
        if let Some(addr) = p.listen_address.clone().filter(|s| !s.is_empty()) {
            if super::sockets::is_unix_address(&addr) && !addr.starts_with('/') {
                p.listen_address = Some(full(&addr, false));
            }
        } else {
            log::alert(
                F,
                0,
                &format!("[pool {name}] no listen address have been defined!"),
            );
            return false;
        }
        if p.process_priority != 64 && !(-19..=20).contains(&p.process_priority) {
            log::error(
                F,
                0,
                &format!("[pool {name}] process.priority must be included into [-19,20]"),
            );
            return false;
        }
        if p.pm == Pm::None {
            log::alert(
                F,
                0,
                &format!(
                    "[pool {name}] the process manager is missing (static, dynamic or ondemand)"
                ),
            );
            return false;
        }
        if p.pm_max_children < 1 {
            log::alert(
                F,
                0,
                &format!("[pool {name}] pm.max_children must be a positive value"),
            );
            return false;
        }
        if p.pm == Pm::Dynamic {
            if p.pm_min_spare_servers <= 0 {
                log::alert(
                    F,
                    0,
                    &format!(
                        "[pool {name}] pm.min_spare_servers({}) must be a positive value",
                        p.pm_min_spare_servers
                    ),
                );
                return false;
            }
            if p.pm_max_spare_servers <= 0 {
                log::alert(
                    F,
                    0,
                    &format!(
                        "[pool {name}] pm.max_spare_servers({}) must be a positive value",
                        p.pm_max_spare_servers
                    ),
                );
                return false;
            }
            if p.pm_min_spare_servers > p.pm_max_children
                || p.pm_max_spare_servers > p.pm_max_children
            {
                log::alert(F, 0, &format!("[pool {name}] pm.min_spare_servers({}) and pm.max_spare_servers({}) cannot be greater than pm.max_children({})", p.pm_min_spare_servers, p.pm_max_spare_servers, p.pm_max_children));
                return false;
            }
            if p.pm_max_spare_servers < p.pm_min_spare_servers {
                log::alert(F, 0, &format!("[pool {name}] pm.max_spare_servers({}) must not be less than pm.min_spare_servers({})", p.pm_max_spare_servers, p.pm_min_spare_servers));
                return false;
            }
            if p.pm_start_servers <= 0 {
                p.pm_start_servers = p.pm_min_spare_servers
                    + ((p.pm_max_spare_servers - p.pm_min_spare_servers) / 2);
                log::notice(
                    F,
                    0,
                    &format!(
                        "[pool {name}] pm.start_servers is not set. It's been set to {}.",
                        p.pm_start_servers
                    ),
                );
            } else if p.pm_start_servers < p.pm_min_spare_servers
                || p.pm_start_servers > p.pm_max_spare_servers
            {
                log::alert(F, 0, &format!("[pool {name}] pm.start_servers({}) must not be less than pm.min_spare_servers({}) and not greater than pm.max_spare_servers({})", p.pm_start_servers, p.pm_min_spare_servers, p.pm_max_spare_servers));
                return false;
            }
            if p.pm_max_spawn_rate < 1 {
                log::alert(
                    F,
                    0,
                    &format!("[pool {name}] pm.max_spawn_rate must be a positive value"),
                );
                return false;
            }
        } else if p.pm == Pm::Ondemand {
            if p.pm_process_idle_timeout < 1 {
                log::alert(
                    F,
                    0,
                    &format!(
                        "[pool {name}] pm.process_idle_timeout({}s) must be greater than 0s",
                        p.pm_process_idle_timeout
                    ),
                );
                return false;
            }
            if p.listen_backlog < BACKLOG_DEFAULT {
                log::warning(F, 0, &format!("[pool {name}] listen.backlog({}) was too low for the ondemand process manager. I updated it for you to {BACKLOG_DEFAULT}.", p.listen_backlog));
                p.listen_backlog = BACKLOG_DEFAULT;
            }
            p.pm_start_servers = 0;
        }
        // `fpm_worker_pool_shared_status_alloc()`: `pm.status_listen` adds
        // an ondemand pool `<name>_status` on that address that serves
        // this pool's status page.
        if p.pm_status_listen.is_some() {
            if p.pm_status_path.is_none() {
                p.pm_status_path = Some("/".into());
            }
            let mut sp = Pool::new(&format!("{name}_status"));
            sp.listen_address.clone_from(&p.pm_status_listen);
            sp.listen_acl_groups.clone_from(&p.listen_acl_groups);
            sp.listen_acl_users.clone_from(&p.listen_acl_users);
            sp.listen_allowed_clients
                .clone_from(&p.listen_allowed_clients);
            sp.listen_group.clone_from(&p.listen_group);
            sp.listen_owner.clone_from(&p.listen_owner);
            sp.listen_mode.clone_from(&p.listen_mode);
            sp.user.clone_from(&p.user);
            sp.group.clone_from(&p.group);
            sp.pm_status_path.clone_from(&p.pm_status_path);
            sp.ping_path.clone_from(&p.ping_path);
            sp.ping_response.clone_from(&p.ping_response);
            sp.php_values.clone_from(&p.php_values);
            sp.php_admin_values.clone_from(&p.php_admin_values);
            sp.pm = Pm::Ondemand;
            sp.pm_max_children = 2;
            sp.shared = Some(idx);
            status_pool = Some(sp);
        }
        if let Some(status) = p.pm_status_path.clone().filter(|s| !s.is_empty()) {
            if !status.starts_with('/') {
                log::error(
                    F,
                    0,
                    &format!("[pool {name}] the status path '{status}' must start with a '/'"),
                );
                return false;
            }
            if p.pm_status_listen.is_none() && p.shared.is_none() && status.len() < 2 {
                log::error(
                    F,
                    0,
                    &format!("[pool {name}] the status path '{status}' is not long enough"),
                );
                return false;
            }
            if !path_chars_ok(&status) {
                log::error(F, 0, &format!("[pool {name}] the status path '{status}' must contain only the following characters '[alphanum]/_-.~'"));
                return false;
            }
        }
        if let Some(ping) = p.ping_path.clone().filter(|s| !s.is_empty()) {
            if !ping.starts_with('/') {
                log::error(
                    F,
                    0,
                    &format!("[pool {name}] the ping path '{ping}' must start with a '/'"),
                );
                return false;
            }
            if ping.len() < 2 {
                log::error(
                    F,
                    0,
                    &format!("[pool {name}] the ping path '{ping}' is not long enough"),
                );
                return false;
            }
            if !path_chars_ok(&ping) {
                log::error(F, 0, &format!("[pool {name}] the ping path '{ping}' must contain only the following characters '[alphanum]/_-.~'"));
                return false;
            }
            match &p.ping_response {
                None => p.ping_response = Some("pong".into()),
                Some(r) if r.is_empty() => {
                    log::error(
                        F,
                        0,
                        &format!("[pool {name}] the ping response page '{r}' is not long enough"),
                    );
                    return false;
                }
                Some(_) => {}
            }
        } else {
            p.ping_response = None;
        }
        if let Some(a) = p.access_log.clone().filter(|s| !s.is_empty()) {
            p.access_log = Some(full(&a, false));
            if p.access_format.is_none() {
                p.access_format = Some("%R - %u %t \"%m %r\" %s".into());
            }
        }
        if let Some(s) = p.slowlog.clone().filter(|s| !s.is_empty()) {
            p.slowlog = Some(full(&s, false));
        }
        if p.request_slowlog_timeout != 0 {
            if p.slowlog.as_deref().is_none_or(str::is_empty) {
                log::error(F, 0, &format!("[pool {name}] 'slowlog' must be specified for use with 'request_slowlog_timeout'"));
                return false;
            }
            if let Some(s) = &p.slowlog {
                if let Err(e) = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(s)
                {
                    log::syserror(F, 0, &format!("Unable to create or open slowlog({s})"), &e);
                    return false;
                }
            }
            if p.request_terminate_timeout != 0
                && p.request_slowlog_timeout > p.request_terminate_timeout
            {
                log::error(F, 0, &format!("[pool {name}] 'request_slowlog_timeout' ({}) can't be greater than 'request_terminate_timeout' ({})", p.request_slowlog_timeout, p.request_terminate_timeout));
                return false;
            }
        }
        if p.request_slowlog_trace_depth != 0 {
            if p.slowlog.as_deref().is_none_or(str::is_empty) {
                log::error(F, 0, &format!("[pool {name}] 'slowlog' must be specified for use with 'request_slowlog_trace_depth'"));
                return false;
            }
            if p.request_slowlog_trace_depth <= 0 {
                log::error(
                    F,
                    0,
                    &format!(
                        "[pool {name}] 'request_slowlog_trace_depth' ({}) must be a positive value",
                        p.request_slowlog_trace_depth
                    ),
                );
                return false;
            }
        } else {
            p.request_slowlog_trace_depth = 20;
        }
        if let Some(c) = p.chroot.clone().filter(|s| !s.is_empty()) {
            let c = full(&c, true);
            if !c.starts_with('/') {
                log::error(
                    F,
                    0,
                    &format!("[pool {name}] the chroot path '{c}' must start with a '/'"),
                );
                return false;
            }
            if !is_dir(&c) {
                log::error(
                    F,
                    0,
                    &format!(
                        "[pool {name}] the chroot path '{c}' does not exist or is not a directory"
                    ),
                );
                return false;
            }
            p.chroot = Some(c);
        }
        if let Some(d) = p.chdir.clone().filter(|s| !s.is_empty()) {
            let d = full(&d, false);
            if !d.starts_with('/') {
                log::error(
                    F,
                    0,
                    &format!("[pool {name}] the chdir path '{d}' must start with a '/'"),
                );
                return false;
            }
            if let Some(c) = &p.chroot {
                let inside = format!("{c}/{d}");
                if !is_dir(&inside) {
                    log::error(F, 0, &format!("[pool {name}] the chdir path '{d}' within the chroot path '{c}' ('{inside}') does not exist or is not a directory"));
                    return false;
                }
            } else if !is_dir(&d) {
                log::error(
                    F,
                    0,
                    &format!(
                        "[pool {name}] the chdir path '{d}' does not exist or is not a directory"
                    ),
                );
                return false;
            }
            p.chdir = Some(d);
        }
        let ext = p
            .security_limit_extensions
            .get_or_insert_with(|| ".php .phar".into())
            .clone();
        p.limit_extensions = ext.split_whitespace().map(str::to_string).collect();
        if let Some(sp) = status_pool {
            conf.pools.push(sp);
        }
        idx += 1;
    }
    for (i, a) in conf.pools.iter().enumerate() {
        for b in conf.pools.iter().skip(i + 1) {
            if let (Some(x), Some(y)) = (&a.listen_address, &b.listen_address) {
                if !x.is_empty() && x == y {
                    log::error(
                        F,
                        0,
                        &format!(
                            "[pool {}] unable to set listen address as it's already used in another pool '{}'",
                            b.name, a.name
                        ),
                    );
                    return false;
                }
            }
        }
    }
    true
}

/// `fpm_conf_init_main()` up to the pools' checks: the file (`-y`, else
/// `${prefix}/etc/php-fpm.conf`, else `PHP_SYSCONFDIR/php-fpm.conf`),
/// then the global settings and the pools. `None` after an error (logged).
pub fn load(
    config: Option<&str>,
    prefix: Option<&str>,
    pid: Option<&str>,
    force_daemon: Option<bool>,
    run_as_root: bool,
) -> Option<Conf> {
    const F: &str = "fpm_conf_init_main";
    if let Some(p) = prefix.filter(|p| !p.is_empty()) {
        if !is_dir(p) {
            log::error(
                F,
                0,
                &format!("the global prefix '{p}' does not exist or is not a directory"),
            );
            return None;
        }
    }
    let file = match config.filter(|c| !c.is_empty()) {
        Some(c) => c.to_string(),
        None => match prefix {
            Some(p) => format!("{p}/etc/php-fpm.conf"),
            None => format!("{PHP_SYSCONFDIR}/php-fpm.conf"),
        },
    };
    let mut conf = Conf {
        file: file.clone(),
        prefix: prefix.map(str::to_string),
        ..Conf::default()
    };
    if let Some(p) = pid.filter(|p| !p.is_empty()) {
        conf.global.pid_file = Some(p.to_string());
    }
    let mut current = None;
    if !load_file(&mut conf, &mut current, &file, 0) {
        log::error(F, 0, &format!("failed to load configuration file '{file}'"));
        return None;
    }
    if !post_process(&mut conf, force_daemon, run_as_root) {
        log::error(F, 0, "failed to post process the configuration");
        return None;
    }
    Some(conf)
}

/// `fpm_conf_post_process()` (the parts before the pools' sockets).
fn post_process(conf: &mut Conf, force_daemon: Option<bool>, run_as_root: bool) -> bool {
    const F: &str = "fpm_conf_post_process";
    let prefix = conf.prefix.clone();
    let g = &mut conf.global;
    if let Some(p) = g.pid_file.clone() {
        g.pid_file = Some(evaluate_full_path(
            &p,
            None,
            prefix.as_deref(),
            Some(PHP_LOCALSTATEDIR),
            false,
        ));
    }
    if let Some(d) = force_daemon {
        g.daemonize = d;
    }
    log::set_level(g.log_level);
    if usize::try_from(g.log_limit).unwrap_or(0) < log::MIN_LIMIT {
        log::error(
            F,
            0,
            &format!("log_limit must be greater than {}", log::MIN_LIMIT),
        );
        return false;
    }
    log::set_limit(usize::try_from(g.log_limit).unwrap_or(log::DEFAULT_LIMIT));
    log::set_buffering(g.log_buffering);
    if g.process_max < 0 {
        log::error(F, 0, "process_max can't be negative");
        return false;
    }
    if g.process_priority != 64 && !(-19..=20).contains(&g.process_priority) {
        log::error(F, 0, "process.priority must be included into [-19,20]");
        return false;
    }
    let error_log = g
        .error_log
        .get_or_insert_with(|| "log/php-fpm.log".into())
        .clone();
    if g.syslog_ident.is_none() {
        g.syslog_ident = Some("php-fpm".into());
    }
    if g.syslog_facility < 0 {
        g.syslog_facility = libc::LOG_DAEMON;
    }
    if !error_log.eq_ignore_ascii_case("syslog") {
        g.error_log = Some(evaluate_full_path(
            &error_log,
            None,
            prefix.as_deref(),
            Some(PHP_LOCALSTATEDIR),
            false,
        ));
    }
    // `fpm_stdio_open_error_log(0)`: from here on errors reach the log.
    if !crate::open_error_log(&conf.global) {
        return false;
    }
    if !process_all_pools(conf, run_as_root) {
        return false;
    }
    // `fpm_log_open(0)`, then each `access.format` is checked.
    crate::open_access_logs(&conf.pools)
        && super::access_log::check_formats(&conf.pools)
        && crate::open_slowlogs(&conf.pools)
}

/// `fpm_conf_dump()` (`-tt`): the configuration as notices.
#[allow(clippy::too_many_lines)]
pub fn dump(conf: &Conf) {
    const F: &str = "fpm_conf_dump";
    let n = |msg: &str| log::notice(F, 0, msg);
    let s = |v: &Option<String>| v.clone().unwrap_or_else(|| "undefined".into());
    let b = |v: bool| if v { "yes" } else { "no" };
    let g = &conf.global;
    n("[global]");
    n(&format!("\tpid = {}", s(&g.pid_file)));
    n(&format!("\terror_log = {}", s(&g.error_log)));
    n(&format!("\tsyslog.ident = {}", s(&g.syslog_ident)));
    n(&format!("\tsyslog.facility = {}", g.syslog_facility));
    n(&format!("\tlog_buffering = {}", b(g.log_buffering)));
    n(&format!("\tlog_level = {}", log::level().name()));
    n(&format!("\tlog_limit = {}", g.log_limit));
    n(&format!(
        "\temergency_restart_interval = {}s",
        g.emergency_restart_interval
    ));
    n(&format!(
        "\temergency_restart_threshold = {}",
        g.emergency_restart_threshold
    ));
    n(&format!(
        "\tprocess_control_timeout = {}s",
        g.process_control_timeout
    ));
    n(&format!("\tprocess.max = {}", g.process_max));
    if g.process_priority == 64 {
        n("\tprocess.priority = undefined");
    } else {
        n(&format!("\tprocess.priority = {}", g.process_priority));
    }
    n(&format!("\tdaemonize = {}", b(g.daemonize)));
    n(&format!("\trlimit_files = {}", g.rlimit_files));
    n(&format!("\trlimit_core = {}", g.rlimit_core));
    n("\tevents.mechanism = epoll");
    n(" ");
    for p in conf.pools.iter().filter(|p| p.shared.is_none()) {
        n(&format!("[{}]", p.name));
        n(&format!("\tprefix = {}", s(&p.prefix)));
        n(&format!("\tuser = {}", s(&p.user)));
        n(&format!("\tgroup = {}", s(&p.group)));
        n(&format!("\tlisten = {}", s(&p.listen_address)));
        n(&format!("\tlisten.backlog = {}", p.listen_backlog));
        n(&format!("\tlisten.owner = {}", s(&p.listen_owner)));
        n(&format!("\tlisten.group = {}", s(&p.listen_group)));
        n(&format!("\tlisten.mode = {}", s(&p.listen_mode)));
        n(&format!(
            "\tlisten.allowed_clients = {}",
            s(&p.listen_allowed_clients)
        ));
        if p.process_priority == 64 {
            n("\tprocess.priority = undefined");
        } else {
            n(&format!("\tprocess.priority = {}", p.process_priority));
        }
        n(&format!("\tprocess.dumpable = {}", b(p.process_dumpable)));
        n(&format!("\tpm = {}", p.pm.name()));
        n(&format!("\tpm.max_children = {}", p.pm_max_children));
        n(&format!("\tpm.start_servers = {}", p.pm_start_servers));
        n(&format!(
            "\tpm.min_spare_servers = {}",
            p.pm_min_spare_servers
        ));
        n(&format!(
            "\tpm.max_spare_servers = {}",
            p.pm_max_spare_servers
        ));
        n(&format!("\tpm.max_spawn_rate = {}", p.pm_max_spawn_rate));
        n(&format!(
            "\tpm.process_idle_timeout = {}",
            p.pm_process_idle_timeout
        ));
        n(&format!("\tpm.max_requests = {}", p.pm_max_requests));
        n(&format!("\tpm.status_path = {}", s(&p.pm_status_path)));
        n(&format!("\tpm.status_listen = {}", s(&p.pm_status_listen)));
        n(&format!("\tping.path = {}", s(&p.ping_path)));
        n(&format!("\tping.response = {}", s(&p.ping_response)));
        n(&format!("\taccess.log = {}", s(&p.access_log)));
        n(&format!("\taccess.format = {}", s(&p.access_format)));
        for v in &p.access_suppress_paths {
            n(&format!("\taccess.suppress_path[] = {v}"));
        }
        n(&format!("\tslowlog = {}", s(&p.slowlog)));
        n(&format!(
            "\trequest_slowlog_timeout = {}s",
            p.request_slowlog_timeout
        ));
        n(&format!(
            "\trequest_slowlog_trace_depth = {}",
            p.request_slowlog_trace_depth
        ));
        n(&format!(
            "\trequest_terminate_timeout = {}s",
            p.request_terminate_timeout
        ));
        n(&format!(
            "\trequest_terminate_timeout_track_finished = {}",
            b(p.request_terminate_timeout_track_finished)
        ));
        n(&format!("\trlimit_files = {}", p.rlimit_files));
        n(&format!("\trlimit_core = {}", p.rlimit_core));
        n(&format!("\tchroot = {}", s(&p.chroot)));
        n(&format!("\tchdir = {}", s(&p.chdir)));
        n(&format!(
            "\tcatch_workers_output = {}",
            b(p.catch_workers_output)
        ));
        n(&format!(
            "\tdecorate_workers_output = {}",
            b(p.decorate_workers_output)
        ));
        n(&format!("\tclear_env = {}", b(p.clear_env)));
        n(&format!(
            "\tsecurity.limit_extensions = {}",
            s(&p.security_limit_extensions)
        ));
        for (k, v) in &p.env {
            n(&format!("\tenv[{k}] = {v}"));
        }
        for (k, v) in &p.php_values {
            n(&format!("\tphp_value[{k}] = {v}"));
        }
        for (k, v) in &p.php_admin_values {
            n(&format!("\tphp_admin_value[{k}] = {v}"));
        }
        n(" ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_parse_like_fpm_conf() {
        assert_eq!(set_boolean("yes"), Ok(true));
        assert_eq!(set_boolean(""), Ok(false));
        assert!(set_boolean("maybe").is_err());
        assert_eq!(set_integer("-3"), Ok(-3));
        // atoi(): the low 32 bits, as glibc's (int) strtol().
        assert_eq!(set_integer("4294967297"), Ok(1));
        assert!(set_integer("3x").is_err());
        assert_eq!(set_time("2m"), Ok(120));
        assert_eq!(set_time("15"), Ok(15));
        assert_eq!(set_time("10s"), Ok(10));
        assert!(set_time("5x").is_err());
        assert_eq!(set_pm("OnDemand"), Ok(Pm::Ondemand));
        assert_eq!(set_rlimit_core("unlimited"), Ok(-1));
        assert_eq!(
            expand_pool("/run/$pool.sock", Some("www")),
            Ok("/run/www.sock".into())
        );
        assert!(expand_pool("$pool", None).is_err());
    }

    #[test]
    fn full_paths() {
        assert_eq!(evaluate_full_path("/abs", None, None, None, false), "/abs");
        assert_eq!(
            evaluate_full_path("log/x.log", None, Some("/opt"), None, false),
            "/opt/log/x.log"
        );
        assert_eq!(
            evaluate_full_path("x", Some("/p"), Some("/opt"), None, false),
            "/p/x"
        );
        assert_eq!(
            evaluate_full_path("$prefix/x", Some("/p"), None, None, true),
            "/p/x"
        );
    }
}
