//! Error reporting (`zend_error`, `php_error_cb`): user error handlers,
//! display, logging, fatal errors and uncaught exceptions.

use crate::object;
use crate::vm::{FrameKind, LastError, Throw, Vm, VmResult};
use php_runtime::{Obj, Value, ZStr};

/// `E_*` constants.
pub mod level {
    /// `E_ERROR`
    pub const ERROR: i64 = 1;
    /// `E_WARNING`
    pub const WARNING: i64 = 2;
    /// `E_PARSE`
    pub const PARSE: i64 = 4;
    /// `E_NOTICE`
    pub const NOTICE: i64 = 8;
    /// `E_CORE_ERROR`
    pub const CORE_ERROR: i64 = 16;
    /// `E_CORE_WARNING`
    pub const CORE_WARNING: i64 = 32;
    /// `E_COMPILE_ERROR`
    pub const COMPILE_ERROR: i64 = 64;
    /// `E_COMPILE_WARNING`
    pub const COMPILE_WARNING: i64 = 128;
    /// `E_USER_ERROR`
    pub const USER_ERROR: i64 = 256;
    /// `E_USER_WARNING`
    pub const USER_WARNING: i64 = 512;
    /// `E_USER_NOTICE`
    pub const USER_NOTICE: i64 = 1024;
    /// `E_STRICT`
    pub const STRICT: i64 = 2048;
    /// `E_RECOVERABLE_ERROR`
    pub const RECOVERABLE_ERROR: i64 = 4096;
    /// `E_DEPRECATED`
    pub const DEPRECATED: i64 = 8192;
    /// `E_USER_DEPRECATED`
    pub const USER_DEPRECATED: i64 = 16384;
    /// `E_ALL`
    pub const ALL: i64 = 30719;
}

/// The label printed for an error level.
pub fn label(l: i64) -> &'static str {
    match l {
        1 | 16 | 64 | 256 => "Fatal error",
        4096 => "Recoverable fatal error",
        2 | 32 | 128 | 512 => "Warning",
        4 => "Parse error",
        8 | 1024 => "Notice",
        2048 => "Strict Standards",
        8192 | 16384 => "Deprecated",
        _ => "Unknown error",
    }
}

fn is_fatal(l: i64) -> bool {
    matches!(l, 1 | 4 | 16 | 64 | 256 | 4096)
}

impl Vm {
    /// The file and line of the currently executing user code.
    pub fn current_location(&self) -> (ZStr, u32) {
        for f in self.frames.iter().rev() {
            if f.kind == FrameKind::Builtin {
                continue;
            }
            return (f.code.filename.clone(), f.line());
        }
        // `php_error_cb` with nothing executing.
        (ZStr::from("Unknown"), 0)
    }

    /// Whether code is executing.
    pub fn is_executing(&self) -> bool {
        self.frames.iter().any(|f| f.kind != FrameKind::Builtin)
    }

    /// Reports an error at the current location.
    pub fn error(&mut self, level: i64, msg: &str) -> VmResult<()> {
        let (file, line) = self.current_location();
        self.error_at(level, msg, &file, line)
    }

    /// Reports a docref-style error (`func(): msg`) from a builtin.
    pub(crate) fn docref(&mut self, level: i64, msg: &str) -> VmResult<()> {
        self.docref_bytes(level, msg.as_bytes())
    }

    /// The function `php_error_docref()` names (`get_active_function_name()`):
    /// the innermost builtin frame, as `Class::method` for methods.
    pub(crate) fn docref_function_name(&self) -> Option<String> {
        self.frames
            .iter()
            .rev()
            .find(|f| f.kind == FrameKind::Builtin)
            .and_then(|f| {
                let n = f.builtin_name.clone()?;
                Some(match &f.scope {
                    Some(c) if f.this.is_some() || f.called.is_some() => {
                        format!("{}::{}", object::display_name(&c.name), n)
                    }
                    _ => n.to_string_lossy(),
                })
            })
    }

