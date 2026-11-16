// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Program execution (`ext/standard/exec.c`): `escapeshellarg`,
//! `escapeshellcmd`, `exec`, `system`, `passthru`, `shell_exec`,
//! `proc_nice`.
//!
//! Commands run through `/bin/sh -c` exactly like `popen(cmd, "r")`: the
//! child inherits stdin and stderr, its stdout is read by the parent.

use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use php_runtime::{ErrorLevel, RtResult, RuntimeContext, RuntimeError, Value, ZArray, ZStr};

use crate::sys;

/// The maximum command line length (`cmd_max_len`, `sysconf(_SC_ARG_MAX)`).
#[must_use]
pub fn cmd_max_len() -> usize {
    static LEN: OnceLock<usize> = OnceLock::new();
    *LEN.get_or_init(|| usize::try_from(sys::arg_max()).unwrap_or(4096))
}

fn nul_error(func: &str, arg: &str) -> RuntimeError {
    RuntimeError::ValueError(format!(
        "{func}(): Argument #1 (${arg}) must not contain any null bytes"
    ))
}

/// `php_escape_shell_cmd()` / `escapeshellcmd($command)`.
///
/// Multibyte characters of the current `LC_CTYPE` are copied verbatim and
/// invalid sequences are dropped, like PHP's use of `mblen()`.
pub fn escapeshellcmd(cmd: &[u8]) -> RtResult<Vec<u8>> {
    if cmd.contains(&0) {
        return Err(nul_error("escapeshellcmd", "command"));
    }
    if cmd.is_empty() {
        return Ok(Vec::new());
    }
    escape_cmd_with(cmd, cmd_max_len(), sys::mblen)
}

/// [`escapeshellcmd`] with an explicit length limit and `mblen` function.
#[allow(clippy::many_single_char_names, clippy::similar_names)]
pub fn escape_cmd_with(
    s: &[u8],
    max_len: usize,
    mblen: impl Fn(&[u8]) -> isize,
) -> RtResult<Vec<u8>> {
    let l = s.len();
    if l > max_len.saturating_sub(3) {
        return Err(RuntimeError::ValueError(format!(
            "Command exceeds the allowed length of {max_len} bytes"
        )));
    }
    let mut out = Vec::with_capacity(2 * l);
    // Index of the matching closing quote (the `p` pointer of the C code).
    let mut p: Option<usize> = None;
    let mut x = 0;
    while x < l {
        let mb_len = mblen(&s[x..]);
        if mb_len < 0 {
            x += 1;
            continue;
        }
        if mb_len > 1 {
            #[allow(clippy::cast_sign_loss)]
            let n = (mb_len as usize).min(l - x);
            out.extend_from_slice(&s[x..x + n]);
            x += n;
            continue;
        }
        let c = s[x];
        match c {
            b'"' | b'\'' => {
                if p.is_none() {
                    p = s[x + 1..].iter().position(|&d| d == c).map(|i| x + 1 + i);
                    if p.is_none() {
                        out.push(b'\\');
                    }
                } else if p.is_some_and(|q| s[q] == c) {
                    p = None;
                } else {
                    out.push(b'\\');
                }
                out.push(c);
            }
            b'#' | b'&' | b';' | b'`' | b'|' | b'*' | b'?' | b'~' | b'<' | b'>' | b'^' | b'('
            | b')' | b'[' | b']' | b'{' | b'}' | b'$' | b'\\' | b'\n' | 0xff => {
                out.push(b'\\');
                out.push(c);
            }
            _ => out.push(c),
        }
        x += 1;
    }
    if out.len() > max_len + 1 {
        return Err(RuntimeError::ValueError(format!(
            "Escaped command exceeds the allowed length of {max_len} bytes"
        )));
    }
    Ok(out)
}

/// `php_escape_shell_arg()` / `escapeshellarg($arg)`.
pub fn escapeshellarg(arg: &[u8]) -> RtResult<Vec<u8>> {
    if arg.contains(&0) {
        return Err(nul_error("escapeshellarg", "arg"));
    }
    escape_arg_with(arg, cmd_max_len(), sys::mblen)
}

