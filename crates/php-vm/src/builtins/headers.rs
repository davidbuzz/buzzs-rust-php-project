//! Response headers under the CLI SAPI (`main/SAPI.c`, `ext/standard/head.c`)
//! and the output start location (`php_output_header()`).
//!
//! The CLI's `header_handler` drops every header, so `headers_list()` is
//! always empty; what remains observable is the response code, the
//! "headers already sent" state with the file/line where output started,
//! and the `header_register_callback()` callback, run when the headers are
//! sent (first output, or request shutdown).

use super::reg;
use super::util::{b, i, s};
use crate::func::Args;
use crate::vm::{Vm, VmResult};
use php_runtime::{Value, ZArray, ZStr};
use std::cell::RefCell;

const E_WARNING: i64 = 2;

/// `SG(sapi_headers)` + `OG(output_start_*)`.
#[derive(Default)]
struct HeaderState {
    /// `SG(sapi_headers).http_response_code` (0: none).
    response_code: i64,
    /// `SG(sapi_headers).http_status_line` (set by `header("HTTP/...")`).
    status_line: Option<Vec<u8>>,
    /// `SG(send_header_fcc)`.
    callback: Option<Value>,
    /// Where the first output was produced (`OG(output_start_filename/lineno)`).
    output_start: Option<(ZStr, u32)>,
    /// `sapi_send_headers()` is running the callback.
    sending: bool,
}

thread_local! {
    static STATE: RefCell<HeaderState> = RefCell::new(HeaderState::default());
}

