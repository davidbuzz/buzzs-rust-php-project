// Derived from php-src ext/standard/ftp_fopen_wrapper.c. Similarity metric to original: 17%
//   Copyright © The PHP Group and Contributors.
//   Authors: Rasmus Lerdorf <rasmus@php.net>
//            Jim Winstead <jimw@php.net>
//            Hartmut Holzgraefe <hholzgra@php.net>
//            Sara Golemon <pollita@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The `ftp://` / `ftps://` stream wrapper
//! (`ext/standard/ftp_fopen_wrapper.c`).
//!
//! [`FtpWrapper`] talks to the server over its own control connection
//! (it does not use ext/ftp): login (`USER`/`PASS`, `AUTH TLS`/`AUTH SSL`,
//! `PBSZ`/`PROT` for `ftps://`), passive mode (`EPSV`, then `PASV`), and
//!
//! * `fopen()` — `RETR` (with the `resume_pos` context option), `STOR`
//!   (refused when the file exists unless `overwrite` is set) or `APPE`;
//!   the returned stream is the data connection, and closing it reads the
//!   transfer result from the control connection and sends `QUIT`;
//! * `opendir()` — `NLST` read line by line (`ftpdir` streams);
//! * `url_stat` — `CWD` (directory or file), `SIZE`, `MDTM`;
//! * `unlink` (`DELE`), `rename` (`RNFR`/`RNTO`), `mkdir` (`MKD`, recursive
//!   by walking up with `CWD`) and `rmdir` (`RMD`).
//!
//! Connections and TLS go through the [`crate::http::HttpEnv`] services
//! installed by the VM (the registered `tcp` transport and the openssl
//! crypto hook).

use std::time::Duration;

use crate::context::{self, notify, ContextRef};
use crate::diag::{self, Severity, StreamErrorCode};
use crate::http::{self, Uri, UriParser};
use crate::stat::StatBuf;
use crate::stream::{OpFailed, Stream, StreamOps, StreamState};
use crate::value::Value;
use crate::wrapper::{
    wrapper_warn, OpenError, Wrapper, DEFAULT_WRAPPER_NAME, MKDIR_RECURSIVE, REPORT_ERRORS,
};

/// The wrapper label (`wrapper_type` in the meta data).
pub const LABEL: &str = "ftp";

/// `php_stream_gets(stream, tmp_line, sizeof(tmp_line) - 1)` with the
/// 512-byte reply buffer: at most 510 bytes per line.
const REPLY_LINE_MAX: usize = 511;

/// `sizeof(php_stream_dirent.d_name)` (`NAME_MAX + 1`).
const DIRENT_NAME_SIZE: usize = 256;

/// `sizeof("123.123.123.123")`: the `PASV` host buffer.
const PASV_IP_SIZE: usize = 16;

/// `S_IFREG`, `S_IFDIR` and the execute bits `url_stat` reports.
const S_IFREG: i64 = 0o100_000;
const S_IFDIR: i64 = 0o040_000;
const S_IXALL: i64 = 0o111;

/// The bytes of `s` up to the first NUL (a C string view).
fn cstr(s: &[u8]) -> &[u8] {
    s.iter().position(|&c| c == 0).map_or(s, |i| &s[..i])
}

/// Whether `code` is a 2xx reply.
fn ok2xx(code: i64) -> bool {
    (200..=299).contains(&code)
}

/// `php_stream_write_string()` on the control connection.
fn send(stream: &mut Stream, data: &[u8]) {
    let _ = stream.write(data);
}

/// Sends `"<cmd> <arg>\r\n"`.
fn send_cmd(stream: &mut Stream, cmd: &[u8], arg: &[u8]) {
    let mut buf = Vec::with_capacity(cmd.len() + arg.len() + 3);
    buf.extend_from_slice(cmd);
    buf.push(b' ');
    buf.extend_from_slice(arg);
    buf.extend_from_slice(b"\r\n");
    send(stream, &buf);
}

/// Whether a reply line is the last one of a reply (`"NNN "`).
fn is_final_line(line: &[u8]) -> bool {
    line.len() >= 4 && line[..3].iter().all(u8::is_ascii_digit) && line[3] == b' '
}

/// `get_ftp_result()`: reads reply lines until one starts with three
/// digits and a space and returns its code. `line` holds the last line
/// read (empty when nothing could be read; a failed read keeps the
/// previous line, as the C buffer does).
fn get_result(stream: &mut Stream, line: &mut Vec<u8>) -> i64 {
    line.clear();
    while let Some(l) = stream.get_line(Some(REPLY_LINE_MAX)) {
        *line = l;
        if is_final_line(line) {
            break;
        }
    }
    php_support::strtol(cstr(line)).0
}