/// [`escapeshellarg`] with an explicit length limit and `mblen` function.
#[allow(clippy::similar_names)]
pub fn escape_arg_with(
    s: &[u8],
    max_len: usize,
    mblen: impl Fn(&[u8]) -> isize,
) -> RtResult<Vec<u8>> {
    let l = s.len();
    if l > max_len.saturating_sub(3) {
        return Err(RuntimeError::ValueError(format!(
            "Argument exceeds the allowed length of {max_len} bytes"
        )));
    }
    let mut out = Vec::with_capacity(l + 2);
    out.push(b'\'');
    let mut x = 0;
    while x < l {
        let mb_len = mblen(&s[x..]);
        if mb_len < 0 {
            x += 1;
            continue;
        }
        if mb_len > 1 {
            #[allow(clippy::cast_sign_loss)]
            let n = (mb_len as usize).min(l - x);
            out.extend_from_slice(&s[x..x + n]);
            x += n;
            continue;
        }
        if s[x] == b'\'' {
            out.extend_from_slice(b"'\\'");
        }
        out.push(s[x]);
        x += 1;
    }
    out.push(b'\'');
    if out.len() > max_len + 1 {
        return Err(RuntimeError::ValueError(format!(
            "Escaped argument exceeds the allowed length of {max_len} bytes"
        )));
    }
    Ok(out)
}

/// C `isspace()` in the "C" locale.
fn c_isspace(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `strip_trailing_whitespace()`.
fn strip_trailing_whitespace(line: &[u8]) -> &[u8] {
    let end = line
        .iter()
        .rposition(|&c| !c_isspace(c))
        .map_or(0, |i| i + 1);
    &line[..end]
}

/// How [`php_exec`] treats the command output (`type` of `php_exec()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecMode {
    /// `exec()` without `$output`: only the last line is returned.
    Exec,
    /// `system()`: every line is written to the output, the last returned.
    System,
    /// `exec()` with `$output`: lines (trailing whitespace stripped) are
    /// appended to the array.
    ExecArray,
    /// `passthru()`: raw output is written, nothing returned.
    Passthru,
}

/// The result of [`php_exec`].
#[derive(Debug)]
pub struct ExecResult {
    /// The return value (`false` when the shell could not be started).
    pub value: Value,
    /// The exit status for `$result_code` (-1 on fork failure).
    pub status: i64,
}

fn spawn_shell(cmd: &[u8]) -> std::io::Result<std::process::Child> {
    use std::os::unix::ffi::OsStrExt;
    Command::new("/bin/sh")
        .arg("-c")
        .arg(std::ffi::OsStr::from_bytes(cmd))
        .stdout(Stdio::piped())
        .spawn()
}

/// `pclose()` result processing of `php_stdiop_close()`: the exit code if
/// the child exited normally, else the raw wait status.
fn pclose_status(status: std::process::ExitStatus) -> i64 {
    match status.code() {
        Some(c) => i64::from(c),
        None => i64::from(status.into_raw()),
    }
}

/// `php_exec()`: runs `cmd` through `/bin/sh -c`. `output` receives what
/// `system()`/`passthru()` print (PHPWRITE + flush); `array` is the
/// separated `$output` array for [`ExecMode::ExecArray`].
pub fn php_exec(
    mode: ExecMode,
    cmd: &[u8],
    array: Option<&mut ZArray>,
    output: &mut dyn FnMut(&[u8]),
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<ExecResult> {
    let Ok(mut child) = spawn_shell(cmd) else {
        ctx.diagnostic(
            ErrorLevel::Warning,
            &format!(
                "{}(): Unable to fork [{}]",
                mode_func(mode),
                String::from_utf8_lossy(cmd)
            ),
        )?;
        return Ok(ExecResult {
            value: Value::Bool(false),
            status: -1,
        });
    };
    let mut data = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        if mode == ExecMode::Passthru {
            let mut buf = [0u8; 4096];
            loop {
                match out.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => output(&buf[..n]),
                }
            }
        } else {
            let _ = out.read_to_end(&mut data);
        }
    }
    let status = child.wait().map_or(-1, pclose_status);
    if mode == ExecMode::Passthru {
        return Ok(ExecResult {
            value: Value::Null,
            status,
        });
    }
    let mut array = array;
    let mut last: &[u8] = &[];
    let mut rest = &data[..];
    while !rest.is_empty() {
        let end = rest
            .iter()
            .position(|&c| c == b'\n')
            .map_or(rest.len(), |i| i + 1);
        let line = &rest[..end];
        rest = &rest[end..];
        match mode {
            ExecMode::System => output(line),
            ExecMode::ExecArray => {
                if let Some(a) = array.as_deref_mut() {
                    let _ = a
                        .make_mut()
                        .push(Value::Str(ZStr::new(strip_trailing_whitespace(line))));
                }
            }
            _ => {}
        }
        last = line;
    }
    Ok(ExecResult {
        value: Value::Str(ZStr::new(strip_trailing_whitespace(last))),
        status,
    })
}

fn mode_func(mode: ExecMode) -> &'static str {
    match mode {
        ExecMode::Exec | ExecMode::ExecArray => "exec",
        ExecMode::System => "system",
        ExecMode::Passthru => "passthru",
    }
}

