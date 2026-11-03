//! `SSL_CTX` construction from the `ssl` context options
//! (`php_openssl_create_server_ctx` and friends), the peer verification
//! policy and peer certificate capture.

use openssl::dh::Dh;
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::ssl::{
    AlpnError, NameType, SniError, SslContext, SslContextBuilder, SslFiletype, SslMethod,
    SslOptions, SslRef, SslVerifyMode, SslVersion,
};
use openssl::x509::{X509Name, X509Ref, X509VerifyResult};
use php_openssl::tls::matches_wildcard_name;
use php_runtime::{Value, ZArray};
use php_streams::diag::StreamErrorCode as Code;
use php_vm::ext::VmResult;
use php_vm::Vm;

use super::opts::SslOpts;
use super::sock::warn;

/// `STREAM_CRYPTO_METHOD_TLSv1_0` … `TLSv1_3` bits.
const TLS1_0: i64 = 1 << 3;
const TLS1_3: i64 = 1 << 6;
/// `PHP_OPENSSL_MIN_PROTO_VERSION` (no `SSLv3` in the linked library).
const MIN_PROTO: i64 = TLS1_0;
/// `PHP_OPENSSL_MAX_PROTO_VERSION`.
const MAX_PROTO: i64 = TLS1_3;
/// `X509_V_ERR_DEPTH_ZERO_SELF_SIGNED_CERT`.
const DEPTH_ZERO_SELF_SIGNED: i32 = 18;
/// `OPENSSL_DEFAULT_STREAM_VERIFY_DEPTH`.
const DEFAULT_VERIFY_DEPTH: i64 = 9;

fn min_proto_flag(flags: i64) -> i64 {
    let mut ver = MIN_PROTO;
    while ver <= MAX_PROTO {
        if flags & ver != 0 {
            return ver;
        }
        ver <<= 1;
    }
    MAX_PROTO
}

fn max_proto_flag(flags: i64) -> i64 {
    let mut ver = MAX_PROTO;
    while ver >= MIN_PROTO {
        if flags & ver != 0 {
            return ver;
        }
        ver >>= 1;
    }
    TLS1_3
}

fn map_proto(flag: i64) -> SslVersion {
    match flag {
        f if f == TLS1_3 => SslVersion::TLS1_3,
        f if f == TLS1_0 << 1 => SslVersion::TLS1_1,
        f if f == TLS1_0 => SslVersion::TLS1,
        _ => SslVersion::TLS1_2,
    }
}

/// `php_openssl_get_proto_version_flags`.
fn proto_version_flags(mut flags: i64, min: i64, max: i64) -> i64 {
    let min = if min == 0 { min_proto_flag(flags) } else { min };
    let max = if max == 0 { max_proto_flag(flags) } else { max };
    let mut ver = MIN_PROTO;
    while ver <= MAX_PROTO {
        if ver >= min && ver <= max {
            flags |= ver;
        } else {
            flags &= !ver;
        }
        ver <<= 1;
    }
    flags
}

/// `php_openssl_alpn_protos_parse`: `"a,b"` → wire format (empty names
/// stay as zero-length entries, which OpenSSL rejects for clients).
pub(crate) fn alpn_parse(input: &str) -> Option<Vec<u8>> {
    let b = input.as_bytes();
    if b.len() >= 65535 {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() + 1);
    for part in b.split(|&c| c == b',') {
        let len = u8::try_from(part.len()).ok()?;
        out.push(len);
        out.extend_from_slice(part);
    }
    Some(out)
}

/// Splits a wire-format protocol list.
fn alpn_entries(wire: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < wire.len() {
        let len = usize::from(wire[i]);
        let Some(p) = wire.get(i + 1..i + 1 + len) else {
            break;
        };
        out.push(p);
        i += 1 + len;
    }
    out
}