/// The connect timeout of `php_stream_xport_create(..., NULL timeout)`:
/// `default_socket_timeout`.
fn default_timeout() -> Option<Duration> {
    let secs = crate::state::settings().default_socket_timeout;
    u64::try_from(secs).ok().map(Duration::from_secs)
}

/// The state of one wrapper operation: where `php_stream_wrapper_log_warn`
/// messages go.
struct Session<'a> {
    options: u32,
    context: Option<&'a ContextRef>,
    log: Vec<Vec<u8>>,
    exception: bool,
}

impl<'a> Session<'a> {
    fn new(options: u32, context: Option<&'a ContextRef>) -> Self {
        Session {
            options,
            context,
            log: Vec::new(),
            exception: false,
        }
    }

    /// `php_stream_wrapper_log_warn()`.
    fn log(&mut self, code: StreamErrorCode, msg: impl Into<Vec<u8>>) {
        let msg = msg.into();
        if self.options & REPORT_ERRORS == 0 {
            self.log.push(msg);
        } else {
            diag::stream_error(Severity::Warning, LABEL, code, true, msg);
        }
    }

    /// `php_stream_context_get_option(context, "ftp", name)`.
    fn opt(&self, name: &str) -> Option<Value> {
        self.context
            .and_then(|c| c.borrow().get(LABEL, name).cloned())
    }

    fn into_error(self) -> OpenError {
        if self.exception {
            OpenError::exception()
        } else {
            OpenError {
                messages: self.log,
                ..OpenError::default()
            }
        }
    }
}

/// A logged-in control connection (`php_ftp_fopen_connect()` result).
struct Connection {
    stream: Stream,
    resource: Uri,
    use_ssl_on_data: bool,
}

impl Connection {
    /// `resource->path` (never absent on a connection).
    fn path(&self) -> Vec<u8> {
        self.resource.path.clone().unwrap_or_default()
    }

    /// The host to open data connections to when `PASV` gave none.
    fn host(&self) -> Vec<u8> {
        cstr(self.resource.host.as_deref().unwrap_or_default()).to_vec()
    }
}

/// `tcp://host:port`.
fn tcp_transport(host: &[u8], port: i64) -> Vec<u8> {
    let mut t = b"tcp://".to_vec();
    t.extend_from_slice(host);
    t.push(b':');
    t.extend_from_slice(port.to_string().as_bytes());
    t
}

/// Opens a client connection the way `php_stream_xport_create()` does
/// with `REPORT_ERRORS` and no error-string pointer: a failure warns
/// "connect() failed: ...".
fn connect_reporting(
    transport: &[u8],
    context: Option<&ContextRef>,
    sess: &mut Session<'_>,
) -> Option<Stream> {
    let env = http::env();
    match env.connect(transport, default_timeout(), context) {
        Ok(s) => Some(s),
        Err(msg) => {
            if env.exception_pending() {
                sess.exception = true;
            } else {
                let text = msg.unwrap_or_else(|| b"Unspecified error".to_vec());
                diag::stream_error(
                    Severity::Warning,
                    DEFAULT_WRAPPER_NAME,
                    StreamErrorCode::ConnectFailed,
                    true,
                    diag::msg(&[b"connect() failed: ", &text]),
                );
            }
            None
        }
    }
}

/// Rejects credentials with control characters (`PHP_FTP_CNTRL_CHK`).
fn has_control(s: &[u8]) -> bool {
    s.iter().any(u8::is_ascii_control)
}

