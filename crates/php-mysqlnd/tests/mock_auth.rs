//! Authentication plugins and TLS against an in-process mock server
//! (MariaDB can't exercise `caching_sha2_password` / `sha256_password`):
//! fast and full caching_sha2 auth (RSA with requested or configured key,
//! cleartext over TLS), auth switch, `sha256_password`, unknown plugins,
//! greeting errors and TLS peer verification.
#![allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::many_single_char_names
)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use openssl::pkey::PKey;
use openssl::rsa::{Padding, Rsa};
use openssl::ssl::{SslAcceptor, SslMethod, SslStream};
use php_mysqlnd::consts::*;
use php_mysqlnd::{Connection, OptionValue};

const SCRAMBLE: &[u8; 20] = b"0123456789abcdefghij";

enum S {
    Plain(TcpStream),
    Tls(Box<SslStream<TcpStream>>),
}

impl S {
    fn io(&mut self) -> &mut dyn ReadWriteDyn {
        match self {
            S::Plain(s) => s,
            S::Tls(t) => t.as_mut(),
        }
    }
}

trait ReadWriteDyn: Read + Write {}
impl<T: Read + Write> ReadWriteDyn for T {}

struct Mock {
    s: Option<S>,
    seq: u8,
}

impl Mock {
    fn read(&mut self) -> Vec<u8> {
        let io = self.s.as_mut().unwrap().io();
        let mut h = [0u8; 4];
        io.read_exact(&mut h).unwrap();
        let len = u32::from_le_bytes([h[0], h[1], h[2], 0]) as usize;
        assert_eq!(h[3], self.seq, "client sequence number");
        self.seq = self.seq.wrapping_add(1);
        let mut b = vec![0u8; len];
        io.read_exact(&mut b).unwrap();
        b
    }

    fn write(&mut self, payload: &[u8]) {
        let io = self.s.as_mut().unwrap().io();
        let mut out = (payload.len() as u32).to_le_bytes()[..3].to_vec();
        out.push(self.seq);
        self.seq = self.seq.wrapping_add(1);
        out.extend_from_slice(payload);
        io.write_all(&out).unwrap();
        io.flush().unwrap();
    }

    fn ok(&mut self) {
        self.write(&[0, 0, 0, 2, 0, 0, 0]);
    }

    fn err(&mut self, errno: u16, state: &str, msg: &str) {
        let mut p = vec![0xFF];
        p.extend_from_slice(&errno.to_le_bytes());
        p.push(b'#');
        p.extend_from_slice(state.as_bytes());
        p.extend_from_slice(msg.as_bytes());
        self.write(&p);
    }

    fn greeting(&mut self, plugin: &str, ssl: bool) {
        let mut caps = CLIENT_LONG_PASSWORD
            | CLIENT_LONG_FLAG
            | CLIENT_CONNECT_WITH_DB
            | CLIENT_PROTOCOL_41
            | CLIENT_TRANSACTIONS
            | CLIENT_SECURE_CONNECTION
            | CLIENT_MULTI_RESULTS
            | CLIENT_PLUGIN_AUTH
            | CLIENT_CONNECT_ATTRS
            | CLIENT_LOCAL_FILES;
        if ssl {
            caps |= CLIENT_SSL;
        }
        let mut g = vec![10u8];
        g.extend_from_slice(b"8.0.36-mock\0");
        g.extend_from_slice(&7u32.to_le_bytes());
        g.extend_from_slice(&SCRAMBLE[..8]);
        g.push(0);
        g.extend_from_slice(&(caps as u16).to_le_bytes());
        g.push(255); // utf8mb4_0900_ai_ci
        g.extend_from_slice(&2u16.to_le_bytes());
        g.extend_from_slice(&((caps >> 16) as u16).to_le_bytes());
        g.push(21);
        g.extend_from_slice(&[0u8; 10]);
        g.extend_from_slice(&SCRAMBLE[8..]);
        g.push(0);
        g.extend_from_slice(plugin.as_bytes());
        g.push(0);
        self.write(&g);
    }

