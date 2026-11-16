// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `Openssl` namespace classes of php-src master: `OpensslException`,
//! `Psk` (a validated pre-shared key value object) and `Session` (an
//! exported/imported TLS session, `SSL_SESSION`).
//!
//! Sessions are normally produced by TLS stream callbacks, which need the
//! (not yet available) `ssl://` transport; `Session::import()` and
//! unserialization work standalone.

use foreign_types::ForeignTypeRef;
use openssl::ssl::{SslSession, SslSessionRef, SslVersion};
use php_openssl::constants::{OPENSSL_ENCODING_DER, OPENSSL_ENCODING_PEM};
use php_openssl::util::{base64_decode, base64_encode};
use php_runtime::{Obj, Value, ZArray, ZStr};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, VmResult};
use php_vm::{object, Vm};

use crate::convert::bytes;

/// `Openssl\OpensslException`.
const EXCEPTION: &str = "Openssl\\OpensslException";
/// `Openssl\Session`.
const SESSION: &str = "Openssl\\Session";
/// `PHP_OPENSSL_PSK_MAX_PSK_LEN`.
const MAX_PSK_LEN: usize = 256;
/// `PHP_OPENSSL_PSK_MAX_IDENTITY_LEN`.
const MAX_IDENTITY_LEN: usize = 128;
/// `ZEND_ACC_NO_DYNAMIC_PROPERTIES` (see `objects.rs`).
const NO_DYNAMIC_PROPERTIES: u32 = 1 << 14;

const PEM_BEGIN: &[u8] = b"-----BEGIN SSL SESSION PARAMETERS-----";
const PEM_END: &[u8] = b"-----END SSL SESSION PARAMETERS-----";

mod ffi {
    use std::os::raw::{c_char, c_int, c_ulong};

    /// `SSL_CIPHER` (opaque).
    #[repr(C)]
    pub struct SslCipher {
        _private: [u8; 0],
    }

    extern "C" {
        pub fn SSL_SESSION_is_resumable(s: *const openssl_sys::SSL_SESSION) -> c_int;
        pub fn SSL_SESSION_has_ticket(s: *const openssl_sys::SSL_SESSION) -> c_int;
        pub fn SSL_SESSION_get_ticket_lifetime_hint(s: *const openssl_sys::SSL_SESSION) -> c_ulong;
        pub fn SSL_SESSION_get0_cipher(s: *const openssl_sys::SSL_SESSION) -> *const SslCipher;
        pub fn SSL_CIPHER_get_name(c: *const SslCipher) -> *const c_char;
    }
}

/// `SSL_SESSION_is_resumable`.
fn is_resumable(s: &SslSessionRef) -> bool {
    // SAFETY: `s` is a live `SSL_SESSION`; the getter only reads it.
    unsafe { ffi::SSL_SESSION_is_resumable(s.as_ptr()) != 0 }
}

/// `SSL_SESSION_has_ticket`.
fn has_ticket(s: &SslSessionRef) -> bool {
    // SAFETY: as above.
    unsafe { ffi::SSL_SESSION_has_ticket(s.as_ptr()) != 0 }
}

/// `SSL_SESSION_get_ticket_lifetime_hint`.
// `c_ulong` is `u32` on some targets.
#[allow(clippy::useless_conversion)]
fn ticket_lifetime_hint(s: &SslSessionRef) -> u64 {
    // SAFETY: as above.
    u64::from(unsafe { ffi::SSL_SESSION_get_ticket_lifetime_hint(s.as_ptr()) })
}

/// `SSL_CIPHER_get_name(SSL_SESSION_get0_cipher(s))`.
fn cipher_name(s: &SslSessionRef) -> Option<Vec<u8>> {
    // SAFETY: the cipher pointer is owned by the library's static cipher
    // table (or NULL); the name is a NUL-terminated static string.
    unsafe {
        let c = ffi::SSL_SESSION_get0_cipher(s.as_ptr());
        if c.is_null() {
            return None;
        }
        let name = ffi::SSL_CIPHER_get_name(c);
        if name.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr(name).to_bytes().to_vec())
    }
}