/// `php_ftp_fopen_connect()`: connects, negotiates TLS for `ftps://` and
/// logs in. `None` on failure (the reasons are logged in `sess`).
#[allow(clippy::too_many_lines)] // a direct port of the C function
fn connect(sess: &mut Session<'_>, path: &[u8]) -> Option<Connection> {
    let Some(parser) = UriParser::from_context(LABEL, sess.context) else {
        http::env().invalid_uri_parser();
        sess.exception = true;
        return None;
    };
    let mut resource = parser.parse(cstr(path))?;
    resource.path.as_ref()?;
    // The whole scheme, in any case (GHSA-f6gg-rchx-xw8j, CVE-2026-128064).
    let use_ssl = resource
        .scheme
        .as_ref()
        .is_some_and(|s| s.eq_ignore_ascii_case(b"ftps"));
    if resource.port == 0 {
        resource.port = 21;
    }
    let host = cstr(resource.host.as_deref().unwrap_or_default()).to_vec();
    let context = sess.context;
    let mut stream = connect_reporting(&tcp_transport(&host, resource.port), context, sess)?;
    stream.context = context.cloned();
    context::notify_info(context, notify::CONNECT, None, 0);

    let mut line = Vec::new();
    let mut result = get_result(&mut stream, &mut line);
    if !ok2xx(result) {
        context::notify_error(context, notify::FAILURE, Some(cstr(&line)), result);
        return None;
    }

    let mut use_ssl_on_data = false;
    if use_ssl {
        let mut reuseid = false;
        send(&mut stream, b"AUTH TLS\r\n");
        result = get_result(&mut stream, &mut line);
        if result != 234 {
            // AUTH TLS not supported: try AUTH SSL (an old ftpd-ssl).
            send(&mut stream, b"AUTH SSL\r\n");
            result = get_result(&mut stream, &mut line);
            if result != 334 {
                sess.log(
                    StreamErrorCode::SslNotSupported,
                    "Server doesn't support FTPS.",
                );
                return None;
            }
            reuseid = true;
        }
        let env = http::env();
        stream = if let Some(s) = env.enable_crypto(stream, None) {
            s
        } else {
            if env.exception_pending() {
                sess.exception = true;
            }
            sess.log(
                StreamErrorCode::SslNotSupported,
                "Unable to activate SSL mode",
            );
            return None;
        };
        send(&mut stream, b"PBSZ 0\r\n");
        let _ = get_result(&mut stream, &mut line);
        send(&mut stream, b"PROT P\r\n");
        result = get_result(&mut stream, &mut line);
        use_ssl_on_data = ok2xx(result) || reuseid;
    }

    if let Some(user) = resource.user.take() {
        let user = php_stdstr::url::rawurldecode(&user);
        if has_control(&user) {
            sess.log(
                StreamErrorCode::AuthFailed,
                diag::msg(&[b"Invalid login ", cstr(&user)]),
            );
            return None;
        }
        send_cmd(&mut stream, b"USER", &user);
    } else {
        send(&mut stream, b"USER anonymous\r\n");
    }
    result = get_result(&mut stream, &mut line);

    if (300..=399).contains(&result) {
        context::notify_info(context, notify::AUTH_REQUIRED, Some(cstr(&line)), 0);
        if let Some(password) = resource.password.take() {
            let password = php_stdstr::url::rawurldecode(&password);
            if has_control(&password) {
                sess.log(
                    StreamErrorCode::AuthFailed,
                    diag::msg(&[b"Invalid password ", cstr(&password)]),
                );
                return None;
            }
            send_cmd(&mut stream, b"PASS", &password);
        } else if let Some(from) = crate::state::settings().from_address {
            send_cmd(&mut stream, b"PASS", &from);
        } else {
            send(&mut stream, b"PASS anonymous\r\n");
        }
        result = get_result(&mut stream, &mut line);
        if ok2xx(result) {
            context::notify_info(context, notify::AUTH_RESULT, Some(cstr(&line)), result);
        } else {
            context::notify_error(context, notify::AUTH_RESULT, Some(cstr(&line)), result);
        }
    }
    if !ok2xx(result) {
        return None;
    }
    Some(Connection {
        stream,
        resource,
        use_ssl_on_data,
    })
}

/// Parses a `227` reply (`227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)`):
/// the port (0 when malformed) and the host.
fn parse_pasv(line: &[u8]) -> (u16, Option<Vec<u8>>) {
    let l = cstr(line);
    let mut i = 4.min(l.len());
    while i < l.len() && !l[i].is_ascii_digit() {
        i += 1;
    }
    if i >= l.len() {
        return (0, None);
    }
    let hoststart = i;
    for _ in 0..4 {
        while i < l.len() && l[i].is_ascii_digit() {
            i += 1;
        }
        if l.get(i) != Some(&b',') {
            return (0, None);
        }
        i += 1;
    }
    let mut ip: Vec<u8> = l[hoststart..i - 1]
        .iter()
        .map(|&c| if c == b',' { b'.' } else { c })
        .collect();
    ip.truncate(PASV_IP_SIZE - 1);
    // `(unsigned short) strtoul(...)`: wrap like the C casts.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let to_u16 = |v: i64| v as u16;
    let (msb, n) = php_support::strtol(&l[i..]);
    let mut portno = to_u16(msb).wrapping_mul(256);
    i += n;
    if l.get(i) != Some(&b',') {
        return (0, None);
    }
    i += 1;
    let (lsb, _) = php_support::strtol(&l[i..]);
    portno = portno.wrapping_add(to_u16(lsb));
    (portno, Some(ip))
}

