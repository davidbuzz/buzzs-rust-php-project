//! The FTP connection (`ftpbuf_t`) and every protocol operation of
//! `ext/ftp/ftp.c`, ported with its reply-code checks, buffer sizes and
//! failure paths.

use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use openssl::ssl::{Ssl, SslContext, SslMethod, SslOptions, SslSession, SslSessionCacheMode};

use crate::channel::{self, io_strerror, secs, Channel};
use crate::list::{ascii_get_chunk, ascii_nb_chunk, split_listing};
use crate::text::{cstr, parse_epsv, parse_mdtm, parse_pasv, strtol};

/// `FTP_BUFSIZE`: the size of the reply buffer and of transfer chunks.
pub const BUFSIZE: usize = 4096;

/// `FTP_DEFAULT_TIMEOUT` (seconds).
pub const DEFAULT_TIMEOUT: i64 = 90;

/// The transfer type (`ftptype_t`); the values are `FTP_ASCII` /
/// `FTP_BINARY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferType {
    /// `TYPE A` (`FTP_ASCII`, `FTP_TEXT`).
    Ascii = 1,
    /// `TYPE I` (`FTP_BINARY`, `FTP_IMAGE`).
    Image = 2,
}

impl TransferType {
    /// The type for a PHP `$mode` argument, if valid.
    pub fn from_mode(mode: i64) -> Option<Self> {
        match mode {
            1 => Some(TransferType::Ascii),
            2 => Some(TransferType::Image),
            _ => None,
        }
    }
}

/// Result of a non-blocking transfer step (`FTP_FAILED`, `FTP_FINISHED`,
/// `FTP_MOREDATA`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NbStatus {
    /// `FTP_FAILED` (0).
    Failed = 0,
    /// `FTP_FINISHED` (1).
    Finished = 1,
    /// `FTP_MOREDATA` (2).
    MoreData = 2,
}

impl NbStatus {
    /// The PHP constant value.
    pub fn code(self) -> i64 {
        self as i64
    }
}

/// Where downloaded data goes (a PHP stream in the binding).
pub trait Sink {
    /// `php_stream_write`; `true` when all of `data` was written.
    fn write(&mut self, data: &[u8]) -> bool;
}

/// Where uploaded data comes from (a PHP stream in the binding).
pub trait Source {
    /// `php_stream_eof`.
    fn eof(&mut self) -> bool;
    /// `php_stream_read`; `None` on error.
    fn read(&mut self, buf: &mut [u8]) -> Option<usize>;
    /// `php_stream_get_line` with Unix line endings: at most `maxlen - 1`
    /// bytes, ending at the first `"\n"`.
    fn get_line(&mut self, maxlen: usize) -> Option<Vec<u8>>;
}

impl Sink for Vec<u8> {
    fn write(&mut self, data: &[u8]) -> bool {
        self.extend_from_slice(data);
        true
    }
}

/// An in-memory [`Source`] (used by tests and simple callers).
#[derive(Debug, Clone, Default)]
pub struct SliceSource {
    data: Vec<u8>,
    pos: usize,
}

impl SliceSource {
    /// A source reading `data`.
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        Self {
            data: data.into(),
            pos: 0,
        }
    }
}

impl Source for SliceSource {
    fn eof(&mut self) -> bool {
        self.pos >= self.data.len()
    }

    fn read(&mut self, buf: &mut [u8]) -> Option<usize> {
        let n = buf.len().min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Some(n)
    }

    fn get_line(&mut self, maxlen: usize) -> Option<Vec<u8>> {
        if maxlen < 2 || self.eof() {
            return None;
        }
        let rest = &self.data[self.pos..];
        let lim = rest.len().min(maxlen - 1);
        let end = rest[..lim]
            .iter()
            .position(|&c| c == b'\n')
            .map_or(lim, |p| p + 1);
        self.pos += end;
        Some(rest[..end].to_vec())
    }
}

/// The data connection (`databuf_t`).
#[derive(Debug)]
struct DataConn {
    /// Listening socket of an active-mode transfer, until accepted.
    listener: Option<TcpListener>,
    /// The connected data socket.
    chan: Option<Channel>,
}

/// FTPS state shared by the control and data connections.
struct TlsState {
    /// The control connection's `SSL_CTX` (reused for data connections).
    ctx: SslContext,
    /// The most recent session negotiated on this context
    /// (`last_ssl_session`).
    session: Arc<Mutex<Option<SslSession>>>,
}

impl std::fmt::Debug for TlsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsState").finish_non_exhaustive()
    }
}

