// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The access log (ref: `sapi/fpm/fpm/fpm_log.c`): the master opens each
//! pool's `access.log` (the workers inherit the descriptor), checks its
//! `access.format` at start, and each worker writes one line per request
//! through an access-log stream (cut at `log_limit`), unless the request
//! matches `access.suppress_path`.

use super::conf::Pool;
use super::log::{self, Stream};
use super::os;
use super::scoreboard::Proc;
use std::os::fd::RawFd;

/// `fpm_log_open()`'s `open()`.
fn open_file(path: &str) -> Option<RawFd> {
    use std::os::fd::IntoRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
    {
        Ok(f) => {
            log::debug("fpm_log_open", 0, &format!("open access log ({path})"));
            Some(f.into_raw_fd())
        }
        Err(e) => {
            log::syserror(
                "fpm_log_open",
                0,
                &format!("failed to open access log ({path})"),
                &e,
            );
            None
        }
    }
}

/// `fpm_log_open(0)`: each pool's access log descriptor; `None` when one
/// fails to open.
pub fn open(pools: &[Pool]) -> Option<Vec<Option<RawFd>>> {
    let mut fds = Vec::new();
    for p in pools {
        match p.access_log.as_deref() {
            None => fds.push(None),
            Some(path) => fds.push(Some(open_file(path)?)),
        }
    }
    Some(fds)
}

/// `fpm_log_open(1)` (SIGUSR1): the files opened again on the same
/// descriptors; `1` when no pool has an access log, `-1` on a failure.
pub fn reopen(pools: &[Pool], fds: &[Option<RawFd>]) -> i32 {
    let mut ret = 1;
    for (p, fd) in pools.iter().zip(fds) {
        let (Some(path), Some(fd)) = (p.access_log.as_deref(), fd) else {
            continue;
        };
        ret = 0;
        let Some(new) = open_file(path) else {
            return -1;
        };
        os::dup2(new, *fd);
        os::close(new);
    }
    ret
}

/// What a request's line shows.
pub struct Entry<'a> {
    /// The pool's name (`%n`).
    pub pool: &'a str,
    /// The worker's slot after the request.
    pub proc: &'a Proc,
    /// The request's `FastCGI` environment (`%{NAME}e`).
    pub env: &'a [(Vec<u8>, Vec<u8>)],
    /// The response's header lines (`%{Name}o`).
    pub headers: &'a [Vec<u8>],
    /// `SG(sapi_headers).http_response_code` (`%s`).
    pub status: i32,
    /// `fcgi_get_last_client_ip()` (`%R`).
    pub client_ip: Option<&'a str>,
    /// `SG(request_info).content_length`.
    pub content_length: i64,
}

/// `fpm_access_log_suppress()`.
fn suppressed(e: &Entry<'_>, paths: &[String]) -> bool {
    let p = e.proc;
    if !p.query.is_empty() || !(p.method == "GET" || p.method == "HEAD") {
        return false;
    }
    if !(200..=299).contains(&e.status) || e.content_length > 0 {
        return false;
    }
    paths.contains(&p.uri)
}

fn dash(s: &str) -> &str {
    if s.is_empty() {
        "-"
    } else {
        s
    }
}

#[allow(clippy::cast_precision_loss)]
fn secs(us: u64) -> f64 {
    us as f64 / 1_000_000.0
}