    /// The parenthesised part of a `php_error_docref()` origin: the
    /// running builtin's arguments rendered as in stack traces when
    /// `error_include_args` is on (`zend_trace_current_function_args_string`),
    /// else empty.
    pub(crate) fn docref_args(&mut self) -> String {
        if !self.ini.get_bool("error_include_args") {
            return String::new();
        }
        let Some(idx) = self
            .frames
            .iter()
            .rposition(|f| f.kind == FrameKind::Builtin)
        else {
            return String::new();
        };
        let args = self.trace_frame_args(idx);
        self.trace_args_list(&args)
    }

    /// Under `error_include_args`, a library diagnostic that already starts
    /// with the running builtin's `name(): ` origin gets the arguments put
    /// into it, as `php_verror` builds the origin itself
    /// (`zend_trace_current_function_args_string`).
    pub(crate) fn origin_with_args(&mut self, msg: &str) -> Option<String> {
        if !self.ini.get_bool("error_include_args") {
            return None;
        }
        let name = self.docref_function_name()?;
        let rest = msg.strip_prefix(name.as_str())?.strip_prefix("(): ")?;
        let args = self.docref_args();
        Some(format!("{name}({args}): {rest}"))
    }

    /// The origin `include`/`require` warnings are prefixed with:
    /// `include()`, or `include('name')` under `error_include_args`
    /// (`php_verror` while `ZEND_INCLUDE_OR_EVAL` executes).
    pub fn include_error_origin(&mut self, kind: &str, name: &[u8]) -> String {
        if self.ini.get_bool("error_include_args") {
            let arg = self.trace_arg(&Value::Str(ZStr::from(name)));
            format!("{kind}({arg})")
        } else {
            format!("{kind}()")
        }
    }

    /// Binary-safe [`Vm::docref`].
    pub(crate) fn docref_bytes(&mut self, level: i64, msg: &[u8]) -> VmResult<()> {
        // `php_verror`: nothing is reported while a delayed OOM is pending
        // (the builtin is bailing out; the fatal follows the call).
        if php_memlimit::is_pending() {
            return Ok(());
        }
        match self.docref_function_name() {
            Some(n) => {
                let args = self.docref_args();
                let mut m = format!("{n}({args}): ").into_bytes();
                m.extend_from_slice(msg);
                self.error_bytes(level, &m)
            }
            None => self.error_bytes(level, msg),
        }
    }

    /// Reports an error at a given location.
    pub fn error_at(&mut self, level: i64, msg: &str, file: &ZStr, line: u32) -> VmResult<()> {
        self.error_at_bytes(level, msg.as_bytes(), file, line)
    }

    /// Reports an error with a binary message at the current location.
    pub fn error_bytes(&mut self, level: i64, msg: &[u8]) -> VmResult<()> {
        let (file, line) = self.current_location();
        self.error_at_bytes(level, msg, &file, line)
    }

    /// Reports an error (binary-safe message) at a given location.
    #[allow(clippy::too_many_lines)]
    pub fn error_at_bytes(
        &mut self,
        level: i64,
        msg: &[u8],
        file: &ZStr,
        line: u32,
    ) -> VmResult<()> {
        // User error handler.
        let handleable = level
            & (level::ERROR
                | level::PARSE
                | level::CORE_ERROR
                | level::CORE_WARNING
                | level::COMPILE_ERROR
                | level::COMPILE_WARNING)
            == 0;
        if handleable {
            if let Some(h) = self.error_handlers.last().cloned() {
                if !h.callback.is_null() && h.mask & level != 0 {
                    self.in_error_handler += 1;
                    let args = vec![
                        Value::Int(level),
                        Value::Str(ZStr::from(msg)),
                        Value::Str(file.clone()),
                        Value::Int(i64::from(line)),
                    ];
                    // zend_error_impl: EG(user_error_handler) is UNDEF while
                    // the handler runs (so its own errors take the default
                    // path, or a handler it installs), and is put back
                    // afterwards unless the handler installed another one.
                    if let Some(top) = self.error_handlers.last_mut() {
                        top.callback = Value::Null;
                    }
                    let r = self.call_value(&h.callback, args);
                    match self.error_handlers.last_mut() {
                        Some(top) if top.callback.is_null() => top.callback = h.callback,
                        Some(_) => {}
                        None => self.error_handlers.push(h),
                    }
                    self.in_error_handler -= 1;
                    let r = r?;
                    if !matches!(r, Value::Bool(false)) {
                        if level == level::USER_ERROR || level == level::RECOVERABLE_ERROR {
                            return Ok(());
                        }
                        return Ok(());
                    }
                }
            }
        }
        // `php_error_cb`: a repeated error is neither stored nor shown.
        if !self.is_repeated_error(msg, file, line) {
            self.last_error = Some(LastError {
                level,
                message: msg.to_vec(),
                file: file.clone(),
                line,
            });
            if self.error_reporting & level != 0
                || level & (level::CORE_ERROR | level::CORE_WARNING) != 0
            {
                self.display_error(level, msg, file, line);
            }
        }
        if is_fatal(level) {
            self.exit_status = 255;
            object::set_destructors_enabled(false);
            return Err(Throw::Fatal);
        }
        Ok(())
    }

