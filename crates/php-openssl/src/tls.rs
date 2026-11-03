//! The `ssl://` / `tls://` stream transport (`xp_ssl.c`): context option
//! table, `crypto_method` bitmask, and a client-side TLS handshake over any
//! `Read + Write` transport — what `https://` and `stream_socket_client`
//! with crypto need.
//!
//! # `ssl` stream context options (as read by `xp_ssl.c`)
//!
//! | option | type | default | meaning |
//! |---|---|---|---|
//! | `verify_peer` | bool | true for clients | verify the certificate chain |
//! | `verify_peer_name` | bool | true for clients | match `peer_name` against SAN/CN |
//! | `allow_self_signed` | bool | false | accept depth-0 self-signed certs |
//! | `verify_depth` | int | 9 | maximum chain depth |
//! | `cafile` / `capath` | string | `openssl.cafile` / `openssl.capath` INI, else system default | trust anchors |
//! | `peer_name` | string | host of the URL | expected name |
//! | `peer_fingerprint` | string\|array | – | md5/sha1 hex string or `[algo => hex]` map; trumps chain checks |
//! | `ciphers` | string | `DEFAULT` | `SSL_CTX_set_cipher_list` |
//! | `security_level` | int | library default | 0–5 |
//! | `alpn_protocols` | string | – | comma-separated list |
//! | `SNI_enabled` | bool | true | send server name |
//! | `crypto_method` | int | `STREAM_CRYPTO_METHOD_TLS_CLIENT` | protocol bitmask (see [`crypto_method`]) |
//! | `min_proto_version` / `max_proto_version` | int | – | override `crypto_method` |
//! | `local_cert` / `local_pk` / `passphrase` | string | – | client certificate |
//! | `disable_compression` | bool | true | |
//! | `capture_peer_cert` / `capture_peer_cert_chain` | bool | false | expose `peer_certificate(_chain)` |
//! | `capture_session_meta` | bool | false | |
//! | server-only: `SNI_server_certs`, `dh_param`, `honor_cipher_order`, `single_dh_use`, `reneg_limit`, `reneg_window`, `session_*`, `no_ticket`, `num_tickets`, PSK and early-data callbacks | | | not implemented |
//!
//! # `stream_socket_enable_crypto` state machine
//!
//! `enable` on a plain socket → build `SSL_CTX` from the options (warnings:
//! "Security level must be between 0 and 5", "Failed parsing comma-separated
//! TLS ALPN protocol string", …) → `SSL_connect` (blocking; retried on
//! `WANT_READ/WRITE`) → peer verification policy ("Could not verify peer:
//! code:%d %s", "Peer certificate CN=`%s' did not match expected CN=`%s'",
//! "`peer_fingerprint` match failure") → on any failure the transport warns
//! `"Failed to enable crypto"` and the call returns `false`. Handshake
//! errors are reported as `"SSL operation failed with code %d. OpenSSL Error
//! messages:\n<one line per queued error>"`. `disable` performs
//! `SSL_shutdown`. Servers (`stream_socket_server` + accept) are not
//! implemented.

use std::io::{Read, Write};

use openssl::ssl::{HandshakeError, SslConnector, SslMethod, SslStream, SslVerifyMode, SslVersion};
use openssl::x509::X509VerifyResult;

use crate::args::Certificate;
use crate::ffi;
use crate::util::bin2hex;