/// `SSL_select_next_proto` succeeding with `OPENSSL_NPN_NEGOTIATED`: the
/// first server protocol the client also offers (as a client slice).
fn select_alpn<'c>(server: &[u8], client: &'c [u8]) -> Option<&'c [u8]> {
    let offered = alpn_entries(client);
    alpn_entries(server)
        .into_iter()
        .find_map(|s| offered.iter().copied().find(|c| *c == s))
}

/// `php_openssl_check_path_ex` for context option paths: expands the
/// path (relative to the working directory).
fn resolve_path(p: &str) -> Option<String> {
    if p.as_bytes().contains(&0) {
        return None;
    }
    php_openssl::util::expand_filepath(p.as_bytes())
        .map(|v| String::from_utf8_lossy(&v).into_owned())
}

/// `php_openssl_load_stream_cafile`: adds the PEM certificates of a file.
fn load_stream_cafile(b: &mut SslContextBuilder, cafile: &str) -> bool {
    let Ok(data) = std::fs::read(cafile) else {
        warn(
            Code::OpenFailed,
            format!("failed loading cafile stream: `{cafile}'"),
        );
        return false;
    };
    let mut added = 0;
    if let Ok(certs) = openssl::x509::X509::stack_from_pem(&data) {
        for c in certs {
            if b.cert_store_mut().add_cert(c).is_ok() {
                added += 1;
            }
        }
    }
    if added == 0 {
        warn(
            Code::DecodingFailed,
            format!("no valid certs found cafile stream: '{cafile}'"),
        );
    }
    added > 0
}

/// `php_openssl_enable_peer_verification`.
fn enable_peer_verification(
    vm: &mut Vm,
    b: &mut SslContextBuilder,
    opts: &SslOpts,
    is_client: bool,
) -> VmResult<bool> {
    let mut cafile = opts.string(vm, "cafile")?;
    let mut capath = opts.string(vm, "capath")?;
    if cafile.is_none() {
        cafile = vm
            .ini_string("openssl.cafile")
            .filter(|v| !v.is_empty())
            .map(|v| String::from_utf8_lossy(&v).into_owned());
    } else if !is_client {
        let path = cafile.clone().unwrap_or_default();
        if let Ok(names) = X509Name::load_client_ca_file(&path) {
            b.set_client_ca_list(names);
        } else {
            warn(
                Code::InvalidFormat,
                "SSL: failed loading CA names from cafile",
            );
            return Ok(false);
        }
    }
    if capath.is_none() {
        capath = vm
            .ini_string("openssl.capath")
            .filter(|v| !v.is_empty())
            .map(|v| String::from_utf8_lossy(&v).into_owned());
    }
    if cafile.is_some() || capath.is_some() {
        let ok = b
            .load_verify_locations(
                cafile.as_deref().map(std::path::Path::new),
                capath.as_deref().map(std::path::Path::new),
            )
            .is_ok();
        if !ok {
            if let Some(f) = &cafile {
                if !load_stream_cafile(b, f) {
                    return Ok(false);
                }
            }
        }
    } else if is_client && b.set_default_verify_paths().is_err() {
        warn(
            Code::ProtocolError,
            "Unable to set default verify locations and no CA settings specified",
        );
        return Ok(false);
    }
    let allow_self = opts.bool(vm, "allow_self_signed")?.unwrap_or(false);
    let depth = opts
        .long(vm, "verify_depth")?
        .unwrap_or(DEFAULT_VERIFY_DEPTH);
    b.set_verify_callback(SslVerifyMode::PEER, move |preverify_ok, ctx| {
        let mut ret = preverify_ok;
        if ctx.error().as_raw() == DEPTH_ZERO_SELF_SIGNED && allow_self {
            ret = true;
        }
        let d = i64::from(ctx.error_depth());
        // C compares as unsigned: a negative allowed depth is huge.
        if depth >= 0 && d > depth {
            ret = false;
            ctx.set_error(php_openssl::ffi::verify_result_chain_too_long());
        }
        ret
    });
    Ok(true)
}

