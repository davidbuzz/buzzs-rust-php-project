// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end TLS over the stream layer: a listener created with
//! `tcp://` + `ssl` context options, an accepted connection upgraded with
//! `stream_socket_enable_crypto()` (it must inherit the listener's
//! context), and a client upgraded the same way — both handshakes driven
//! non-blockingly in one process.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(code: &str) -> String {
    let sink = Sink::default();
    let streams = php_ext_streams::Streams;
    let ssl = php_ext_openssl::OpenSsl;
    let exts: [&dyn Extension; 2] = [&streams, &ssl];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn accepted_socket_inherits_listener_ssl_context() {
    let dir = std::env::temp_dir().join(format!("php-ext-openssl-tls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    let code = format!(
        r#"
        $k = openssl_pkey_new(["private_key_type" => OPENSSL_KEYTYPE_EC, "curve_name" => "prime256v1"]);
        $csr = openssl_csr_new(["commonName" => "localhost"], $k, ["digest_alg" => "sha256"]);
        $x = openssl_csr_sign($csr, null, $k, 1, ["digest_alg" => "sha256"]);
        openssl_x509_export_to_file($x, "{cert}");
        openssl_pkey_export_to_file($k, "{key}");

        $ctx = stream_context_create(["ssl" => ["local_cert" => "{cert}", "local_pk" => "{key}"]]);
        $srv = stream_socket_server("tcp://127.0.0.1:0", $e, $m, STREAM_SERVER_BIND | STREAM_SERVER_LISTEN, $ctx);
        $addr = stream_socket_get_name($srv, false);
        $cctx = stream_context_create(["ssl" => ["verify_peer" => false, "verify_peer_name" => false, "capture_peer_cert" => true]]);
        $c = stream_socket_client("tcp://$addr", $e, $m, 5, STREAM_CLIENT_CONNECT, $cctx);
        $s = stream_socket_accept($srv, 5);
        echo stream_get_meta_data($s)["stream_type"], "\n";
        stream_set_blocking($c, false);
        stream_set_blocking($s, false);
        $a = $b = 0;
        for ($i = 0; $i < 5000 && ($a !== true || $b !== true); $i++) {{
            if ($a !== true) $a = stream_socket_enable_crypto($c, true, STREAM_CRYPTO_METHOD_TLS_CLIENT);
            if ($b !== true) $b = stream_socket_enable_crypto($s, true, STREAM_CRYPTO_METHOD_TLS_SERVER);
            if ($a === false || $b === false) break;
        }}
        var_dump($a, $b);
        stream_set_blocking($c, true);
        stream_set_blocking($s, true);
        fwrite($c, "ping\n");
        echo fgets($s);
        echo stream_get_meta_data($s)["crypto"]["protocol"], "\n";
        var_dump(stream_context_get_options($cctx)["ssl"]["peer_certificate"] instanceof OpenSSLCertificate);
        var_dump(stream_socket_get_crypto_status($c));
        "#,
        cert = cert.display(),
        key = key.display(),
    );
    let out = run(&code);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        out,
        "tcp_socket/ssl\nbool(true)\nbool(true)\nping\nTLSv1.3\nbool(true)\nint(0)\n"
    );
}

#[test]
fn enable_crypto_requires_method() {
    let out = run(r#"$srv = stream_socket_server("tcp://127.0.0.1:0");
        $c = stream_socket_client("tcp://" . stream_socket_get_name($srv, false));
        try { stream_socket_enable_crypto($c, true); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
        $ssl = $tls13 = false;
        foreach (stream_get_transports() as $t) { $ssl = $ssl || $t === "ssl"; $tls13 = $tls13 || $t === "tlsv1.3"; }
        var_dump($ssl, $tls13);"#);
    assert_eq!(
        out,
        "stream_socket_enable_crypto(): Argument #3 ($crypto_method) must be specified when enabling encryption\nbool(true)\nbool(true)\n"
    );
}

/// `STREAM_CLIENT_ASYNC_CONNECT` alone connects a `tcp://` client (C's
/// `php_stream_xport_create()` connects for it as for
/// `STREAM_CLIENT_CONNECT`); the stream is a `tcp_socket/ssl` one that
/// becomes writable once connected. Expected output from php8.5.
#[test]
fn async_connect_connects() {
    let out = run(r#"$srv = stream_socket_server("tcp://127.0.0.1:0");
        $addr = stream_socket_get_name($srv, false);
        $c = stream_socket_client("tcp://$addr", $errno, $errstr, 5, STREAM_CLIENT_ASYNC_CONNECT);
        var_dump(stream_get_meta_data($c)['stream_type'], $errno);
        stream_set_blocking($c, false);
        $r = null; $w = [$c]; $e = null;
        var_dump(stream_select($r, $w, $e, 5));
        var_dump(fwrite($c, "ping"));
        $peer = stream_socket_accept($srv, 5);
        var_dump(fread($peer, 4));"#);
    assert_eq!(
        out,
        "string(14) \"tcp_socket/ssl\"\nint(0)\nint(1)\nint(4)\nstring(4) \"ping\"\n"
    );
}