/// Parses a `229` reply (`229 Entering Extended Passive Mode (|||port|)`):
/// the port, 0 when malformed.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // `(unsigned short) strtoul()`
fn parse_epsv(line: &[u8]) -> u16 {
    let l = cstr(line);
    let mut bars = 0;
    let mut i = 4.min(l.len());
    while i < l.len() {
        if l[i] == b'|' {
            bars += 1;
            if bars == 3 {
                break;
            }
        }
        i += 1;
    }
    if bars < 3 {
        return 0;
    }
    php_support::strtol(&l[i + 1..]).0 as u16
}

/// `php_fopen_do_pasv()`: `EPSV`, falling back to `PASV`. Returns the data
/// port (0 on failure) and the host a `PASV` reply named.
fn do_pasv(stream: &mut Stream) -> (u16, Option<Vec<u8>>) {
    let mut line = Vec::new();
    send(stream, b"EPSV\r\n");
    if get_result(stream, &mut line) == 229 {
        return (parse_epsv(&line), None);
    }
    send(stream, b"PASV\r\n");
    if get_result(stream, &mut line) != 227 {
        return (0, None);
    }
    parse_pasv(&line)
}

/// `php_stream_ftp_stream_close()`: after the data connection closed,
/// reads the transfer result (for streams whose mode has `w`, `a` or `+`,
/// which includes every socket stream) and sends `QUIT`.
fn close_transfer(mut control: Stream, check_result: bool) {
    if check_result {
        let mut line = Vec::new();
        let result = get_result(&mut control, &mut line);
        if result != 226 && result != 250 {
            wrapper_warn(
                LABEL,
                REPORT_ERRORS,
                StreamErrorCode::ProtocolError,
                diag::msg(&[
                    b"FTP server error ",
                    result.to_string().as_bytes(),
                    b":",
                    cstr(&line),
                ]),
            );
        }
    }
    send(&mut control, b"QUIT\r\n");
    control.close();
}

/// The `errexit` path of the openers: notifies the failure, closes the
/// control connection and logs the server's last reply and the data
/// channel error.
fn fail(
    mut sess: Session<'_>,
    stream: Option<Stream>,
    line: &[u8],
    result: i64,
    data_error: Option<Vec<u8>>,
) -> OpenError {
    if let Some(mut s) = stream {
        context::notify_error(sess.context, notify::FAILURE, Some(cstr(line)), result);
        s.close();
    }
    if line.first().is_some_and(|&c| c != 0) {
        sess.log(
            StreamErrorCode::ProtocolError,
            diag::msg(&[b"FTP server reports ", cstr(line)]),
        );
    }
    if let Some(e) = data_error {
        sess.log(
            StreamErrorCode::NetworkSendFailed,
            diag::msg(&[b"Failed to set up data channel: ", &e]),
        );
    }
    sess.into_error()
}