fn with<R>(f: impl FnOnce(&mut HeaderState) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

/// `sapi_activate()` / `php_output_activate()`: a new request on this
/// thread starts with no response code, callback or output position.
pub(crate) fn reset() {
    with(|s| *s = HeaderState::default());
}

/// `php_output_get_start_filename()` / `php_output_get_start_lineno()`.
#[must_use]
pub fn output_start() -> Option<(ZStr, u32)> {
    with(|s| s.output_start.clone())
}

/// `php_output_header()` → `php_header()` → `sapi_send_headers()`: called by
/// the output layer before output reaches the SAPI. Records where output
/// started and runs the `header_register_callback()` callback once.
/// Returns whether output is allowed (always, under the CLI).
pub fn send_headers(vm: &mut Vm) -> bool {
    if vm.headers_sent || with(|s| s.sending) {
        return true;
    }
    if vm.is_executing() && with(|s| s.output_start.is_none()) {
        let loc = vm.current_location();
        with(|s| s.output_start = Some(loc));
    }
    if let Some(hook) = vm.send_headers_hook {
        // A non-CLI SAPI's `php_header()`: its header list, its
        // `header_register_callback()` callable and its send; `false`
        // refuses output (a HEAD request). Re-entrant like
        // `sapi_send_headers()`: output of the callable comes back here
        // and sends the headers from inside the callable, and a refusal
        // then disables that output too.
        let allowed = hook(vm);
        vm.headers_sent = true;
        return allowed;
    }
    // The callback is cleared before it runs (it may produce output).
    if let Some(cb) = with(|s| s.callback.take()) {
        with(|s| s.sending = true);
        let r = vm.call_value(&cb, Vec::new());
        with(|s| s.sending = false);
        if let Err(t) = r {
            if vm.pending_throw.is_none() {
                vm.pending_throw = Some(t);
            }
        }
    }
    vm.headers_sent = true;
    true
}

/// The "headers already sent" warning of `sapi_header_op()`.
fn already_sent(vm: &mut Vm) -> VmResult<()> {
    let msg = match output_start() {
        Some((f, l)) => format!(
            "Cannot modify header information - headers already sent by (output started at {}:{l})",
            f.to_string_lossy()
        ),
        None => "Cannot modify header information - headers already sent".to_string(),
    };
    vm.error(E_WARNING, &msg)
}

/// `sapi_update_response_code()`.
fn update_response_code(code: i64) {
    with(|s| {
        if s.response_code != code {
            s.status_line = None;
            s.response_code = code;
        }
    });
}

/// `sapi_extract_response_code()`: the number after the first space.
fn extract_response_code(line: &[u8]) -> i64 {
    let Some(sp) = line.iter().position(|&c| c == b' ') else {
        return 0;
    };
    let rest = &line[sp + 1..];
    let digits: Vec<u8> = rest
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .copied()
        .collect();
    std::str::from_utf8(&digits)
        .ok()
        .and_then(|d| d.parse::<i64>().ok())
        .unwrap_or(0)
}

/// `sapi_header_op(SAPI_HEADER_REPLACE/ADD)` under the CLI.
fn header_add(vm: &mut Vm, line: &[u8], response_code: i64) -> VmResult<()> {
    if vm.headers_sent {
        return already_sent(vm);
    }
    if line.is_empty() {
        return Ok(());
    }
    let mut len = line.len();
    while len > 0 && line[len - 1].is_ascii_whitespace() {
        len -= 1;
    }
    let line = &line[..len];
    for &c in line {
        if c == b'\n' || c == b'\r' {
            return vm.error(
                E_WARNING,
                "Header may not contain more than a single header, new line detected",
            );
        }
        if c == 0 {
            return vm.error(E_WARNING, "Header may not contain NUL bytes");
        }
    }
    if line.len() >= 5 && line[..5].eq_ignore_ascii_case(b"HTTP/") {
        update_response_code(extract_response_code(line));
        with(|s| s.status_line = Some(line.to_vec()));
        return Ok(());
    }
    if let Some(colon) = line.iter().position(|&c| c == b':') {
        let name = &line[..colon];
        if name.eq_ignore_ascii_case(b"Location") {
            let cur = with(|s| s.response_code);
            if !(300..=399).contains(&cur) && cur != 201 {
                // No request method under the CLI: a plain 302.
                update_response_code(if response_code != 0 {
                    response_code
                } else {
                    302
                });
            }
        } else if name.eq_ignore_ascii_case(b"Content-Length") {
            // The script cannot know the compressed length (req #44164).
            alter_ini(vm, "zlib.output_compression", b"0")?;
        } else if name.eq_ignore_ascii_case(b"WWW-Authenticate") {
            update_response_code(401);
        }
    }
    if response_code != 0 {
        update_response_code(response_code);
    }
    vm.note_response_header(line);
    Ok(())
}

/// Internal header additions use the same validation as CLI `header()`.
pub(crate) fn add_response_header(vm: &mut Vm, line: &[u8]) -> VmResult<bool> {
    let trimmed = line.trim_ascii_end();
    let accepted = !vm.headers_sent
        && !line.is_empty()
        && !trimmed.iter().any(|c| matches!(c, 0 | b'\r' | b'\n'));
    header_add(vm, line, 0)?;
    Ok(accepted)
}

/// `zend_alter_ini_entry_chars(..., PHP_INI_USER, PHP_INI_STAGE_RUNTIME)`.
pub(crate) fn alter_ini(vm: &mut Vm, name: &str, value: &[u8]) -> VmResult<()> {
    if vm.ini.get(name).is_none() {
        return Ok(());
    }
    if let Some(h) = vm.ini_modify_hooks.get(name).copied() {
        if !h(vm, value)? {
            return Ok(());
        }
    }
    let mut diagnostics = Vec::new();
    vm.ini.ini_set(name, value, &mut diagnostics);
    vm.sync_ini(name);
    Ok(())
}

fn header(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let line = s(a, 0);
    let code = if a.has(2) { i(a, 2) } else { 0 };
    let _replace = !a.has(1) || b(a, 1);
    header_add(vm, line.as_bytes(), code)?;
    Ok(Value::Null)
}

fn header_remove(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    if vm.headers_sent {
        already_sent(vm)?;
        return Ok(Value::Null);
    }
    if let Value::Str(name) = a.at(0) {
        if name.as_bytes().contains(&b':') {
            vm.error(E_WARNING, "Header to delete may not contain colon.")?;
        }
    }
    Ok(Value::Null)
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn headers_sent(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let sent = vm.headers_sent;
    let (file, line) = if sent {
        output_start().map_or((ZStr::empty(), 0), |(f, l)| (f, l))
    } else {
        (ZStr::empty(), 0)
    };
    // `ZEND_TRY_ASSIGN_REF_STRING`/`_LONG`: each old value is destroyed
    // after its variable became null; a destructor exception surfaces
    // once both outputs are assigned.
    let mut thrown = Ok(());
    if a.has(0) {
        thrown = vm.try_assign_ref_arg(a, 0, Value::Str(file));
    }
    if a.has(1) {
        let r = vm.try_assign_ref_arg(a, 1, Value::Int(i64::from(line)));
        if thrown.is_ok() {
            thrown = r;
        }
    }
    thrown?;
    Ok(Value::Bool(sent))
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn headers_list(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(ZArray::new()))
}

fn http_response_code(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let code = i(a, 0);
    if code != 0 {
        if vm.headers_sent {
            let msg = match output_start() {
                Some((f, l)) => format!(
                    "Cannot set response code - headers already sent (output started at {}:{l})",
                    f.to_string_lossy()
                ),
                None => "Cannot set response code - headers already sent".to_string(),
            };
            vm.docref(E_WARNING, &msg)?;
            return Ok(Value::Bool(false));
        }
        if with(|s| s.status_line.is_some()) {
            vm.docref(
                E_WARNING,
                "Calling http_response_code() after header('HTTP/...') has no effect",
            )?;
        }
        let old = with(|s| std::mem::replace(&mut s.response_code, code));
        return Ok(if old != 0 {
            Value::Int(old)
        } else {
            Value::Bool(true)
        });
    }
    let cur = with(|s| s.response_code);
    Ok(if cur == 0 {
        Value::Bool(false)
    } else {
        Value::Int(cur)
    })
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn header_register_callback(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cb = a.at(0);
    // Not kept once the headers are out: it would never run.
    let keep = if vm.headers_sent { None } else { Some(cb) };
    with(|s| s.callback = keep);
    Ok(Value::Bool(true))
}

/// Registers the header functions.
pub(crate) fn register(vm: &mut Vm) {
    reg(
        vm,
        "header(string $header, bool $replace = true, int $response_code = 0): void",
        header,
    );
    reg(
        vm,
        "header_remove(?string $name = null): void",
        header_remove,
    );
    reg(
        vm,
        "headers_sent(&$filename = null, &$line = null): bool",
        headers_sent,
    );
    reg(vm, "headers_list(): array", headers_list);
    reg(
        vm,
        "http_response_code(int $response_code = 0): int|bool",
        http_response_code,
    );
    reg(
        vm,
        "header_register_callback(callable $callback): true",
        header_register_callback,
    );
}

#[cfg(test)]
mod tests {
    use super::extract_response_code;
    use crate::{Vm, VmOptions};
    use php_runtime::Value;

    #[test]
    fn response_code_from_status_line() {
        assert_eq!(extract_response_code(b"HTTP/1.1 404 Not Found"), 404);
        assert_eq!(extract_response_code(b"HTTP/1.0"), 0);
        assert_eq!(extract_response_code(b"HTTP/1.1 abc"), 0);
    }

    #[test]
    fn internal_headers_preserve_cli_lists_and_first_mime_type() {
        let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(Vec::<u8>::new()), &[]);
        assert!(vm
            .add_response_header(b"Content-Type: text/plain", true)
            .expect("header"));
        assert!(vm
            .add_response_header(b"Content-Type: application/json", true)
            .expect("header"));
        assert_eq!(
            vm.response_mimetype(),
            Some(b"text/plain;charset=UTF-8".to_vec())
        );
        let list = vm
            .call_value(&Value::from("headers_list"), Vec::new())
            .expect("list");
        assert!(matches!(list, Value::Array(a) if a.is_empty()));
    }

    #[test]
    fn sapi_send_headers_hook_runs_once_and_may_refuse_output() {
        let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(Vec::<u8>::new()), &[]);
        vm.send_headers_hook = Some(|_| false);
        assert!(!super::send_headers(&mut vm), "the hook refused output");
        assert!(vm.headers_sent);
        // Already sent: the hook is not consulted again.
        vm.send_headers_hook = Some(|_| unreachable!("headers were sent"));
        assert!(super::send_headers(&mut vm));
    }

    #[test]
    fn sapi_send_headers_hook_is_reentrant() {
        thread_local! {
            static CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        // The SAPI's php_header() whose header callable produces output:
        // that output asks for the headers again before the outer call
        // has marked them sent, and is refused as well.
        fn hook(vm: &mut Vm) -> bool {
            let n = CALLS.with(|c| {
                c.set(c.get() + 1);
                c.get()
            });
            if n == 1 {
                assert!(!super::send_headers(vm), "the nested call refuses output");
                assert!(vm.headers_sent, "the nested call sent the headers");
            }
            false
        }
        let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(Vec::<u8>::new()), &[]);
        vm.send_headers_hook = Some(hook);
        assert!(!super::send_headers(&mut vm));
        assert_eq!(CALLS.with(std::cell::Cell::get), 2);
        assert!(super::send_headers(&mut vm), "sent: output passes");
        assert_eq!(CALLS.with(std::cell::Cell::get), 2);
    }

    #[test]
    fn rejected_sapi_header_does_not_report_success_or_change_mime() {
        let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(Vec::<u8>::new()), &[]);
        vm.response_header_hook = Some(|_, _, _, _| Ok(false));
        assert!(!vm
            .add_response_header(b"Content-Type: text/plain", true)
            .expect("header"));
        assert_eq!(vm.response_mimetype(), None);
        assert!(vm.sends_default_content_type());
    }
}