    /// Parses a handshake response: (flags, user, auth data, db, plugin).
    fn handshake_response(&mut self) -> (u32, Vec<u8>, Vec<u8>, Vec<u8>, String) {
        let b = self.read();
        let flags = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let mut p = 32;
        let nul = b[p..].iter().position(|&x| x == 0).unwrap();
        let user = b[p..p + nul].to_vec();
        p += nul + 1;
        let alen = b[p] as usize;
        p += 1;
        let auth = b[p..p + alen].to_vec();
        p += alen;
        let mut db = Vec::new();
        if flags & CLIENT_CONNECT_WITH_DB != 0 {
            let nul = b[p..].iter().position(|&x| x == 0).unwrap();
            db = b[p..p + nul].to_vec();
            p += nul + 1;
        }
        let nul = b[p..].iter().position(|&x| x == 0).unwrap();
        let plugin = String::from_utf8(b[p..p + nul].to_vec()).unwrap();
        (flags, user, auth, db, plugin)
    }

    fn start_tls(&mut self, acceptor: &SslAcceptor) {
        let S::Plain(tcp) = self.s.take().unwrap() else {
            panic!()
        };
        let tls = acceptor.accept(tcp).unwrap();
        self.s = Some(S::Tls(Box::new(tls)));
    }

    /// Expects `COM_QUIT` (or EOF) after the test finished.
    fn finish(&mut self) {
        self.seq = 0;
        let io = self.s.as_mut().unwrap().io();
        let mut h = [0u8; 5];
        let _ = io.read_exact(&mut h);
    }
}

fn serve<F: FnOnce(&mut Mock) + Send + 'static>(f: F) -> (u16, thread::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut m = Mock {
            s: Some(S::Plain(s)),
            seq: 0,
        };
        f(&mut m);
    });
    (port, h)
}

fn connect(
    port: u16,
    pass: &[u8],
    flags: u32,
    setup: impl FnOnce(&Connection),
) -> (Connection, php_mysqlnd::Result<()>) {
    let c = Connection::init();
    setup(&c);
    let r = c.connect(
        Some("127.0.0.1"),
        Some(b"bob"),
        Some(pass),
        Some(b"db1"),
        u32::from(port),
        None,
        flags,
    );
    (c, r)
}

fn sha256(d: &[u8]) -> [u8; 32] {
    openssl::sha::sha256(d)
}

fn expected_sha2(pass: &[u8]) -> Vec<u8> {
    let h1 = sha256(pass);
    let h2 = sha256(&h1);
    let mut x = h2.to_vec();
    x.extend_from_slice(SCRAMBLE);
    let h3 = sha256(&x);
    h1.iter().zip(h3.iter()).map(|(a, b)| a ^ b).collect()
}

fn expected_native(scramble: &[u8], pass: &[u8]) -> Vec<u8> {
    let s1 = openssl::sha::sha1(pass);
    let s2 = openssl::sha::sha1(&s1);
    let mut x = scramble[..20].to_vec();
    x.extend_from_slice(&s2);
    let s3 = openssl::sha::sha1(&x);
    s1.iter().zip(s3.iter()).map(|(a, b)| a ^ b).collect()
}

fn decrypt_password(rsa: &Rsa<openssl::pkey::Private>, enc: &[u8], scramble: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; rsa.size() as usize];
    let n = rsa
        .private_decrypt(enc, &mut out, Padding::PKCS1_OAEP)
        .unwrap();
    out.truncate(n);
    out.iter()
        .enumerate()
        .map(|(i, b)| b ^ scramble[i % 20])
        .collect()
}

#[test]
fn caching_sha2_fast_auth() {
    let (port, h) = serve(|m| {
        m.greeting("caching_sha2_password", false);
        let (flags, user, auth, db, plugin) = m.handshake_response();
        assert_eq!(user, b"bob");
        assert_eq!(db, b"db1");
        assert_eq!(plugin, "caching_sha2_password");
        assert_ne!(flags & CLIENT_CONNECT_ATTRS, 0);
        assert_eq!(auth, expected_sha2(b"secret"));
        m.write(&[0x01, 0x03]);
        m.ok();
        m.finish();
    });
    let (c, r) = connect(port, b"secret", 0, |_| {});
    r.unwrap();
    assert_eq!(c.server_info().as_deref(), Some("8.0.36-mock"));
    assert_eq!(c.auth_protocol().as_deref(), Some("caching_sha2_password"));
    assert_eq!(c.character_set_name(), Some("utf8mb4"));
    drop(c);
    h.join().unwrap();
}