    /// `php_error_cb`'s repeated-error check: under
    /// `ignore_repeated_errors`, an error whose message equals the last
    /// stored one (and, unless `ignore_repeated_source` is on, whose file
    /// and line match too) is neither stored nor displayed or logged.
    fn is_repeated_error(&self, msg: &[u8], file: &ZStr, line: u32) -> bool {
        let Some(last) = &self.last_error else {
            return false;
        };
        if last.message != msg || !self.ini.get_bool("ignore_repeated_errors") {
            return false;
        }
        self.ini.get_bool("ignore_repeated_source") || (last.line == line && last.file == *file)
    }

    /// Displays/logs an error according to the ini settings.
    pub(crate) fn display_error(&mut self, level: i64, msg: &[u8], file: &ZStr, line: u32) {
        let lbl = label(level);
        let file_s = file.to_string_lossy();
        if self.ini.get_bool("log_errors") {
            let log = format!(
                "PHP {lbl}:  {} in {file_s} on line {line}",
                String::from_utf8_lossy(msg)
            );
            self.log_error(&log);
        }
        let de = self
            .ini
            .get("display_errors")
            .map(<[u8]>::to_ascii_lowercase)
            .unwrap_or_default();
        let display = match de.as_slice() {
            b"stderr" => Some(true),
            b"stdout" | b"1" | b"on" | b"yes" | b"true" => Some(false),
            b"" | b"0" | b"off" | b"no" | b"false" | b"none" => None,
            _ => {
                if std::str::from_utf8(&de)
                    .ok()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .unwrap_or(0)
                    != 0
                {
                    Some(false)
                } else {
                    None
                }
            }
        };
        let Some(to_stderr) = display else {
            return;
        };
        let prepend = self
            .ini
            .get("error_prepend_string")
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        let append = self
            .ini
            .get("error_append_string")
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        if self.ini.get_bool("html_errors") {
            let mut out = prepend.clone();
            out.extend_from_slice(format!("<br />\n<b>{lbl}</b>:  ").as_bytes());
            // `php_error_cb`: the message is HTML-escaped with html_errors.
            let internal_encoding = self.ini.get("internal_encoding");
            let default_charset = self.ini.get("default_charset");
            let charset =
                php_stdstr::html::effective_charset_hint(None, internal_encoding, default_charset);
            out.extend_from_slice(&html_escape(msg, charset));
            out.extend_from_slice(
                format!(" in <b>{file_s}</b> on line <b>{line}</b><br />\n").as_bytes(),
            );
            out.extend_from_slice(&append);
            self.echo(&out);
            return;
        }
        if to_stderr {
            self.flush_stdout();
            let mut s = format!("{lbl}: ").into_bytes();
            s.extend_from_slice(msg);
            s.extend_from_slice(format!(" in {file_s} on line {line}\n").as_bytes());
            let _ = std::io::Write::write_all(&mut std::io::stderr(), &s);
            return;
        }
        let mut out = prepend;
        out.extend_from_slice(format!("\n{lbl}: ").as_bytes());
        out.extend_from_slice(msg);
        out.extend_from_slice(format!(" in {file_s} on line {line}\n").as_bytes());
        out.extend_from_slice(&append);
        self.echo(&out);
    }