/// `fpm_log_write()`: one line for `entry` on `stream`, or (`entry`
/// `None`) only a check of `format`; `false` for a format error (logged
/// as a warning) or a suppressed request.
#[allow(clippy::too_many_lines)]
pub fn write(
    format: &str,
    stream: &mut Stream,
    entry: Option<&Entry<'_>>,
    suppress: &[String],
) -> bool {
    const F: &str = "fpm_log_write";
    let test = entry.is_none();
    if let Some(e) = entry {
        if suppressed(e, suppress) {
            return false;
        }
    }
    let spec = format.as_bytes();
    let mut pos = 0;
    let mut token = false;
    let mut fmt = String::new();
    let now = os::now().0;
    stream.start();
    let over = |stream: &Stream| {
        if stream.is_over_limit() {
            log::notice(
                F,
                0,
                "the log buffer is over the configured limit. The access log request has been truncated.",
            );
            true
        } else {
            false
        }
    };
    while pos < spec.len() {
        let byte = spec[pos];
        if !token && byte == b'%' {
            token = true;
            fmt.clear();
            pos += 1;
            continue;
        }
        if token {
            token = false;
            let ch = char::from(byte);
            let out = |stream: &mut Stream, v: &str| {
                if !test {
                    stream.str(v.as_bytes());
                }
            };
            match byte {
                b'%' => stream.char(b'%'),
                b'C' => {
                    let modifier = fmt.to_ascii_lowercase();
                    if !(modifier.is_empty()
                        || modifier == "total"
                        || modifier == "user"
                        || modifier == "system")
                    {
                        log::warning(F, 0, &format!("only 'total', 'user' or 'system' are allowed as a modifier for %{ch} ('{fmt}')"));
                        return false;
                    }
                    fmt.clear();
                    if let Some(e) = entry {
                        let p = e.proc;
                        #[allow(clippy::cast_precision_loss)]
                        let v = if p.duration_us == 0 {
                            f64::NAN
                        } else {
                            p.cpu_us as f64 / p.duration_us as f64 * 100.0
                        };
                        out(stream, &format!("{v:.2}"));
                    }
                }
                b'd' => {
                    let modifier = fmt.to_ascii_lowercase();
                    let v = entry.map_or(0, |e| e.proc.duration_us);
                    let shown = match modifier.as_str() {
                        "" | "seconds" => format!("{:.3}", secs(v)),
                        "milliseconds" | "milli" | "miliseconds" | "mili" => {
                            #[allow(clippy::cast_precision_loss)]
                            let ms = v as f64 / 1000.0;
                            format!("{ms:.3}")
                        }
                        "microseconds" | "micro" => v.to_string(),
                        _ => {
                            log::warning(F, 0, &format!("only 'seconds', 'milli', 'milliseconds', 'micro' or 'microseconds' are allowed as a modifier for %{ch} ('{fmt}')"));
                            return false;
                        }
                    };
                    out(stream, &shown);
                    fmt.clear();
                }
                b'e' => {
                    if fmt.is_empty() {
                        log::warning(F, 0, &format!("the name of the environment variable must be set between embraces for %{ch}"));
                        return false;
                    }
                    if let Some(e) = entry {
                        let v = e
                            .env
                            .iter()
                            .find(|(k, _)| k == fmt.as_bytes())
                            .map(|(_, v)| String::from_utf8_lossy(v).into_owned());
                        out(stream, v.as_deref().unwrap_or("-"));
                    }
                    fmt.clear();
                }
                b'f' => {
                    if let Some(e) = entry {
                        out(stream, dash(&e.proc.script));
                    }
                }
                b'l' => {
                    if let Some(e) = entry {
                        out(stream, &e.proc.content_length.to_string());
                    }
                }
                b'm' => {
                    if let Some(e) = entry {
                        out(stream, dash(&e.proc.method));
                    }
                }
                b'M' => {
                    let modifier = fmt.to_ascii_lowercase();
                    let m = entry.map_or(0, |e| e.proc.memory);
                    let v = match modifier.as_str() {
                        "" | "bytes" => m,
                        "kilobytes" | "kilo" => m / 1024,
                        "megabytes" | "mega" => m / 1024 / 1024,
                        _ => {
                            log::warning(F, 0, &format!("only 'bytes', 'kilo', 'kilobytes', 'mega' or 'megabytes' are allowed as a modifier for %{ch} ('{fmt}')"));
                            return false;
                        }
                    };
                    out(stream, &v.to_string());
                    fmt.clear();
                }
                b'n' => {
                    if let Some(e) = entry {
                        out(stream, dash(e.pool));
                    }
                }
                b'o' => {
                    if fmt.is_empty() {
                        log::warning(
                            F,
                            0,
                            &format!(
                                "the name of the header must be set between embraces for %{ch}"
                            ),
                        );
                        return false;
                    }
                    if let Some(e) = entry {
                        let name = fmt.as_bytes();
                        let found = e.headers.iter().find_map(|h| {
                            (!h.is_empty()
                                && h.windows(name.len()).any(|w| w == name)
                                && h.len() > name.len() + 2
                                && h.get(name.len()) == Some(&b':')
                                && h.get(name.len() + 1) == Some(&b' '))
                            .then(|| &h[name.len() + 2..])
                        });
                        match found {
                            Some(v) if !v.is_empty() => out(stream, &String::from_utf8_lossy(v)),
                            Some(_) => out(stream, "-"),
                            None => stream.char(b'-'),
                        }
                    }
                    fmt.clear();
                }
                b'p' => {
                    if !test {
                        out(stream, &os::getpid().to_string());
                    }
                }
                b'P' => {
                    if !test {
                        out(stream, &std::os::unix::process::parent_id().to_string());
                    }
                }
                b'q' => {
                    if let Some(e) = entry {
                        out(stream, &e.proc.query);
                    }
                }
                b'Q' => {
                    if let Some(e) = entry {
                        out(stream, if e.proc.query.is_empty() { "" } else { "?" });
                    }
                }
                b'r' => {
                    if let Some(e) = entry {
                        out(stream, &e.proc.uri);
                    }
                }
                b'R' => {
                    if let Some(e) = entry {
                        out(stream, e.client_ip.unwrap_or("-"));
                    }
                }
                b's' => {
                    if let Some(e) = entry {
                        out(stream, &e.status.to_string());
                    }
                }
                b't' | b'T' => {
                    if let Some(e) = entry {
                        let t = if byte == b't' {
                            e.proc.accepted_epoch
                        } else {
                            now
                        };
                        let pattern = if fmt.is_empty() {
                            "%d/%b/%Y:%H:%M:%S %z"
                        } else {
                            fmt.as_str()
                        };
                        out(stream, &os::strftime(pattern, t));
                    }
                    fmt.clear();
                }
                b'u' => {
                    if let Some(e) = entry {
                        out(stream, &e.proc.user);
                    }
                }
                b'{' => {
                    token = true;
                    let start = pos + 1;
                    pos = start;
                    while pos < spec.len() {
                        if spec[pos] == b'}' {
                            let l = (pos - start).min(128);
                            fmt = String::from_utf8_lossy(&spec[start..start + l]).into_owned();
                            break;
                        }
                        pos += 1;
                    }
                    if pos + 1 >= spec.len() {
                        log::warning(F, 0, "missing closing embrace in the access.format");
                        return false;
                    }
                }
                _ => {
                    log::warning(F, 0, &format!("Invalid token in the access.format (%{ch})"));
                    return false;
                }
            }
            if spec.get(pos) != Some(&b'}') && !fmt.is_empty() {
                log::warning(F, 0, &format!("embrace is not allowed for modifier %{ch}"));
                return false;
            }
            pos += 1;
            if over(stream) {
                break;
            }
            continue;
        }
        if over(stream) {
            break;
        }
        if !test {
            stream.char(byte);
        }
        pos += 1;
    }
    if !test {
        stream.finish();
    }
    true
}

/// `fpm_conf_post_process()`'s check of each pool's `access.format`.
pub fn check_formats(pools: &[Pool]) -> bool {
    for p in pools {
        if p.access_log.as_deref().is_none_or(str::is_empty) {
            continue;
        }
        let format = p.access_format.as_deref().unwrap_or_default();
        let mut s = Stream::access(-1);
        if !write(format, &mut s, None, &[]) {
            log::error(
                "fpm_conf_post_process",
                0,
                &format!(
                    "[pool {}] wrong format for access.format '{format}'",
                    p.name
                ),
            );
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_errors() {
        let mut s = Stream::access(-1);
        assert!(write("%m %r %s", &mut s, None, &[]));
        assert!(write("%{REMOTE_ADDR}e %{%H}t", &mut s, None, &[]));
        assert!(!write("%e", &mut s, None, &[]));
        assert!(!write("%{x}m", &mut s, None, &[]));
        assert!(!write("%Z", &mut s, None, &[]));
        assert!(!write("%{bogus}d", &mut s, None, &[]));
    }
}