#[test]
fn caching_sha2_full_auth_requests_rsa_key() {
    let rsa = Rsa::generate(2048).unwrap();
    let pem = rsa.public_key_to_pem().unwrap();
    let (port, h) = serve(move |m| {
        m.greeting("caching_sha2_password", false);
        let _ = m.handshake_response();
        m.write(&[0x01, 0x04]);
        assert_eq!(m.read(), vec![0x02], "public key request");
        let mut p = vec![0x01];
        p.extend_from_slice(&pem);
        m.write(&p);
        let enc = m.read();
        assert_eq!(enc.len(), 256);
        assert_eq!(decrypt_password(&rsa, &enc, SCRAMBLE), b"secret\0");
        m.ok();
        m.finish();
    });
    let (_c, r) = connect(port, b"secret", 0, |_| {});
    r.unwrap();
    h.join().unwrap();
}

#[test]
fn caching_sha2_full_auth_with_configured_key_file() {
    let rsa = Rsa::generate(2048).unwrap();
    let pem = rsa.public_key_to_pem().unwrap();
    let path = std::env::temp_dir().join(format!("mysqlnd_rs_pk_{}.pem", std::process::id()));
    std::fs::write(&path, &pem).unwrap();
    let (port, h) = serve(move |m| {
        m.greeting("caching_sha2_password", false);
        let _ = m.handshake_response();
        m.write(&[0x01, 0x04]);
        let enc = m.read();
        assert_eq!(decrypt_password(&rsa, &enc, SCRAMBLE), b"pw\0");
        m.ok();
        m.finish();
    });
    let p = path.to_string_lossy().into_owned();
    let (_c, r) = connect(port, b"pw", 0, |c| {
        assert!(c.set_option(opt::MYSQL_SERVER_PUBLIC_KEY, OptionValue::Str(p.as_bytes())));
    });
    r.unwrap();
    h.join().unwrap();
    let _ = std::fs::remove_file(path);
}

#[test]
fn caching_sha2_full_auth_rejected() {
    let (port, h) = serve(|m| {
        m.greeting("caching_sha2_password", false);
        let _ = m.handshake_response();
        m.err(
            1045,
            "28000",
            "Access denied for user 'bob'@'localhost' (using password: YES)",
        );
    });
    let (c, r) = connect(port, b"bad", 0, |_| {});
    let e = r.unwrap_err();
    assert_eq!(e.errno, 1045);
    assert_eq!(
        e.sqlstate, "HY000",
        "mysqlnd overrides the SQLSTATE of auth errors"
    );
    assert_eq!(
        e.message,
        b"Access denied for user 'bob'@'localhost' (using password: YES)"
    );
    assert!(!c.is_connected());
    h.join().unwrap();
}

#[test]
fn auth_switch_to_native() {
    let new_scramble = *b"ZYXWVUTSRQPONMLKJIHG";
    let (port, h) = serve(move |m| {
        m.greeting("caching_sha2_password", false);
        let (_, _, auth, _, plugin) = m.handshake_response();
        assert_eq!(plugin, "caching_sha2_password");
        assert_eq!(auth.len(), 32);
        // the server doesn't do a cached-sha2 result; it switches plugins right away
        let mut p = vec![0xFE];
        p.extend_from_slice(b"mysql_native_password\0");
        p.extend_from_slice(&new_scramble);
        p.push(0);
        m.write(&p);
        let resp = m.read();
        assert_eq!(resp, expected_native(&new_scramble, b"secret"));
        m.ok();
        m.finish();
    });
    let (c, r) = connect(port, b"secret", 0, |_| {});
    // caching_sha2's handler reads the 0xFE first, so the switch is honoured
    r.unwrap();
    assert_eq!(c.auth_protocol().as_deref(), Some("mysql_native_password"));
    drop(c);
    h.join().unwrap();
}

