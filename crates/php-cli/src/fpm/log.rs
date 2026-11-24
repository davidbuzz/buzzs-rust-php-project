// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! FPM's error log (ref: `sapi/fpm/fpm/zlog.c`): lines such as
//! `[22-Nov-2026 13:00:00] NOTICE: fpm is running, pid 123` on standard
//! error, the `error_log` file or syslog, cut at `log_limit` bytes.

use std::fmt::Write as _;
use std::io::Write;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

/// `ZLOG_*` levels (the values of `FCGI_ERROR`'s scale).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// `ZLOG_DEBUG`
    Debug = 1,
    /// `ZLOG_NOTICE`
    Notice = 2,
    /// `ZLOG_WARNING`
    Warning = 3,
    /// `ZLOG_ERROR`
    Error = 4,
    /// `ZLOG_ALERT`
    Alert = 5,
}

impl Level {
    /// `level_names[]`.
    pub fn name(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Notice => "NOTICE",
            Level::Warning => "WARNING",
            Level::Error => "ERROR",
            Level::Alert => "ALERT",
        }
    }

    /// `fpm_conf_set_log_level()`'s names (lower case).
    pub fn parse(s: &str) -> Option<Level> {
        Some(match s.to_ascii_lowercase().as_str() {
            "debug" => Level::Debug,
            "notice" => Level::Notice,
            "warning" | "warn" => Level::Warning,
            "error" => Level::Error,
            "alert" => Level::Alert,
            _ => return None,
        })
    }

    fn from_i32(v: i32) -> Level {
        match v {
            1 => Level::Debug,
            3 => Level::Warning,
            4 => Level::Error,
            5 => Level::Alert,
            _ => Level::Notice,
        }
    }

    /// `syslog_priorities[]`.
    fn syslog_priority(self) -> libc::c_int {
        match self {
            Level::Debug => libc::LOG_DEBUG,
            Level::Notice => libc::LOG_NOTICE,
            Level::Warning => libc::LOG_WARNING,
            Level::Error => libc::LOG_ERR,
            Level::Alert => libc::LOG_ALERT,
        }
    }
}

/// `ZLOG_SYSLOG`: the descriptor value that means syslog.
pub const SYSLOG: RawFd = -2;
/// `ZLOG_DEFAULT_LIMIT`.
pub const DEFAULT_LIMIT: usize = 1024;
/// `ZLOG_MIN_LIMIT`.
pub const MIN_LIMIT: usize = 512;
/// `MAX_BUF_LENGTH`.
const MAX_BUF_LENGTH: usize = 2048;
/// `MAX_WRAPPING_PREFIX_LENGTH`.
const MAX_WRAPPING_PREFIX_LENGTH: usize = 512;

static FD: AtomicI32 = AtomicI32::new(-1);
static FD_IS_STDERR: AtomicBool = AtomicBool::new(false);
static LEVEL: AtomicI32 = AtomicI32::new(Level::Notice as i32);
static LIMIT: AtomicUsize = AtomicUsize::new(DEFAULT_LIMIT);
static LAUNCHED: AtomicBool = AtomicBool::new(false);
static IS_CHILD: AtomicBool = AtomicBool::new(false);
static BUFFERING: AtomicBool = AtomicBool::new(true);

/// `zlog_set_external_logger()`'s logger: a worker also sends its lines to
/// the web server (`sapi_cgi_log_fastcgi()`).
type External = Box<dyn Fn(Level, &[u8])>;

thread_local! {
    static EXTERNAL: std::cell::RefCell<Option<External>> = const { std::cell::RefCell::new(None) };
}

/// `zlog_set_external_logger()`.
pub fn set_external_logger(logger: Option<External>) {
    EXTERNAL.with(|e| *e.borrow_mut() = logger);
}

fn has_external() -> bool {
    EXTERNAL.with(|e| e.borrow().is_some())
}

fn external(level: Level, msg: &[u8]) {
    EXTERNAL.with(|e| {
        if let Some(f) = e.borrow().as_ref() {
            f(level, msg);
        }
    });
}

/// `zlog_set_buffering()` (`log_buffering`).
pub fn set_buffering(on: bool) {
    BUFFERING.store(on, Ordering::Relaxed);
}

