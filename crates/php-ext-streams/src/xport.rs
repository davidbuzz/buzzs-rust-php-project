//! Socket transport registry (`php_stream_xport_register`) and the hooks
//! other extensions use to plug in transports (`ssl://`, `tls://`, ...)
//! and crypto (`stream_socket_enable_crypto()`).
//!
//! Built-in `tcp`/`udp`/`unix` clients go through the `php-streams`
//! library; servers and accepted connections use [`crate::net::FdSockOps`].
//! A registered transport receives an [`XportRequest`] and returns a
//! [`Stream`] (typically wrapping a socket it connected itself). For TLS
//! upgrades of existing sockets, [`CryptoHooks::enable`] gets the stream
//! and may return a replacement; the resource keeps its identity
//! ([`crate::res::StreamCell::replace_stream`]).

use std::cell::RefCell;
use std::time::Duration;

use php_runtime::Value;
use php_streams::context::ContextRef;
use php_streams::stream::Stream;
use php_vm::ext::VmResult;
use php_vm::Vm;

/// What `stream_socket_client()` / `stream_socket_server()` /
/// `fsockopen()` asked a registered transport for.
#[derive(Debug, Clone)]
pub struct XportRequest {
    /// The scheme (`ssl`, `tls`, `tlsv1.2`, ...), lower-case.
    pub scheme: String,
    /// The full address as given (`ssl://host:443`).
    pub address: String,
    /// Host part (brackets of IPv6 literals removed).
    pub host: String,
    /// Port (0 when none was given).
    pub port: u16,
    /// Connect / accept timeout (`None`: wait forever).
    pub timeout: Option<Duration>,
    /// Whether a server (bind/listen) is requested.
    pub server: bool,
    /// `STREAM_CLIENT_*` / `STREAM_SERVER_*` flags.
    pub flags: i64,
    /// The persistent id for `pfsockopen()` / `STREAM_CLIENT_PERSISTENT`.
    pub persistent_id: Option<String>,
    /// The library context (options as library values).
    pub context: Option<ContextRef>,
    /// The `ssl` context options as PHP values (`[]` when none).
    pub ssl_options: Value,
}

/// A failed transport creation: `errno` and message for the
/// `$error_code`/`$error_message` out parameters and the
/// "Unable to connect to %s (%s)" warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XportError {
    /// `$error_code`.
    pub errno: i32,
    /// `$error_message`.
    pub message: String,
}

/// Creates a stream for a registered scheme.
pub type XportFactory = fn(&mut Vm, &XportRequest) -> VmResult<Result<Stream, XportError>>;

/// Wraps a connection accepted on a server stream created by this
/// transport (server handshake). Receives the listener and the plain
/// accepted socket stream.
pub type AcceptHook =
    fn(&mut Vm, &mut Stream, Stream, Option<Duration>) -> VmResult<Result<Stream, XportError>>;

/// A registered transport.
#[derive(Debug, Clone, Copy)]
pub struct Transport {
    /// Client/server creation.
    pub create: XportFactory,
    /// Accepted-connection wrapper.
    pub accept: Option<AcceptHook>,
}

/// `stream_socket_enable_crypto($stream, $enable, $method, $session)`:
/// returns the PHP return value (`true`, `false` or `0` for "would
/// block") and optionally a replacement stream for the resource.
pub type EnableCryptoHook = fn(
    &mut Vm,
    &mut Stream,
    bool,
    Option<i64>,
    Option<&mut Stream>,
) -> VmResult<(Value, Option<Stream>)>;

/// Crypto hooks of a TLS-capable extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct CryptoHooks {
    /// `stream_socket_enable_crypto()`.
    pub enable: Option<EnableCryptoHook>,
    /// `stream_socket_get_crypto_status()`.
    pub status: Option<fn(&mut Stream) -> i64>,
    /// `stream_get_meta_data()["crypto"]` (`None`: no crypto on the
    /// stream).
    pub meta: Option<fn(&Stream) -> Option<Value>>,
}