/// Loads a private key file, decrypting it with the `passphrase` option.
fn use_private_key(b: &mut SslContextBuilder, path: &str, passphrase: Option<&str>) -> bool {
    match passphrase {
        Some(p) => {
            let Ok(pem) = std::fs::read(path) else {
                return false;
            };
            PKey::private_key_from_pem_passphrase(&pem, p.as_bytes())
                .ok()
                .is_some_and(|k| b.set_private_key(&k).is_ok())
        }
        None => b.set_private_key_file(path, SslFiletype::PEM).is_ok(),
    }
}

/// `php_openssl_set_local_cert`.
fn set_local_cert(vm: &mut Vm, b: &mut SslContextBuilder, opts: &SslOpts) -> VmResult<bool> {
    let Some(certfile) = opts.string(vm, "local_cert")? else {
        return Ok(true);
    };
    let Some(mut resolved) = resolve_path(&certfile) else {
        warn(
            Code::NotFound,
            format!("Unable to get real path of certificate file `{certfile}'"),
        );
        return Ok(false);
    };
    if b.set_certificate_chain_file(&resolved).is_err() {
        warn(
            Code::WriteFailed,
            format!(
                "Unable to set local cert chain file `{certfile}'; Check that your cafile/capath settings include details of your certificate and its issuer"
            ),
        );
        return Ok(false);
    }
    if let Some(pk) = opts.string(vm, "local_pk")? {
        let Some(r) = resolve_path(&pk) else {
            warn(
                Code::NotFound,
                format!("Unable to get real path of private key file `{pk}'"),
            );
            return Ok(false);
        };
        resolved = r;
    }
    let passphrase = opts.string(vm, "passphrase")?;
    if !use_private_key(b, &resolved, passphrase.as_deref()) {
        warn(
            Code::WriteFailed,
            format!("Unable to set private key file `{resolved}'"),
        );
        return Ok(false);
    }
    if b.check_private_key().is_err() {
        warn(
            Code::PermissionDenied,
            "Private key does not match certificate!",
        );
    }
    Ok(true)
}

/// `php_openssl_set_server_dh_param`.
fn set_server_dh_param(vm: &mut Vm, b: &mut SslContextBuilder, opts: &SslOpts) -> VmResult<bool> {
    let Some(path) = opts.string(vm, "dh_param")? else {
        return Ok(true);
    };
    let Ok(pem) = std::fs::read(&path) else {
        warn(Code::NotFound, "Invalid dh_param");
        return Ok(false);
    };
    let Ok(dh) = Dh::params_from_pem(&pem) else {
        warn(Code::ReadFailed, "Failed reading DH params");
        return Ok(false);
    };
    if b.set_tmp_dh(&dh).is_err() {
        warn(Code::WriteFailed, "Failed assigning DH params");
        return Ok(false);
    }
    Ok(true)
}

/// `php_openssl_set_server_specific_opts`.
fn set_server_specific_opts(
    vm: &mut Vm,
    b: &mut SslContextBuilder,
    opts: &SslOpts,
) -> VmResult<bool> {
    if opts.get("rsa_key_size").is_some() {
        warn(
            Code::Generic,
            "rsa_key_size context option has been removed",
        );
    }
    if !set_server_dh_param(vm, b, opts)? {
        return Ok(false);
    }
    let mut o = SslOptions::empty();
    if opts.bool(vm, "single_dh_use")?.unwrap_or(true) {
        o |= SslOptions::SINGLE_DH_USE;
    }
    if opts.bool(vm, "honor_cipher_order")?.unwrap_or(true) {
        o |= SslOptions::CIPHER_SERVER_PREFERENCE;
    }
    b.set_options(o);
    Ok(true)
}

