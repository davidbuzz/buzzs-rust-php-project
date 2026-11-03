//! Client TLS against a local test server.

mod common;
use common::*;
use php_openssl::tls::{connect, PeerFingerprint, TlsOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

fn server() -> u16 {
    use openssl::ssl::{SslAcceptor, SslFiletype, SslMethod};
    let mut b = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).unwrap();
    b.set_security_level(0);
    b.set_certificate_chain_file(fixture("cert.crt")).unwrap();
    b.set_private_key_file(fixture("private_rsa_1024.key"), SslFiletype::PEM)
        .unwrap();
    b.set_alpn_select_callback(|_, client| {
        openssl::ssl::select_next_proto(b"\x08http/1.1", client)
            .ok_or(openssl::ssl::AlpnError::NOACK)
    });
    let acceptor = b.build();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            if let Ok(mut tls) = acceptor.accept(s) {
                let mut buf = [0u8; 4];
                if tls.read_exact(&mut buf).is_ok() {
                    let _ = tls.write_all(b"pong");
                }
            }
        }
    });
    port
}

fn base() -> TlsOptions {
    TlsOptions {
        security_level: Some(0),
        ..TlsOptions::default()
    }
}

#[test]
fn tls_client_policies() {
    let port = server();
    let tcp = || TcpStream::connect(("127.0.0.1", port)).unwrap();

    // No verification: works, ALPN negotiated, peer cert captured.
    let opts = TlsOptions {
        verify_peer: Some(false),
        verify_peer_name: Some(false),
        alpn_protocols: Some("h2,http/1.1".into()),
        capture_peer_cert: true,
        ..base()
    };
    let mut c = connect(tcp(), Some("localhost"), &opts, "", "").unwrap();
    assert_eq!(c.alpn_protocol.as_deref(), Some(&b"http/1.1"[..]));
    c.stream.write_all(b"ping").unwrap();
    let mut buf = [0u8; 4];
    c.stream.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"pong");
    let mut o = php_openssl::OpenSsl::new();
    let cert = c.peer_certificate.as_ref().unwrap();
    let fp = o
        .x509_fingerprint(php_openssl::CertArg::Object(cert), b"sha1", false)
        .unwrap()
        .unwrap();
    assert_eq!(fp, b"6e6fd1ea10a5a23071d61c728ee9b40df6dbc33c");

    // Default verification with a self-signed, expired cert fails.
    let err = connect(tcp(), Some("localhost"), &base(), "", "").unwrap_err();
    assert_eq!(err.warnings.len(), 2);
    assert!(err.warnings[0].starts_with(
        "SSL operation failed with code 1. OpenSSL Error messages:\nerror:0A000086:SSL routines::"
    ));
    assert_eq!(err.warnings[1], "Failed to enable crypto");

    // Fingerprint pinning trumps name checks.
    let opts = TlsOptions {
        verify_peer: Some(false),
        verify_peer_name: Some(false),
        peer_fingerprint: Some(PeerFingerprint::Hex(
            b"6E6FD1EA10A5A23071D61C728EE9B40DF6DBC33C".to_vec(),
        )),
        ..base()
    };
    assert!(connect(tcp(), Some("localhost"), &opts, "", "").is_ok());
    let opts = TlsOptions {
        verify_peer: Some(false),
        verify_peer_name: Some(false),
        peer_fingerprint: Some(PeerFingerprint::Hex(vec![b'0'; 40])),
        ..base()
    };
    assert_eq!(
        connect(tcp(), Some("localhost"), &opts, "", "")
            .unwrap_err()
            .warnings,
        ["peer_fingerprint match failure", "Failed to enable crypto"]
    );

    // Name verification against the CN.
    let opts = TlsOptions {
        verify_peer: Some(false),
        ..base()
    };
    assert_eq!(
        connect(tcp(), Some("localhost"), &opts, "", "")
            .unwrap_err()
            .warnings,
        [
            "Peer certificate CN=`Henrique do N. Angelo' did not match expected CN=`localhost'",
            "Failed to enable crypto"
        ]
    );
    let opts = TlsOptions {
        verify_peer: Some(false),
        peer_name: Some("Henrique do N. Angelo".into()),
        ..base()
    };
    assert!(connect(tcp(), Some("localhost"), &opts, "", "").is_ok());
}