/// `STREAM_CRYPTO_METHOD_*` bit values.
pub mod crypto_method {
    /// Client flag bit.
    pub const CLIENT: i64 = 1;
    /// `SSLv2`.
    pub const SSLV2: i64 = 1 << 1;
    /// `SSLv3`.
    pub const SSLV3: i64 = 1 << 2;
    /// TLS 1.0.
    pub const TLSV1_0: i64 = 1 << 3;
    /// TLS 1.1.
    pub const TLSV1_1: i64 = 1 << 4;
    /// TLS 1.2.
    pub const TLSV1_2: i64 = 1 << 5;
    /// TLS 1.3.
    pub const TLSV1_3: i64 = 1 << 6;
    /// `STREAM_CRYPTO_METHOD_TLS_CLIENT` (the default for `tls://`).
    pub const TLS_CLIENT: i64 = TLSV1_0 | TLSV1_1 | TLSV1_2 | TLSV1_3 | CLIENT;
    /// `STREAM_CRYPTO_METHOD_ANY_CLIENT`.
    pub const ANY_CLIENT: i64 = SSLV2 | SSLV3 | TLS_CLIENT;
}

/// `peer_fingerprint` option value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerFingerprint {
    /// A hex digest; md5 (32 chars) or sha1 (40 chars) chosen by length.
    Hex(Vec<u8>),
    /// `[algo => hex]` pairs; all must match.
    Map(Vec<(Vec<u8>, Vec<u8>)>),
    /// Any other type ("must be a string or an array").
    Invalid,
}

/// Client-relevant `ssl` context options.
#[derive(Debug, Clone, Default)]
pub struct TlsOptions {
    /// `verify_peer` (`None` = default true).
    pub verify_peer: Option<bool>,
    /// `verify_peer_name` (`None` = default true).
    pub verify_peer_name: Option<bool>,
    /// `allow_self_signed`.
    pub allow_self_signed: bool,
    /// `verify_depth`.
    pub verify_depth: Option<u32>,
    /// `cafile` (falls back to `openssl.cafile`).
    pub cafile: Option<String>,
    /// `capath` (falls back to `openssl.capath`).
    pub capath: Option<String>,
    /// `peer_name`.
    pub peer_name: Option<String>,
    /// `peer_fingerprint`.
    pub peer_fingerprint: Option<PeerFingerprint>,
    /// `ciphers`.
    pub ciphers: Option<String>,
    /// `security_level`.
    pub security_level: Option<i64>,
    /// `alpn_protocols`.
    pub alpn_protocols: Option<String>,
    /// `SNI_enabled` (`None` = default true).
    pub sni_enabled: Option<bool>,
    /// `crypto_method` (`None` = TLS client).
    pub crypto_method: Option<i64>,
    /// `local_cert` path.
    pub local_cert: Option<String>,
    /// `local_pk` path.
    pub local_pk: Option<String>,
    /// `capture_peer_cert`.
    pub capture_peer_cert: bool,
    /// `capture_peer_cert_chain`.
    pub capture_peer_cert_chain: bool,
}

/// An established client connection.
pub struct TlsClient<S> {
    /// The encrypted stream (implements `Read + Write`).
    pub stream: SslStream<S>,
    /// `peer_certificate` (when captured).
    pub peer_certificate: Option<Certificate>,
    /// `peer_certificate_chain` (when captured).
    pub peer_certificate_chain: Vec<Certificate>,
    /// Negotiated ALPN protocol.
    pub alpn_protocol: Option<Vec<u8>>,
}

impl<S> std::fmt::Debug for TlsClient<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsClient")
            .field("alpn_protocol", &self.alpn_protocol)
            .finish_non_exhaustive()
    }
}

/// A failed handshake: the stream warnings PHP would emit, in order
/// (the last one is always `"Failed to enable crypto"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsError {
    /// Warning texts.
    pub warnings: Vec<String>,
}

impl std::fmt::Display for TlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.warnings.join("; "))
    }
}

impl std::error::Error for TlsError {}