/// `php_openssl_create_sni_server_ctx`.
fn sni_ctx(cert: &str, key: &str) -> Option<SslContextBuilder> {
    let Ok(mut b) = SslContextBuilder::new(SslMethod::tls_server()) else {
        warn(Code::CreateFailed, "Failed to create the SSL context");
        return None;
    };
    if b.set_certificate_chain_file(cert).is_err() {
        warn(
            Code::ReadFailed,
            format!(
                "Failed setting local cert chain file `{cert}'; check that your cafile/capath settings include details of your certificate and its issuer"
            ),
        );
        return None;
    }
    if b.set_private_key_file(key, SslFiletype::PEM).is_err() {
        warn(
            Code::ReadFailed,
            format!("Failed setting private key from file `{key}'"),
        );
        return None;
    }
    Some(b)
}

/// `php_openssl_enable_server_sni`.
#[allow(clippy::too_many_lines)]
fn enable_server_sni(
    vm: &mut Vm,
    b: &mut SslContextBuilder,
    opts: &SslOpts,
    verify_peer: bool,
) -> VmResult<bool> {
    if opts.bool(vm, "SNI_enabled")? == Some(false) {
        return Ok(true);
    }
    let Some(val) = opts.get("SNI_server_certs") else {
        return Ok(true);
    };
    let Value::Array(certs) = val else {
        warn(
            Code::Generic,
            "SNI_server_certs requires an array mapping host names to cert paths",
        );
        return Ok(false);
    };
    if certs.is_empty() {
        warn(
            Code::Generic,
            "SNI_server_certs host cert array must not be empty",
        );
        return Ok(false);
    }
    let mut list: Vec<(String, SslContext)> = Vec::new();
    for (k, v) in certs.iter() {
        let php_runtime::ArrayKey::Str(key) = k else {
            warn(
                Code::Generic,
                "SNI_server_certs array requires string host name keys",
            );
            return Ok(false);
        };
        let ctx = match v.deref() {
            Value::Array(entry) => {
                let Some(local_cert) = entry.get_str(b"local_cert").map(Value::deref) else {
                    warn(Code::Generic, "local_cert not present in the array");
                    return Ok(false);
                };
                let cert = vm.value_to_string(&local_cert)?.to_string_lossy();
                let Some(cert_path) = resolve_path(&cert) else {
                    warn(
                        Code::OpenFailed,
                        format!(
                            "Failed setting local cert chain file `{cert}'; could not open file"
                        ),
                    );
                    return Ok(false);
                };
                let Some(local_pk) = entry.get_str(b"local_pk").map(Value::deref) else {
                    warn(Code::Generic, "local_pk not present in the array");
                    return Ok(false);
                };
                let pk = vm.value_to_string(&local_pk)?.to_string_lossy();
                let Some(pk_path) = resolve_path(&pk) else {
                    warn(
                        Code::OpenFailed,
                        format!(
                            "Failed setting local private key file `{pk}';  could not open file"
                        ),
                    );
                    return Ok(false);
                };
                sni_ctx(&cert_path, &pk_path)
            }
            Value::Str(s) => {
                let p = s.to_string_lossy();
                if let Some(r) = resolve_path(&p) {
                    sni_ctx(&r, &r)
                } else {
                    warn(
                        Code::NotFound,
                        format!("Failed setting local cert chain file `{p}'; file not found"),
                    );
                    None
                }
            }
            _ => {
                warn(
                    Code::Generic,
                    "SNI_server_certs options values must be of type array|string",
                );
                None
            }
        };
        let Some(mut ctx) = ctx else {
            return Ok(false);
        };
        if !verify_peer {
            ctx.set_verify(SslVerifyMode::NONE);
        } else if !enable_peer_verification(vm, &mut ctx, opts, false)? {
            return Ok(false);
        }
        list.push((key.to_string_lossy(), ctx.build()));
    }
    b.set_servername_callback(move |ssl, _alert| {
        let Some(name) = ssl.servername(NameType::HOST_NAME).map(str::to_string) else {
            return Err(SniError::NOACK);
        };
        for (n, ctx) in &list {
            if matches_wildcard_name(&name, n) {
                return ssl.set_ssl_context(ctx).map_err(|_| SniError::NOACK);
            }
        }
        Err(SniError::NOACK)
    });
    Ok(true)
}