thread_local! {
    static TRANSPORTS: RefCell<Vec<(String, Transport)>> = const { RefCell::new(Vec::new()) };
    static CRYPTO: RefCell<CryptoHooks> = RefCell::new(CryptoHooks::default());
    /// Server streams created by registered transports: stream id → scheme.
    static SERVERS: RefCell<Vec<(u64, String)>> = const { RefCell::new(Vec::new()) };
}

/// Registers (or replaces) a transport for `scheme`.
pub fn register_transport(scheme: &str, t: Transport) {
    let scheme = scheme.to_ascii_lowercase();
    TRANSPORTS.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|(s, _)| *s != scheme);
        r.push((scheme, t));
    });
}

/// Installs the crypto hooks.
pub fn set_crypto_hooks(h: CryptoHooks) {
    CRYPTO.with(|c| *c.borrow_mut() = h);
}

/// The crypto hooks.
pub fn crypto_hooks() -> CryptoHooks {
    CRYPTO.with(|c| *c.borrow())
}

/// The transport registered for `scheme`.
pub fn find(scheme: &str) -> Option<Transport> {
    let scheme = scheme.to_ascii_lowercase();
    TRANSPORTS.with(|r| {
        r.borrow()
            .iter()
            .find(|(s, _)| *s == scheme)
            .map(|(_, t)| *t)
    })
}

/// Registered scheme names, in registration order.
pub fn schemes() -> Vec<String> {
    TRANSPORTS.with(|r| r.borrow().iter().map(|(s, _)| s.clone()).collect())
}

/// Remembers that server stream `id` belongs to `scheme`.
pub fn note_server(id: u64, scheme: &str) {
    SERVERS.with(|s| s.borrow_mut().push((id, scheme.to_string())));
}

/// The registered transport of a server stream.
pub fn server_transport(id: u64) -> Option<Transport> {
    let scheme = SERVERS.with(|s| {
        s.borrow()
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, sc)| sc.clone())
    })?;
    find(&scheme)
}

/// Splits `scheme://rest` (`tcp` when there is no scheme).
pub fn split_scheme(addr: &str) -> (String, &str) {
    match addr.find("://") {
        Some(i) if i > 0 => (addr[..i].to_ascii_lowercase(), &addr[i + 3..]),
        _ => ("tcp".to_string(), addr),
    }
}

/// `parse_ip_address_ex()`: `host:port` / `[v6]:port`; the port is read
/// like `atoi` (trailing junk ignored).
pub fn parse_host_port(rest: &str) -> Option<(String, u16)> {
    let (host, port) = if let Some(r) = rest.strip_prefix('[') {
        let end = r.find(']')?;
        (r[..end].to_string(), r[end + 1..].strip_prefix(':')?)
    } else {
        let c = rest.rfind(':')?;
        (rest[..c].to_string(), &rest[c + 1..])
    };
    let digits: String = port.chars().take_while(char::is_ascii_digit).collect();
    let port = digits.parse::<u32>().ok().filter(|p| *p <= 65535)?;
    Some((host, u16::try_from(port).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port() {
        assert_eq!(parse_host_port("[::1]:1337/"), Some(("::1".into(), 1337)));
        assert_eq!(
            parse_host_port("127.0.0.1:80"),
            Some(("127.0.0.1".into(), 80))
        );
        assert_eq!(parse_host_port("abc"), None);
        assert_eq!(split_scheme("ssl://x:1").0, "ssl");
        assert_eq!(split_scheme("x:1").0, "tcp");
    }

    #[test]
    fn registry() {
        fn f(_: &mut Vm, _: &XportRequest) -> VmResult<Result<Stream, XportError>> {
            Ok(Err(XportError {
                errno: 0,
                message: String::new(),
            }))
        }
        register_transport(
            "SSL",
            Transport {
                create: f,
                accept: None,
            },
        );
        assert!(find("ssl").is_some());
        assert!(schemes().contains(&"ssl".to_string()));
    }
}
