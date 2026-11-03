//! `ext/openssl` for the phprust VM: binds the `php-openssl` library
//! through the `php_vm::ext` extension API.
//!
//! * every `openssl_*` function of `openssl.stub.php` (aliases
//!   `openssl_get_publickey`, `openssl_get_privatekey`, `openssl_free_key`
//!   included), with PHP's argument handling: union `Class|string`
//!   parameters, the untyped key parameters (`[key, passphrase]` arrays,
//!   `file://` paths, PEM strings, `__toString` objects), by-reference
//!   outputs and the deprecations of the `*_free` functions;
//! * the final classes `OpenSSLCertificate`, `OpenSSLCertificateSigningRequest`
//!   and `OpenSSLAsymmetricKey` (see [`objects`]);
//! * all `OPENSSL_*` / `PKCS7_*` / `X509_PURPOSE_*` constants with the
//!   values of the linked library, and the `openssl.cafile` /
//!   `openssl.capath` INI directives.
//!
//! The library's per-request state ([`php_openssl::OpenSsl`]: the error
//! ring buffer behind `openssl_error_string()`) lives in a thread local
//! that is reset at request shutdown. After every library call the
//! diagnostics it queued are raised as `E_WARNING` / `E_DEPRECATED` /
//! `E_NOTICE` with the usual `func(): ` prefix, and its [`Throw`] values
//! become `ValueError` / `Exception` throwables.
//!
//! The master-only `Openssl\OpensslException`, `Openssl\Psk` and
//! `Openssl\Session` classes are provided too ([`session`]).
//!
//! # TLS stream transports (not wired yet)
//!
//! The `ssl://` / `tls://` / `tlsv1.x://` transports,
//! `stream_socket_enable_crypto()` and `stream_socket_get_crypto_status()`
//! belong to the streams layer (`php-streams` / `php-ext-streams`), which
//! currently has no way for another crate to plug in a transport. To wire
//! `php_openssl::tls` (client handshake over any `Read + Write`) in, the
//! streams crates need to provide:
//!
//! 1. a **transport registry** (`php_stream_xport_register`): a
//!    thread-local or VM-owned map from scheme (`ssl`, `tls`, `tlsv1.0`,
//!    `tlsv1.1`, `tlsv1.2`, `tlsv1.3`) to a factory called by
//!    `stream_socket_client()` / `stream_socket_server()` / `fsockopen()`
//!    with the parsed host/port, timeout, flags, persistent id and the
//!    `ssl` context options (as a `php_runtime` array), returning a
//!    `Stream` (and contributing to `stream_get_transports()`);
//! 2. a **crypto hook on socket streams**: `StreamOption::Crypto
//!    { setup(method, session_stream), enable(bool), status }` routed by
//!    `stream_socket_enable_crypto()` / `stream_socket_get_crypto_status()`
//!    to the stream's ops, plus a way to *replace a stream's ops in place*
//!    (the resource must stay the same while the plain socket ops become
//!    TLS ops wrapping them), or `StreamCell` access that lets an
//!    extension swap the `Stream` it holds;
//! 3. **accepted-connection hooks** so `stream_socket_accept()` on an
//!    `ssl://` server produces a TLS stream performing the server handshake
//!    with the listener's context (`SNI_server_certs`, `local_cert`,
//!    `local_pk`, `passphrase`, session/PSK callbacks …);
//! 4. **metadata hooks**: `stream_get_meta_data()["crypto"]`
//!    (`protocol`, `cipher_name`, `cipher_bits`, `cipher_version`,
//!    `alpn_protocol`) and writing `peer_certificate` /
//!    `peer_certificate_chain` / `session_meta` back into the stream's
//!    context (`capture_peer_cert*` options), which needs mutable access to
//!    the context resource of a stream;
//! 5. the `StreamErrorCode` mapping for TLS failures (`stream_last_errors()`).
//!
//! On the library side `php_openssl::tls` implements the client
//! (verification policy, `peer_fingerprint`, ALPN, `security_level`,
//! `crypto_method`, PHP's warning texts); the server side, renegotiation
//! limiting, session resumption callbacks, PSK and early data still have to
//! be written. Until then the ~100 `ext/openssl/tests` that start a TLS
//! server through `ServerClientTestCase.inc` fail.

// Native handlers must match the VM's `NativeFn` signature even when they
// cannot fail.
#![allow(clippy::unnecessary_wraps)]

mod convert;
mod crypto;
mod keys;
mod objects;
mod session;
mod smime;
mod state;
mod x509;

use php_openssl::constants::{self, ConstValue};
use php_runtime::Value;
use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_vm::ext::{Extension, ModuleBuilder};

/// The `openssl` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenSsl;

impl Extension for OpenSsl {
    fn name(&self) -> &'static str {
        "openssl"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        objects::register(m);
        session::register(m);
        for (name, value) in constants::all() {
            let v = match value {
                ConstValue::Int(i) => Value::Int(i),
                ConstValue::Str(s) => Value::string(s),
            };
            m.constant(name, v);
        }
        m.ini(IniDirective::new(
            "openssl.libctx",
            Some("custom"),
            Access::PERDIR,
            ModifyHandler::None,
            "openssl",
        ));
        for name in ["openssl.cafile", "openssl.capath"] {
            m.ini(IniDirective::new(
                name,
                None,
                Access::PERDIR,
                ModifyHandler::None,
                "openssl",
            ));
        }
        x509::register(m);
        keys::register(m);
        crypto::register(m);
        smime::register(m);
        m.on_request_shutdown(state::reset);
    }
}