/// `php_openssl_create_server_ctx` (used for clients too).
#[allow(clippy::too_many_lines)]
pub(crate) fn create_ctx(
    vm: &mut Vm,
    opts: &SslOpts,
    is_client: bool,
    method_flags: i64,
) -> VmResult<Option<SslContext>> {
    let method = if is_client {
        SslMethod::tls_client()
    } else {
        SslMethod::tls_server()
    };
    let Ok(mut b) = SslContextBuilder::new(method) else {
        warn(Code::CreateFailed, "SSL context creation failure");
        return Ok(None);
    };
    let min = opts.long(vm, "min_proto_version")?.unwrap_or(0);
    let max = opts.long(vm, "max_proto_version")?.unwrap_or(0);
    let flags = proto_version_flags(method_flags, min, max);
    let mut o = SslOptions::ALL;
    if opts.bool(vm, "no_ticket")? == Some(true) {
        o |= SslOptions::NO_TICKET;
    }
    if let Some(n) = opts.long(vm, "num_tickets")? {
        if let Ok(n) = usize::try_from(n) {
            let _ = b.set_num_tickets(n);
        }
    }
    o &= !SslOptions::DONT_INSERT_EMPTY_FRAGMENTS;
    o |= SslOptions::IGNORE_UNEXPECTED_EOF;
    if opts.bool(vm, "disable_compression")?.unwrap_or(true) {
        o |= SslOptions::NO_COMPRESSION;
    }
    let verify_peer = if opts.bool(vm, "verify_peer")? == Some(false) {
        b.set_verify(SslVerifyMode::NONE);
        false
    } else {
        if !enable_peer_verification(vm, &mut b, opts, is_client)? {
            return Ok(None);
        }
        true
    };
    let ciphers = opts
        .string(vm, "ciphers")?
        .unwrap_or_else(|| php_openssl::constants::OPENSSL_DEFAULT_STREAM_CIPHERS.to_string());
    if b.set_cipher_list(&ciphers).is_err() {
        return Ok(None);
    }
    if let Some(level) = opts.long(vm, "security_level")? {
        if !(0..=5).contains(&level) {
            warn(Code::Generic, "Security level must be between 0 and 5");
        }
        b.set_security_level(u32::try_from(level.max(0)).unwrap_or(u32::MAX));
    }
    if let Some(alpn) = opts.string(vm, "alpn_protocols")? {
        let Some(wire) = alpn_parse(&alpn) else {
            warn(
                Code::DecodingFailed,
                "Failed parsing comma-separated TLS ALPN protocol string",
            );
            return Ok(None);
        };
        if is_client {
            if b.set_alpn_protos(&wire).is_err() {
                warn(
                    Code::DecodingFailed,
                    "Failed setting TLS ALPN protocols, protocol names must not be empty",
                );
                return Ok(None);
            }
        } else {
            b.set_alpn_select_callback(move |_ssl, client| {
                select_alpn(&wire, client).ok_or(AlpnError::NOACK)
            });
        }
    }
    if !set_local_cert(vm, &mut b, opts)? {
        return Ok(None);
    }
    b.set_options(o);
    let _ = b.set_min_proto_version(Some(map_proto(min_proto_flag(flags))));
    let _ = b.set_max_proto_version(Some(map_proto(max_proto_flag(flags))));
    if !is_client && opts.has_context() && !set_server_specific_opts(vm, &mut b, opts)? {
        return Ok(None);
    }
    if !is_client && !enable_server_sni(vm, &mut b, opts, verify_peer)? {
        return Ok(None);
    }
    Ok(Some(b.build()))
}