/// `zlog_set_fd()`: where lines go (`SYSLOG` for syslog).
pub fn set_fd(fd: RawFd) {
    set_fd_ex(fd, false);
}

/// `zlog_set_fd(fd, is_stderr)`: `is_stderr` when `error_log` names
/// standard error (`/dev/stderr`), so lines are not written there twice.
pub fn set_fd_ex(fd: RawFd, is_stderr: bool) {
    FD.store(fd, Ordering::Relaxed);
    FD_IS_STDERR.store(is_stderr, Ordering::Relaxed);
}

/// The current destination.
pub fn fd() -> RawFd {
    FD.load(Ordering::Relaxed)
}

/// `zlog_set_level()`.
pub fn set_level(l: Level) {
    LEVEL.store(l as i32, Ordering::Relaxed);
}

/// `zlog_level`.
pub fn level() -> Level {
    Level::from_i32(LEVEL.load(Ordering::Relaxed))
}

/// `zlog_set_limit()`.
pub fn set_limit(n: usize) {
    LIMIT.store(n, Ordering::Relaxed);
}

/// `zlog_limit`.
pub fn limit() -> usize {
    LIMIT.load(Ordering::Relaxed)
}

/// `zlog_set_launched()`: from now on notices no longer go to standard
/// error as well as to the error log.
pub fn set_launched() {
    LAUNCHED.store(true, Ordering::Relaxed);
}

/// `fpm_globals.is_child`: a worker's lines have no time stamp (the
/// master adds one when it relays them).
pub fn set_child() {
    IS_CHILD.store(true, Ordering::Relaxed);
}