/// An FTP connection (`ftpbuf_t`).
///
/// Every operation returns PHP's success value; the text of the last
/// server reply is available from [`Ftp::last_reply`] (`ftp->inbuf`, which
/// the PHP functions emit as a warning on failure) and other diagnostics
/// accumulate in [`Ftp::take_warnings`].
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // mirrors ftpbuf_t's flags
pub struct Ftp {
    /// Control connection (`None` once shut down).
    ctrl: Option<Channel>,
    /// Local address of the control connection.
    local_addr: SocketAddr,
    /// Last response code.
    resp: i32,
    /// Last response text (C string semantics).
    inbuf: Box<[u8; BUFSIZE]>,
    /// Offset of unconsumed bytes after the last line in `inbuf`.
    extra: Option<usize>,
    /// Number of unconsumed bytes.
    extralen: usize,
    /// Cached working directory.
    pwd: Option<Vec<u8>>,
    /// Cached system type.
    syst: Option<Vec<u8>>,
    /// Current transfer type (`None` until the first `TYPE`).
    ty: Option<TransferType>,
    /// 0 = active, 1 = passive, 2 = passive and address known.
    pasv: u8,
    /// Passive-mode data address.
    pasv_addr: Option<SocketAddr>,
    /// `FTP_TIMEOUT_SEC`.
    pub timeout_sec: i64,
    /// `FTP_AUTOSEEK`.
    pub autoseek: bool,
    /// `FTP_USEPASVADDRESS`.
    pub usepasvaddress: bool,
    /// The data connection of the current transfer.
    data: Option<DataConn>,
    /// A non-blocking transfer is in progress.
    nb: bool,
    /// Last character of the previous non-blocking chunk.
    lastch: u8,
    /// Direction of the non-blocking transfer (`true` = upload).
    sending: bool,
    /// `ftp_ssl_connect`: negotiate TLS at login.
    use_ssl: bool,
    /// Encrypt data connections.
    use_ssl_for_data: bool,
    /// `AUTH SSL` (implicit data protection).
    old_ssl: bool,
    /// TLS active on the control connection.
    ssl_active: bool,
    /// The TLS context, once negotiated.
    tls: Option<TlsState>,
    /// Pending diagnostics (warning texts without the function prefix).
    warnings: Vec<Vec<u8>>,
}

impl Drop for Ftp {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Ftp {
    /// `ftp_open`: connects to `host:port` (port 0 means 21) and reads the
    /// 220 greeting. On failure returns the warnings to emit.
    pub fn open(host: &str, port: u16, timeout_sec: i64) -> Result<Ftp, Vec<Vec<u8>>> {
        let mut warns = Vec::new();
        let port = if port == 0 { 21 } else { port };
        let Some(sock) = channel::connect(host, port, secs(timeout_sec), &mut warns) else {
            return Err(warns);
        };
        let local_addr = match sock.local_addr() {
            Ok(a) => a,
            Err(e) => {
                let (msg, no) = io_strerror(&e);
                warns.push(format!("getsockname failed: {msg} ({no})").into_bytes());
                return Err(warns);
            }
        };
        let mut ftp = Ftp {
            ctrl: Some(Channel::Plain(sock)),
            local_addr,
            resp: 0,
            inbuf: Box::new([0; BUFSIZE]),
            extra: None,
            extralen: 0,
            pwd: None,
            syst: None,
            ty: None,
            pasv: 0,
            pasv_addr: None,
            timeout_sec,
            autoseek: true,
            usepasvaddress: true,
            data: None,
            nb: false,
            lastch: 0,
            sending: false,
            use_ssl: false,
            use_ssl_for_data: false,
            old_ssl: false,
            ssl_active: false,
            tls: None,
            warnings: warns,
        };
        if !ftp.getresp() || ftp.resp != 220 {
            let mut w = ftp.take_warnings();
            ftp.ctrl = None;
            w.extend(ftp.take_warnings());
            return Err(w);
        }
        Ok(ftp)
    }

    /// Enables FTPS (`ftp_ssl_connect`): TLS is negotiated by
    /// [`Ftp::login`].
    pub fn set_use_ssl(&mut self, on: bool) {
        self.use_ssl = on;
    }

    /// The last reply text (`ftp->inbuf` as a C string).
    pub fn last_reply(&self) -> &[u8] {
        cstr(&self.inbuf[..])
    }

    /// The last response code.
    pub fn last_code(&self) -> i32 {
        self.resp
    }

    /// Takes the pending warnings.
    pub fn take_warnings(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.warnings)
    }

    /// Whether a non-blocking transfer is in progress.
    pub fn nb_active(&self) -> bool {
        self.nb
    }

    /// Whether the non-blocking transfer uploads.
    pub fn nb_sending(&self) -> bool {
        self.sending
    }

    fn timeout(&self) -> Duration {
        secs(self.timeout_sec)
    }

    fn warn(&mut self, msg: impl Into<Vec<u8>>) {
        self.warnings.push(msg.into());
    }

    /// `ftp_close` minus the free: closes the data and control connections
    /// (TLS shutdown included). Idempotent.
    pub fn shutdown(&mut self) {
        self.data_close();
        if let Some(c) = self.ctrl.take() {
            c.close(&mut self.warnings);
        }
        self.pwd = None;
        self.syst = None;
    }

    // ------------------------------------------------------------------
    // Control connection
    // ------------------------------------------------------------------