/// Policy options read before verifying the peer.
#[derive(Debug, Clone)]
pub(crate) struct Policy {
    verify_peer: bool,
    verify_peer_name: bool,
    allow_self_signed: bool,
    fingerprint: Option<Value>,
    peer_name: Option<String>,
}

impl Policy {
    /// Reads the options (`GET_VER_OPT`), defaults depending on the side.
    pub(crate) fn read(
        vm: &mut Vm,
        opts: &SslOpts,
        is_client: bool,
        url_name: Option<&str>,
    ) -> VmResult<Self> {
        let peer_name = opts
            .string(vm, "peer_name")?
            .or_else(|| is_client.then(|| url_name.map(str::to_string)).flatten());
        Ok(Policy {
            verify_peer: opts.bool(vm, "verify_peer")?.unwrap_or(is_client),
            verify_peer_name: opts.bool(vm, "verify_peer_name")?.unwrap_or(is_client),
            allow_self_signed: opts.bool(vm, "allow_self_signed")?.unwrap_or(false),
            fingerprint: opts.get("peer_fingerprint"),
            peer_name,
        })
    }
}

/// Lower-case hex digest of a certificate, or `None` (with the warning)
/// for an unknown algorithm.
fn fingerprint(peer: &X509Ref, algo: &str) -> Option<String> {
    let Some(md) = MessageDigest::from_name(algo) else {
        warn(Code::Generic, "Unknown digest algorithm");
        return None;
    };
    let d = peer.digest(md).ok()?;
    Some(String::from_utf8_lossy(&php_openssl::util::bin2hex(&d)).into_owned())
}

/// `php_openssl_x509_fingerprint_match`.
fn fingerprint_match(peer: &X509Ref, val: &Value) -> bool {
    match val {
        Value::Str(s) => {
            let algo = match s.len() {
                32 => "md5",
                40 => "sha1",
                _ => {
                    warn(
                        Code::AuthFailed,
                        "peer_fingerprint length doesn't match a md5 or sha1 hash",
                    );
                    return false;
                }
            };
            let ok = fingerprint(peer, algo)
                .is_some_and(|f| f.as_bytes().eq_ignore_ascii_case(s.as_bytes()));
            if !ok {
                warn(Code::AuthFailed, "peer_fingerprint match failure");
            }
            ok
        }
        Value::Array(arr) => {
            const FORM: &str =
                "Invalid peer_fingerprint array; [algo => fingerprint] form required";
            if arr.is_empty() {
                warn(Code::Generic, FORM);
                return false;
            }
            for (k, v) in arr.iter() {
                let (php_runtime::ArrayKey::Str(algo), Value::Str(expected)) = (k, v.deref())
                else {
                    warn(Code::Generic, FORM);
                    return false;
                };
                let ok = fingerprint(peer, &algo.to_string_lossy())
                    .is_some_and(|f| f.as_bytes().eq_ignore_ascii_case(expected.as_bytes()));
                if !ok {
                    warn(Code::AuthFailed, "peer_fingerprint match failure");
                    return false;
                }
            }
            true
        }
        _ => {
            warn(
                Code::Generic,
                "Invalid peer_fingerprint value; fingerprint string or array of the form [algo => fingerprint] required",
            );
            false
        }
    }
}

/// `EXPAND_IPV6_ADDRESS`.
fn expand_ipv6(b: &[u8; 16]) -> String {
    let w: Vec<String> = b
        .chunks(2)
        .map(|c| format!("{:X}", u16::from(c[0]) << 8 | u16::from(c[1])))
        .collect();
    w.join(":")
}