#[test]
fn sha256_password_via_switch() {
    let rsa = Rsa::generate(2048).unwrap();
    let pem = rsa.public_key_to_pem().unwrap();
    let (port, h) = serve(move |m| {
        m.greeting("mysql_native_password", false);
        let (_, _, auth, _, plugin) = m.handshake_response();
        assert_eq!(plugin, "mysql_native_password");
        assert_eq!(auth, expected_native(SCRAMBLE, b"secret"));
        let mut p = vec![0xFE];
        p.extend_from_slice(b"sha256_password\0");
        p.extend_from_slice(SCRAMBLE);
        p.push(0);
        m.write(&p);
        // sha256_password without TLS first asks for the public key
        assert_eq!(m.read(), vec![0x01]);
        let mut k = vec![0x01];
        k.extend_from_slice(&pem);
        m.write(&k);
        let enc = m.read();
        assert_eq!(decrypt_password(&rsa, &enc, SCRAMBLE), b"secret\0");
        m.ok();
        m.finish();
    });
    let (c, r) = connect(port, b"secret", 0, |_| {});
    r.unwrap();
    assert_eq!(c.auth_protocol().as_deref(), Some("sha256_password"));
    drop(c);
    h.join().unwrap();
}

#[test]
fn unknown_plugin_switch_fails() {
    let (port, h) = serve(|m| {
        m.greeting("mysql_native_password", false);
        let _ = m.handshake_response();
        let mut p = vec![0xFE];
        p.extend_from_slice(b"client_ed25519\0");
        p.extend_from_slice(SCRAMBLE);
        m.write(&p);
    });
    let (_c, r) = connect(port, b"secret", 0, |_| {});
    let e = r.unwrap_err();
    assert_eq!(e.errno, CR_NOT_IMPLEMENTED);
    assert_eq!(
        e.message,
        b"The server requested authentication method unknown to the client [client_ed25519]"
    );
    h.join().unwrap();
}

#[test]
fn unknown_greeting_plugin_falls_back_to_native() {
    let (port, h) = serve(|m| {
        m.greeting("client_ed25519", false);
        let (_, _, auth, _, plugin) = m.handshake_response();
        // mysqlnd sends the default plugin name with empty auth data
        assert_eq!(plugin, "mysql_native_password");
        assert!(auth.is_empty());
        m.ok();
        m.finish();
    });
    let (_c, r) = connect(port, b"secret", 0, |_| {});
    r.unwrap();
    h.join().unwrap();
}

#[test]
fn greeting_error_too_many_connections() {
    let (port, h) = serve(|m| {
        let mut p = vec![0xFF];
        p.extend_from_slice(&1040u16.to_le_bytes());
        p.extend_from_slice(b"Too many connections");
        m.write(&p);
    });
    let (_c, r) = connect(port, b"x", 0, |_| {});
    let e = r.unwrap_err();
    assert_eq!((e.errno, e.sqlstate.as_str()), (1040, "08004"));
    assert_eq!(e.message, b"Too many connections");
    h.join().unwrap();
}

#[test]
fn packets_out_of_order() {
    let (port, h) = serve(|m| {
        m.greeting("mysql_native_password", false);
        let _ = m.handshake_response();
        m.seq = 9;
        m.ok();
    });
    let _ = php_mysqlnd::diag::take();
    let (_c, r) = connect(port, b"x", 0, |_| {});
    let e = r.unwrap_err();
    assert_eq!(e.errno, CR_SERVER_GONE_ERROR);
    let d = php_mysqlnd::diag::take();
    assert!(
        d.iter()
            .any(|d| d.message == "Packets out of order. Expected 2 received 9. Packet size=7"),
        "{d:?}"
    );
    h.join().unwrap();
}

