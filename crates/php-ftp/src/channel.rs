//! Sockets with ext/ftp's I/O semantics (`my_send`, `my_recv`,
//! `my_accept`, `data_available`): plain TCP or TLS over TCP, every
//! operation bounded by the connection timeout, failures reported as the
//! `strerror()` texts PHP prints.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use openssl::ssl::{ShutdownResult, SslStream};

/// `ETIMEDOUT`.
pub(crate) const ETIMEDOUT: i32 = 110;

/// A connected socket, optionally wrapped in TLS.
#[derive(Debug)]
pub(crate) enum Channel {
    /// Plain TCP.
    Plain(TcpStream),
    /// TLS over TCP.
    Tls(Box<SslStream<TcpStream>>),
}

impl Channel {
    /// The underlying TCP socket.
    pub(crate) fn tcp(&self) -> &TcpStream {
        match self {
            Channel::Plain(s) => s,
            Channel::Tls(s) => s.get_ref(),
        }
    }

    /// Whether TLS is active on this channel.
    pub(crate) fn is_tls(&self) -> bool {
        matches!(self, Channel::Tls(_))
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Channel::Plain(s) => s.read(buf),
            Channel::Tls(s) => s.read(buf),
        }
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Channel::Plain(s) => s.write(buf),
            Channel::Tls(s) => s.write(buf),
        }
    }

    /// `my_recv`: waits up to `timeout` for data, then reads. `None` on
    /// failure (a timeout adds PHP's warning to `warns`).
    pub(crate) fn recv(
        &mut self,
        buf: &mut [u8],
        timeout: Duration,
        warns: &mut Vec<Vec<u8>>,
    ) -> Option<usize> {
        let _ = self.tcp().set_read_timeout(Some(timeout));
        loop {
            match self.read(buf) {
                Ok(n) => return Some(n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if is_timeout(&e) => {
                    warns.push(strerror(ETIMEDOUT).into_bytes());
                    return None;
                }
                Err(_) => {
                    if self.is_tls() {
                        warns.push(b"SSL read failed".to_vec());
                    }
                    return None;
                }
            }
        }
    }

    /// `my_send`: writes all of `buf`, each chunk bounded by `timeout`.
    pub(crate) fn send(&mut self, buf: &[u8], timeout: Duration, warns: &mut Vec<Vec<u8>>) -> bool {
        let _ = self.tcp().set_write_timeout(Some(timeout));
        let mut rest = buf;
        while !rest.is_empty() {
            match self.write(rest) {
                Ok(0) => return false,
                Ok(n) => rest = &rest[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if is_timeout(&e) => {
                    warns.push(strerror(ETIMEDOUT).into_bytes());
                    return false;
                }
                Err(_) => {
                    if self.is_tls() {
                        warns.push(b"SSL write failed".to_vec());
                    }
                    return false;
                }
            }
        }
        true
    }

    /// `data_available`: whether the socket becomes readable within
    /// `timeout` (EOF counts as readable). A timeout warns unless `silent`.
    pub(crate) fn available(
        &mut self,
        timeout: Duration,
        silent: bool,
        warns: &mut Vec<Vec<u8>>,
    ) -> bool {
        if let Channel::Tls(s) = self {
            if s.ssl().pending() > 0 {
                return true;
            }
        }
        let tcp = self.tcp();
        let _ = tcp.set_read_timeout(Some(timeout));
        let mut probe = [0u8; 1];
        loop {
            match tcp.peek(&mut probe) {
                Ok(_) => return true,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if is_timeout(&e) => {
                    if !silent {
                        warns.push(strerror(ETIMEDOUT).into_bytes());
                    }
                    return false;
                }
                Err(e) => {
                    warns.push(io_strerror(&e).0.into_bytes());
                    return false;
                }
            }
        }
    }

    /// Closes the channel; a TLS channel performs PHP's bidirectional
    /// shutdown (`ftp_ssl_shutdown`), draining session tickets.
    pub(crate) fn close(self, warns: &mut Vec<Vec<u8>>) {
        if let Channel::Tls(mut s) = self {
            let done = match s.shutdown() {
                Ok(ShutdownResult::Received) => true,
                Ok(ShutdownResult::Sent) => false,
                Err(e) => {
                    if e.io_error().is_none() {
                        warns.push(b"SSL_shutdown failed".to_vec());
                    }
                    true
                }
            };
            if !done {
                let _ = s.get_ref().set_read_timeout(Some(Duration::from_secs(1)));
                let mut buf = [0u8; 256];
                // Read until close_notify / EOF / error.
                while let Ok(n) = s.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                }
            }
        }
    }
}