/// `php_openssl_matches_san_list`: `(matched, has_service_id)`.
fn matches_san_list(peer: &X509Ref, name: &str) -> (bool, bool) {
    let mut has_service_id = false;
    let name_v6 = name
        .parse::<std::net::Ipv6Addr>()
        .ok()
        .map(|a| expand_ipv6(&a.octets()));
    let Some(sans) = peer.subject_alt_names() else {
        return (false, false);
    };
    for san in &sans {
        if let Some(dns) = san.dnsname() {
            has_service_id = true;
            let dns = dns.strip_suffix('.').unwrap_or(dns);
            if !dns.contains('\0') && matches_wildcard_name(name, dns) {
                return (true, has_service_id);
            }
        } else if let Some(ip) = san.ipaddress() {
            if ip.len() == 4 {
                let s = format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
                if name.eq_ignore_ascii_case(&s) {
                    return (true, has_service_id);
                }
            } else if let (Some(want), Ok(got)) = (&name_v6, <[u8; 16]>::try_from(ip)) {
                if want.eq_ignore_ascii_case(&expand_ipv6(&got)) {
                    return (true, has_service_id);
                }
            }
        } else if san.uri().is_some() || is_srv_name(san) {
            has_service_id = true;
        }
    }
    (false, has_service_id)
}

/// Whether a `GENERAL_NAME` is an `otherName` SRV-ID (1.3.6.1.5.5.7.8.7).
fn is_srv_name(san: &openssl::x509::GeneralNameRef) -> bool {
    // The crate exposes no otherName accessor; detect it through the DER
    // encoding of the type id OID inside the name.
    const SRV_OID: &[u8] = &[0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x08, 0x07];
    san.dnsname().is_none()
        && san.email().is_none()
        && san.uri().is_none()
        && san.ipaddress().is_none()
        && san.directory_name().is_none()
        && other_name_der(san).is_some_and(|d| d.windows(SRV_OID.len()).any(|w| w == SRV_OID))
}

/// DER of a `GENERAL_NAME`.
fn other_name_der(san: &openssl::x509::GeneralNameRef) -> Option<Vec<u8>> {
    use foreign_types::ForeignTypeRef;
    extern "C" {
        fn i2d_GENERAL_NAME(a: *mut libc::c_void, out: *mut *mut u8) -> libc::c_int;
    }
    // SAFETY: `san` is a live GENERAL_NAME; the first call only measures,
    // the second writes exactly that many bytes into `buf`.
    unsafe {
        let p = san.as_ptr().cast::<libc::c_void>();
        let len = i2d_GENERAL_NAME(p, std::ptr::null_mut());
        let len = usize::try_from(len).ok().filter(|&l| l > 0)?;
        let mut buf = vec![0u8; len];
        let mut out = buf.as_mut_ptr();
        i2d_GENERAL_NAME(p, &raw mut out);
        Some(buf)
    }
}

/// `php_openssl_matches_common_name`.
fn matches_common_name(peer: &X509Ref, name: &str) -> bool {
    let Some(cn) = peer
        .subject_name()
        .entries_by_nid(openssl::nid::Nid::COMMONNAME)
        .next()
    else {
        warn(
            Code::NetworkRecvFailed,
            "Unable to locate peer certificate CN",
        );
        return false;
    };
    let raw = cn.data().as_slice();
    let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    let shown = String::from_utf8_lossy(&raw[..end]).into_owned();
    if end != raw.len() {
        warn(
            Code::AuthFailed,
            format!("Peer certificate CN=`{shown}' is malformed"),
        );
        false
    } else if matches_wildcard_name(name, &shown) {
        true
    } else {
        warn(
            Code::AuthFailed,
            format!("Peer certificate CN=`{shown}' did not match expected CN=`{name}'"),
        );
        false
    }
}