    /// Writes to the error log (stderr in the CLI, or the `error_log` file).
    pub(crate) fn log_error(&mut self, msg: &str) {
        let target = self
            .ini
            .get("error_log")
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        if !target.is_empty() && target != b"syslog" {
            if let Ok(path) = std::str::from_utf8(&target) {
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let now = crate::builtins::log_time_string(self);
                    let _ =
                        std::io::Write::write_all(&mut f, format!("[{now}] {msg}\n").as_bytes());
                    return;
                }
            }
        }
        // `sapi_module.log_message`: a SAPI may route the log itself.
        if let Some(mut hook) = self.log_hook.take() {
            hook(msg.as_bytes());
            self.log_hook = Some(hook);
            return;
        }
        self.flush_stdout();
        let _ = std::io::Write::write_all(&mut std::io::stderr(), format!("{msg}\n").as_bytes());
    }

    /// Reports a fatal error at the current location and returns the bailout.
    pub fn fatal(&mut self, msg: &str) -> Throw {
        let (file, line) = self.current_location();
        self.report_fatal(msg, &file, line);
        Throw::Fatal
    }

    /// Reports a fatal error (`E_ERROR`) without user handlers.
    pub(crate) fn report_fatal(&mut self, msg: &str, file: &ZStr, line: u32) {
        self.report_fatal_bytes(msg.as_bytes(), file, line);
    }

    fn report_fatal_bytes(&mut self, msg: &[u8], file: &ZStr, line: u32) {
        self.display_fatal_bytes(msg, file, line);
        object::set_destructors_enabled(false);
    }

    /// Records and displays an `E_ERROR` without the bailout's
    /// `zend_objects_store_mark_destructed` — the `E_DONT_BAIL` path of
    /// `php_error_cb` that `zend_exception_error` takes, so objects still
    /// get their destructors at shutdown after an uncaught exception.
    fn display_fatal_bytes(&mut self, msg: &[u8], file: &ZStr, line: u32) {
        self.last_error = Some(LastError {
            level: level::ERROR,
            message: msg.to_vec(),
            file: file.clone(),
            line,
        });
        if self.error_reporting & level::ERROR != 0 {
            self.display_error(level::ERROR, msg, file, line);
        }
        self.exit_status = 255;
    }

    /// Reports a compile error (`E_COMPILE_ERROR`).
    pub(crate) fn report_compile_error(&mut self, msg: &str, file: &ZStr, line: u32, parse: bool) {
        let lv = if parse {
            level::PARSE
        } else {
            level::COMPILE_ERROR
        };
        self.last_error = Some(LastError {
            level: lv,
            message: msg.as_bytes().to_vec(),
            file: file.clone(),
            line,
        });
        if self.error_reporting & lv != 0 {
            self.display_error(lv, msg.as_bytes(), file, line);
        }
        self.exit_status = 255;
        object::set_destructors_enabled(false);
    }

    /// Handles an exception that escaped all frames: the user exception
    /// handler or the "Uncaught" fatal error.
    ///
    /// `zend_user_exception_handler`: the handler is pushed and none is
    /// current while it runs; afterwards it becomes current again unless
    /// it installed another one (or restored an older one). An exception
    /// the handler throws goes to the handler current at that point
    /// (`zend_throw_exception_internal` without a frame), else is fatal.
    pub(crate) fn uncaught_exception(&mut self, e: Obj) -> Result<(), Throw> {
        if let Some(h) = self.exception_handlers.last().cloned() {
            if !h.is_null() {
                self.exception_handlers.push(Value::Null);
                let r = match self.call_value(&h, vec![Value::Object(e)]) {
                    Ok(_) => Ok(()),
                    Err(Throw::Exception(e2)) => self.uncaught_exception(e2),
                    Err(t) => Err(t),
                };
                if self.exception_handlers.last().is_some_and(Value::is_null) {
                    self.exception_handlers.pop();
                }
                return r;
            }
        }
        self.report_uncaught(&e);
        Err(Throw::Fatal)
    }

    /// Prints "Fatal error: Uncaught ...".
    pub(crate) fn report_uncaught(&mut self, e: &Obj) {
        let cls = object::class_of(e).clone();
        if cls.lc_name.as_bytes() == b"parseerror" || cls.lc_name.as_bytes() == b"compileerror" {
            let msg = self.exc_prop(e, "message");
            let file = self.exc_prop(e, "file");
            let line = self.exc_prop(e, "line");
            let msg = msg.as_str().map(ZStr::to_string_lossy).unwrap_or_default();
            let file = file.as_str().cloned().unwrap_or_default();
            let line = u32::try_from(line.as_int().unwrap_or(0)).unwrap_or(0);
            // Reported with `E_DONT_BAIL` as well: the destructors still run.
            let enabled = object::destructors_enabled();
            self.report_compile_error(&msg, &file, line, cls.lc_name.as_bytes() == b"parseerror");
            object::set_destructors_enabled(enabled);
            return;
        }
        // `zend_exception_error`.
        match self.call_method_name(e, "__toString", Vec::new()) {
            Ok(v) => match v.deref() {
                Value::Str(s) => self.set_exc_prop(e, "string", Value::Str(s)),
                _ => {
                    let _ =
                        self.warning(&format!("{}::__toString() must return a string", cls.name));
                }
            },
            Err(Throw::Exception(inner)) => {
                // Thrown with no executing frame: php-src reports the inner
                // exception right away (`zend_throw_exception_internal`).
                if !self.is_executing() {
                    self.report_uncaught(&inner);
                    return;
                }
                let file = self
                    .exc_prop(&inner, "file")
                    .as_str()
                    .cloned()
                    .unwrap_or_default();
                let file = if file.is_empty() {
                    ZStr::from("Unknown")
                } else {
                    file
                };
                let line =
                    u32::try_from(self.exc_prop(&inner, "line").as_int().unwrap_or(0)).unwrap_or(0);
                let msg = format!(
                    "Uncaught {} in exception handling during call to {}::__toString()",
                    object::class_of(&inner).name,
                    cls.name
                );
                let _ = self.error_at(level::WARNING, &msg, &file, line);
            }
            Err(_) => {}
        }
        let s = self.exc_prop(e, "string");
        let s = s.as_str().cloned().unwrap_or_default();
        let file = self
            .exc_prop(e, "file")
            .as_str()
            .cloned()
            .unwrap_or_default();
        let file = if file.is_empty() {
            ZStr::from("Unknown")
        } else {
            file
        };
        let line = u32::try_from(self.exc_prop(e, "line").as_int().unwrap_or(0)).unwrap_or(0);
        let mut msg = b"Uncaught ".to_vec();
        msg.extend_from_slice(s.as_bytes());
        msg.extend_from_slice(b"\n  thrown");
        self.display_fatal_bytes(&msg, &file, line);
    }
}

/// `escape_html` of `php_error_cb`: retry invalid input with substitution.
fn html_escape(msg: &[u8], charset: Option<&[u8]>) -> Vec<u8> {
    use php_stdstr::html::{escape_html_entities, ENT_COMPAT, ENT_SUBSTITUTE};

    let mut notices = php_stdstr::Notices::new();
    let out = escape_html_entities(
        msg,
        false,
        ENT_COMPAT,
        charset,
        true,
        true,
        "htmlspecialchars",
        &mut notices,
    );
    if out.is_empty() {
        return escape_html_entities(
            msg,
            false,
            ENT_COMPAT | ENT_SUBSTITUTE,
            charset,
            true,
            true,
            "htmlspecialchars",
            &mut notices,
        );
    }
    out
}