    /// `ftp_putcmd`: sends `"cmd args\r\n"` (or `"cmd\r\n"` when `args` is
    /// empty). Commands containing CR/LF are refused without touching the
    /// reply buffer.
    fn putcmd(&mut self, cmd: &[u8], args: &[u8]) -> bool {
        let cmd_c = cstr(cmd);
        if cmd_c.iter().any(|&c| c == b'\r' || c == b'\n') {
            return false;
        }
        let mut line = Vec::with_capacity(cmd.len() + args.len() + 3);
        if args.first().is_some_and(|&c| c != 0) {
            if cmd.len() + args.len() + 4 > BUFSIZE {
                return false;
            }
            let args_c = cstr(args);
            if args_c.iter().any(|&c| c == b'\r' || c == b'\n') {
                return false;
            }
            line.extend_from_slice(cmd_c);
            line.push(b' ');
            line.extend_from_slice(args_c);
        } else {
            if cmd.len() + 3 > BUFSIZE {
                return false;
            }
            line.extend_from_slice(cmd_c);
        }
        line.extend_from_slice(b"\r\n");
        self.inbuf[0] = 0;
        self.extra = None;
        let t = self.timeout();
        match self.ctrl.as_mut() {
            Some(c) => c.send(&line, t, &mut self.warnings),
            None => false,
        }
    }

    /// `ftp_readline`: reads one line into `inbuf` (terminated by CR, LF
    /// or CRLF), keeping any following bytes as `extra`.
    fn readline(&mut self) -> bool {
        let mut size = BUFSIZE;
        let mut rcvd = 0usize;
        if let Some(e) = self.extra {
            self.inbuf.copy_within(e..e + self.extralen, 0);
            rcvd = self.extralen;
        }
        let mut data = 0usize;
        loop {
            size -= rcvd;
            let mut eol = data;
            while rcvd > 0 {
                match self.inbuf[eol] {
                    b'\r' => {
                        self.inbuf[eol] = 0;
                        let mut extra = eol + 1;
                        if rcvd > 1 && self.inbuf[eol + 1] == b'\n' {
                            extra += 1;
                            rcvd -= 1;
                        }
                        rcvd -= 1;
                        self.extralen = rcvd;
                        self.extra = (rcvd != 0).then_some(extra);
                        return true;
                    }
                    b'\n' => {
                        self.inbuf[eol] = 0;
                        rcvd -= 1;
                        self.extralen = rcvd;
                        self.extra = (rcvd != 0).then_some(eol + 1);
                        return true;
                    }
                    _ => {}
                }
                rcvd -= 1;
                eol += 1;
            }
            data = eol;
            if size < 2 {
                self.inbuf[data] = 0;
                return false;
            }
            let t = self.timeout();
            let got = match self.ctrl.as_mut() {
                Some(c) => c.recv(
                    &mut self.inbuf[data..data + size - 1],
                    t,
                    &mut self.warnings,
                ),
                None => None,
            };
            match got {
                Some(n) if n >= 1 => rcvd = n,
                _ => {
                    self.inbuf[data] = 0;
                    return false;
                }
            }
        }
    }

    /// Whether `inbuf` starts with a final reply tag (`"ddd "`).
    fn is_final_line(&self) -> bool {
        self.inbuf[..3].iter().all(u8::is_ascii_digit) && self.inbuf[3] == b' '
    }

    /// `ftp_getresp`: reads a (possibly multi-line) reply, sets `resp` and
    /// leaves the text after the code in `inbuf`.
    fn getresp(&mut self) -> bool {
        self.resp = 0;
        loop {
            if !self.readline() {
                return false;
            }
            if self.is_final_line() {
                break;
            }
        }
        let d = |c: u8| i32::from(c - b'0');
        self.resp = 100 * d(self.inbuf[0]) + 10 * d(self.inbuf[1]) + d(self.inbuf[2]);
        self.inbuf.copy_within(4.., 0);
        if let Some(e) = self.extra.as_mut() {
            *e -= 4;
        }
        true
    }

    /// `putcmd` + `getresp` + code check.
    fn command(&mut self, cmd: &[u8], args: &[u8], ok: impl Fn(i32) -> bool) -> bool {
        self.putcmd(cmd, args) && self.getresp() && ok(self.resp)
    }

    // ------------------------------------------------------------------
    // Session commands
    // ------------------------------------------------------------------

    /// `ftp_quit`: `QUIT`, expecting 221.
    pub fn quit(&mut self) -> bool {
        if !self.command(b"QUIT", b"", |r| r == 221) {
            return false;
        }
        self.pwd = None;
        true
    }

    /// `ftp_login`: negotiates TLS first for FTPS connections, then
    /// `USER`/`PASS`.
    pub fn login(&mut self, user: &[u8], pass: &[u8]) -> bool {
        if self.use_ssl && !self.ssl_active && !self.start_tls() {
            return false;
        }
        if !self.putcmd(b"USER", user) || !self.getresp() {
            return false;
        }
        if self.resp == 230 {
            return true;
        }
        if self.resp != 331 {
            return false;
        }
        self.command(b"PASS", pass, |r| r == 230)
    }