/// `php_openssl_apply_peer_verification_policy`.
pub(crate) fn apply_policy(ssl: &SslRef, peer: Option<&X509Ref>, p: &Policy) -> bool {
    let must_fp = p.fingerprint.is_some();
    if (p.verify_peer || p.verify_peer_name || must_fp) && peer.is_none() {
        warn(Code::NetworkRecvFailed, "Could not get peer certificate");
        return false;
    }
    let Some(peer) = peer else {
        return true;
    };
    if p.verify_peer {
        let r = ssl.verify_result();
        let ok = r == X509VerifyResult::OK
            || (r.as_raw() == DEPTH_ZERO_SELF_SIGNED && p.allow_self_signed);
        if !ok {
            warn(
                Code::AuthFailed,
                format!(
                    "Could not verify peer: code:{} {}",
                    r.as_raw(),
                    r.error_string()
                ),
            );
            return false;
        }
    }
    if let Some(fp) = &p.fingerprint {
        if matches!(fp, Value::Str(_) | Value::Array(_)) {
            if !fingerprint_match(peer, fp) {
                return false;
            }
        } else {
            warn(
                Code::AuthFailed,
                "Expected peer fingerprint must be a string or an array",
            );
            return false;
        }
    }
    if p.verify_peer_name {
        let Some(name) = &p.peer_name else {
            return false;
        };
        let (matched, has_service_id) = matches_san_list(peer, name);
        if matched {
            return true;
        }
        if has_service_id {
            warn(
                Code::AuthFailed,
                format!("Peer certificate subjectAltName did not match expected name `{name}'"),
            );
            return false;
        }
        return matches_common_name(peer, name);
    }
    true
}

/// A PHP `OpenSSLCertificate` for a certificate.
fn cert_value(vm: &mut Vm, x: &X509Ref) -> VmResult<Value> {
    let Ok(pem) = x.to_pem() else {
        return Ok(Value::Null);
    };
    let cert =
        crate::state::with_lib(|s| s.x509_read(php_openssl::CertArg::Str(&pem)).ok().flatten());
    match cert {
        Some(c) => crate::objects::new_cert(vm, c),
        None => Ok(Value::Null),
    }
}

/// `php_openssl_capture_peer_certs`.
pub(crate) fn capture_peer_certs(
    vm: &mut Vm,
    ssl: &SslRef,
    peer: &X509Ref,
    opts: &SslOpts,
) -> VmResult<()> {
    if opts.bool(vm, "capture_peer_cert")? == Some(true) {
        let v = cert_value(vm, peer)?;
        opts.set("peer_certificate", v);
    }
    if opts.bool(vm, "capture_peer_cert_chain")? == Some(true) {
        let arr = match ssl.peer_cert_chain() {
            Some(chain) if !chain.is_empty() => {
                let mut a = ZArray::new();
                for c in chain {
                    let v = cert_value(vm, c)?;
                    let _ = a.make_mut().push(v);
                }
                Value::Array(a)
            }
            _ => Value::Null,
        };
        opts.set("peer_certificate_chain", arr);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proto_flags() {
        assert_eq!(proto_version_flags(0b111_1001, 0, 0), 0b111_1001);
        assert_eq!(proto_version_flags(1 << 5 | 1, 0, 0), 1 << 5 | 1);
        assert_eq!(proto_version_flags(1 | 1 << 3, 0, 1 << 5), 1 | 0b11_1000);
        assert_eq!(map_proto(min_proto_flag(0)), SslVersion::TLS1_3);
    }

    #[test]
    fn alpn() {
        assert_eq!(alpn_parse("h2,http/1.1").unwrap(), b"\x02h2\x08http/1.1");
        assert_eq!(alpn_parse("").unwrap(), b"\x00");
        assert_eq!(alpn_parse("h2,").unwrap(), b"\x02h2\x00");
        assert!(alpn_parse(&"x".repeat(256)).is_none());
    }

    #[test]
    fn ipv6_expansion() {
        let a: std::net::Ipv6Addr = "::1".parse().unwrap();
        assert_eq!(expand_ipv6(&a.octets()), "0:0:0:0:0:0:0:1");
    }
}