/// Whether an I/O error is a socket timeout.
pub(crate) fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// `strerror(errno)` for the errors ext/ftp reports.
pub(crate) fn strerror(errno: i32) -> String {
    let e = io::Error::from_raw_os_error(errno);
    strip_os_error(&e.to_string())
}

/// `(strerror(errno), errno)` of an I/O error.
pub(crate) fn io_strerror(e: &io::Error) -> (String, i32) {
    match e.raw_os_error() {
        Some(n) => (strip_os_error(&e.to_string()), n),
        None if is_timeout(e) => (strerror(ETIMEDOUT), ETIMEDOUT),
        None => (e.to_string(), 0),
    }
}

/// `"Connection refused (os error 111)"` → `"Connection refused"`.
fn strip_os_error(s: &str) -> String {
    match s.rfind(" (os error ") {
        Some(p) => s[..p].to_string(),
        None => s.to_string(),
    }
}

/// `php_network_connect_socket_to_host`: resolves `host` and connects to
/// the first address that accepts within `timeout`. Resolution failures
/// produce PHP's `php_network_getaddresses` warning; connection failures
/// are silent (ext/ftp passes no error string).
pub(crate) fn connect(
    host: &str,
    port: u16,
    timeout: Duration,
    warns: &mut Vec<Vec<u8>>,
) -> Option<TcpStream> {
    let addrs: Vec<SocketAddr> = match (host, port).to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(e) => {
            let msg = e.to_string();
            let reason = msg
                .strip_prefix("failed to lookup address information: ")
                .unwrap_or(&msg);
            warns.push(
                format!("php_network_getaddresses: getaddrinfo for {host} failed: {reason}")
                    .into_bytes(),
            );
            return None;
        }
    };
    if addrs.is_empty() {
        warns.push(
            format!("php_network_getaddresses: getaddrinfo for {host} failed: Name or service not known")
                .into_bytes(),
        );
        return None;
    }
    addrs
        .iter()
        .find_map(|a| TcpStream::connect_timeout(a, timeout).ok())
}

/// `my_accept`: waits up to `timeout` for a connection on `listener`.
pub(crate) fn accept(
    listener: &TcpListener,
    timeout: Duration,
    warns: &mut Vec<Vec<u8>>,
) -> Option<TcpStream> {
    if listener.set_nonblocking(true).is_err() {
        return None;
    }
    let deadline = Instant::now().checked_add(timeout);
    loop {
        match listener.accept() {
            Ok((s, _)) => {
                let _ = s.set_nonblocking(false);
                return Some(s);
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::Interrupted =>
            {
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    warns.push(strerror(ETIMEDOUT).into_bytes());
                    return None;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => return None,
        }
    }
}

/// A timeout in seconds as a `Duration` (at least one second).
pub(crate) fn secs(timeout_sec: i64) -> Duration {
    Duration::from_secs(u64::try_from(timeout_sec).unwrap_or(1).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strerror_texts() {
        assert_eq!(strerror(ETIMEDOUT), "Connection timed out");
        assert_eq!(strerror(111), "Connection refused");
    }

    #[test]
    fn plain_roundtrip_and_timeout() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.write_all(b"hello").unwrap();
            let mut b = [0u8; 3];
            s.read_exact(&mut b).unwrap();
            b
        });
        let mut warns = Vec::new();
        let s = connect("127.0.0.1", addr.port(), Duration::from_secs(5), &mut warns).unwrap();
        let mut ch = Channel::Plain(s);
        let mut buf = [0u8; 16];
        let n = ch
            .recv(&mut buf, Duration::from_secs(5), &mut warns)
            .unwrap();
        assert_eq!(&buf[..n], b"hello");
        assert!(ch.send(b"abc", Duration::from_secs(5), &mut warns));
        assert_eq!(&t.join().unwrap(), b"abc");
        assert!(warns.is_empty());
    }

    #[test]
    fn accept_times_out() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut warns = Vec::new();
        assert!(accept(&l, Duration::from_millis(50), &mut warns).is_none());
        assert_eq!(warns, vec![b"Connection timed out".to_vec()]);
    }

    #[test]
    fn unresolvable_host_warns() {
        let mut warns = Vec::new();
        assert!(connect("totes.invalid", 21, Duration::from_secs(1), &mut warns).is_none());
        let w = String::from_utf8(warns.remove(0)).unwrap();
        assert!(w.starts_with("php_network_getaddresses: getaddrinfo for totes.invalid failed: "));
    }
}