fn tls_acceptor() -> (SslAcceptor, Vec<u8>) {
    let rsa = Rsa::generate(2048).unwrap();
    let key = PKey::from_rsa(rsa).unwrap();
    let mut name = openssl::x509::X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", "localhost").unwrap();
    let name = name.build();
    let mut b = openssl::x509::X509::builder().unwrap();
    b.set_version(2).unwrap();
    b.set_subject_name(&name).unwrap();
    b.set_issuer_name(&name).unwrap();
    b.set_pubkey(&key).unwrap();
    b.set_not_before(&openssl::asn1::Asn1Time::days_from_now(0).unwrap())
        .unwrap();
    b.set_not_after(&openssl::asn1::Asn1Time::days_from_now(2).unwrap())
        .unwrap();
    let serial = openssl::bn::BigNum::from_u32(1)
        .unwrap()
        .to_asn1_integer()
        .unwrap();
    b.set_serial_number(&serial).unwrap();
    let san = openssl::x509::extension::SubjectAlternativeName::new()
        .dns("localhost")
        .ip("127.0.0.1")
        .build(&b.x509v3_context(None, None))
        .unwrap();
    b.append_extension(san).unwrap();
    b.sign(&key, openssl::hash::MessageDigest::sha256())
        .unwrap();
    let cert = b.build();
    let mut a = SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).unwrap();
    a.set_private_key(&key).unwrap();
    a.set_certificate(&cert).unwrap();
    (a.build(), cert.to_pem().unwrap())
}

#[test]
fn tls_with_caching_sha2_cleartext_full_auth() {
    let (acceptor, _) = tls_acceptor();
    let (port, h) = serve(move |m| {
        m.greeting("caching_sha2_password", true);
        let req = m.read();
        assert_eq!(req.len(), 32, "SSL request packet");
        let flags = u32::from_le_bytes([req[0], req[1], req[2], req[3]]);
        assert_ne!(flags & CLIENT_SSL, 0);
        m.start_tls(&acceptor);
        let (_, user, auth, _, _) = m.handshake_response();
        assert_eq!(user, b"bob");
        assert_eq!(auth, expected_sha2(b"tlspw"));
        m.write(&[0x01, 0x04]);
        assert_eq!(m.read(), b"tlspw\0", "cleartext password over TLS");
        m.ok();
        // a query over TLS
        m.seq = 0;
        let q = m.read();
        assert_eq!(&q[..], b"\x03SELECT 1");
        m.write(&[0, 0, 0, 2, 0, 0, 0]);
        m.finish();
    });
    let (c, r) = connect(port, b"tlspw", CLIENT_SSL, |_| {});
    r.unwrap();
    assert!(c.is_ssl());
    c.query(b"SELECT 1").unwrap();
    drop(c);
    h.join().unwrap();
}

#[test]
fn tls_peer_verification_failure() {
    let (acceptor, _) = tls_acceptor();
    let (port, h) = serve(move |m| {
        m.greeting("mysql_native_password", true);
        let _ = m.read();
        let S::Plain(tcp) = m.s.take().unwrap() else {
            panic!()
        };
        let _ = acceptor.accept(tcp);
    });
    // a CA that didn't sign the server certificate
    let (_, other_ca) = tls_acceptor();
    let ca = std::env::temp_dir().join(format!("mysqlnd_rs_ca_{}.pem", std::process::id()));
    std::fs::write(&ca, other_ca).unwrap();
    let cap = ca.to_string_lossy().into_owned();
    let (_c, r) = connect(
        port,
        b"x",
        CLIENT_SSL | CLIENT_SSL_VERIFY_SERVER_CERT,
        |c| {
            c.ssl_set(None, None, Some(&cap), None, None);
        },
    );
    let e = r.unwrap_err();
    assert_eq!(
        (e.errno, e.message.as_slice()),
        (
            CR_CONNECTION_ERROR,
            &b"Cannot connect to MySQL using SSL"[..]
        )
    );
    h.join().unwrap();
    let _ = std::fs::remove_file(ca);
}

#[test]
fn tls_verified_with_right_ca() {
    let (acceptor, cert) = tls_acceptor();
    let ca = std::env::temp_dir().join(format!("mysqlnd_rs_ca_ok_{}.pem", std::process::id()));
    std::fs::write(&ca, cert).unwrap();
    let (port, h) = serve(move |m| {
        m.greeting("mysql_native_password", true);
        let _ = m.read();
        m.start_tls(&acceptor);
        let _ = m.handshake_response();
        m.ok();
        m.finish();
    });
    let cap = ca.to_string_lossy().into_owned();
    let (c, r) = connect(port, b"x", CLIENT_SSL_VERIFY_SERVER_CERT, |c| {
        c.ssl_set(None, None, Some(&cap), None, None);
    });
    r.unwrap();
    assert!(c.is_ssl());
    drop(c);
    h.join().unwrap();
    let _ = std::fs::remove_file(ca);
}