    /// `AUTH TLS` (falling back to `AUTH SSL`), the handshake, then
    /// `PBSZ 0` / `PROT P`.
    fn start_tls(&mut self) -> bool {
        if !self.putcmd(b"AUTH", b"TLS") || !self.getresp() {
            return false;
        }
        if self.resp != 234 {
            if !self.putcmd(b"AUTH", b"SSL") || !self.getresp() {
                return false;
            }
            if self.resp != 334 {
                return false;
            }
            self.old_ssl = true;
            self.use_ssl_for_data = true;
        }
        let session: Arc<Mutex<Option<SslSession>>> = Arc::new(Mutex::new(None));
        let ctx = {
            let Ok(mut b) = SslContext::builder(SslMethod::tls_client()) else {
                self.warn("Failed to create the SSL context");
                return false;
            };
            b.set_options(SslOptions::ALL & !SslOptions::DONT_INSERT_EMPTY_FRAGMENTS);
            b.set_session_cache_mode(SslSessionCacheMode::BOTH | SslSessionCacheMode::NO_INTERNAL);
            let slot = Arc::clone(&session);
            b.set_new_session_callback(move |_ssl, sess| {
                *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(sess);
            });
            b.build()
        };
        let Ok(ssl) = Ssl::new(&ctx) else {
            self.warn("Failed to create the SSL handle");
            return false;
        };
        let Some(sock) = self.ctrl.as_ref().and_then(|c| match c {
            Channel::Plain(s) => s.try_clone().ok(),
            Channel::Tls(_) => None,
        }) else {
            return false;
        };
        let _ = sock.set_read_timeout(Some(self.timeout()));
        let _ = sock.set_write_timeout(Some(self.timeout()));
        let Ok(s) = ssl.connect(sock) else {
            self.warn("SSL/TLS handshake failed");
            return false;
        };
        self.ctrl = Some(Channel::Tls(Box::new(s)));
        self.tls = Some(TlsState { ctx, session });
        self.ssl_active = true;
        if !self.old_ssl {
            if !self.putcmd(b"PBSZ", b"0") || !self.getresp() {
                return false;
            }
            if !self.putcmd(b"PROT", b"P") || !self.getresp() {
                return false;
            }
            self.use_ssl_for_data = (200..=299).contains(&self.resp);
        }
        true
    }

    /// `ftp_reinit`: `REIN`, expecting 220.
    pub fn reinit(&mut self) -> bool {
        self.pwd = None;
        self.syst = None;
        self.nb = false;
        self.command(b"REIN", b"", |r| r == 220)
    }

    /// `ftp_syst`: the first word of the 215 reply (cached).
    pub fn syst(&mut self) -> Option<Vec<u8>> {
        if let Some(s) = &self.syst {
            return Some(s.clone());
        }
        if !self.command(b"SYST", b"", |r| r == 215) {
            return None;
        }
        let text = self.last_reply();
        let start = text.iter().position(|&c| c != b' ').unwrap_or(text.len());
        let word = &text[start..];
        let end = word.iter().position(|&c| c == b' ').unwrap_or(word.len());
        let s = word[..end].to_vec();
        self.syst = Some(s.clone());
        Some(s)
    }

    /// `ftp_pwd`: the quoted path of the 257 reply (cached).
    pub fn pwd(&mut self) -> Option<Vec<u8>> {
        if let Some(p) = &self.pwd {
            return Some(p.clone());
        }
        if !self.command(b"PWD", b"", |r| r == 257) {
            return None;
        }
        let p = quoted(self.last_reply())?;
        self.pwd = Some(p.clone());
        Some(p)
    }

    /// `ftp_exec`: `SITE EXEC cmd`, expecting 200.
    pub fn exec(&mut self, cmd: &[u8]) -> bool {
        self.command(b"SITE EXEC", cmd, |r| r == 200)
    }

    /// `ftp_raw`: sends `cmd` verbatim and returns every reply line up to
    /// the final one; `None` when the command could not be sent.
    pub fn raw(&mut self, cmd: &[u8]) -> Option<Vec<Vec<u8>>> {
        if !self.putcmd(cmd, b"") {
            return None;
        }
        let mut lines = Vec::new();
        while self.readline() {
            lines.push(self.last_reply().to_vec());
            if self.is_final_line() {
                break;
            }
        }
        Some(lines)
    }

    /// `ftp_chdir`: `CWD dir`, expecting 250.
    pub fn chdir(&mut self, dir: &[u8]) -> bool {
        self.pwd = None;
        self.command(b"CWD", dir, |r| r == 250)
    }

    /// `ftp_cdup`: `CDUP`, expecting 250.
    pub fn cdup(&mut self) -> bool {
        self.pwd = None;
        self.command(b"CDUP", b"", |r| r == 250)
    }

    /// `ftp_mkdir`: `MKD dir`; the quoted path of the 257 reply, or `dir`
    /// itself when the reply has no quotes.
    pub fn mkdir(&mut self, dir: &[u8]) -> Option<Vec<u8>> {
        if !self.command(b"MKD", dir, |r| r == 257) {
            return None;
        }
        let text = self.last_reply();
        if !text.contains(&b'"') {
            return Some(dir.to_vec());
        }
        quoted(text)
    }

    /// `ftp_rmdir`: `RMD dir`, expecting 250.
    pub fn rmdir(&mut self, dir: &[u8]) -> bool {
        self.command(b"RMD", dir, |r| r == 250)
    }