fn check_command(func: &str, cmd: &[u8]) -> RtResult<()> {
    if cmd.contains(&0) {
        return Err(RuntimeError::ValueError(format!(
            "{func}(): Argument #1 ($command) must not contain any null bytes"
        )));
    }
    if cmd.is_empty() {
        return Err(RuntimeError::ValueError(format!(
            "{func}(): Argument #1 ($command) must not be empty"
        )));
    }
    Ok(())
}

/// `exec($command, &$output, &$result_code)`: `output` is the current
/// value of `$output` when the argument was passed (it is reused if it is
/// an array, else replaced by a new array). Returns the return value, the
/// new `$output` (if passed) and the status for `$result_code`.
pub fn exec(
    cmd: &[u8],
    output: Option<Value>,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<(Value, Option<ZArray>, i64)> {
    check_command("exec", cmd)?;
    let mut sink = |_: &[u8]| {};
    match output {
        None => {
            let r = php_exec(ExecMode::Exec, cmd, None, &mut sink, ctx)?;
            Ok((r.value, None, r.status))
        }
        Some(v) => {
            let mut arr = match v.deref() {
                Value::Array(a) => a,
                _ => ZArray::new(),
            };
            let r = php_exec(ExecMode::ExecArray, cmd, Some(&mut arr), &mut sink, ctx)?;
            Ok((r.value, Some(arr), r.status))
        }
    }
}

/// `system($command, &$result_code)`.
pub fn system(
    cmd: &[u8],
    output: &mut dyn FnMut(&[u8]),
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<ExecResult> {
    check_command("system", cmd)?;
    php_exec(ExecMode::System, cmd, None, output, ctx)
}

/// `passthru($command, &$result_code)`.
pub fn passthru(
    cmd: &[u8],
    output: &mut dyn FnMut(&[u8]),
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<ExecResult> {
    check_command("passthru", cmd)?;
    php_exec(ExecMode::Passthru, cmd, None, output, ctx)
}

/// `shell_exec($command)` (and the backtick operator): the complete
/// output, `null` when it is empty, `false` if the shell cannot start.
pub fn shell_exec(cmd: &[u8], ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<Value> {
    check_command("shell_exec", cmd)?;
    let Ok(mut child) = spawn_shell(cmd) else {
        ctx.diagnostic(
            ErrorLevel::Warning,
            &format!(
                "shell_exec(): Unable to execute '{}'",
                String::from_utf8_lossy(cmd)
            ),
        )?;
        return Ok(Value::Bool(false));
    };
    let mut data = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_end(&mut data);
    }
    let _ = child.wait();
    Ok(if data.is_empty() {
        Value::Null
    } else {
        Value::Str(ZStr::new(data))
    })
}

/// `proc_nice($priority)`.
pub fn proc_nice(priority: i64, ctx: &mut (impl RuntimeContext + ?Sized)) -> RtResult<bool> {
    #[allow(clippy::cast_possible_truncation)]
    let inc = priority as i32;
    if sys::nice(inc).is_err() {
        ctx.diagnostic(
            ErrorLevel::Warning,
            "proc_nice(): Only a super user may attempt to increase the priority of a process",
        )?;
        return Ok(false);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf8_mblen(s: &[u8]) -> isize {
        match s[0] {
            0 => 0,
            0x01..=0x7f => 1,
            0xc2..=0xdf if s.len() >= 2 && s[1] & 0xc0 == 0x80 => 2,
            0xe0..=0xef if s.len() >= 3 && s[1] & 0xc0 == 0x80 && s[2] & 0xc0 == 0x80 => 3,
            _ => -1,
        }
    }

    #[test]
    fn escape_arg() {
        let e = |s: &[u8]| escape_arg_with(s, 4096, utf8_mblen).unwrap();
        assert_eq!(e(b"a'b"), b"'a'\\''b'");
        assert_eq!(e(b""), b"''");
        assert_eq!(e(b"a\x80b\xc3\xa9c'"), b"'ab\xc3\xa9c'\\'''");
    }

    #[test]
    fn escape_cmd() {
        let e = |s: &[u8]| escape_cmd_with(s, 4096, utf8_mblen).unwrap();
        assert_eq!(e(b"echo 'a' \"b"), b"echo 'a' \\\"b");
        assert_eq!(e(b"a;b|c"), b"a\\;b\\|c");
        assert_eq!(e(b"'a\"b'c'"), b"'a\\\"b'c\\'");
    }

    #[test]
    fn whitespace() {
        assert_eq!(strip_trailing_whitespace(b"a \t\x0b\n"), b"a");
        assert_eq!(strip_trailing_whitespace(b" \n"), b"");
    }
}