#[test]
fn diagnostics_are_delivered_before_the_next_packet() {
    use std::sync::{Arc, Mutex};
    let timeline = Arc::new(Mutex::new(Vec::<String>::new()));
    let t_srv = timeline.clone();
    let (port, h) = serve(move |m| {
        m.greeting("sha256_password", false);
        // sha256_password without TLS asks for the key before the handshake response
        assert_eq!(m.read(), vec![0x01]);
        m.write(&[]); // empty public-key response
        let (_, _, auth, _, _) = m.handshake_response();
        t_srv
            .lock()
            .unwrap()
            .push("server: handshake response".into());
        assert!(auth.is_empty());
        m.ok();
        m.finish();
    });
    let t_cli = timeline.clone();
    let _ = php_mysqlnd::diag::set_sink(Some(Box::new(move |d: &php_mysqlnd::diag::Diag| {
        t_cli.lock().unwrap().push(format!("warn: {}", d.message));
    })));
    let (c, r) = connect(port, b"secret", 0, |_| {});
    let _ = php_mysqlnd::diag::set_sink(None);
    r.unwrap();
    drop(c);
    h.join().unwrap();
    let t = timeline.lock().unwrap().clone();
    let pid = std::process::id();
    assert_eq!(
        t,
        vec![
            "warn: Premature end of data (mysqlnd_wireprotocol.c:2120)".to_string(),
            "warn: SHA256_PK_REQUEST_RESPONSE packet shorter than expected".to_string(),
            format!("warn: Error while receiving public key. PID={pid}"),
            "server: handshake response".to_string(),
        ]
    );
}

#[test]
fn malformed_auth_and_change_user_replies() {
    // auth switch with an unterminated plugin name -> old-password error
    let (port, h) = serve(|m| {
        m.greeting("mysql_native_password", false);
        let _ = m.handshake_response();
        m.write(b"\xfemysql_native_password");
    });
    let _ = php_mysqlnd::diag::take();
    let (_c, r) = connect(port, b"secret", 0, |_| {});
    let e = r.unwrap_err();
    assert_eq!(
        (e.errno, e.message.as_slice()),
        (CR_UNKNOWN_ERROR, MSG_OLD_PASSWD.as_bytes())
    );
    let d: Vec<String> = php_mysqlnd::diag::take()
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert_eq!(d[1], "AUTH_RESPONSE packet shorter than expected");
    h.join().unwrap();

    // OK with an over-long message -> its own warning, unknown connect error
    let (port, h) = serve(|m| {
        m.greeting("mysql_native_password", false);
        let _ = m.handshake_response();
        m.write(&[0, 0, 0, 2, 0, 0, 0, 0xfc, 0xff]);
    });
    let (_c, r) = connect(port, b"", 0, |_| {});
    let e = r.unwrap_err();
    assert!(e
        .message
        .starts_with(b"Unknown error while trying to connect via tcp://127.0.0.1:"));
    let d: Vec<String> = php_mysqlnd::diag::take()
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert_eq!(
        d,
        vec!["OK packet message length is past the packet size".to_string()]
    );
    h.join().unwrap();

    // affected rows as NULL length is accepted
    let (port, h) = serve(|m| {
        m.greeting("mysql_native_password", false);
        let _ = m.handshake_response();
        m.write(&[0, 0xfb, 0, 2, 0, 0, 0]);
        m.finish();
    });
    let (_c, r) = connect(port, b"", 0, |_| {});
    r.unwrap();
    h.join().unwrap();

    // a one-byte change-user reply fails
    let (port, h) = serve(|m| {
        m.greeting("mysql_native_password", false);
        let _ = m.handshake_response();
        m.ok();
        m.seq = 0;
        let _ = m.read();
        m.write(&[0]);
        m.finish();
    });
    let (c, r) = connect(port, b"", 0, |_| {});
    r.unwrap();
    let _ = php_mysqlnd::diag::take();
    assert!(c.change_user(b"root2", b"", None, false).is_err());
    let d: Vec<String> = php_mysqlnd::diag::take()
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert_eq!(d[1], "CHANGE_USER packet shorter than expected");
    drop(c);
    h.join().unwrap();
}