/// `PEM_write_bio_SSL_SESSION`.
fn to_pem(s: &SslSessionRef) -> Option<Vec<u8>> {
    let der = s.to_der().ok()?;
    let mut out = PEM_BEGIN.to_vec();
    out.push(b'\n');
    for line in base64_encode(&der).chunks(64) {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out.extend_from_slice(PEM_END);
    out.push(b'\n');
    Some(out)
}

/// `PEM_read_bio_SSL_SESSION`: the first session block in `data`.
fn from_pem(data: &[u8]) -> Option<SslSession> {
    let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|w| w == needle);
    let start = find(data, PEM_BEGIN)? + PEM_BEGIN.len();
    let end = start + find(&data[start..], PEM_END)?;
    let body: Vec<u8> = data[start..end]
        .iter()
        .copied()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    SslSession::from_der(&base64_decode(&body)).ok()
}

/// Native state of an `Openssl\Session` (`None` until initialised).
#[derive(Default)]
struct SessionObj(Option<SslSession>);

impl std::fmt::Debug for SessionObj {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SessionObj")
            .field(&self.0.is_some())
            .finish()
    }
}

fn create(_vm: &mut Vm, o: &Obj) -> VmResult<()> {
    object::set_internal(o, SessionObj::default());
    Ok(())
}

fn exception(vm: &mut Vm, msg: &str) -> php_vm::ext::Throw {
    vm.throw(EXCEPTION, msg)
}

/// `PHP_OPENSSL_SESSION_CHECK`: runs `f` on the session of `$this`.
fn with_session<R>(vm: &mut Vm, a: &Args, f: impl FnOnce(&SslSessionRef) -> R) -> VmResult<R> {
    let o = a.this_obj();
    object::internal::<SessionObj, _>(&o, |s| s.0.as_deref().map(f))
        .flatten()
        .ok_or_else(|| exception(vm, "Session is not valid"))
}

/// Installs `session` in `o` and fills the `id` property.
fn init(vm: &mut Vm, o: &Obj, session: SslSession) {
    let id = bytes(session.id());
    object::set_internal(o, SessionObj(Some(session)));
    vm.set_property_raw(o, "id", id);
}

/// A new `Openssl\\Session` object wrapping `session`
/// (`php_openssl_session_object_init`).
pub(crate) fn session_value(vm: &mut Vm, session: SslSession) -> VmResult<Value> {
    let Some(cls) = vm.find_class(SESSION.as_bytes()) else {
        return Err(vm.throw("Error", "Class \"Openssl\\Session\" not found"));
    };
    let o = vm.new_object(&cls)?;
    init(vm, &o, session);
    Ok(Value::Object(o))
}

/// For an `Openssl\\Session` object: its session (`None` when the object
/// was never initialised). `None` for any other value.
#[allow(clippy::option_option)]
pub(crate) fn session_of(v: &Value) -> Option<Option<SslSession>> {
    let Value::Object(o) = v else {
        return None;
    };
    object::internal::<SessionObj, _>(o, |s| s.0.clone())
}

fn session_ctor(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw(
        "Error",
        "Cannot directly construct Openssl\\Session, use Openssl\\Session::import() or TLS session callbacks",
    ))
}

/// `zend_argument_value_error` for a method (the VM helper only knows
/// the bare method name).
fn method_value_error(
    vm: &mut Vm,
    method: &str,
    arg: usize,
    name: &str,
    msg: &str,
) -> php_vm::ext::Throw {
    vm.throw(
        "ValueError",
        &format!("{method}(): Argument #{arg} (${name}) {msg}"),
    )
}

fn format_arg(a: &Args, n: usize) -> i64 {
    match a.get(n) {
        Some(Value::Int(i)) => i,
        _ => OPENSSL_ENCODING_PEM,
    }
}