/// `php_openssl_matches_wildcard_name`.
pub fn matches_wildcard_name(subject: &str, cert: &str) -> bool {
    if subject.eq_ignore_ascii_case(cert) {
        return true;
    }
    let Some(w) = cert.find('*') else {
        return false;
    };
    if cert[..w].contains('.') {
        return false;
    }
    let prefix = &cert[..w];
    if !prefix.is_empty()
        && !subject
            .get(..prefix.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
    {
        return false;
    }
    let suffix = &cert[w + 1..];
    if suffix.len() + prefix.len() <= subject.len() {
        let tail = &subject[subject.len() - suffix.len()..];
        let middle = &subject[prefix.len()..subject.len() - suffix.len()];
        return tail.eq_ignore_ascii_case(suffix) && !middle.contains('.');
    }
    false
}

/// `min_proto`/`max_proto` from a `crypto_method` bitmask.
fn proto_range(method: i64) -> (Option<SslVersion>, Option<SslVersion>) {
    use crypto_method::{TLSV1_0, TLSV1_1, TLSV1_2, TLSV1_3};
    let table = [
        (TLSV1_0, SslVersion::TLS1),
        (TLSV1_1, SslVersion::TLS1_1),
        (TLSV1_2, SslVersion::TLS1_2),
        (TLSV1_3, SslVersion::TLS1_3),
    ];
    let set: Vec<SslVersion> = table
        .iter()
        .filter(|(b, _)| method & b != 0)
        .map(|(_, v)| *v)
        .collect();
    (set.first().copied(), set.last().copied())
}

fn error_lines(stack: &openssl::error::ErrorStack) -> String {
    stack
        .errors()
        .iter()
        .map(|e| ffi::err_error_string(e.code()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn to_certificate(x: &openssl::x509::X509Ref) -> Option<Certificate> {
    let pem = x.to_pem().ok()?;
    let bio = ffi::Bio::mem_buf(&pem)?;
    ffi::X509Cert::read_pem(&bio).map(|x509| Certificate { x509 })
}

fn fingerprint_hex(x: &openssl::x509::X509Ref, algo: &[u8]) -> Option<Vec<u8>> {
    let md = openssl::hash::MessageDigest::from_name(std::str::from_utf8(algo).ok()?)?;
    x.digest(md).ok().map(|d| bin2hex(&d))
}

/// Runs PHP's peer verification policy; returns warnings on failure.
fn verify_policy(
    ssl: &openssl::ssl::SslRef,
    host: Option<&str>,
    opts: &TlsOptions,
) -> Result<(), String> {
    let must_peer = opts.verify_peer.unwrap_or(true);
    let must_name = opts.verify_peer_name.unwrap_or(true);
    let peer = ssl.peer_certificate();
    if (must_peer || must_name || opts.peer_fingerprint.is_some()) && peer.is_none() {
        return Err("Could not get peer certificate".into());
    }
    let Some(peer) = peer else { return Ok(()) };
    if must_peer {
        let r = ssl.verify_result();
        let self_signed = r.as_raw() == 18; // X509_V_ERR_DEPTH_ZERO_SELF_SIGNED_CERT
        if r != X509VerifyResult::OK && !(self_signed && opts.allow_self_signed) {
            return Err(format!(
                "Could not verify peer: code:{} {}",
                r.as_raw(),
                r.error_string()
            ));
        }
    }
    match &opts.peer_fingerprint {
        None => {}
        Some(PeerFingerprint::Invalid) => {
            return Err("Expected peer fingerprint must be a string or an array".into())
        }
        Some(PeerFingerprint::Hex(h)) => {
            let algo: &[u8] = match h.len() {
                32 => b"md5",
                40 => b"sha1",
                _ => return Err("peer_fingerprint length doesn't match a md5 or sha1 hash".into()),
            };
            if !fingerprint_hex(&peer, algo).is_some_and(|f| f.eq_ignore_ascii_case(h)) {
                return Err("peer_fingerprint match failure".into());
            }
        }
        Some(PeerFingerprint::Map(m)) => {
            if m.is_empty() {
                return Err(
                    "Invalid peer_fingerprint array; [algo => fingerprint] form required".into(),
                );
            }
            for (algo, h) in m {
                let Some(f) = fingerprint_hex(&peer, algo) else {
                    return Err(format!(
                        "Unknown digest algorithm {}",
                        String::from_utf8_lossy(algo)
                    ));
                };
                if !f.eq_ignore_ascii_case(h) {
                    return Err("peer_fingerprint match failure".into());
                }
            }
        }
    }
    if must_name {
        let Some(name) = opts.peer_name.as_deref().or(host) else {
            return Err(String::new());
        };
        let mut has_service_id = false;
        if let Some(sans) = peer.subject_alt_names() {
            for san in &sans {
                if let Some(dns) = san.dnsname() {
                    has_service_id = true;
                    let dns = dns.strip_suffix('.').unwrap_or(dns);
                    if matches_wildcard_name(name, dns) {
                        return Ok(());
                    }
                } else if let Some(ip) = san.ipaddress() {
                    if ip.len() == 4 && name == format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]) {
                        return Ok(());
                    }
                    if ip.len() == 16 {
                        if let (Ok(want), Ok(got)) =
                            (name.parse::<std::net::Ipv6Addr>(), <[u8; 16]>::try_from(ip))
                        {
                            if want.octets() == got {
                                return Ok(());
                            }
                        }
                    }
                } else if san.uri().is_some() {
                    has_service_id = true;
                }
            }
        }
        if has_service_id {
            return Err(format!(
                "Peer certificate subjectAltName did not match expected name `{name}'"
            ));
        }
        let cn = peer
            .subject_name()
            .entries_by_nid(openssl::nid::Nid::COMMONNAME)
            .next();
        let Some(cn) = cn else {
            return Err("Unable to locate peer certificate CN".into());
        };
        let raw = cn.data().as_slice();
        let shown = String::from_utf8_lossy(raw).into_owned();
        if raw.contains(&0) {
            return Err(format!("Peer certificate CN=`{shown}' is malformed"));
        }
        if !matches_wildcard_name(name, &shown) {
            return Err(format!(
                "Peer certificate CN=`{shown}' did not match expected CN=`{name}'"
            ));
        }
    }
    Ok(())
}

/// Performs a client handshake over `stream` (`stream_socket_enable_crypto`
/// / `https://`). `host` is the URL host used for SNI and name checks;
/// `ini_cafile`/`ini_capath` are the `openssl.*` INI fallbacks.
pub fn connect<S: Read + Write>(
    stream: S,
    host: Option<&str>,
    opts: &TlsOptions,
    ini_cafile: &str,
    ini_capath: &str,
) -> Result<TlsClient<S>, TlsError> {
    let fail = |mut w: Vec<String>| {
        w.retain(|s| !s.is_empty());
        w.push("Failed to enable crypto".into());
        Err(TlsError { warnings: w })
    };
    let Ok(mut b) = SslConnector::builder(SslMethod::tls_client()) else {
        return fail(vec!["SSL context creation failure".into()]);
    };
    let (min, max) = proto_range(opts.crypto_method.unwrap_or(crypto_method::TLS_CLIENT));
    if min.is_none() {
        return fail(vec!["No supported crypto method in crypto_method".into()]);
    }
    let _ = b.set_min_proto_version(min);
    let _ = b.set_max_proto_version(max);
    if let Some(level) = opts.security_level {
        let Ok(level) =
            u32::try_from(level)
                .map_err(drop)
                .and_then(|l| if l <= 5 { Ok(l) } else { Err(()) })
        else {
            return fail(vec!["Security level must be between 0 and 5".into()]);
        };
        b.set_security_level(level);
    }
    if let Some(c) = &opts.ciphers {
        if b.set_cipher_list(c).is_err() {
            return fail(vec![format!("Failed setting cipher list: `{c}'")]);
        }
    }
    if let Some(alpn) = &opts.alpn_protocols {
        let mut wire = Vec::new();
        for p in alpn.split(',') {
            if p.is_empty() || p.len() > 255 {
                return fail(vec![
                    "Failed setting TLS ALPN protocols, protocol names must not be empty".into(),
                ]);
            }
            #[allow(clippy::cast_possible_truncation)]
            wire.push(p.len() as u8);
            wire.extend_from_slice(p.as_bytes());
        }
        let _ = b.set_alpn_protos(&wire);
    }
    let must_verify = opts.verify_peer.unwrap_or(true);
    if must_verify {
        let cafile = opts
            .cafile
            .clone()
            .or_else(|| (!ini_cafile.is_empty()).then(|| ini_cafile.to_string()));
        let capath = opts
            .capath
            .clone()
            .or_else(|| (!ini_capath.is_empty()).then(|| ini_capath.to_string()));
        if cafile.is_some() || capath.is_some() {
            let store = b.cert_store_mut();
            let mut ok = true;
            if let Some(f) = &cafile {
                if let Ok(pem) = std::fs::read(f) {
                    if let Ok(certs) = openssl::x509::X509::stack_from_pem(&pem) {
                        for c in certs {
                            let _ = store.add_cert(c);
                        }
                    } else {
                        ok = false;
                    }
                } else {
                    ok = false;
                }
            }
            if !ok {
                return fail(vec![format!(
                    "Failed to load cafile `{}'",
                    cafile.unwrap_or_default()
                )]);
            }
            if let Some(p) = &capath {
                let _ = b.load_verify_locations(None, Some(std::path::Path::new(p)));
            }
        } else if b.set_default_verify_paths().is_err() {
            return fail(vec![
                "Unable to set default verify locations and no CA settings specified".into(),
            ]);
        }
        let allow_self = opts.allow_self_signed;
        let depth = opts.verify_depth.unwrap_or(9);
        b.set_verify_callback(SslVerifyMode::PEER, move |ok, ctx| {
            let mut ret = ok;
            if ctx.error().as_raw() == 18 && allow_self {
                ret = true;
            }
            if ctx.error_depth() > depth {
                ret = false;
                ctx.set_error(ffi::verify_result_chain_too_long());
            }
            ret
        });
    } else {
        b.set_verify(SslVerifyMode::NONE);
    }
    if let Some(cert) = &opts.local_cert {
        if b.set_certificate_chain_file(cert).is_err() {
            return fail(vec![format!(
                "Unable to set local cert chain file `{cert}'; Check that your cafile/capath settings include details of your certificate and its issuer"
            )]);
        }
        let pk = opts.local_pk.as_deref().unwrap_or(cert);
        if b.set_private_key_file(pk, openssl::ssl::SslFiletype::PEM)
            .is_err()
        {
            return fail(vec![format!("Unable to set private key file `{pk}'")]);
        }
        if b.check_private_key().is_err() {
            return fail(vec!["Private key does not match certificate!".into()]);
        }
    }
    let connector = b.build();
    let Ok(mut cfg) = connector.configure() else {
        return fail(vec!["SSL handle creation failure".into()]);
    };
    cfg.set_verify_hostname(false);
    cfg.set_use_server_name_indication(opts.sni_enabled.unwrap_or(true));
    let sni = opts.peer_name.as_deref().or(host).unwrap_or("");
    let ssl_stream = match cfg.connect(sni, stream) {
        Ok(s) => s,
        Err(HandshakeError::Failure(mid)) => {
            let e = mid.error();
            let lines = e.ssl_error().map(error_lines).unwrap_or_default();
            let code = e.code().as_raw();
            let msg = if lines.is_empty() {
                format!("SSL operation failed with code {code}. ")
            } else {
                format!("SSL operation failed with code {code}. OpenSSL Error messages:\n{lines}")
            };
            return fail(vec![msg]);
        }
        Err(HandshakeError::SetupFailure(e)) => {
            return fail(vec![format!(
                "SSL handle creation failure: {}",
                error_lines(&e)
            )])
        }
        Err(HandshakeError::WouldBlock(_)) => return fail(vec!["SSL: Handshake timed out".into()]),
    };
    if let Err(w) = verify_policy(ssl_stream.ssl(), host, opts) {
        return fail(vec![w]);
    }
    let ssl = ssl_stream.ssl();
    let peer_certificate = if opts.capture_peer_cert {
        ssl.peer_certificate().as_deref().and_then(to_certificate)
    } else {
        None
    };
    let peer_certificate_chain = if opts.capture_peer_cert_chain {
        ssl.peer_cert_chain()
            .map(|c| c.iter().filter_map(to_certificate).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let alpn_protocol = ssl.selected_alpn_protocol().map(<[u8]>::to_vec);
    Ok(TlsClient {
        stream: ssl_stream,
        peer_certificate,
        peer_certificate_chain,
        alpn_protocol,
    })
}

/// The certificate blocks `php_openssl_load_stream_cafile()` hands to
/// `PEM_read_bio_X509()`: each runs from a line that is exactly
/// `-----BEGIN CERTIFICATE-----` (with a `\n` or `\r\n` ending) to the next
/// `-----END CERTIFICATE-----` line (which may lack its line ending at the
/// end of the data), both included. A block left open at the end of the
/// data is dropped, and so is anything between blocks.
#[must_use]
pub fn cafile_pem_blocks(data: &[u8]) -> Vec<&[u8]> {
    const BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----";
    const END: &[u8] = b"-----END CERTIFICATE-----";
    let mut blocks = Vec::new();
    let mut start = None;
    let mut pos = 0;
    while pos < data.len() {
        // `php_stream_get_line()`: a line keeps its `\n`.
        let end = data[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(data.len(), |i| pos + i + 1);
        let line = &data[pos..end];
        let body = line
            .strip_suffix(b"\r\n")
            .or_else(|| line.strip_suffix(b"\n"));
        match start {
            None if body == Some(BEGIN) => start = Some(pos),
            Some(s) if body.unwrap_or(line) == END => {
                blocks.push(&data[s..end]);
                start = None;
            }
            _ => {}
        }
        pos = end;
    }
    blocks
}

/// `php_openssl_load_stream_cafile()` once the stream has been read: adds
/// every certificate block of `pem` (see [`cafile_pem_blocks`]) that
/// parses to `store`. Returns how many were added; `0` is the caller's cue
/// for the "no valid certs found cafile stream" warning.
pub fn add_cafile_certs(
    store: &mut openssl::x509::store::X509StoreBuilderRef,
    pem: &[u8],
) -> usize {
    let mut added = 0;
    for block in cafile_pem_blocks(pem) {
        if let Ok(cert) = openssl::x509::X509::from_pem(block) {
            if store.add_cert(cert).is_ok() {
                added += 1;
            }
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(matches_wildcard_name("www.example.com", "*.example.com"));
        assert!(matches_wildcard_name("WWW.example.com", "www.EXAMPLE.com"));
        assert!(!matches_wildcard_name("a.b.example.com", "*.example.com"));
        assert!(matches_wildcard_name(
            "foo1.example.com",
            "foo*.example.com"
        ));
        assert!(!matches_wildcard_name("www.example.com", "www.*.com"));
        assert!(!matches_wildcard_name("example.com", "*.example.com"));
    }

    #[test]
    fn proto_ranges() {
        assert_eq!(
            proto_range(crypto_method::TLS_CLIENT),
            (Some(SslVersion::TLS1), Some(SslVersion::TLS1_3))
        );
        assert_eq!(
            proto_range(crypto_method::TLSV1_2 | crypto_method::CLIENT),
            (Some(SslVersion::TLS1_2), Some(SslVersion::TLS1_2))
        );
        assert_eq!(proto_range(crypto_method::SSLV3), (None, None));
    }

    #[test]
    fn handshake_failure_reports_php_warnings() {
        // A peer that immediately closes: the handshake fails.
        struct Closed;
        impl Read for Closed {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Ok(0)
            }
        }
        impl Write for Closed {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let err = connect(Closed, Some("example.com"), &TlsOptions::default(), "", "").unwrap_err();
        assert_eq!(
            err.warnings.last().map(String::as_str),
            Some("Failed to enable crypto")
        );
        assert!(err.warnings[0].starts_with("SSL operation failed with code"));
    }

    #[test]
    fn security_level_validation() {
        let opts = TlsOptions {
            security_level: Some(9),
            ..TlsOptions::default()
        };
        let err = connect(std::io::Cursor::new(Vec::new()), None, &opts, "", "").unwrap_err();
        assert_eq!(
            err.warnings,
            [
                "Security level must be between 0 and 5",
                "Failed to enable crypto"
            ]
        );
    }

    fn ca_pem(cn: &str) -> Vec<u8> {
        use openssl::x509::{X509Builder, X509NameBuilder};
        let key = openssl::pkey::PKey::from_ec_key(
            openssl::ec::EcKey::generate(
                &openssl::ec::EcGroup::from_curve_name(openssl::nid::Nid::X9_62_PRIME256V1)
                    .expect("curve"),
            )
            .expect("ec key"),
        )
        .expect("pkey");
        let mut name = X509NameBuilder::new().expect("name");
        name.append_entry_by_text("CN", cn).expect("cn");
        let name = name.build();
        let mut b = X509Builder::new().expect("builder");
        b.set_version(2).expect("version");
        b.set_subject_name(&name).expect("subject");
        b.set_issuer_name(&name).expect("issuer");
        b.set_pubkey(&key).expect("pubkey");
        b.set_not_before(&openssl::asn1::Asn1Time::days_from_now(0).expect("time"))
            .expect("nb");
        b.set_not_after(&openssl::asn1::Asn1Time::days_from_now(1).expect("time"))
            .expect("na");
        b.sign(&key, openssl::hash::MessageDigest::sha256())
            .expect("sign");
        b.build().to_pem().expect("pem")
    }

    #[test]
    fn cafile_blocks_follow_the_line_scanner() {
        let a = ca_pem("a");
        let crlf: Vec<u8> = String::from_utf8_lossy(&ca_pem("b"))
            .replace('\n', "\r\n")
            .into_bytes();
        let mut data = b"junk\n -----BEGIN CERTIFICATE-----\n".to_vec();
        data.extend_from_slice(&a);
        data.extend_from_slice(b"between\n");
        data.extend_from_slice(&crlf);
        // The last END line may lack its newline.
        let tail = ca_pem("c");
        data.extend_from_slice(&tail[..tail.len() - 1]);
        let blocks = cafile_pem_blocks(&data);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0], a.as_slice());
        assert_eq!(blocks[1], crlf.as_slice());
        assert_eq!(blocks[2], &tail[..tail.len() - 1]);
        // An unterminated block is dropped.
        assert!(cafile_pem_blocks(&a[..a.len() - 10]).is_empty());
        assert!(cafile_pem_blocks(b"").is_empty());
        assert!(cafile_pem_blocks(b"plain text\n").is_empty());
    }

    #[test]
    fn cafile_certs_are_added_to_the_store() {
        let mut data = ca_pem("one");
        data.extend_from_slice(
            b"-----BEGIN CERTIFICATE-----\nnot base64\n-----END CERTIFICATE-----\n",
        );
        data.extend_from_slice(&ca_pem("two"));
        let mut store = openssl::x509::store::X509StoreBuilder::new().expect("store");
        assert_eq!(add_cafile_certs(&mut store, &data), 2);
        assert_eq!(store.build().all_certificates().len(), 2);
        let mut empty = openssl::x509::store::X509StoreBuilder::new().expect("store");
        assert_eq!(add_cafile_certs(&mut empty, b"no certs here\n"), 0);
    }
}