/// `php_stream_url_wrap_ftp()`.
///
/// # Errors
/// The logged messages, joined into "Failed to open stream: ..." by the
/// caller.
#[allow(clippy::too_many_lines)] // a direct port of the C function
pub fn open_url(
    path: &[u8],
    mode: &str,
    options: u32,
    context: Option<&ContextRef>,
) -> Result<Stream, OpenError> {
    let mut sess = Session::new(options, context);
    let mut read_write = 0u8;
    if mode.contains(['r', '+']) {
        read_write = 1;
    }
    if mode.contains(['w', 'a', '+']) {
        if read_write != 0 {
            sess.log(
                StreamErrorCode::ModeNotSupported,
                "FTP does not support simultaneous read/write connections",
            );
            return Err(sess.into_error());
        }
        read_write = if mode.contains('a') { 3 } else { 2 };
    }
    if read_write == 0 {
        sess.log(StreamErrorCode::InvalidMode, "Unknown file open mode");
        return Err(sess.into_error());
    }
    if sess.opt("proxy").is_some() {
        if read_write == 1 {
            // The HTTP wrapper proxies the request.
            return http::open_url_as(LABEL, path, mode, options, context);
        }
        sess.log(
            StreamErrorCode::ModeNotSupported,
            "FTP proxy may only be used in read mode",
        );
        return Err(sess.into_error());
    }

    let Some(conn) = connect(&mut sess, path) else {
        return Err(sess.into_error());
    };
    let rpath = conn.path();
    let default_host = conn.host();
    let use_ssl_on_data = conn.use_ssl_on_data;
    let mut stream = conn.stream;
    let mut line = Vec::new();
    let mut file_size = 0u64;

    send(&mut stream, b"TYPE I\r\n");
    let mut result = get_result(&mut stream, &mut line);
    if !ok2xx(result) {
        return Err(fail(sess, Some(stream), &line, result, None));
    }

    // The size of the file (verifying it exists).
    send_cmd(&mut stream, b"SIZE", &rpath);
    result = get_result(&mut stream, &mut line);
    if read_write == 1 {
        // When reading, the file must exist.
        if !ok2xx(result) {
            return Err(fail(sess, Some(stream), &line, result, None));
        }
        let l = cstr(&line);
        if let Some(sp) = l.iter().position(|&c| c == b' ') {
            // `size_t file_size = atoi(...)`.
            file_size =
                u64::from_ne_bytes(i64::from(php_support::atoi(&l[sp + 1..])).to_ne_bytes());
            context::notify_file_size(context, file_size, l, result);
        }
    } else if read_write == 2 {
        // When writing (not appending), the file must not exist unless
        // the context allows overwriting it.
        let allow_overwrite = sess.opt("overwrite").is_some_and(|v| v.is_true());
        if ok2xx(result) {
            if !allow_overwrite {
                sess.log(
                    StreamErrorCode::AlreadyExists,
                    "Remote file already exists and overwrite context option not specified",
                );
                return Err(fail(sess, Some(stream), &line, result, None));
            }
            send_cmd(&mut stream, b"DELE", &rpath);
            result = get_result(&mut stream, &mut line);
            if !ok2xx(result) {
                return Err(fail(sess, Some(stream), &line, result, None));
            }
        }
    }

    // Set up the passive connection.
    let (portno, pasv_host) = do_pasv(&mut stream);
    if portno == 0 {
        return Err(fail(sess, Some(stream), &line, result, None));
    }

    let cmd: &[u8] = match read_write {
        1 => {
            if let Some(Value::Int(pos)) = sess.opt("resume_pos") {
                if pos > 0 {
                    send_cmd(&mut stream, b"REST", pos.to_string().as_bytes());
                    result = get_result(&mut stream, &mut line);
                    if !(300..=399).contains(&result) {
                        sess.log(
                            StreamErrorCode::ResumptionFailed,
                            format!("Unable to resume from offset {pos}"),
                        );
                        return Err(fail(sess, Some(stream), &line, result, None));
                    }
                }
            }
            b"RETR"
        }
        2 => b"STOR",
        _ => b"APPE",
    };
    send_cmd(&mut stream, cmd, cstr(&rpath));

    // Open the data channel.
    let host = pasv_host.unwrap_or(default_host);
    let env = http::env();
    let mut datastream = match env.connect(
        &tcp_transport(&host, i64::from(portno)),
        default_timeout(),
        context,
    ) {
        Ok(s) => s,
        Err(e) => {
            if env.exception_pending() {
                sess.exception = true;
            }
            return Err(fail(sess, Some(stream), &[], result, e));
        }
    };

    result = get_result(&mut stream, &mut line);
    if result != 150 && result != 125 {
        // The server could not retrieve or store the file.
        datastream.close();
        return Err(fail(sess, Some(stream), &line, result, None));
    }

    datastream.context = context.cloned();
    context::notify_progress_init(context, 0, file_size);

    if use_ssl_on_data {
        datastream = if let Some(s) = env.enable_crypto(datastream, None) {
            s
        } else {
            sess.log(
                StreamErrorCode::SslNotSupported,
                "Unable to activate SSL mode",
            );
            return Err(fail(sess, Some(stream), &[], result, None));
        };
    }

    // Remember the control connection (`datastream->wrapperthis`).
    let check_result = datastream.mode.contains(['w', 'a', '+']);
    datastream.set_wrapper_closer(Box::new(move || close_transfer(stream, check_result)));
    Ok(datastream)
}

/// The `ftpdir` directory stream: `NLST` output, one name per line.
#[derive(Debug)]
struct FtpDirOps {
    data: Stream,
    control: Option<Stream>,
}

/// `php_ftp_dirstream_read()` on one `NLST` line: its basename, cut to
/// the dirent size, without trailing whitespace.
fn dirent_name(line: &[u8]) -> Vec<u8> {
    let mut name = crate::pathname::basename(line, b"");
    name.truncate(DIRENT_NAME_SIZE - 1);
    let len = cstr(&name).len();
    name.truncate(len);
    while matches!(name.last(), Some(b'\n' | b'\r' | b'\t' | b' ')) {
        name.pop();
    }
    name
}

