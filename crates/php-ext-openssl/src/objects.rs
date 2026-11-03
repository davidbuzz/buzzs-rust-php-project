//! The three opaque classes (`OpenSSLCertificate`,
//! `OpenSSLCertificateSigningRequest`, `OpenSSLAsymmetricKey`) and the
//! owned forms of the "object or string" arguments.
//!
//! Each object carries its library value behind an `Rc` as internal state,
//! so argument holders can keep it alive while the library borrows it.

use std::rc::Rc;

use php_openssl::{
    AsymmetricKey, CertArg, Certificate, CertificateSigningRequest, CsrArg, KeyArg,
    CONSTRUCT_ERRORS,
};
use php_runtime::{Obj, Value};
use php_vm::ext::{acc, class_flags, Args, ClassBuilder, ModuleBuilder, VmResult};
use php_vm::{object, Vm};

/// `OpenSSLCertificate`.
pub(crate) const CERT_CLASS: &str = "OpenSSLCertificate";
/// `OpenSSLCertificateSigningRequest`.
pub(crate) const CSR_CLASS: &str = "OpenSSLCertificateSigningRequest";
/// `OpenSSLAsymmetricKey`.
pub(crate) const KEY_CLASS: &str = "OpenSSLAsymmetricKey";

/// `ZEND_ACC_NO_DYNAMIC_PROPERTIES` (`@strict-properties`). The bit is not
/// re-exported by `php_vm::ext::class_flags`, so it is spelled out here
/// with the value of `php_vm::class::cflags::NO_DYNAMIC_PROPERTIES`.
const NO_DYNAMIC_PROPERTIES: u32 = 1 << 14;

fn construct_error(class: &str) -> &'static str {
    CONSTRUCT_ERRORS
        .iter()
        .find(|(c, _)| *c == class)
        .map_or("Cannot directly construct object", |(_, m)| *m)
}

/// The `get_constructor` handler: direct construction is an `Error`.
fn ctor(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let class = a
        .this
        .as_ref()
        .map(|o| object::class_of(o).name.to_string())
        .unwrap_or_default();
    Err(vm.throw("Error", construct_error(&class)))
}

/// Registers the three classes.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    for name in [CERT_CLASS, CSR_CLASS, KEY_CLASS] {
        ClassBuilder::new(name)
            .flags(
                class_flags::FINAL
                    | class_flags::NOT_SERIALIZABLE
                    | class_flags::NO_CLONE
                    | NO_DYNAMIC_PROPERTIES,
            )
            .method("__construct()", acc::PUBLIC, ctor)
            .register(m.vm());
    }
}

fn wrap<T: 'static>(vm: &mut Vm, class: &str, state: T) -> VmResult<Value> {
    let Some(cls) = vm.find_class(class.as_bytes()) else {
        return Err(vm.throw("Error", &format!("Class \"{class}\" not found")));
    };
    let o = vm.new_object(&cls)?;
    object::set_internal(&o, Rc::new(state));
    Ok(Value::Object(o))
}

/// A new `OpenSSLCertificate`.
pub(crate) fn new_cert(vm: &mut Vm, c: Certificate) -> VmResult<Value> {
    wrap(vm, CERT_CLASS, c)
}

/// A new `OpenSSLCertificateSigningRequest`.
pub(crate) fn new_csr(vm: &mut Vm, c: CertificateSigningRequest) -> VmResult<Value> {
    wrap(vm, CSR_CLASS, c)
}

/// A new `OpenSSLAsymmetricKey`.
pub(crate) fn new_key(vm: &mut Vm, k: AsymmetricKey) -> VmResult<Value> {
    wrap(vm, KEY_CLASS, k)
}

/// The certificate inside an `OpenSSLCertificate` object.
pub(crate) fn cert_of(o: &Obj) -> Option<Rc<Certificate>> {
    object::internal::<Rc<Certificate>, _>(o, |c| Rc::clone(c))
}

/// The request inside an `OpenSSLCertificateSigningRequest` object.
pub(crate) fn csr_of(o: &Obj) -> Option<Rc<CertificateSigningRequest>> {
    object::internal::<Rc<CertificateSigningRequest>, _>(o, |c| Rc::clone(c))
}

/// The key inside an `OpenSSLAsymmetricKey` object.
pub(crate) fn key_of(o: &Obj) -> Option<Rc<AsymmetricKey>> {
    object::internal::<Rc<AsymmetricKey>, _>(o, |k| Rc::clone(k))
}

/// An owned `OpenSSLCertificate|string` argument.
#[derive(Debug, Clone)]
pub(crate) enum CertHold {
    /// A certificate object.
    Obj(Rc<Certificate>),
    /// PEM data or a `file://` path.
    Str(Vec<u8>),
}

impl CertHold {
    /// Borrows as the library argument.
    pub(crate) fn arg(&self) -> CertArg<'_> {
        match self {
            CertHold::Obj(c) => CertArg::Object(c),
            CertHold::Str(s) => CertArg::Str(s),
        }
    }
}

/// An owned `OpenSSLCertificateSigningRequest|string` argument.
#[derive(Debug, Clone)]
pub(crate) enum CsrHold {
    /// A request object.
    Obj(Rc<CertificateSigningRequest>),
    /// PEM data or a `file://` path.
    Str(Vec<u8>),
}

impl CsrHold {
    /// Borrows as the library argument.
    pub(crate) fn arg(&self) -> CsrArg<'_> {
        match self {
            CsrHold::Obj(c) => CsrArg::Object(c),
            CsrHold::Str(s) => CsrArg::Str(s),
        }
    }
}

/// An owned key argument (`OpenSSLAsymmetricKey|OpenSSLCertificate|array|string`).
#[derive(Debug, Clone)]
pub(crate) enum KeyHold {
    /// A key object.
    Key(Rc<AsymmetricKey>),
    /// A certificate object (its public key).
    Cert(Rc<Certificate>),
    /// PEM data or a `file://` path.
    Str(Vec<u8>),
    /// `[key, passphrase]`.
    Array {
        /// Element 0.
        key: Option<Box<KeyHold>>,
        /// Element 1, converted to a string.
        passphrase: Option<Vec<u8>>,
    },
    /// Anything else (yields `false` without a message).
    Other,
}

impl KeyHold {
    /// Borrows as the library argument.
    pub(crate) fn arg(&self) -> KeyArg<'_> {
        match self {
            KeyHold::Key(k) => KeyArg::Key(k),
            KeyHold::Cert(c) => KeyArg::Cert(c),
            KeyHold::Str(s) => KeyArg::Str(s),
            KeyHold::Array { key, passphrase } => KeyArg::Array {
                key: key.as_ref().map(|k| Box::new(k.arg())),
                passphrase: passphrase.clone(),
            },
            KeyHold::Other => KeyArg::Other,
        }
    }
}