    /// `ftp_chmod`: `SITE CHMOD <octal mode> filename`, expecting 200.
    pub fn chmod(&mut self, mode: i64, filename: &[u8]) -> bool {
        if filename.is_empty() {
            return false;
        }
        // `spprintf("%o", (int) mode)`: the low 32 bits, unsigned.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let m = mode as i32 as u32;
        let mut arg = format!("CHMOD {m:o} ").into_bytes();
        arg.extend_from_slice(cstr(filename));
        self.command(b"SITE", &arg, |r| r == 200)
    }

    /// `ftp_alloc`: `ALLO size`. Returns the success flag and, when a
    /// reply was read, its text.
    pub fn alloc(&mut self, size: i64) -> (bool, Option<Vec<u8>>) {
        if size <= 0 {
            return (false, None);
        }
        if !self.putcmd(b"ALLO", size.to_string().as_bytes()) || !self.getresp() {
            return (false, None);
        }
        let text = self.last_reply().to_vec();
        ((200..300).contains(&self.resp), Some(text))
    }

    /// `ftp_size`: `SIZE path` in binary mode; -1 on failure.
    pub fn size(&mut self, path: &[u8]) -> i64 {
        if !self.set_type(TransferType::Image) || !self.command(b"SIZE", path, |r| r == 213) {
            return -1;
        }
        strtol(self.last_reply())
    }

    /// `ftp_mdtm`: `MDTM path` as a Unix timestamp; -1 on failure.
    pub fn mdtm(&mut self, path: &[u8]) -> i64 {
        if !self.command(b"MDTM", path, |r| r == 213) {
            return -1;
        }
        parse_mdtm(self.last_reply())
    }

    /// `ftp_delete`: `DELE path`, expecting 250.
    pub fn delete(&mut self, path: &[u8]) -> bool {
        self.command(b"DELE", path, |r| r == 250)
    }

    /// `ftp_rename`: `RNFR src` (350) then `RNTO dest` (250).
    pub fn rename(&mut self, src: &[u8], dest: &[u8]) -> bool {
        self.command(b"RNFR", src, |r| r == 350) && self.command(b"RNTO", dest, |r| r == 250)
    }

    /// `ftp_site`: `SITE cmd`, expecting 2xx.
    pub fn site(&mut self, cmd: &[u8]) -> bool {
        self.command(b"SITE", cmd, |r| (200..300).contains(&r))
    }

    /// `ftp_type`: `TYPE A` / `TYPE I` unless already selected.
    fn set_type(&mut self, ty: TransferType) -> bool {
        if self.ty == Some(ty) {
            return true;
        }
        let c: &[u8] = match ty {
            TransferType::Ascii => b"A",
            TransferType::Image => b"I",
        };
        if !self.command(b"TYPE", c, |r| r == 200) {
            return false;
        }
        self.ty = Some(ty);
        true
    }

    // ------------------------------------------------------------------
    // Data connections
    // ------------------------------------------------------------------

    /// `ftp_pasv`: switches passive mode on (asking the server for the data
    /// address: `EPSV` first on IPv6, then `PASV`) or off.
    pub fn pasv(&mut self, on: bool) -> bool {
        if on && self.pasv == 2 {
            return true;
        }
        self.pasv = 0;
        if !on {
            return true;
        }
        let Some(mut addr) = self.ctrl.as_ref().and_then(|c| c.tcp().peer_addr().ok()) else {
            return false;
        };
        if addr.is_ipv6() {
            if !self.putcmd(b"EPSV", b"") || !self.getresp() {
                return false;
            }
            if self.resp == 229 {
                let Some(port) = parse_epsv(self.last_reply()) else {
                    return false;
                };
                addr.set_port(port);
                self.pasv_addr = Some(addr);
                self.pasv = 2;
                return true;
            }
        }
        if !self.command(b"PASV", b"", |r| r == 227) {
            return false;
        }
        let Some(b) = parse_pasv(self.last_reply()) else {
            return false;
        };
        if self.usepasvaddress {
            if let SocketAddr::V4(v4) = &mut addr {
                v4.set_ip(Ipv4Addr::new(b[0], b[1], b[2], b[3]));
            }
        }
        addr.set_port(u16::from_be_bytes([b[4], b[5]]));
        self.pasv_addr = Some(addr);
        self.pasv = 2;
        true
    }