impl StreamOps for FtpDirOps {
    fn label(&self) -> &'static str {
        "ftpdir"
    }

    fn read(&mut self, _st: &mut StreamState, _buf: &mut [u8]) -> Result<usize, OpFailed> {
        Err(OpFailed)
    }

    fn can_write(&self) -> bool {
        false
    }

    fn readdir(&mut self, _st: &mut StreamState) -> Option<Vec<u8>> {
        if self.data.eof() {
            return None;
        }
        let line = self.data.get_line(Some(DIRENT_NAME_SIZE))?;
        Some(dirent_name(&line))
    }

    fn close(&mut self, _st: &mut StreamState, _preserve_handle: bool) -> i32 {
        if let Some(mut c) = self.control.take() {
            c.close();
        }
        self.data.close();
        0
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// `php_stream_ftp_opendir()`.
///
/// # Errors
/// The logged messages, joined into "Failed to open directory: ..." by
/// the caller.
pub fn opendir_url(
    path: &[u8],
    options: u32,
    context: Option<&ContextRef>,
) -> Result<Stream, OpenError> {
    let mut sess = Session::new(options, context);
    let Some(conn) = connect(&mut sess, path) else {
        return Err(sess.into_error());
    };
    let rpath = conn.path();
    let default_host = conn.host();
    let use_ssl_on_data = conn.use_ssl_on_data;
    let mut stream = conn.stream;
    let mut line = Vec::new();

    send(&mut stream, b"TYPE A\r\n");
    let mut result = get_result(&mut stream, &mut line);
    if !ok2xx(result) {
        return Err(fail(sess, Some(stream), &line, result, None));
    }
    line.clear();

    let (portno, pasv_host) = do_pasv(&mut stream);
    if portno == 0 {
        return Err(fail(sess, Some(stream), &line, result, None));
    }

    // `php_stream_sock_open_host()`: no context, errors reported.
    let host = pasv_host.unwrap_or(default_host);
    let Some(mut datastream) =
        connect_reporting(&tcp_transport(&host, i64::from(portno)), None, &mut sess)
    else {
        return Err(fail(sess, Some(stream), &line, result, None));
    };

    send_cmd(&mut stream, b"NLST", cstr(&rpath));
    result = get_result(&mut stream, &mut line);
    if result != 150 && result != 125 {
        datastream.close();
        return Err(fail(sess, Some(stream), &line, result, None));
    }

    datastream.context = context.cloned();
    if use_ssl_on_data {
        datastream = if let Some(s) = http::env().enable_crypto(datastream, None) {
            s
        } else {
            sess.log(
                StreamErrorCode::SslNotSupported,
                "Unable to activate SSL mode",
            );
            return Err(fail(sess, Some(stream), &line, result, None));
        };
    }

    Ok(Stream::new(
        Box::new(FtpDirOps {
            data: datastream,
            control: Some(stream),
        }),
        "r",
    ))
}

/// Scans `sscanf(p, "%4d%2d%2d%2d%2d%2d")`: each field skips blanks, takes
/// an optional sign and digits within its width.
fn scan_mdtm(s: &[u8]) -> Option<[i32; 6]> {
    const WIDTHS: [usize; 6] = [4, 2, 2, 2, 2, 2];
    let mut out = [0i32; 6];
    let mut i = 0;
    for (k, width) in WIDTHS.into_iter().enumerate() {
        while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
            i += 1;
        }
        let start = i;
        let mut neg = false;
        if i < s.len() && matches!(s[i], b'+' | b'-') {
            neg = s[i] == b'-';
            i += 1;
        }
        let digits_start = i;
        let mut v: i32 = 0;
        while i < s.len() && i - start < width && s[i].is_ascii_digit() {
            v = v * 10 + i32::from(s[i] - b'0');
            i += 1;
        }
        if i == digits_start {
            return None;
        }
        out[k] = if neg { -v } else { v };
    }
    Some(out)
}

/// The `MDTM` reply's time, `None` when it cannot be parsed.
fn mdtm_time(line: &[u8]) -> Option<i64> {
    let l = cstr(line);
    let mut p = 4.min(l.len());
    while p < l.len() && !l[p].is_ascii_digit() {
        p += 1;
    }
    let fields = scan_mdtm(&l[p..])?;
    crate::sys::ftp_mdtm_mktime(fields)
}