/// Whether this process is a worker.
pub fn is_child() -> bool {
    IS_CHILD.load(Ordering::Relaxed)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `zlog_print_time()`: `[22-Nov-2026 13:00:00` (with `.%06d` micro
/// seconds at the debug level).
pub fn time_prefix(debug: bool) -> String {
    let (secs, usec) = super::os::now();
    let t = super::os::localtime(secs);
    let mut s = format!(
        "[{:02}-{}-{} {:02}:{:02}:{:02}",
        t.mday,
        MONTHS[usize::try_from(t.mon).unwrap_or(0) % 12],
        t.year,
        t.hour,
        t.min,
        t.sec
    );
    if debug {
        let _ = write!(s, ".{usec:06}");
    }
    s.push(']');
    s
}

/// `zlog_buf_prefix()`: the time and the level name (in debug mode also
/// the PID, function and line, as `zlog_ex(__func__, __LINE__, ...)`).
fn prefix(level: Level, function: &str, line: u32, syslog: bool) -> String {
    let debug = self::level() == Level::Debug;
    if syslog {
        return if debug {
            format!("[{}] {function}(), line {line}: ", level.name())
        } else {
            format!("[{}] ", level.name())
        };
    }
    let mut s = String::new();
    if !is_child() {
        s.push_str(&time_prefix(debug));
        s.push(' ');
    }
    if debug {
        if is_child() {
            let _ = write!(s, "{}: {function}(), line {line}: ", level.name());
        } else {
            let _ = write!(
                s,
                "{}: pid {}, {function}(), line {line}: ",
                level.name(),
                std::process::id()
            );
        }
    } else {
        s.push_str(level.name());
        s.push_str(": ");
    }
    s
}

/// `zlog_truncate_buf()`: `len` bytes ending in `...`.
fn truncate(buf: &mut Vec<u8>, len: usize) {
    buf.truncate(len.saturating_sub(1));
    let n = buf.len();
    if n >= 3 {
        buf[n - 3..].copy_from_slice(b"...");
    }
}

/// `vzlog()`: one line at `level` (dropped below the configured level);
/// `errno` adds `: <strerror> (<errno>)` (`ZLOG_HAVE_ERRNO`). A line is
/// cut (ending in `...`) only when it does not fit `MAX_BUF_LENGTH`, or
/// with the error text `log_limit`.
pub fn log(level: Level, function: &str, line: u32, msg: &str, errno: Option<i32>) {
    if has_external() {
        let mut m = msg.as_bytes().to_vec();
        if m.len() >= MAX_BUF_LENGTH {
            m.truncate(MAX_BUF_LENGTH);
            truncate(&mut m, MAX_BUF_LENGTH + 1);
        }
        external(level, &m);
    }
    if level < self::level() {
        return;
    }
    let fd = fd();
    let syslog = fd == SYSLOG;
    let mut buf = prefix(level, function, line, syslog).into_bytes();
    let mut truncated = buf.len() > MAX_BUF_LENGTH - 1;
    if !truncated {
        buf.extend_from_slice(msg.as_bytes());
        truncated = buf.len() >= MAX_BUF_LENGTH;
    }
    if !truncated {
        if let Some(e) = errno {
            buf.extend_from_slice(format!(": {} ({e})", super::os::strerror(e)).as_bytes());
            truncated = buf.len() >= limit();
        }
    }
    if truncated {
        // `zlog_truncate_buf(buf, MIN(zlog_limit, buf_size), 1)`.
        truncate(&mut buf, limit().min(MAX_BUF_LENGTH));
    }
    if syslog {
        super::os::syslog(level.syslog_priority(), &buf);
        buf.push(b'\n');
    } else {
        buf.push(b'\n');
        super::os::write_all(if fd > -1 { fd } else { libc::STDERR_FILENO }, &buf);
    }
    if !FD_IS_STDERR.load(Ordering::Relaxed)
        && fd != -1
        && !LAUNCHED.load(Ordering::Relaxed)
        && level >= Level::Notice
    {
        let _ = std::io::stderr().write_all(&buf);
    }
}

/// `zlog(ZLOG_NOTICE, ...)`.
pub fn notice(function: &str, line: u32, msg: &str) {
    log(Level::Notice, function, line, msg, None);
}

/// `zlog(ZLOG_WARNING, ...)`.
pub fn warning(function: &str, line: u32, msg: &str) {
    log(Level::Warning, function, line, msg, None);
}

/// `zlog(ZLOG_ERROR, ...)`.
pub fn error(function: &str, line: u32, msg: &str) {
    log(Level::Error, function, line, msg, None);
}

/// `zlog(ZLOG_SYSERROR, ...)`: with the last OS error.
pub fn syserror(function: &str, line: u32, msg: &str, err: &std::io::Error) {
    log(
        Level::Error,
        function,
        line,
        msg,
        err.raw_os_error().or(Some(0)),
    );
}

/// `zlog(ZLOG_ALERT, ...)`.
pub fn alert(function: &str, line: u32, msg: &str) {
    log(Level::Alert, function, line, msg, None);
}

/// `zlog(ZLOG_DEBUG, ...)`.
pub fn debug(function: &str, line: u32, msg: &str) {
    log(Level::Debug, function, line, msg, None);
}

/// `struct zlog_stream`: a log line built in pieces, wrapped (or cut) at
/// `log_limit` bytes, optionally buffered, decorated with a message prefix,
/// quotes and suffixes (the master's relay of what workers write).
#[allow(clippy::struct_excessive_bools)] // `zlog_stream`'s bit fields
pub struct Stream {
    level: Level,
    use_syslog: bool,
    use_fd: bool,
    use_buffer: bool,
    use_stderr: bool,
    prefix_buffer: bool,
    finished: bool,
    full: bool,
    wrap: bool,
    msg_quote: bool,
    decorate: bool,
    is_stdout: bool,
    over_limit: bool,
    use_external_logger: bool,
    fd: RawFd,
    function: Option<&'static str>,
    line: u32,
    child_pid: i32,
    /// `buf.data` (its length is `buf.size`).
    buf: Vec<u8>,
    len: usize,
    buf_init_size: usize,
    prefix_len: usize,
    msg_prefix: Option<Vec<u8>>,
    msg_suffix: Option<Vec<u8>>,
    msg_final_suffix: Option<Vec<u8>>,
}

impl Stream {
    /// `zlog_stream_init_internal()`; `fd` 0 is the log's descriptor.
    fn init_internal(level: Level, capacity: usize, fd: RawFd) -> Stream {
        let fd = if fd == 0 { self::fd() } else { fd };
        let use_syslog = fd == SYSLOG;
        let use_fd = fd > 0;
        let use_external_logger = has_external();
        let use_stderr = fd < 0
            || (fd != libc::STDERR_FILENO
                && fd != libc::STDOUT_FILENO
                && !LAUNCHED.load(Ordering::Relaxed)
                && level >= Level::Notice);
        Stream {
            level,
            use_syslog,
            use_fd,
            use_buffer: BUFFERING.load(Ordering::Relaxed) || use_external_logger || use_syslog,
            use_stderr,
            prefix_buffer: level >= self::level() && (use_fd || use_stderr || use_syslog),
            finished: false,
            full: false,
            wrap: false,
            msg_quote: false,
            decorate: false,
            is_stdout: false,
            over_limit: false,
            use_external_logger,
            fd: if fd > -1 { fd } else { libc::STDERR_FILENO },
            function: None,
            line: 0,
            child_pid: 0,
            buf: Vec::new(),
            len: 0,
            buf_init_size: capacity,
            prefix_len: 0,
            msg_prefix: None,
            msg_suffix: None,
            msg_final_suffix: None,
        }
    }

    /// `zlog_stream_init_ex()`: a wrapping stream to `fd`.
    pub fn new(level: Level, fd: RawFd) -> Stream {
        let mut s = Stream::init_internal(level, 1024, fd);
        s.wrap = true;
        s
    }

    /// `zlog_stream_init_ex(stream, ZLOG_ACCESS_LOG, fd)` without wrapping:
    /// an access log line (buffered, no prefix, cut at the limit).
    pub fn access(fd: RawFd) -> Stream {
        let mut s = Stream::init_internal(Level::Notice, 1024, fd);
        s.use_external_logger = false;
        s.use_buffer = true;
        s.use_stderr = fd < 0;
        s.prefix_buffer = false;
        s
    }

    /// `zlog_stream_is_over_limit()`.
    pub fn is_over_limit(&self) -> bool {
        self.over_limit
    }

    /// `zlog_stream_char()`.
    pub fn char(&mut self, c: u8) {
        if self.full {
            return;
        }
        if self.finished {
            self.start();
        }
        if self.use_buffer {
            self.buf_copy_char(c);
        } else {
            self.direct_write(&[c]);
        }
    }

    /// `zlog_stream_set_decorating()`.
    pub fn set_decorating(&mut self, decorate: bool) {
        self.decorate = decorate;
        if !decorate {
            self.msg_quote = false;
            self.prefix_buffer = false;
        }
    }

    /// `zlog_stream_set_wrapping()`.
    pub fn set_wrapping(&mut self, wrap: bool) {
        self.wrap = wrap;
    }

    /// `zlog_stream_set_is_stdout()`.
    pub fn set_is_stdout(&mut self, is_stdout: bool) {
        self.is_stdout = is_stdout;
    }

    /// The stream the stream relays (`is_stdout`).
    pub fn is_stdout(&self) -> bool {
        self.is_stdout
    }

    /// `zlog_stream_set_child_pid()`.
    pub fn set_child_pid(&mut self, pid: i32) {
        self.child_pid = pid;
    }

    /// The worker whose output the stream relays.
    pub fn child_pid(&self) -> i32 {
        self.child_pid
    }

    /// `zlog_stream_set_msg_quoting()`.
    pub fn set_msg_quoting(&mut self, quote: bool) {
        self.msg_quote = quote && self.decorate;
    }

    /// `zlog_stream_set_msg_prefix()`: at most half the line limit.
    pub fn set_msg_prefix(&mut self, prefix: &[u8]) {
        if !self.decorate {
            return;
        }
        let n = prefix
            .len()
            .min((MAX_WRAPPING_PREFIX_LENGTH - 1).min(limit() / 2));
        self.msg_prefix = Some(prefix[..n].to_vec());
    }

    /// `zlog_stream_set_msg_suffix()`.
    pub fn set_msg_suffix(&mut self, suffix: Option<&[u8]>, final_suffix: Option<&[u8]>) {
        if !self.wrap || !self.decorate {
            return;
        }
        if let Some(s) = suffix {
            self.msg_suffix = Some(s.to_vec());
        }
        if let Some(f) = final_suffix {
            self.msg_final_suffix = Some(f.to_vec());
        }
    }

    fn suffix_len(&self) -> usize {
        self.msg_suffix.as_ref().map_or(0, Vec::len)
    }

    /// `zlog_stream_buf_alloc_ex()`: the buffer doubles, up to the limit.
    fn buf_alloc_ex(&mut self, needed: usize) {
        let size = if self.buf.is_empty() {
            self.buf_init_size
        } else {
            self.buf.len() << 1
        };
        let size = limit().min(size.max(needed));
        self.buf.resize(size, 0);
    }

    /// `zlog_stream_buf_alloc()`.
    fn buf_alloc(&mut self) {
        if !self.buf.is_empty() && self.buf.len() >= limit().min(self.buf_init_size) {
            return;
        }
        self.buf_alloc_ex(0);
    }

    /// `zlog_stream_direct_write_ex()`.
    fn direct_write_ex(&self, data: &[u8], append: &[u8]) -> usize {
        if self.use_fd {
            super::os::write_all(self.fd, data);
            if !append.is_empty() {
                super::os::write_all(self.fd, append);
            }
        }
        if self.use_stderr {
            super::os::write_all(libc::STDERR_FILENO, data);
            if !append.is_empty() {
                super::os::write_all(libc::STDERR_FILENO, append);
            }
        }
        data.len()
    }

    fn direct_write(&self, data: &[u8]) -> usize {
        self.direct_write_ex(data, &[])
    }

    /// `zlog_buf_prefix()` for this stream.
    fn level_prefix(&self, function: &str, line: u32) -> Vec<u8> {
        prefix(self.level, function, line, self.use_syslog).into_bytes()
    }

    /// `zlog_stream_prefix_ex()`: the time and level, the message prefix and
    /// the opening quote; returns the line's length so far.
    fn prefix_ex(&mut self, function: &'static str, line: u32) -> usize {
        if !self.prefix_buffer {
            return 0;
        }
        if self.wrap && self.function.is_none() {
            self.function = Some(function);
            self.line = line;
        }
        let p = self.level_prefix(function, line);
        if self.use_buffer {
            self.buf_alloc();
            let mut len = p.len();
            if len + 1 > self.buf.len() {
                // The prefix and its NUL did not fit: a larger buffer, and
                // (as in C) the NUL counts as part of the line.
                len = (len + 1).min(limit());
                self.buf_alloc_ex(len);
                let n = p.len().min(self.buf.len());
                self.buf[..n].copy_from_slice(&p[..n]);
                if n < self.buf.len() {
                    self.buf[n] = 0;
                }
            } else {
                self.buf[..len].copy_from_slice(&p);
            }
            let len = len.min(self.buf.len());
            self.len = len;
            self.prefix_len = len;
            if let Some(mp) = self.msg_prefix.clone() {
                self.buf_copy(&mp);
            }
            if self.msg_quote {
                self.buf_copy_char(b'"');
            }
            self.len
        } else {
            let mut written = self.direct_write(&p[..p.len().min(1024)]);
            if let Some(mp) = &self.msg_prefix {
                written += self.direct_write(mp);
            }
            if self.msg_quote {
                written += self.direct_write(b"\"");
            }
            written
        }
    }

    /// `zlog_stream_buf_copy_cstr()`: as much of `data` as fits.
    fn buf_copy(&mut self, data: &[u8]) -> usize {
        let mut n = data.len();
        if self.buf.len() - self.len <= n {
            self.buf_alloc_ex(n + self.len);
        }
        if self.buf.len() - self.len <= n {
            self.over_limit = true;
            n = self.buf.len() - self.len;
        }
        self.buf[self.len..self.len + n].copy_from_slice(&data[..n]);
        self.len += n;
        n
    }

    /// `zlog_stream_buf_copy_char()`.
    fn buf_copy_char(&mut self, c: u8) -> bool {
        if self.buf.len() == self.len {
            self.buf_alloc_ex(1);
        }
        if self.buf.len() == self.len {
            self.over_limit = true;
            return false;
        }
        self.buf[self.len] = c;
        self.len += 1;
        true
    }

    /// `zlog_stream_buf_flush()`: the buffered line goes out.
    fn buf_flush(&mut self) {
        if self.use_syslog {
            let pushed = self.buf_copy_char(0);
            let end = if pushed { self.len - 1 } else { self.len };
            super::os::syslog(self.level.syslog_priority(), &self.buf[..end]);
            self.len = self.len.saturating_sub(1);
        }
        if self.use_external_logger {
            let start = self.prefix_len.min(self.len);
            external(self.level, &self.buf[start..self.len]);
        }
        self.buf_copy_char(b'\n');
        self.direct_write(&self.buf[..self.len]);
        self.len = 0;
    }

    /// `zlog_stream_buf_append()`.
    fn buf_append(&mut self, data: &[u8]) -> usize {
        if self.len == 0 {
            self.len = self.prefix_ex(self.function.unwrap_or("(null)"), self.line);
        }
        let reserved = self.len + self.suffix_len() + usize::from(self.msg_quote);
        let over = reserved + data.len() >= limit();
        if over {
            self.over_limit = true;
        }
        let available = if over {
            limit().saturating_sub(reserved + 1)
        } else {
            data.len()
        };
        self.buf_copy(&data[..available]);
        if !over {
            return available;
        }
        if self.wrap {
            if self.msg_quote {
                self.buf_copy_char(b'"');
            }
            if let Some(sfx) = self.msg_suffix.clone() {
                self.buf_copy(&sfx);
            }
            self.buf_flush();
            self.prefix_ex(self.function.unwrap_or("(null)"), self.line);
            return available + self.buf_append(&data[available..]);
        }
        // `zlog_truncate_buf(buf, len, 0)`.
        if self.len >= 3 {
            self.buf[self.len - 3..self.len].copy_from_slice(b"...");
        }
        self.full = true;
        available
    }

    /// `zlog_stream_unbuffered_write()`.
    fn unbuffered_write(&mut self, data: &[u8]) -> usize {
        if self.len == 0 {
            self.len = self.prefix_ex(self.function.unwrap_or("(null)"), self.line);
        }
        let reserved = self.len + self.suffix_len() + usize::from(self.msg_quote);
        let required = reserved + data.len();
        let mut n = data.len();
        let mut append: &[u8] = &[];
        if required >= limit() {
            if self.wrap {
                let available = limit().saturating_sub(reserved + 1).min(data.len());
                self.direct_write(&data[..available]);
                if required != limit() {
                    if self.msg_quote {
                        self.direct_write(b"\"");
                    }
                    if let Some(sfx) = &self.msg_suffix {
                        self.direct_write(sfx);
                    }
                    self.direct_write(b"\n");
                }
                self.len = 0;
                return available + self.unbuffered_write(&data[available..]);
            }
            self.full = true;
            if required != limit() {
                append = b"...";
                n = limit().saturating_sub(self.len + 3).min(data.len());
            }
        }
        let written = self.direct_write_ex(&data[..n], append);
        self.len += written;
        written
    }

    /// `zlog_stream_start()`.
    pub fn start(&mut self) {
        self.finished = false;
        self.len = 0;
        self.full = false;
        self.over_limit = false;
    }

    /// `zlog_stream_str()`.
    pub fn str(&mut self, data: &[u8]) -> usize {
        if data.is_empty() || self.full {
            return 0;
        }
        if self.finished {
            self.start();
        }
        if self.use_buffer {
            self.buf_append(data)
        } else {
            self.unbuffered_write(data)
        }
    }

    /// `zlog_stream_finish_buffer_suffix()`.
    fn finish_buffer_suffix(&mut self) {
        if self.msg_quote {
            self.buf_copy_char(b'"');
        }
        if let Some(sfx) = self.msg_suffix.clone() {
            self.buf_copy(&sfx);
        }
        if let Some(fs) = self.msg_final_suffix.clone() {
            if self.len + fs.len() >= limit() {
                let quoting = self.msg_quote;
                let wrap = (self.len + fs.len() + 1 - limit()).min(fs.len());
                self.buf_copy(&fs[..fs.len() - wrap]);
                self.buf_copy_char(b'\n');
                self.buf_flush();
                self.msg_quote = false;
                self.prefix_ex(self.function.unwrap_or("(null)"), self.line);
                self.msg_quote = quoting;
                self.buf_copy(&fs[fs.len() - wrap..]);
                self.buf_copy_char(b'\n');
            } else {
                self.buf_copy(&fs);
            }
        }
    }

    /// `zlog_stream_finish_direct_suffix()`.
    fn finish_direct_suffix(&mut self) {
        if self.msg_quote {
            self.direct_write(b"\"");
            self.len += 1;
        }
        if let Some(sfx) = &self.msg_suffix {
            self.direct_write(sfx);
            self.len += sfx.len();
        }
        if let Some(fs) = self.msg_final_suffix.clone() {
            if self.len + fs.len() >= limit() {
                let quoting = self.msg_quote;
                let wrap = (self.len + fs.len() + 1 - limit()).min(fs.len());
                self.direct_write_ex(&fs[..fs.len() - wrap], b"\n");
                self.msg_quote = false;
                self.prefix_ex(self.function.unwrap_or("(null)"), self.line);
                self.msg_quote = quoting;
                self.direct_write_ex(&fs[fs.len() - wrap..], b"\n");
            } else {
                self.direct_write_ex(&fs, b"\n");
            }
        } else {
            self.direct_write(b"\n");
        }
    }

    /// `zlog_stream_finish()`: the line ends (with its suffixes).
    pub fn finish(&mut self) {
        if self.finished || self.len == 0 {
            return;
        }
        if self.use_buffer {
            if self.decorate {
                self.finish_buffer_suffix();
            }
            self.buf_flush();
        } else if self.decorate {
            self.finish_direct_suffix();
        } else {
            self.direct_write(b"\n");
        }
        self.finished = true;
    }
}

/// `zlog_msg_ex()`: `prefix` and `msg` as one line at `level`, cut (not
/// wrapped) at the limit; through the external logger too.
pub fn msg(level: Level, function: &'static str, line: u32, prefix: &[u8], msg: &[u8]) {
    let mut s = Stream::init_internal(level, msg.len() + prefix.len(), 0);
    s.prefix_ex(function, line);
    s.str(prefix);
    s.str(msg);
    s.finish();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_parse_and_name() {
        assert_eq!(Level::parse("WARNING"), Some(Level::Warning));
        assert_eq!(Level::parse("bogus"), None);
        assert_eq!(Level::Alert.name(), "ALERT");
        assert!(Level::Debug < Level::Notice);
    }

    /// What `f` writes through a stream on a pipe.
    fn captured(f: impl FnOnce(RawFd)) -> String {
        let (r, w) = super::super::os::pipe().unwrap();
        f(w);
        super::super::os::close(w);
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = super::super::os::read_status(r, &mut buf) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        super::super::os::close(r);
        String::from_utf8_lossy(&out).into_owned()
    }

    fn relay(w: RawFd, decorate: bool) -> Stream {
        let mut s = Stream::new(Level::Warning, w);
        s.set_decorating(decorate);
        s.set_wrapping(true);
        s.set_msg_prefix(b"[pool www] child 7 said into stderr: ");
        s.set_msg_quoting(true);
        s
    }

    #[test]
    fn decorated_relay_line() {
        let out = captured(|w| {
            let mut s = relay(w, true);
            s.str(b"hello");
            s.finish();
        });
        assert!(out.starts_with('['), "{out}");
        assert!(
            out.ends_with("] WARNING: [pool www] child 7 said into stderr: \"hello\"\n"),
            "{out}"
        );
    }

    #[test]
    fn long_relayed_line_wraps_at_the_limit() {
        let out = captured(|w| {
            let mut s = relay(w, true);
            s.str("x".repeat(3000).as_bytes());
            s.finish();
        });
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() >= 3, "{out}");
        for l in &lines {
            assert!(l.len() < DEFAULT_LIMIT, "{}", l.len());
            assert!(
                l.contains("said into stderr: \"") && l.ends_with('"'),
                "{l}"
            );
        }
        let xs: usize = lines.iter().map(|l| l.matches('x').count()).sum();
        assert_eq!(xs, 3000);
    }

    #[test]
    fn undecorated_relay_is_raw() {
        let out = captured(|w| {
            let mut s = relay(w, false);
            s.str(b"plain");
            s.finish();
            s.str(b"next");
            s.finish();
        });
        assert_eq!(out, "plain\nnext\n");
    }

    #[test]
    fn access_stream_cuts_with_dots() {
        let out = captured(|w| {
            let mut s = Stream::access(w);
            s.start();
            s.str("y".repeat(2000).as_bytes());
            assert!(s.is_over_limit());
            s.finish();
        });
        assert!(out.ends_with("...\n"), "{out}");
        assert!(out.len() <= DEFAULT_LIMIT + 1);
    }

    #[test]
    fn truncation_ends_with_dots() {
        let mut b = b"0123456789".to_vec();
        truncate(&mut b, 8);
        assert_eq!(b, b"0123...");
    }
}