    /// `ftp_getdata`: prepares the data connection — connects to the
    /// passive address, or listens and announces it with `PORT`/`EPRT`.
    fn getdata(&mut self) -> bool {
        if self.pasv != 0 && !self.pasv(true) {
            return false;
        }
        if self.pasv != 0 {
            self.pasv = 1;
            let Some(addr) = self.pasv_addr else {
                return false;
            };
            return match TcpStream::connect_timeout(&addr, self.timeout()) {
                Ok(s) => {
                    self.data = Some(DataConn {
                        listener: None,
                        chan: Some(Channel::Plain(s)),
                    });
                    true
                }
                Err(e) => {
                    let (msg, no) = io_strerror(&e);
                    self.warn(format!("php_connect_nonb() failed: {msg} ({no})"));
                    false
                }
            };
        }
        let any: IpAddr = if self.local_addr.is_ipv6() {
            Ipv6Addr::UNSPECIFIED.into()
        } else {
            Ipv4Addr::UNSPECIFIED.into()
        };
        let listener = match TcpListener::bind((any, 0)) {
            Ok(l) => l,
            Err(e) => {
                let (msg, no) = io_strerror(&e);
                self.warn(format!("bind() failed: {msg} ({no})"));
                return false;
            }
        };
        let port = match listener.local_addr() {
            Ok(a) => a.port(),
            Err(e) => {
                let (msg, no) = io_strerror(&e);
                self.warn(format!("getsockname() failed: {msg} ({no})"));
                return false;
            }
        };
        let ok = match self.local_addr.ip() {
            IpAddr::V6(ip) => {
                let arg = format!("|2|{ip}|{port}|");
                self.command(b"EPRT", arg.as_bytes(), |r| r == 200)
            }
            IpAddr::V4(ip) => {
                let o = ip.octets();
                let p = port.to_be_bytes();
                let mut arg = String::new();
                let _ = write!(arg, "{},{},{},{},{},{}", o[0], o[1], o[2], o[3], p[0], p[1]);
                self.command(b"PORT", arg.as_bytes(), |r| r == 200)
            }
        };
        if !ok {
            return false;
        }
        self.data = Some(DataConn {
            listener: Some(listener),
            chan: None,
        });
        true
    }

    /// `data_accept`: accepts the active-mode connection and starts TLS on
    /// it for protected FTPS data.
    fn data_accept(&mut self) -> bool {
        let t = self.timeout();
        let Some(data) = self.data.as_mut() else {
            return false;
        };
        if data.chan.is_none() {
            let Some(l) = data.listener.take() else {
                return false;
            };
            match channel::accept(&l, t, &mut self.warnings) {
                Some(s) => data.chan = Some(Channel::Plain(s)),
                None => return false,
            }
        }
        if self.use_ssl && self.use_ssl_for_data {
            return self.data_start_tls();
        }
        true
    }

    /// TLS on the data connection, resuming the control connection's
    /// session (required: PHP refuses servers without resumption).
    fn data_start_tls(&mut self) -> bool {
        let Some(tls) = self.tls.as_ref() else {
            self.warn("data_accept: failed to retrieve the existing SSL context");
            return false;
        };
        let Ok(mut ssl) = Ssl::new(&tls.ctx) else {
            self.warn("data_accept: failed to create the SSL handle");
            return false;
        };
        let session = tls
            .session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(session) = session else {
            self.warn(
                "data_accept: failed to retrieve the existing SSL session from the control connection. \
                 The server does not support TLS session resumption on the data connection, \
                 which is necessary to protect against session data stealing. \
                 PHP does not support such configuration.",
            );
            return false;
        };
        // SAFETY: `set_session` requires the session to come from the same
        // SSL_CTX; it was recorded by this context's new-session callback.
        if unsafe { ssl.set_session(&session) }.is_err() {
            self.warn("data_accept: failed to set the existing SSL session");
            return false;
        }
        let Some(Channel::Plain(sock)) = self.data.as_mut().and_then(|d| d.chan.take()) else {
            return false;
        };
        let _ = sock.set_read_timeout(Some(self.timeout()));
        let Ok(s) = ssl.connect(sock) else {
            self.warn("data_accept: SSL/TLS handshake failed");
            return false;
        };
        if let Some(d) = self.data.as_mut() {
            d.chan = Some(Channel::Tls(Box::new(s)));
        }
        true
    }

    /// `data_close`: drops the data connection (TLS shutdown included).
    fn data_close(&mut self) {
        if let Some(d) = self.data.take() {
            drop(d.listener);
            if let Some(c) = d.chan {
                c.close(&mut self.warnings);
            }
        }
    }

    /// `my_recv` on the data connection.
    fn data_recv(&mut self, buf: &mut [u8]) -> Option<usize> {
        let t = self.timeout();
        let chan = self.data.as_mut()?.chan.as_mut()?;
        chan.recv(buf, t, &mut self.warnings)
    }

    /// `my_send` on the data connection.
    fn data_send(&mut self, buf: &[u8]) -> bool {
        let t = self.timeout();
        match self.data.as_mut().and_then(|d| d.chan.as_mut()) {
            Some(c) => c.send(buf, t, &mut self.warnings),
            None => false,
        }
    }

    /// `REST pos` (350) when `pos > 0`.
    fn rest(&mut self, pos: i64) -> bool {
        pos <= 0 || self.command(b"REST", pos.to_string().as_bytes(), |r| r == 350)
    }

    // ------------------------------------------------------------------
    // Listings
    // ------------------------------------------------------------------

    /// `ftp_nlist`: `NLST path`.
    pub fn nlist(&mut self, path: &[u8]) -> Option<Vec<Vec<u8>>> {
        self.genlist(b"NLST", path)
    }