/// `php_stream_ftp_url_stat()`.
fn url_stat_impl(url: &[u8], context: Option<&ContextRef>) -> Result<StatBuf, ()> {
    let mut sess = Session::new(0, context);
    let conn = connect(&mut sess, url).ok_or(())?;
    let rpath = conn.path();
    let rpath = cstr(&rpath);
    let mut stream = conn.stream;
    let mut line = Vec::new();
    let mut sb = StatBuf {
        mode: 0o644,
        ..StatBuf::default()
    };

    // A path we can CWD to is a directory (maybe a link; FTP cannot tell).
    send_cmd(&mut stream, b"CWD", rpath);
    let mut result = get_result(&mut stream, &mut line);
    if ok2xx(result) {
        sb.mode |= S_IFDIR | S_IXALL;
    } else {
        sb.mode |= S_IFREG;
    }

    // Some servers refuse SIZE in ASCII mode.
    send(&mut stream, b"TYPE I\r\n");
    result = get_result(&mut stream, &mut line);
    if !ok2xx(result) {
        return Err(());
    }

    send_cmd(&mut stream, b"SIZE", rpath);
    result = get_result(&mut stream, &mut line);
    if ok2xx(result) {
        sb.size = i64::from(php_support::atoi(line.get(4..).unwrap_or_default()));
    } else if sb.mode & S_IFDIR != 0 {
        // Servers may fail SIZE on directories.
        sb.size = 0;
    } else {
        return Err(());
    }

    send_cmd(&mut stream, b"MDTM", rpath);
    result = get_result(&mut stream, &mut line);
    sb.mtime = if result == 213 {
        mdtm_time(&line).unwrap_or(-1)
    } else {
        -1
    };

    sb.ino = 0;
    sb.dev = 0;
    sb.uid = 0;
    sb.gid = 0;
    sb.atime = -1;
    sb.ctime = -1;
    sb.nlink = 1;
    sb.rdev = -1;
    sb.blksize = 4096;
    // `(int)((4095 + st_size) / st_blksize)`.
    #[allow(clippy::cast_possible_truncation)]
    let blocks = (sb.size.wrapping_add(4095) / sb.blksize) as i32;
    sb.blocks = i64::from(blocks);
    stream.close();
    Ok(sb)
}

/// Connects for a URL operation; warns "Unable to connect to ..." on
/// failure.
fn connect_for(
    url: &[u8],
    shown: &[u8],
    options: u32,
    context: Option<&ContextRef>,
) -> Option<Connection> {
    let mut sess = Session::new(0, context);
    let conn = connect(&mut sess, url);
    if conn.is_none() {
        wrapper_warn(
            LABEL,
            options,
            StreamErrorCode::AuthFailed,
            diag::msg(&[b"Unable to connect to ", cstr(shown)]),
        );
    }
    conn
}

/// The recursive branch of `php_stream_ftp_mkdir()`: finds the deepest
/// existing parent with `CWD`, then creates every missing level with
/// `MKD`. Returns the last reply code.
fn mkdir_recursive(stream: &mut Stream, path: &[u8], options: u32) -> i64 {
    let mut buf = cstr(path).to_vec();
    let e = buf.len();
    let mut line = Vec::new();
    let mut result;
    let as_arg = |b: &[u8]| -> Vec<u8> {
        let c = cstr(b);
        if c.is_empty() {
            b"/".to_vec()
        } else {
            c.to_vec()
        }
    };
    // Find the top level directory that has to be created.
    let mut p = None;
    while let Some(idx) = cstr(&buf).iter().rposition(|&c| c == b'/') {
        buf[idx] = 0;
        send_cmd(stream, b"CWD", &as_arg(&buf));
        result = get_result(stream, &mut line);
        if ok2xx(result) {
            buf[idx] = b'/';
            p = Some(idx);
            break;
        }
    }

    send_cmd(stream, b"MKD", &as_arg(&buf));
    result = get_result(stream, &mut line);

    if ok2xx(result) {
        // Create the remaining levels once the first one worked.
        let mut i = p.unwrap_or(0);
        while i != e {
            if buf[i] == 0 && buf.get(i + 1).is_some_and(|&c| c != 0) {
                buf[i] = b'/';
                send_cmd(stream, b"MKD", cstr(&buf));
                result = get_result(stream, &mut line);
                if !ok2xx(result) {
                    wrapper_warn(
                        LABEL,
                        options,
                        StreamErrorCode::MkdirFailed,
                        cstr(&line).to_vec(),
                    );
                    break;
                }
            }
            i += 1;
        }
    }
    result
}

/// The stream wrapper (`php_stream_ftp_wrapper`), registered for both
/// `ftp` and `ftps`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FtpWrapper;