fn export(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let format = format_arg(a, 0);
    let r = with_session(vm, a, |s| match format {
        OPENSSL_ENCODING_DER => Some(s.to_der().ok()),
        OPENSSL_ENCODING_PEM => Some(to_pem(s)),
        _ => None,
    })?;
    match r {
        Some(Some(data)) => Ok(bytes(data)),
        Some(None) if format == OPENSSL_ENCODING_DER => {
            Err(exception(vm, "Failed to export session"))
        }
        Some(None) => Err(exception(vm, "Failed to export session as PEM")),
        None => Err(method_value_error(
            vm,
            "Openssl\\Session::export",
            1,
            "format",
            "must be OPENSSL_ENCODING_DER or OPENSSL_ENCODING_PEM",
        )),
    }
}

fn import(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.str(0);
    let session = match format_arg(a, 1) {
        OPENSSL_ENCODING_DER => SslSession::from_der(data.as_bytes()).ok(),
        OPENSSL_ENCODING_PEM => from_pem(data.as_bytes()),
        _ => {
            return Err(method_value_error(
                vm,
                "Openssl\\Session::import",
                2,
                "format",
                "must be OPENSSL_ENCODING_DER or OPENSSL_ENCODING_PEM",
            ))
        }
    };
    let Some(session) = session else {
        return Err(exception(vm, "Failed to import session data"));
    };
    let Some(cls) = vm.find_class(SESSION.as_bytes()) else {
        return Err(vm.throw("Error", "Class \"Openssl\\Session\" not found"));
    };
    let o = vm.new_object(&cls)?;
    init(vm, &o, session);
    Ok(Value::Object(o))
}

fn is_resumable_m(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(with_session(vm, a, is_resumable)?))
}

fn get_timeout(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(with_session(vm, a, SslSessionRef::timeout)?))
}

// `SSL_SESSION_get_time` returns `long` or `time_t` depending on the
// library version.
#[allow(clippy::useless_conversion)]
fn get_created_at(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let t = with_session(vm, a, |s| i64::try_from(s.time()).unwrap_or(i64::MAX))?;
    Ok(Value::Int(t))
}

fn get_protocol(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = with_session(vm, a, SslSessionRef::protocol_version)?;
    Ok(match v {
        SslVersion::TLS1_3 => Value::string("TLSv1.3"),
        SslVersion::TLS1_2 => Value::string("TLSv1.2"),
        SslVersion::TLS1_1 => Value::string("TLSv1.1"),
        SslVersion::TLS1 => Value::string("TLSv1.0"),
        _ => Value::Null,
    })
}

fn get_cipher(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(with_session(vm, a, cipher_name)?.map_or(Value::Null, bytes))
}

fn has_ticket_m(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(with_session(vm, a, has_ticket)?))
}

fn get_ticket_lifetime_hint(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = with_session(vm, a, |s| has_ticket(s).then(|| ticket_lifetime_hint(s)))?;
    Ok(r.map_or(Value::Null, |h| {
        Value::Int(i64::try_from(h).unwrap_or(i64::MAX))
    }))
}

fn serialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(pem) = with_session(vm, a, to_pem)? else {
        return Err(exception(vm, "Failed to serialize session"));
    };
    let mut out = ZArray::new();
    out.make_mut().insert(ZStr::from("pem"), bytes(pem));
    Ok(Value::Array(out))
}

fn unserialize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    let initialised = object::internal::<SessionObj, _>(&o, |s| s.0.is_some()).unwrap_or(false);
    if initialised {
        return Err(vm.throw(
            "Error",
            "Cannot call Openssl\\Session::__unserialize() on an already-initialized session",
        ));
    }
    let data = a.array(0);
    let Some(Value::Str(pem)) = data.get_str(b"pem").map(Value::deref) else {
        return Err(exception(vm, "Invalid serialization data"));
    };
    let Some(session) = from_pem(pem.as_bytes()) else {
        return Err(exception(vm, "Failed to unserialize session"));
    };
    init(vm, &o, session);
    Ok(Value::Null)
}