    /// `ftp_list`: `LIST path` (`LIST -R path` when recursive).
    pub fn list(&mut self, path: &[u8], recursive: bool) -> Option<Vec<Vec<u8>>> {
        let cmd: &[u8] = if recursive { b"LIST -R" } else { b"LIST" };
        self.genlist(cmd, path)
    }

    /// `ftp_mlsd`: the raw `MLSD path` lines (parse each with
    /// [`crate::parse_mlsd_line`]).
    pub fn mlsd(&mut self, path: &[u8]) -> Option<Vec<Vec<u8>>> {
        self.genlist(b"MLSD", path)
    }

    /// `ftp_genlist`.
    fn genlist(&mut self, cmd: &[u8], path: &[u8]) -> Option<Vec<Vec<u8>>> {
        let r = self.genlist_inner(cmd, path);
        if r.is_none() {
            self.data_close();
        }
        r
    }

    fn genlist_inner(&mut self, cmd: &[u8], path: &[u8]) -> Option<Vec<Vec<u8>>> {
        if !self.set_type(TransferType::Ascii) || !self.getdata() {
            return None;
        }
        if !self.command(cmd, path, |r| r == 150 || r == 125 || r == 226) {
            return None;
        }
        // Some servers don't open a data connection for an empty listing.
        if self.resp == 226 {
            self.data_close();
            return Some(Vec::new());
        }
        if !self.data_accept() {
            return None;
        }
        let mut all = Vec::new();
        let mut buf = vec![0u8; BUFSIZE];
        loop {
            match self.data_recv(&mut buf)? {
                0 => break,
                n => all.extend_from_slice(&buf[..n]),
            }
        }
        self.data_close();
        let lines = split_listing(&all);
        if !self.getresp() || (self.resp != 226 && self.resp != 250) {
            return None;
        }
        Some(lines)
    }

    // ------------------------------------------------------------------
    // Blocking transfers
    // ------------------------------------------------------------------

    /// `ftp_get`: `RETR path` into `out` (from `resumepos` via `REST`).
    pub fn get(
        &mut self,
        out: &mut dyn Sink,
        path: &[u8],
        ty: TransferType,
        resumepos: i64,
    ) -> bool {
        let ok = self.get_inner(out, path, ty, resumepos);
        if !ok {
            self.data_close();
        }
        ok
    }

    fn get_inner(
        &mut self,
        out: &mut dyn Sink,
        path: &[u8],
        ty: TransferType,
        resumepos: i64,
    ) -> bool {
        if !self.set_type(ty) || !self.getdata() || !self.rest(resumepos) {
            return false;
        }
        if !self.command(b"RETR", path, |r| r == 150 || r == 125) || !self.data_accept() {
            return false;
        }
        let mut buf = vec![0u8; BUFSIZE];
        let mut pending_cr = false;
        let mut conv = Vec::with_capacity(BUFSIZE);
        loop {
            let n = match self.data_recv(&mut buf) {
                None => return false,
                Some(0) => break,
                Some(n) => n,
            };
            if ty == TransferType::Ascii {
                conv.clear();
                ascii_get_chunk(&buf[..n], &mut pending_cr, &mut conv);
                if !conv.is_empty() {
                    out.write(&conv);
                }
            } else if !out.write(&buf[..n]) {
                return false;
            }
        }
        if pending_cr {
            out.write(b"\r");
        }
        self.data_close();
        self.getresp() && (self.resp == 226 || self.resp == 250)
    }

    /// `ftp_send_stream_to_data_socket`: uploads `src` (ASCII mode turns
    /// `"\n"` into `"\r\n"`, buffering whole lines up to `BUFSIZE`). With
    /// `once`, returns after the first chunk.
    fn send_stream(&mut self, src: &mut dyn Source, ty: TransferType, once: bool) -> bool {
        let mut buf = vec![0u8; BUFSIZE];
        if ty == TransferType::Ascii {
            let mut ptr = 0usize;
            while !src.eof() {
                let Some(line) = src.get_line(BUFSIZE - ptr) else {
                    break;
                };
                if line.is_empty() {
                    break;
                }
                buf[ptr..ptr + line.len()].copy_from_slice(&line);
                ptr += line.len() - 1;
                if buf[ptr] == b'\n' {
                    buf[ptr] = b'\r';
                    ptr += 1;
                    buf[ptr] = b'\n';
                }
                ptr += 1;
                if BUFSIZE - ptr < 2 {
                    if !self.data_send(&buf[..ptr]) {
                        return false;
                    }
                    ptr = 0;
                    if once {
                        break;
                    }
                }
            }
            if ptr > 0 && !self.data_send(&buf[..ptr]) {
                return false;
            }
        } else {
            while !src.eof() {
                let n = match src.read(&mut buf) {
                    None => return false,
                    Some(0) => break,
                    Some(n) => n,
                };
                if !self.data_send(&buf[..n]) {
                    return false;
                }
                if once {
                    break;
                }
            }
        }
        true
    }

    /// `ftp_put`: `STOR path` from `src` (from `startpos` via `REST`).
    pub fn put(
        &mut self,
        path: &[u8],
        src: &mut dyn Source,
        ty: TransferType,
        startpos: i64,
    ) -> bool {
        let ok = self.store(b"STOR", path, src, ty, startpos);
        if !ok {
            self.data_close();
        }
        ok
    }