impl Wrapper for FtpWrapper {
    fn label(&self) -> &'static str {
        LABEL
    }

    fn is_url(&self) -> bool {
        true
    }

    fn open(
        &self,
        path: &[u8],
        mode: &str,
        options: u32,
        _opened_path: &mut Option<Vec<u8>>,
        context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        open_url(path, mode, options, context)
    }

    fn url_stat(
        &self,
        path: &[u8],
        _flags: u32,
        context: Option<&ContextRef>,
    ) -> Option<Result<StatBuf, ()>> {
        Some(url_stat_impl(path, context))
    }

    fn has_dir_opener(&self) -> bool {
        true
    }

    fn opendir(
        &self,
        path: &[u8],
        options: u32,
        context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        opendir_url(path, options, context)
    }

    fn has_unlink(&self) -> bool {
        true
    }

    fn has_rename(&self) -> bool {
        true
    }

    fn unlink(&self, url: &[u8], options: u32, context: Option<&ContextRef>) -> Option<bool> {
        let Some(conn) = connect_for(url, url, options, context) else {
            return Some(false);
        };
        let rpath = conn.path();
        let mut stream = conn.stream;
        let mut line = Vec::new();
        send_cmd(&mut stream, b"DELE", &rpath);
        let result = get_result(&mut stream, &mut line);
        let ok = ok2xx(result);
        if !ok {
            wrapper_warn(
                LABEL,
                options,
                StreamErrorCode::UnlinkFailed,
                diag::msg(&[b"Error Deleting file: ", cstr(&line)]),
            );
        }
        stream.close();
        Some(ok)
    }

    fn rename(
        &self,
        from: &[u8],
        to: &[u8],
        options: u32,
        context: Option<&ContextRef>,
    ) -> Option<bool> {
        let Some(parser) = UriParser::from_context(LABEL, context) else {
            http::env().invalid_uri_parser();
            return Some(false);
        };
        let Some(rf) = parser.parse(cstr(from)) else {
            return Some(false);
        };
        let Some(rt) = parser.parse(cstr(to)) else {
            return Some(false);
        };
        // Same scheme, host and port (or a 21/0 combination), and paths.
        let port_mismatch = rf.port != rt.port
            && rf.port.wrapping_mul(rt.port) != 0
            && rf.port.wrapping_add(rt.port) != 21;
        let (Some(fs), Some(ts)) = (&rf.scheme, &rt.scheme) else {
            return Some(false);
        };
        let (Some(fh), Some(th)) = (&rf.host, &rt.host) else {
            return Some(false);
        };
        let (Some(fp), Some(tp)) = (&rf.path, &rt.path) else {
            return Some(false);
        };
        if fs != ts || fh != th || port_mismatch {
            return Some(false);
        }
        let Some(conn) = connect_for(from, fh, options, context) else {
            return Some(false);
        };
        let mut stream = conn.stream;
        let mut line = Vec::new();
        send_cmd(&mut stream, b"RNFR", fp);
        let mut result = get_result(&mut stream, &mut line);
        let mut ok = (300..=399).contains(&result);
        if ok {
            send_cmd(&mut stream, b"RNTO", tp);
            result = get_result(&mut stream, &mut line);
            ok = ok2xx(result);
        }
        if !ok {
            wrapper_warn(
                LABEL,
                options,
                StreamErrorCode::RenameFailed,
                diag::msg(&[b"Error Renaming file: ", cstr(&line)]),
            );
        }
        stream.close();
        Some(ok)
    }

    fn mkdir(
        &self,
        url: &[u8],
        _mode: i64,
        options: u32,
        context: Option<&ContextRef>,
    ) -> Option<bool> {
        let Some(conn) = connect_for(url, url, options, context) else {
            return Some(false);
        };
        let rpath = conn.path();
        let mut stream = conn.stream;
        let result = if options & MKDIR_RECURSIVE == 0 {
            let mut line = Vec::new();
            send_cmd(&mut stream, b"MKD", &rpath);
            get_result(&mut stream, &mut line)
        } else {
            mkdir_recursive(&mut stream, &rpath, options)
        };
        stream.close();
        Some(ok2xx(result))
    }

    fn rmdir(&self, url: &[u8], options: u32, context: Option<&ContextRef>) -> Option<bool> {
        let Some(conn) = connect_for(url, url, options, context) else {
            return Some(false);
        };
        let rpath = conn.path();
        let mut stream = conn.stream;
        let mut line = Vec::new();
        send_cmd(&mut stream, b"RMD", &rpath);
        let result = get_result(&mut stream, &mut line);
        let ok = ok2xx(result);
        if !ok {
            wrapper_warn(
                LABEL,
                options,
                StreamErrorCode::RmdirFailed,
                cstr(&line).to_vec(),
            );
        }
        stream.close();
        Some(ok)
    }
}

#[cfg(test)]
mod tests;