fn psk_ctor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    const CTOR: &str = "Openssl\\Psk::__construct";
    let psk = a.str(0);
    let identity = match a.opt(1) {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    };
    if psk.is_empty() {
        return Err(method_value_error(vm, CTOR, 1, "psk", "must not be empty"));
    }
    if psk.len() > MAX_PSK_LEN {
        let msg = format!("must not exceed {MAX_PSK_LEN} bytes");
        return Err(method_value_error(vm, CTOR, 1, "psk", &msg));
    }
    if identity
        .as_ref()
        .is_some_and(|i| i.len() > MAX_IDENTITY_LEN)
    {
        let msg = format!("must not exceed {MAX_IDENTITY_LEN} bytes");
        return Err(method_value_error(vm, CTOR, 2, "identity", &msg));
    }
    let o = a.this_obj();
    vm.set_property_raw(&o, "psk", Value::Str(psk));
    vm.set_property_raw(&o, "identity", identity.map_or(Value::Null, Value::Str));
    Ok(Value::Null)
}

/// Registers `Openssl\OpensslException`, `Openssl\Psk` and `Openssl\Session`.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    ClassBuilder::new(EXCEPTION)
        .extends(vm, "Exception")
        .register(vm);
    let strict = class_flags::FINAL | NO_DYNAMIC_PROPERTIES;
    let ro = acc::PUBLIC | acc::READONLY;
    let psk_max = i64::try_from(MAX_PSK_LEN).unwrap_or(0);
    let id_max = i64::try_from(MAX_IDENTITY_LEN).unwrap_or(0);
    ClassBuilder::new("Openssl\\Psk")
        .flags(strict)
        .constant("MAX_PSK_LEN", Value::Int(psk_max))
        .constant("MAX_IDENTITY_LEN", Value::Int(id_max))
        .prop("psk", ro, Some("string"), None)
        .prop("identity", ro, Some("?string"), None)
        .method(
            "__construct(string $psk, ?string $identity = null)",
            acc::PUBLIC,
            psk_ctor,
        )
        .register(vm);
    ClassBuilder::new(SESSION)
        .flags(strict)
        .prop("id", ro, Some("string"), None)
        .method("__construct()", acc::PUBLIC, session_ctor)
        .method(
            "export(int $format = OPENSSL_ENCODING_PEM): string",
            acc::PUBLIC,
            export,
        )
        .method(
            "import(string $data, int $format = OPENSSL_ENCODING_PEM): Openssl\\Session",
            acc::PUBLIC | acc::STATIC,
            import,
        )
        .method("isResumable(): bool", acc::PUBLIC, is_resumable_m)
        .method("getTimeout(): int", acc::PUBLIC, get_timeout)
        .method("getCreatedAt(): int", acc::PUBLIC, get_created_at)
        .method("getProtocol(): ?string", acc::PUBLIC, get_protocol)
        .method("getCipher(): ?string", acc::PUBLIC, get_cipher)
        .method("hasTicket(): bool", acc::PUBLIC, has_ticket_m)
        .method(
            "getTicketLifetimeHint(): ?int",
            acc::PUBLIC,
            get_ticket_lifetime_hint,
        )
        .method("__serialize(): array", acc::PUBLIC, serialize)
        .method("__unserialize(array $data): void", acc::PUBLIC, unserialize)
        .hooks(ClassHooks {
            create: Some(create),
            ..ClassHooks::default()
        })
        .register(vm);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pem_garbage_is_rejected() {
        assert!(from_pem(b"invalid").is_none());
        let fake = [PEM_BEGIN, b"\nAAAA\n", PEM_END, b"\n"].concat();
        assert!(from_pem(&fake).is_none());
    }
}