    /// `ftp_append`: `APPE path` from `src`.
    pub fn append(&mut self, path: &[u8], src: &mut dyn Source, ty: TransferType) -> bool {
        let ok = self.store(b"APPE", path, src, ty, 0);
        if !ok {
            self.data_close();
        }
        ok
    }

    fn store(
        &mut self,
        cmd: &[u8],
        path: &[u8],
        src: &mut dyn Source,
        ty: TransferType,
        startpos: i64,
    ) -> bool {
        if !self.set_type(ty) || !self.getdata() || !self.rest(startpos) {
            return false;
        }
        if !self.command(cmd, path, |r| r == 150 || r == 125) || !self.data_accept() {
            return false;
        }
        if !self.send_stream(src, ty, false) {
            return false;
        }
        self.data_close();
        self.getresp() && matches!(self.resp, 226 | 250 | 200)
    }

    // ------------------------------------------------------------------
    // Non-blocking transfers
    // ------------------------------------------------------------------

    /// `ftp_nb_get`: starts a download (aborting a pending transfer first)
    /// and performs the first step.
    pub fn nb_get(
        &mut self,
        out: &mut dyn Sink,
        path: &[u8],
        ty: TransferType,
        resumepos: i64,
    ) -> NbStatus {
        self.sending = false;
        if self.data.is_some() {
            self.data_close();
            if !self.getresp() || (self.resp != 226 && self.resp != 250) {
                self.data_close();
                return NbStatus::Failed;
            }
        }
        if !self.set_type(ty)
            || !self.getdata()
            || !self.rest(resumepos)
            || !self.command(b"RETR", path, |r| r == 150 || r == 125)
            || !self.data_accept()
        {
            self.data_close();
            return NbStatus::Failed;
        }
        self.lastch = 0;
        self.nb = true;
        self.nb_continue_read(out)
    }

    /// `ftp_nb_continue_read`: one download step.
    pub fn nb_continue_read(&mut self, out: &mut dyn Sink) -> NbStatus {
        let ready = match self.data.as_mut().and_then(|d| d.chan.as_mut()) {
            Some(c) => c.available(Duration::from_secs(1), false, &mut self.warnings),
            None => return self.nb_bail(),
        };
        if !ready {
            return NbStatus::MoreData;
        }
        let ty = self.ty;
        let mut lastch = self.lastch;
        let mut buf = vec![0u8; BUFSIZE];
        match self.data_recv(&mut buf) {
            None => self.nb_bail(),
            Some(0) => {
                if ty == Some(TransferType::Ascii) && lastch == b'\r' {
                    out.write(b"\r");
                }
                self.data_close();
                if !self.getresp() || (self.resp != 226 && self.resp != 250) {
                    return self.nb_bail();
                }
                self.nb = false;
                NbStatus::Finished
            }
            Some(n) => {
                if ty == Some(TransferType::Ascii) {
                    let mut conv = Vec::with_capacity(n);
                    ascii_nb_chunk(&buf[..n], &mut lastch, &mut conv);
                    if !conv.is_empty() {
                        out.write(&conv);
                    }
                } else if !out.write(&buf[..n]) {
                    return self.nb_bail();
                }
                self.lastch = lastch;
                NbStatus::MoreData
            }
        }
    }

    fn nb_bail(&mut self) -> NbStatus {
        self.nb = false;
        self.data_close();
        NbStatus::Failed
    }

    /// `ftp_nb_put`: starts an upload and performs the first step.
    pub fn nb_put(
        &mut self,
        path: &[u8],
        src: &mut dyn Source,
        ty: TransferType,
        startpos: i64,
    ) -> NbStatus {
        self.sending = true;
        if !self.set_type(ty)
            || !self.getdata()
            || !self.rest(startpos)
            || !self.command(b"STOR", path, |r| r == 150 || r == 125)
            || !self.data_accept()
        {
            self.data_close();
            return NbStatus::Failed;
        }
        self.lastch = 0;
        self.nb = true;
        self.nb_continue_write(src)
    }

    /// `ftp_nb_continue_write`: one upload step.
    pub fn nb_continue_write(&mut self, src: &mut dyn Source) -> NbStatus {
        if self.data.is_none() {
            return self.nb_bail();
        }
        let ty = self.ty.unwrap_or(TransferType::Image);
        if !self.send_stream(src, ty, true) {
            return self.nb_bail();
        }
        if !src.eof() {
            return NbStatus::MoreData;
        }
        self.data_close();
        if !self.getresp() || (self.resp != 226 && self.resp != 250) {
            return self.nb_bail();
        }
        self.nb = false;
        NbStatus::Finished
    }
}

/// The text between the first `"` and the last `"` after it.
fn quoted(text: &[u8]) -> Option<Vec<u8>> {
    let start = text.iter().position(|&c| c == b'"')? + 1;
    let end = start + text[start..].iter().rposition(|&c| c == b'"')?;
    Some(text[start..end].to_vec())
}
