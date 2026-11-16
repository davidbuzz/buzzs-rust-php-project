// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The only module containing `unsafe`: raw libcrypto declarations plus thin
//! RAII handles and safe wrappers around them.
//!
//! The `openssl` crate's safe API is not used for the operations PHP
//! performs because PHP relies on behaviour the safe wrappers forbid or
//! assert against (zero-padded keys of the "wrong" length, `NULL`-input
//! cipher updates for CCM, tag lengths outside the documented range,
//! OpenSSL's exact error-queue side effects, …). Every wrapper here mirrors
//! one libcrypto call as used by `ext/openssl`, leaving errors on
//! OpenSSL's thread-local error queue so callers can drain them exactly
//! where PHP calls `php_openssl_store_errors()`.
//!
//! All opaque C types are declared locally; `openssl-sys` is depended on
//! for linking against the system libcrypto/libssl (via pkg-config).

// C interop: integer widths follow the C prototypes, mirroring PHP's own
// casts (`(int) len`, …); lengths handled here are bounded by PHP strings.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::upper_case_acronyms,
    non_camel_case_types
)]

use std::ffi::{c_char, c_int, c_long, c_uchar, c_uint, c_ulong, c_void, CStr, CString};
use std::marker::PhantomData;
use std::ptr::{self, NonNull};

use openssl_sys as _; // links libcrypto/libssl

// ---------------------------------------------------------------------------
// Opaque C types
// ---------------------------------------------------------------------------

macro_rules! opaque {
    ($($name:ident),* $(,)?) => {
        $(
            #[repr(C)]
            #[doc = concat!("Opaque `", stringify!($name), "`.")]
            struct $name {
                _private: [u8; 0],
            }
        )*
    };
}

opaque!(
    BIO,
    BIO_METHOD,
    EVP_CIPHER,
    EVP_CIPHER_CTX,
    EVP_MD,
    EVP_MD_CTX,
    EVP_PKEY,
    EVP_PKEY_CTX,
    X509,
    X509_REQ,
    X509_NAME,
    X509_NAME_ENTRY,
    X509_EXTENSION,
    X509_STORE,
    X509_STORE_CTX,
    X509_LOOKUP,
    X509_LOOKUP_METHOD,
    X509_PURPOSE,
    X509_PUBKEY,
    X509_CRL,
    X509V3_EXT_METHOD,
    ASN1_STRING,
    ASN1_OBJECT,
    BIGNUM,
    BN_CTX,
    EC_GROUP,
    EC_POINT,
    OSSL_PARAM,
    OSSL_PARAM_BLD,
    OSSL_LIB_CTX,
    OPENSSL_STACK,
    CONF,
    CONF_METHOD,
    PKCS7,
    PKCS12,
    CMS_ContentInfo,
    ENGINE,
);

/// `ASN1_INTEGER`, `ASN1_TIME`, … are all `ASN1_STRING` in C.
type ASN1_INTEGER = ASN1_STRING;
/// See [`ASN1_INTEGER`].
type ASN1_TIME = ASN1_STRING;

#[repr(C)]
struct OBJ_NAME {
    type_: c_int,
    alias: c_int,
    name: *const c_char,
    data: *const c_char,
}

#[repr(C)]
struct EC_builtin_curve {
    nid: c_int,
    comment: *const c_char,
}

#[repr(C)]
struct GENERAL_NAME {
    type_: c_int,
    d: *mut ASN1_STRING,
}

#[repr(C)]
struct CONF_VALUE {
    section: *mut c_char,
    name: *mut c_char,
    value: *mut c_char,
}

/// Leading fields of `X509_INFO` (only `x509` is accessed).
#[repr(C)]
struct X509_INFO {
    x509: *mut X509,
}

/// `NETSCAPE_SPKAC` (public struct in `<openssl/x509.h>`).
#[repr(C)]
struct NETSCAPE_SPKAC {
    pubkey: *mut X509_PUBKEY,
    challenge: *mut ASN1_STRING,
}

/// Leading field of `NETSCAPE_SPKI`.
#[repr(C)]
struct NETSCAPE_SPKI {
    spkac: *mut NETSCAPE_SPKAC,
}

/// Leading fields of `PKCS7` (public struct in `<openssl/pkcs7.h>`).
#[repr(C)]
struct PKCS7_HEAD {
    asn1: *mut c_uchar,
    length: c_long,
    state: c_int,
    detached: c_int,
    type_: *mut ASN1_OBJECT,
    d: *mut PKCS7_SIGNED_HEAD,
}

/// Leading fields shared by `PKCS7_SIGNED` and `PKCS7_SIGN_ENVELOPE`.
#[repr(C)]
struct PKCS7_SIGNED_HEAD {
    version: *mut ASN1_INTEGER,
    md_algs: *mut OPENSSL_STACK,
    cert: *mut OPENSSL_STACK,
    crl: *mut OPENSSL_STACK,
}

/// Generously sized, zeroed storage for `X509V3_CTX` (whose layout grew
/// across OpenSSL releases); only ever initialised via `X509V3_set_ctx`.
#[repr(C, align(16))]
struct X509V3_CTX {
    _storage: [u8; 256],
}

type PemPasswordCb = unsafe extern "C" fn(*mut c_char, c_int, c_int, *mut c_void) -> c_int;
type FreeFn = unsafe extern "C" fn(*mut c_void);

// ---------------------------------------------------------------------------
// Raw declarations
// ---------------------------------------------------------------------------

#[allow(clippy::upper_case_acronyms, clashing_extern_declarations)]
extern "C" {
    // init / errors / memory
    fn OPENSSL_init_ssl(opts: u64, settings: *const c_void) -> c_int;
    fn ERR_get_error() -> c_ulong;
    fn ERR_error_string_n(e: c_ulong, buf: *mut c_char, len: usize);
    fn ERR_set_mark() -> c_int;
    fn ERR_pop_to_mark() -> c_int;
    fn ERR_clear_error();
    fn CRYPTO_free(p: *mut c_void, file: *const c_char, line: c_int);
    fn OpenSSL_version(t: c_int) -> *const c_char;
    fn OpenSSL_version_num() -> c_ulong;

    // BIO
    fn BIO_new(m: *const BIO_METHOD) -> *mut BIO;
    fn BIO_s_mem() -> *const BIO_METHOD;
    fn BIO_new_mem_buf(buf: *const c_void, len: c_int) -> *mut BIO;
    fn BIO_new_file(name: *const c_char, mode: *const c_char) -> *mut BIO;
    fn BIO_free(b: *mut BIO) -> c_int;
    fn BIO_write(b: *mut BIO, data: *const c_void, len: c_int) -> c_int;
    fn BIO_puts(b: *mut BIO, s: *const c_char) -> c_int;
    fn BIO_ctrl(b: *mut BIO, cmd: c_int, larg: c_long, parg: *mut c_void) -> c_long;

    // objects
    fn OBJ_NAME_get(name: *const c_char, type_: c_int) -> *const c_char;
    fn OBJ_NAME_do_all_sorted(
        type_: c_int,
        f: unsafe extern "C" fn(*const OBJ_NAME, *mut c_void),
        arg: *mut c_void,
    );
    fn OBJ_nid2sn(n: c_int) -> *const c_char;
    fn OBJ_nid2ln(n: c_int) -> *const c_char;
    fn OBJ_sn2nid(s: *const c_char) -> c_int;
    fn OBJ_ln2nid(s: *const c_char) -> c_int;
    fn OBJ_txt2nid(s: *const c_char) -> c_int;
    fn OBJ_obj2nid(o: *const ASN1_OBJECT) -> c_int;
    fn OBJ_nid2obj(n: c_int) -> *mut ASN1_OBJECT;
    fn OBJ_obj2txt(buf: *mut c_char, len: c_int, a: *const ASN1_OBJECT, no_name: c_int) -> c_int;
    fn OBJ_create(oid: *const c_char, sn: *const c_char, ln: *const c_char) -> c_int;
    fn OBJ_create_objects(b: *mut BIO) -> c_int;
    fn ASN1_OBJECT_free(o: *mut ASN1_OBJECT);

    // ciphers
    fn EVP_CIPHER_fetch(
        ctx: *mut OSSL_LIB_CTX,
        name: *const c_char,
        props: *const c_char,
    ) -> *mut EVP_CIPHER;
    fn EVP_CIPHER_free(c: *mut EVP_CIPHER);
    fn EVP_CIPHER_get_iv_length(c: *const EVP_CIPHER) -> c_int;
    fn EVP_CIPHER_get_key_length(c: *const EVP_CIPHER) -> c_int;
    fn EVP_CIPHER_get_block_size(c: *const EVP_CIPHER) -> c_int;
    fn EVP_CIPHER_get_mode(c: *const EVP_CIPHER) -> c_int;
    fn EVP_CIPHER_get_nid(c: *const EVP_CIPHER) -> c_int;
    fn EVP_CIPHER_get0_name(c: *const EVP_CIPHER) -> *const c_char;
    fn EVP_CIPHER_do_all_provided(
        ctx: *mut OSSL_LIB_CTX,
        f: unsafe extern "C" fn(*mut EVP_CIPHER, *mut c_void),
        arg: *mut c_void,
    );
    fn EVP_CIPHER_names_do_all(
        c: *const EVP_CIPHER,
        f: unsafe extern "C" fn(*const c_char, *mut c_void),
        arg: *mut c_void,
    ) -> c_int;
    fn EVP_CIPHER_CTX_new() -> *mut EVP_CIPHER_CTX;
    fn EVP_CIPHER_CTX_free(c: *mut EVP_CIPHER_CTX);
    fn EVP_CIPHER_CTX_reset(c: *mut EVP_CIPHER_CTX) -> c_int;
    fn EVP_CIPHER_CTX_ctrl(c: *mut EVP_CIPHER_CTX, t: c_int, arg: c_int, p: *mut c_void) -> c_int;
    fn EVP_CIPHER_CTX_set_key_length(c: *mut EVP_CIPHER_CTX, len: c_int) -> c_int;
    fn EVP_CIPHER_CTX_set_padding(c: *mut EVP_CIPHER_CTX, pad: c_int) -> c_int;
    fn EVP_CipherInit_ex(
        c: *mut EVP_CIPHER_CTX,
        t: *const EVP_CIPHER,
        e: *mut ENGINE,
        key: *const c_uchar,
        iv: *const c_uchar,
        enc: c_int,
    ) -> c_int;
    fn EVP_CipherUpdate(
        c: *mut EVP_CIPHER_CTX,
        out: *mut c_uchar,
        outl: *mut c_int,
        inp: *const c_uchar,
        inl: c_int,
    ) -> c_int;
    fn EVP_EncryptFinal_ex(c: *mut EVP_CIPHER_CTX, out: *mut c_uchar, outl: *mut c_int) -> c_int;
    fn EVP_DecryptFinal_ex(c: *mut EVP_CIPHER_CTX, out: *mut c_uchar, outl: *mut c_int) -> c_int;
    fn EVP_EncryptInit(
        c: *mut EVP_CIPHER_CTX,
        t: *const EVP_CIPHER,
        key: *const c_uchar,
        iv: *const c_uchar,
    ) -> c_int;
    fn EVP_SealInit(
        c: *mut EVP_CIPHER_CTX,
        t: *const EVP_CIPHER,
        ek: *mut *mut c_uchar,
        ekl: *mut c_int,
        iv: *mut c_uchar,
        pubk: *mut *mut EVP_PKEY,
        npubk: c_int,
    ) -> c_int;
    fn EVP_SealFinal(c: *mut EVP_CIPHER_CTX, out: *mut c_uchar, outl: *mut c_int) -> c_int;
    fn EVP_OpenInit(
        c: *mut EVP_CIPHER_CTX,
        t: *const EVP_CIPHER,
        ek: *const c_uchar,
        ekl: c_int,
        iv: *const c_uchar,
        priv_: *mut EVP_PKEY,
    ) -> c_int;
    fn EVP_OpenFinal(c: *mut EVP_CIPHER_CTX, out: *mut c_uchar, outl: *mut c_int) -> c_int;

    // digests
    fn EVP_MD_fetch(
        ctx: *mut OSSL_LIB_CTX,
        name: *const c_char,
        props: *const c_char,
    ) -> *mut EVP_MD;
    fn EVP_MD_free(m: *mut EVP_MD);
    fn EVP_MD_get_size(m: *const EVP_MD) -> c_int;
    fn EVP_md_null() -> *const EVP_MD;
    fn EVP_MD_CTX_new() -> *mut EVP_MD_CTX;
    fn EVP_MD_CTX_free(c: *mut EVP_MD_CTX);
    fn EVP_DigestInit(c: *mut EVP_MD_CTX, t: *const EVP_MD) -> c_int;
    fn EVP_DigestUpdate(c: *mut EVP_MD_CTX, d: *const c_void, n: usize) -> c_int;
    fn EVP_DigestFinal(c: *mut EVP_MD_CTX, md: *mut c_uchar, s: *mut c_uint) -> c_int;
    fn EVP_DigestSignInit(
        c: *mut EVP_MD_CTX,
        pctx: *mut *mut EVP_PKEY_CTX,
        t: *const EVP_MD,
        e: *mut ENGINE,
        pkey: *mut EVP_PKEY,
    ) -> c_int;
    fn EVP_DigestSign(
        c: *mut EVP_MD_CTX,
        sig: *mut c_uchar,
        siglen: *mut usize,
        tbs: *const c_uchar,
        tbslen: usize,
    ) -> c_int;
    fn EVP_DigestVerifyInit(
        c: *mut EVP_MD_CTX,
        pctx: *mut *mut EVP_PKEY_CTX,
        t: *const EVP_MD,
        e: *mut ENGINE,
        pkey: *mut EVP_PKEY,
    ) -> c_int;
    fn EVP_DigestVerify(
        c: *mut EVP_MD_CTX,
        sig: *const c_uchar,
        siglen: usize,
        tbs: *const c_uchar,
        tbslen: usize,
    ) -> c_int;

    // random / kdf
    fn RAND_bytes(buf: *mut c_uchar, num: c_int) -> c_int;
    fn RAND_load_file(file: *const c_char, max: c_long) -> c_int;
    fn RAND_write_file(file: *const c_char) -> c_int;
    fn RAND_file_name(buf: *mut c_char, num: usize) -> *const c_char;
    fn RAND_status() -> c_int;
    fn PKCS5_PBKDF2_HMAC(
        pass: *const c_char,
        passlen: c_int,
        salt: *const c_uchar,
        saltlen: c_int,
        iter: c_int,
        md: *const EVP_MD,
        keylen: c_int,
        out: *mut c_uchar,
    ) -> c_int;

    // EC curves
    fn EC_get_builtin_curves(r: *mut EC_builtin_curve, n: usize) -> usize;

    // default locations
    fn X509_get_default_cert_file() -> *const c_char;
    fn X509_get_default_cert_file_env() -> *const c_char;
    fn X509_get_default_cert_dir() -> *const c_char;
    fn X509_get_default_cert_dir_env() -> *const c_char;
    fn X509_get_default_private_dir() -> *const c_char;
    fn X509_get_default_cert_area() -> *const c_char;

    // stacks
    fn OPENSSL_sk_num(s: *const OPENSSL_STACK) -> c_int;
    fn OPENSSL_sk_value(s: *const OPENSSL_STACK, i: c_int) -> *mut c_void;
    fn OPENSSL_sk_new_null() -> *mut OPENSSL_STACK;
    fn OPENSSL_sk_push(s: *mut OPENSSL_STACK, d: *const c_void) -> c_int;
    fn OPENSSL_sk_shift(s: *mut OPENSSL_STACK) -> *mut c_void;
    fn OPENSSL_sk_free(s: *mut OPENSSL_STACK);
    fn OPENSSL_sk_pop_free(s: *mut OPENSSL_STACK, f: FreeFn);
}

const OPENSSL_INIT_LOAD_CONFIG: u64 = 0x0000_0040;
const OBJ_NAME_TYPE_MD_METH: c_int = 1;
const OBJ_NAME_TYPE_CIPHER_METH: c_int = 2;
const BIO_CTRL_RESET: c_int = 1;
const BIO_CTRL_INFO: c_int = 3;
const BIO_C_SET_BUF_MEM_EOF_RETURN: c_int = 130;

/// `EVP_CIPH_*_MODE` values.
pub mod cipher_mode {
    /// GCM.
    pub const GCM: i32 = 0x6;
    /// CCM.
    pub const CCM: i32 = 0x7;
    /// Key wrap.
    pub const WRAP: i32 = 0x10002;
    /// OCB.
    pub const OCB: i32 = 0x10003;
    /// SIV.
    pub const SIV: i32 = 0x10004;
}
/// `NID_chacha20_poly1305`.
pub const NID_CHACHA20_POLY1305: i32 = 1018;
/// `EVP_CTRL_AEAD_SET_IVLEN`.
pub const EVP_CTRL_AEAD_SET_IVLEN: i32 = 0x9;
/// `EVP_CTRL_AEAD_GET_TAG`.
pub const EVP_CTRL_AEAD_GET_TAG: i32 = 0x10;
/// `EVP_CTRL_AEAD_SET_TAG`.
pub const EVP_CTRL_AEAD_SET_TAG: i32 = 0x11;

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Copies a C string returned by OpenSSL (`NULL` → `None`).
fn cstr_opt(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        // SAFETY: OpenSSL returns NUL-terminated strings that stay valid
        // at least until the next call on the same object.
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

/// Copies and frees an `OPENSSL_malloc`ed C string.
fn take_cstr(p: *mut c_char) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    // SAFETY: `p` is a NUL-terminated string allocated by OpenSSL that we
    // own; it is copied before being released with `OPENSSL_free`.
    let v = unsafe { CStr::from_ptr(p) }.to_bytes().to_vec();
    openssl_free(p.cast());
    Some(v)
}

fn openssl_free(p: *mut c_void) {
    // SAFETY: `p` was allocated by OpenSSL's allocator (or is NULL, which
    // `CRYPTO_free` ignores).
    unsafe { CRYPTO_free(p, c"php-openssl".as_ptr(), 0) }
}

/// Converts bytes to a C string, truncating at the first NUL like C code
/// receiving a `char *` would.
pub fn to_cstring(bytes: &[u8]) -> CString {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    CString::new(&bytes[..end]).unwrap_or_default()
}

fn c_len(len: usize) -> c_int {
    c_int::try_from(len).unwrap_or(c_int::MAX)
}

macro_rules! handle {
    ($(#[$m:meta])* $name:ident, $raw:ty, $free:expr) => {
        $(#[$m])*
        pub struct $name(NonNull<$raw>);

        impl $name {
            /// Wraps an owned raw pointer (`NULL` → `None`).
            fn from_raw(p: *mut $raw) -> Option<Self> {
                NonNull::new(p).map(Self)
            }

            /// The raw pointer (still owned by `self`).
            fn as_ptr(&self) -> *mut $raw {
                self.0.as_ptr()
            }

            /// Releases ownership of the raw pointer.
            #[allow(dead_code)]
            fn into_raw(self) -> *mut $raw {
                let p = self.0.as_ptr();
                std::mem::forget(self);
                p
            }
        }

        impl Drop for $name {
            fn drop(&mut self) {
                let f: unsafe extern "C" fn(*mut $raw) = $free;
                // SAFETY: we own the pointer and free it exactly once.
                unsafe { f(self.0.as_ptr()) }
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, concat!(stringify!($name), "({:p})"), self.0.as_ptr())
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Library init, errors, version
// ---------------------------------------------------------------------------

/// `OPENSSL_init_ssl(OPENSSL_INIT_LOAD_CONFIG, NULL)` as done at PHP's
/// MINIT. Idempotent.
pub fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: plain library initialisation call.
        unsafe {
            OPENSSL_init_ssl(OPENSSL_INIT_LOAD_CONFIG, ptr::null());
        }
    });
}

/// Pops the oldest error code from OpenSSL's thread-local queue (0 = none).
pub fn err_get_error() -> u64 {
    // SAFETY: no preconditions.
    unsafe { ERR_get_error() }
}

/// `ERR_error_string_n` into a 256-byte buffer, as PHP does.
pub fn err_error_string(code: u64) -> String {
    let mut buf = [0 as c_char; 256];
    // SAFETY: buffer and its length are valid.
    unsafe { ERR_error_string_n(code as c_ulong, buf.as_mut_ptr(), buf.len()) };
    // SAFETY: ERR_error_string_n always NUL-terminates.
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// `ERR_set_mark()`.
pub fn err_set_mark() {
    // SAFETY: no preconditions.
    unsafe { ERR_set_mark() };
}

/// `ERR_pop_to_mark()`.
pub fn err_pop_to_mark() {
    // SAFETY: no preconditions.
    unsafe { ERR_pop_to_mark() };
}

/// `ERR_clear_error()`.
pub fn err_clear() {
    // SAFETY: no preconditions.
    unsafe { ERR_clear_error() }
}

/// `OpenSSL_version(OPENSSL_VERSION)`.
pub fn version_text() -> String {
    // SAFETY: returns a static string.
    cstr_opt(unsafe { OpenSSL_version(0) }).unwrap_or_default()
}

/// `OpenSSL_version_num()`.
pub fn version_number() -> u64 {
    // SAFETY: no preconditions.
    unsafe { OpenSSL_version_num() }
}

// ---------------------------------------------------------------------------
// BIO
// ---------------------------------------------------------------------------

/// An owned `BIO`, possibly borrowing caller memory for its lifetime.
pub struct Bio<'a> {
    ptr: NonNull<BIO>,
    _data: PhantomData<&'a [u8]>,
}

impl std::fmt::Debug for Bio<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bio({:p})", self.ptr.as_ptr())
    }
}

impl Drop for Bio<'_> {
    fn drop(&mut self) {
        // SAFETY: owned BIO freed once.
        unsafe { BIO_free(self.ptr.as_ptr()) };
    }
}

impl Bio<'static> {
    /// `BIO_new(BIO_s_mem())`.
    pub fn mem() -> Option<Self> {
        // SAFETY: plain constructor.
        Self::wrap(unsafe { BIO_new(BIO_s_mem()) })
    }

    /// `BIO_new_file(path, mode)`.
    pub fn file(path: &[u8], mode: &str) -> Option<Self> {
        let p = to_cstring(path);
        let m = to_cstring(mode.as_bytes());
        // SAFETY: both strings are valid NUL-terminated C strings.
        Self::wrap(unsafe { BIO_new_file(p.as_ptr(), m.as_ptr()) })
    }

    /// A writable memory BIO pre-filled with `data` (`BIO_new` + `BIO_write`).
    /// Returns the BIO and `BIO_write`'s return value.
    pub fn mem_written(data: &[u8]) -> Option<(Self, i32)> {
        let b = Self::mem()?;
        let n = b.write(data);
        Some((b, n))
    }

    fn wrap(p: *mut BIO) -> Option<Self> {
        NonNull::new(p).map(|ptr| Bio {
            ptr,
            _data: PhantomData,
        })
    }
}

impl<'a> Bio<'a> {
    /// `BIO_new_mem_buf(data, len)`: a read-only BIO over `data`.
    pub fn mem_buf(data: &'a [u8]) -> Option<Self> {
        // SAFETY: `data` outlives the BIO thanks to the lifetime parameter.
        let p = unsafe { BIO_new_mem_buf(data.as_ptr().cast(), c_len(data.len())) };
        NonNull::new(p).map(|ptr| Bio {
            ptr,
            _data: PhantomData,
        })
    }

    fn as_ptr(&self) -> *mut BIO {
        self.ptr.as_ptr()
    }

    /// `BIO_write`.
    pub fn write(&self, data: &[u8]) -> i32 {
        // SAFETY: valid BIO and buffer.
        unsafe { BIO_write(self.as_ptr(), data.as_ptr().cast(), c_len(data.len())) }
    }

    /// `BIO_puts`.
    pub fn puts(&self, s: &[u8]) -> i32 {
        let c = to_cstring(s);
        // SAFETY: valid BIO and C string.
        unsafe { BIO_puts(self.as_ptr(), c.as_ptr()) }
    }

    /// `BIO_reset` (rewinds memory/file BIOs).
    pub fn reset(&self) -> i32 {
        // SAFETY: valid BIO.
        unsafe { BIO_ctrl(self.as_ptr(), BIO_CTRL_RESET, 0, ptr::null_mut()) as i32 }
    }

    /// Makes an empty memory BIO report EOF instead of "retry".
    pub fn set_mem_eof_return(&self, v: i32) {
        // SAFETY: valid BIO; harmless for non-memory BIOs.
        unsafe {
            BIO_ctrl(
                self.as_ptr(),
                BIO_C_SET_BUF_MEM_EOF_RETURN,
                c_long::from(v),
                ptr::null_mut(),
            )
        };
    }

    /// Contents of a memory BIO (`BIO_get_mem_data`).
    pub fn contents(&self) -> Vec<u8> {
        let mut p: *mut c_char = ptr::null_mut();
        // SAFETY: BIO_CTRL_INFO stores a pointer to the internal buffer.
        let n = unsafe { BIO_ctrl(self.as_ptr(), BIO_CTRL_INFO, 0, ptr::addr_of_mut!(p).cast()) };
        if p.is_null() || n <= 0 {
            return Vec::new();
        }
        // SAFETY: OpenSSL guarantees `n` readable bytes at `p`.
        unsafe { std::slice::from_raw_parts(p.cast::<u8>(), n as usize) }.to_vec()
    }
}

// ---------------------------------------------------------------------------
// Ciphers
// ---------------------------------------------------------------------------

handle!(
    /// A cipher from `OBJ_NAME_get` or `EVP_CIPHER_fetch`
    /// (`EVP_CIPHER_free` is a no-op for the static ones).
    Cipher,
    EVP_CIPHER,
    EVP_CIPHER_free
);

/// `php_openssl_get_evp_cipher_by_name`: legacy name table, then fetch.
pub fn cipher_by_name(name: &[u8]) -> Option<Cipher> {
    let c = to_cstring(name);
    // SAFETY: valid C string; OBJ_NAME_get returns a static EVP_CIPHER.
    let p = unsafe { OBJ_NAME_get(c.as_ptr(), OBJ_NAME_TYPE_CIPHER_METH) };
    if !p.is_null() {
        return Cipher::from_raw(p.cast_mut().cast());
    }
    // SAFETY: valid C string, default library context.
    Cipher::from_raw(unsafe { EVP_CIPHER_fetch(ptr::null_mut(), c.as_ptr(), ptr::null()) })
}

impl Cipher {
    /// `EVP_CIPHER_get_iv_length`.
    pub fn iv_length(&self) -> i32 {
        // SAFETY: valid cipher.
        unsafe { EVP_CIPHER_get_iv_length(self.as_ptr()) }
    }
    /// `EVP_CIPHER_get_key_length`.
    pub fn key_length(&self) -> i32 {
        // SAFETY: valid cipher.
        unsafe { EVP_CIPHER_get_key_length(self.as_ptr()) }
    }
    /// `EVP_CIPHER_get_block_size`.
    pub fn block_size(&self) -> i32 {
        // SAFETY: valid cipher.
        unsafe { EVP_CIPHER_get_block_size(self.as_ptr()) }
    }
    /// `EVP_CIPHER_get_mode`.
    pub fn mode(&self) -> i32 {
        // SAFETY: valid cipher.
        unsafe { EVP_CIPHER_get_mode(self.as_ptr()) }
    }
    /// `EVP_CIPHER_get_nid`.
    pub fn nid(&self) -> i32 {
        // SAFETY: valid cipher.
        unsafe { EVP_CIPHER_get_nid(self.as_ptr()) }
    }
}

handle!(
    /// `EVP_CIPHER_CTX`.
    CipherCtx,
    EVP_CIPHER_CTX,
    EVP_CIPHER_CTX_free
);

impl CipherCtx {
    /// `EVP_CIPHER_CTX_new`.
    pub fn new() -> Option<Self> {
        // SAFETY: constructor.
        Self::from_raw(unsafe { EVP_CIPHER_CTX_new() })
    }

    /// `EVP_CipherInit_ex(ctx, cipher, NULL, key, iv, enc)`. `None` passes
    /// `NULL`. Key/IV buffers must be at least as long as the cipher
    /// expects; callers pad them as PHP does.
    pub fn init(
        &self,
        cipher: Option<&Cipher>,
        key: Option<&[u8]>,
        iv: Option<&[u8]>,
        enc: i32,
    ) -> bool {
        // SAFETY: pointers are valid or NULL; OpenSSL reads at most the
        // configured key/IV length, which callers guarantee is available.
        unsafe {
            EVP_CipherInit_ex(
                self.as_ptr(),
                cipher.map_or(ptr::null(), |c| c.as_ptr().cast_const()),
                ptr::null_mut(),
                key.map_or(ptr::null(), <[u8]>::as_ptr),
                iv.map_or(ptr::null(), <[u8]>::as_ptr),
                enc,
            ) == 1
        }
    }

    /// `EVP_CIPHER_CTX_ctrl(ctx, type, arg, NULL)`.
    pub fn ctrl(&self, t: i32, arg: i32) -> i32 {
        // SAFETY: NULL data pointer is valid for length-setting ctrls.
        unsafe { EVP_CIPHER_CTX_ctrl(self.as_ptr(), t, arg, ptr::null_mut()) }
    }

    /// `EVP_CIPHER_CTX_ctrl(ctx, SET_TAG, len, tag)`.
    pub fn ctrl_set_tag(&self, t: i32, tag: &[u8]) -> i32 {
        let mut copy = tag.to_vec();
        // SAFETY: buffer of exactly `len` bytes.
        unsafe {
            EVP_CIPHER_CTX_ctrl(
                self.as_ptr(),
                t,
                c_len(copy.len()),
                copy.as_mut_ptr().cast(),
            )
        }
    }

    /// `EVP_CIPHER_CTX_ctrl(ctx, GET_TAG, len, buf)`.
    pub fn ctrl_get_tag(&self, t: i32, len: i32) -> Option<Vec<u8>> {
        let size = usize::try_from(len).unwrap_or(0).min(1 << 16);
        let mut buf = vec![0u8; size.max(1)];
        // SAFETY: buffer is at least `len` bytes when len is positive;
        // non-positive lengths are rejected by OpenSSL before writing.
        let r = unsafe { EVP_CIPHER_CTX_ctrl(self.as_ptr(), t, len, buf.as_mut_ptr().cast()) };
        if r > 0 {
            buf.truncate(size);
            Some(buf)
        } else {
            None
        }
    }

    /// `EVP_CIPHER_CTX_set_key_length`.
    pub fn set_key_length(&self, len: i32) -> bool {
        // SAFETY: valid ctx.
        unsafe { EVP_CIPHER_CTX_set_key_length(self.as_ptr(), len) == 1 }
    }

    /// `EVP_CIPHER_CTX_set_padding`.
    pub fn set_padding(&self, pad: bool) {
        // SAFETY: valid ctx.
        unsafe { EVP_CIPHER_CTX_set_padding(self.as_ptr(), c_int::from(pad)) };
    }

    /// `EVP_CipherUpdate(ctx, NULL, &i, NULL, len)` (CCM total length).
    pub fn update_len_only(&self, len: usize) -> bool {
        let mut i = 0;
        // SAFETY: NULL in/out with a length is the documented CCM idiom.
        unsafe {
            EVP_CipherUpdate(
                self.as_ptr(),
                ptr::null_mut(),
                &raw mut i,
                ptr::null(),
                c_len(len),
            ) == 1
        }
    }

    /// `EVP_CipherUpdate(ctx, NULL, &i, aad, len)`.
    pub fn update_aad(&self, aad: &[u8]) -> bool {
        let mut i = 0;
        // SAFETY: NULL output with AAD input; `aad` may be empty (we pass
        // a valid, non-NULL pointer for an empty slice).
        unsafe {
            EVP_CipherUpdate(
                self.as_ptr(),
                ptr::null_mut(),
                &raw mut i,
                aad.as_ptr(),
                c_len(aad.len()),
            ) == 1
        }
    }

    /// `EVP_CipherUpdate` into a fresh buffer of `out_cap` bytes.
    pub fn update(&self, input: &[u8], out_cap: usize) -> Option<Vec<u8>> {
        let mut out = vec![0u8; out_cap.max(1)];
        let mut n: c_int = 0;
        // SAFETY: output buffer sized by the caller to the PHP bound
        // (`inl + block_size`, plus slack), which is OpenSSL's maximum.
        let ok = unsafe {
            EVP_CipherUpdate(
                self.as_ptr(),
                out.as_mut_ptr(),
                &raw mut n,
                input.as_ptr(),
                c_len(input.len()),
            )
        };
        if ok == 1 {
            out.truncate(usize::try_from(n).unwrap_or(0));
            Some(out)
        } else {
            None
        }
    }

    /// `EVP_EncryptFinal` / `EVP_DecryptFinal`, appending to `out`.
    pub fn finalize(&self, enc: bool, out: &mut Vec<u8>) -> bool {
        let mut buf = vec![0u8; 64];
        let mut n: c_int = 0;
        // SAFETY: 64 bytes exceed any cipher block size.
        let ok = unsafe {
            if enc {
                EVP_EncryptFinal_ex(self.as_ptr(), buf.as_mut_ptr(), &raw mut n)
            } else {
                EVP_DecryptFinal_ex(self.as_ptr(), buf.as_mut_ptr(), &raw mut n)
            }
        };
        if ok == 1 {
            out.extend_from_slice(&buf[..usize::try_from(n).unwrap_or(0)]);
            true
        } else {
            false
        }
    }

    /// `EVP_CIPHER_CTX_reset`.
    pub fn reset(&self) {
        // SAFETY: valid ctx.
        unsafe { EVP_CIPHER_CTX_reset(self.as_ptr()) };
    }

    /// `EVP_EncryptInit(ctx, cipher, NULL, NULL)`.
    pub fn encrypt_init_cipher(&self, cipher: &Cipher) -> bool {
        // SAFETY: valid pointers.
        unsafe { EVP_EncryptInit(self.as_ptr(), cipher.as_ptr(), ptr::null(), ptr::null()) == 1 }
    }
}

/// Result of [`seal`].
#[derive(Debug)]
pub struct Sealed {
    /// Ciphertext.
    pub data: Vec<u8>,
    /// One encrypted key per public key.
    pub keys: Vec<Vec<u8>>,
    /// Generated IV (`EVP_MAX_IV_LENGTH` bytes; truncate to the cipher's).
    pub iv: Vec<u8>,
}

/// `EVP_SealInit` + `EVP_SealUpdate` + `EVP_SealFinal` exactly as
/// `openssl_seal` drives them.
pub fn seal(ctx: &CipherCtx, cipher: &Cipher, keys: &[Pkey], data: &[u8]) -> Option<Sealed> {
    let mut eks: Vec<Vec<u8>> = keys
        .iter()
        .map(|k| vec![0u8; k.size().max(0) as usize + 1])
        .collect();
    let mut ek_ptrs: Vec<*mut c_uchar> = eks.iter_mut().map(std::vec::Vec::as_mut_ptr).collect();
    let mut ekl: Vec<c_int> = vec![0; keys.len()];
    let mut pk_ptrs: Vec<*mut EVP_PKEY> = keys.iter().map(Pkey::as_ptr).collect();
    let mut iv = vec![0u8; 17];
    let mut out = vec![0u8; data.len() + cipher.block_size().max(0) as usize + 64];
    let (mut l1, mut l2): (c_int, c_int) = (0, 0);
    // SAFETY: every buffer is sized as PHP sizes it (EVP_PKEY_size per
    // key, EVP_MAX_IV_LENGTH IV, input + block size output).
    unsafe {
        if EVP_SealInit(
            ctx.as_ptr(),
            cipher.as_ptr(),
            ek_ptrs.as_mut_ptr(),
            ekl.as_mut_ptr(),
            iv.as_mut_ptr(),
            pk_ptrs.as_mut_ptr(),
            c_len(keys.len()),
        ) <= 0
        {
            return None;
        }
        if EVP_CipherUpdate(
            ctx.as_ptr(),
            out.as_mut_ptr(),
            &raw mut l1,
            data.as_ptr(),
            c_len(data.len()),
        ) != 1
        {
            return None;
        }
        if EVP_SealFinal(ctx.as_ptr(), out.as_mut_ptr().add(l1 as usize), &raw mut l2) != 1 {
            return None;
        }
    }
    out.truncate((l1 + l2) as usize);
    for (ek, len) in eks.iter_mut().zip(&ekl) {
        ek.truncate(usize::try_from(*len).unwrap_or(0));
    }
    iv.truncate(16);
    Some(Sealed {
        data: out,
        keys: eks,
        iv,
    })
}

/// `EVP_OpenInit` + `EVP_OpenUpdate` + `EVP_OpenFinal`; `None` on failure.
pub fn open(
    ctx: &CipherCtx,
    cipher: &Cipher,
    ekey: &[u8],
    iv: Option<&[u8]>,
    pkey: &Pkey,
    data: &[u8],
) -> Option<Vec<u8>> {
    let mut out = vec![0u8; data.len() + 64];
    let (mut l1, mut l2): (c_int, c_int) = (0, 0);
    // SAFETY: IV (when given) has the cipher's IV length (checked by the
    // caller); output sized input + slack as decryption never expands.
    unsafe {
        if EVP_OpenInit(
            ctx.as_ptr(),
            cipher.as_ptr(),
            ekey.as_ptr(),
            c_len(ekey.len()),
            iv.map_or(ptr::null(), <[u8]>::as_ptr),
            pkey.as_ptr(),
        ) == 0
        {
            return None;
        }
        if EVP_CipherUpdate(
            ctx.as_ptr(),
            out.as_mut_ptr(),
            &raw mut l1,
            data.as_ptr(),
            c_len(data.len()),
        ) != 1
        {
            return None;
        }
        if EVP_OpenFinal(ctx.as_ptr(), out.as_mut_ptr().add(l1 as usize), &raw mut l2) == 0 {
            return None;
        }
    }
    out.truncate((l1 + l2) as usize);
    Some(out)
}

/// Collects names for `openssl_get_cipher_methods` (provided ciphers,
/// lower-cased; all aliases when requested). Unsorted.
pub fn provided_cipher_names(aliases: bool) -> Vec<String> {
    unsafe extern "C" fn add_name(name: *const c_char, arg: *mut c_void) {
        // SAFETY: `arg` is the `Vec<String>` passed below; `name` is a
        // valid C string for the duration of the callback.
        let v = unsafe { &mut *arg.cast::<Vec<String>>() };
        if let Some(s) = cstr_opt(name) {
            v.push(s.to_ascii_lowercase());
        }
    }
    unsafe extern "C" fn each(cipher: *mut EVP_CIPHER, arg: *mut c_void) {
        // SAFETY: `cipher` is valid during the callback; `arg` is the
        // `(bool, Vec<String>)` pair below.
        unsafe {
            let st = &mut *arg.cast::<(bool, Vec<String>)>();
            if st.0 {
                EVP_CIPHER_names_do_all(cipher, add_name, ptr::addr_of_mut!(st.1).cast());
            } else {
                add_name(EVP_CIPHER_get0_name(cipher), ptr::addr_of_mut!(st.1).cast());
            }
        }
    }
    let mut st: (bool, Vec<String>) = (aliases, Vec::new());
    // SAFETY: the callback only touches `st`, which outlives the call.
    unsafe { EVP_CIPHER_do_all_provided(ptr::null_mut(), each, ptr::addr_of_mut!(st).cast()) };
    st.1
}

/// `OBJ_NAME_do_all_sorted(OBJ_NAME_TYPE_MD_METH, …)` for
/// `openssl_get_md_methods`.
pub fn md_names(aliases: bool) -> Vec<String> {
    unsafe extern "C" fn each(name: *const OBJ_NAME, arg: *mut c_void) {
        // SAFETY: valid OBJ_NAME during the callback; `arg` is our state.
        unsafe {
            let st = &mut *arg.cast::<(bool, Vec<String>)>();
            let n = &*name;
            if st.0 || n.alias == 0 {
                if let Some(s) = cstr_opt(n.name) {
                    st.1.push(s);
                }
            }
        }
    }
    let mut st: (bool, Vec<String>) = (aliases, Vec::new());
    // SAFETY: callback only touches `st`.
    unsafe { OBJ_NAME_do_all_sorted(OBJ_NAME_TYPE_MD_METH, each, ptr::addr_of_mut!(st).cast()) };
    st.1
}

// ---------------------------------------------------------------------------
// Digests
// ---------------------------------------------------------------------------

handle!(
    /// A digest from `OBJ_NAME_get` or `EVP_MD_fetch`.
    Md,
    EVP_MD,
    EVP_MD_free
);

/// `php_openssl_get_evp_md_by_name`.
pub fn md_by_name(name: &[u8]) -> Option<Md> {
    let c = to_cstring(name);
    // SAFETY: valid C string.
    let p = unsafe { OBJ_NAME_get(c.as_ptr(), OBJ_NAME_TYPE_MD_METH) };
    if !p.is_null() {
        return Md::from_raw(p.cast_mut().cast());
    }
    // SAFETY: valid C string.
    Md::from_raw(unsafe { EVP_MD_fetch(ptr::null_mut(), c.as_ptr(), ptr::null()) })
}

/// `EVP_md_null()`.
pub fn md_null() -> Option<Md> {
    // SAFETY: returns a static object; EVP_MD_free ignores static digests.
    Md::from_raw(unsafe { EVP_md_null() }.cast_mut())
}

impl Md {
    /// `EVP_MD_get_size`.
    pub fn size(&self) -> i32 {
        // SAFETY: valid digest.
        unsafe { EVP_MD_get_size(self.as_ptr()) }
    }
}

handle!(
    /// `EVP_MD_CTX`.
    MdCtx,
    EVP_MD_CTX,
    EVP_MD_CTX_free
);

/// `openssl_digest`'s create/init/update/final sequence.
pub fn digest(md: &Md, data: &[u8]) -> Option<Vec<u8>> {
    // SAFETY: constructor.
    let ctx = MdCtx::from_raw(unsafe { EVP_MD_CTX_new() })?;
    let mut out = vec![0u8; 64.max(md.size().max(0) as usize)];
    let mut n: c_uint = 0;
    // SAFETY: valid ctx/md, output buffer ≥ EVP_MAX_MD_SIZE.
    unsafe {
        if EVP_DigestInit(ctx.as_ptr(), md.as_ptr()) != 1
            || EVP_DigestUpdate(ctx.as_ptr(), data.as_ptr().cast(), data.len()) != 1
            || EVP_DigestFinal(ctx.as_ptr(), out.as_mut_ptr(), &raw mut n) != 1
        {
            return None;
        }
    }
    out.truncate(n as usize);
    Some(out)
}

/// `EVP_DigestSignInit` + optional RSA setup + `EVP_DigestSign`.
/// `setup` receives the `EVP_PKEY_CTX` and returns false to abort.
pub fn digest_sign(
    md: Option<&Md>,
    pkey: &Pkey,
    data: &[u8],
    setup: &mut dyn FnMut(&PkeyCtxRef) -> bool,
) -> Option<Vec<u8>> {
    // SAFETY: constructor.
    let ctx = MdCtx::from_raw(unsafe { EVP_MD_CTX_new() })?;
    let mut pctx: *mut EVP_PKEY_CTX = ptr::null_mut();
    // SAFETY: valid pointers; pctx is owned by the md ctx.
    unsafe {
        if EVP_DigestSignInit(
            ctx.as_ptr(),
            &raw mut pctx,
            md.map_or(ptr::null(), |m| m.as_ptr().cast_const()),
            ptr::null_mut(),
            pkey.as_ptr(),
        ) != 1
        {
            return None;
        }
    }
    if !setup(&PkeyCtxRef(pctx)) {
        return None;
    }
    let mut len: usize = 0;
    // SAFETY: size query then sign into a buffer of that size.
    unsafe {
        if EVP_DigestSign(
            ctx.as_ptr(),
            ptr::null_mut(),
            &raw mut len,
            data.as_ptr(),
            data.len(),
        ) != 1
        {
            return None;
        }
        let mut sig = vec![0u8; len];
        if EVP_DigestSign(
            ctx.as_ptr(),
            sig.as_mut_ptr(),
            &raw mut len,
            data.as_ptr(),
            data.len(),
        ) != 1
        {
            return None;
        }
        sig.truncate(len);
        Some(sig)
    }
}

/// Outcome of [`digest_verify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyInit {
    /// Ctx creation failed.
    NoCtx,
    /// Init or setup failed.
    InitFailed,
    /// `EVP_DigestVerify` result.
    Result(i32),
}

/// `EVP_DigestVerifyInit` + setup + `EVP_DigestVerify`.
pub fn digest_verify(
    md: Option<&Md>,
    pkey: &Pkey,
    sig: &[u8],
    data: &[u8],
    setup: &mut dyn FnMut(&PkeyCtxRef) -> bool,
) -> VerifyInit {
    // SAFETY: constructor.
    let Some(ctx) = MdCtx::from_raw(unsafe { EVP_MD_CTX_new() }) else {
        return VerifyInit::NoCtx;
    };
    let mut pctx: *mut EVP_PKEY_CTX = ptr::null_mut();
    // SAFETY: valid pointers.
    let ok = unsafe {
        EVP_DigestVerifyInit(
            ctx.as_ptr(),
            &raw mut pctx,
            md.map_or(ptr::null(), |m| m.as_ptr().cast_const()),
            ptr::null_mut(),
            pkey.as_ptr(),
        )
    };
    if ok != 1 || !setup(&PkeyCtxRef(pctx)) {
        return VerifyInit::InitFailed;
    }
    // SAFETY: valid buffers.
    VerifyInit::Result(unsafe {
        EVP_DigestVerify(
            ctx.as_ptr(),
            sig.as_ptr(),
            sig.len(),
            data.as_ptr(),
            data.len(),
        )
    })
}

// ---------------------------------------------------------------------------
// Random, PBKDF2, curves, locations
// ---------------------------------------------------------------------------

/// `RAND_bytes`.
pub fn rand_bytes(len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; len];
    // SAFETY: buffer of `len` bytes.
    (unsafe { RAND_bytes(buf.as_mut_ptr(), c_len(len)) } > 0).then_some(buf)
}

/// `RAND_file_name` (default seed file path).
pub fn rand_file_name() -> Option<Vec<u8>> {
    let mut buf = [0 as c_char; 4096];
    // SAFETY: valid buffer.
    let p = unsafe { RAND_file_name(buf.as_mut_ptr(), buf.len()) };
    // SAFETY: returns `buf` (NUL-terminated) or NULL.
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_bytes().to_vec())
}

/// `RAND_load_file(file, -1)`.
pub fn rand_load_file(file: &[u8]) -> i32 {
    let c = to_cstring(file);
    // SAFETY: valid C string.
    unsafe { RAND_load_file(c.as_ptr(), -1) }
}

/// `RAND_write_file(file)`.
pub fn rand_write_file(file: &[u8]) -> i32 {
    let c = to_cstring(file);
    // SAFETY: valid C string.
    unsafe { RAND_write_file(c.as_ptr()) }
}

/// `RAND_status()`.
pub fn rand_status() -> i32 {
    // SAFETY: no preconditions.
    unsafe { RAND_status() }
}

/// `PKCS5_PBKDF2_HMAC`.
pub fn pbkdf2(pass: &[u8], salt: &[u8], iter: i32, md: &Md, keylen: usize) -> Option<Vec<u8>> {
    let mut out = vec![0u8; keylen];
    // SAFETY: valid buffers.
    let ok = unsafe {
        PKCS5_PBKDF2_HMAC(
            pass.as_ptr().cast(),
            c_len(pass.len()),
            salt.as_ptr(),
            c_len(salt.len()),
            iter,
            md.as_ptr(),
            c_len(keylen),
            out.as_mut_ptr(),
        )
    };
    (ok == 1).then_some(out)
}

/// Short names of all built-in curves (`openssl_get_curve_names`).
pub fn builtin_curve_names() -> Vec<String> {
    // SAFETY: NULL/0 queries the count.
    let n = unsafe { EC_get_builtin_curves(ptr::null_mut(), 0) };
    let mut v: Vec<EC_builtin_curve> = (0..n)
        .map(|_| EC_builtin_curve {
            nid: 0,
            comment: ptr::null(),
        })
        .collect();
    // SAFETY: buffer has room for `n` entries.
    let got = unsafe { EC_get_builtin_curves(v.as_mut_ptr(), n) };
    v.truncate(got);
    v.iter().filter_map(|c| nid2sn(c.nid)).collect()
}

/// `OBJ_nid2sn`.
pub fn nid2sn(nid: i32) -> Option<String> {
    // SAFETY: static string or NULL.
    cstr_opt(unsafe { OBJ_nid2sn(nid) })
}

/// `OBJ_nid2ln`.
pub fn nid2ln(nid: i32) -> Option<String> {
    // SAFETY: static string or NULL.
    cstr_opt(unsafe { OBJ_nid2ln(nid) })
}

/// `OBJ_sn2nid`.
pub fn sn2nid(sn: &[u8]) -> i32 {
    let c = to_cstring(sn);
    // SAFETY: valid C string.
    unsafe { OBJ_sn2nid(c.as_ptr()) }
}

/// `OBJ_ln2nid`.
pub fn ln2nid(ln: &[u8]) -> i32 {
    let c = to_cstring(ln);
    // SAFETY: valid C string.
    unsafe { OBJ_ln2nid(c.as_ptr()) }
}

/// `OBJ_txt2nid`.
pub fn txt2nid(txt: &[u8]) -> i32 {
    let c = to_cstring(txt);
    // SAFETY: valid C string.
    unsafe { OBJ_txt2nid(c.as_ptr()) }
}

/// `OBJ_create(oid, name, name)`; returns the new NID (0 on failure).
pub fn obj_create(oid: &[u8], name: &[u8]) -> i32 {
    let o = to_cstring(oid);
    let n = to_cstring(name);
    // SAFETY: valid C strings.
    unsafe { OBJ_create(o.as_ptr(), n.as_ptr(), n.as_ptr()) }
}

/// `OBJ_create_objects(bio)`.
pub fn obj_create_objects(bio: &Bio<'_>) -> i32 {
    // SAFETY: valid BIO.
    unsafe { OBJ_create_objects(bio.as_ptr()) }
}

/// Dotted OID of a NID (`OBJ_obj2txt(OBJ_nid2obj(nid), 1)`).
pub fn nid2oid(nid: i32) -> Option<String> {
    // SAFETY: OBJ_nid2obj returns a (possibly static) object we free.
    unsafe {
        let obj = OBJ_nid2obj(nid);
        if obj.is_null() {
            return None;
        }
        let s = obj2txt(obj, true);
        ASN1_OBJECT_free(obj);
        Some(s)
    }
}

/// `OBJ_obj2txt` into an 80/256-byte buffer.
unsafe fn obj2txt(obj: *const ASN1_OBJECT, no_name: bool) -> String {
    let mut buf = [0 as c_char; 256];
    // SAFETY: valid buffer; obj valid per caller.
    let n = unsafe {
        OBJ_obj2txt(
            buf.as_mut_ptr(),
            (buf.len() - 1) as c_int,
            obj,
            c_int::from(no_name),
        )
    };
    if n <= 0 {
        return String::new();
    }
    // SAFETY: NUL-terminated by OBJ_obj2txt.
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// The `X509_get_default_*` strings, in `openssl_get_cert_locations` order.
pub fn default_cert_locations() -> [(&'static str, String); 6] {
    // SAFETY: all return static strings.
    unsafe {
        [
            (
                "default_cert_file",
                cstr_opt(X509_get_default_cert_file()).unwrap_or_default(),
            ),
            (
                "default_cert_file_env",
                cstr_opt(X509_get_default_cert_file_env()).unwrap_or_default(),
            ),
            (
                "default_cert_dir",
                cstr_opt(X509_get_default_cert_dir()).unwrap_or_default(),
            ),
            (
                "default_cert_dir_env",
                cstr_opt(X509_get_default_cert_dir_env()).unwrap_or_default(),
            ),
            (
                "default_private_dir",
                cstr_opt(X509_get_default_private_dir()).unwrap_or_default(),
            ),
            (
                "default_default_cert_area",
                cstr_opt(X509_get_default_cert_area()).unwrap_or_default(),
            ),
        ]
    }
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

#[allow(clippy::upper_case_acronyms, clashing_extern_declarations)]
extern "C" {
    fn EVP_PKEY_new() -> *mut EVP_PKEY;
    fn EVP_PKEY_free(k: *mut EVP_PKEY);
    fn EVP_PKEY_up_ref(k: *mut EVP_PKEY) -> c_int;
    fn EVP_PKEY_get_bits(k: *const EVP_PKEY) -> c_int;
    fn EVP_PKEY_get_size(k: *const EVP_PKEY) -> c_int;
    fn EVP_PKEY_get_id(k: *const EVP_PKEY) -> c_int;
    fn EVP_PKEY_get_base_id(k: *const EVP_PKEY) -> c_int;
    fn EVP_PKEY_type(nid: c_int) -> c_int;
    fn EVP_PKEY_get0_type_name(k: *const EVP_PKEY) -> *const c_char;
    fn EVP_PKEY_get_bn_param(
        k: *const EVP_PKEY,
        name: *const c_char,
        bn: *mut *mut BIGNUM,
    ) -> c_int;
    fn EVP_PKEY_get_octet_string_param(
        k: *const EVP_PKEY,
        name: *const c_char,
        buf: *mut c_uchar,
        max: usize,
        out: *mut usize,
    ) -> c_int;
    fn EVP_PKEY_get_utf8_string_param(
        k: *const EVP_PKEY,
        name: *const c_char,
        buf: *mut c_char,
        max: usize,
        out: *mut usize,
    ) -> c_int;
    fn EVP_PKEY_copy_parameters(to: *mut EVP_PKEY, from: *const EVP_PKEY) -> c_int;
    fn EVP_PKEY_set1_encoded_public_key(
        k: *mut EVP_PKEY,
        pub_: *const c_uchar,
        len: usize,
    ) -> c_int;
    fn PEM_read_bio_PrivateKey(
        b: *mut BIO,
        x: *mut *mut EVP_PKEY,
        cb: Option<PemPasswordCb>,
        u: *mut c_void,
    ) -> *mut EVP_PKEY;
    fn PEM_read_bio_PUBKEY(
        b: *mut BIO,
        x: *mut *mut EVP_PKEY,
        cb: Option<PemPasswordCb>,
        u: *mut c_void,
    ) -> *mut EVP_PKEY;
    fn PEM_write_bio_PUBKEY(b: *mut BIO, k: *const EVP_PKEY) -> c_int;
    fn PEM_write_bio_PrivateKey(
        b: *mut BIO,
        k: *const EVP_PKEY,
        enc: *const EVP_CIPHER,
        kstr: *const c_uchar,
        klen: c_int,
        cb: Option<PemPasswordCb>,
        u: *mut c_void,
    ) -> c_int;

    fn EVP_PKEY_CTX_new(k: *mut EVP_PKEY, e: *mut ENGINE) -> *mut EVP_PKEY_CTX;
    fn EVP_PKEY_CTX_new_from_name(
        lib: *mut OSSL_LIB_CTX,
        name: *const c_char,
        props: *const c_char,
    ) -> *mut EVP_PKEY_CTX;
    fn EVP_PKEY_CTX_new_from_pkey(
        lib: *mut OSSL_LIB_CTX,
        k: *mut EVP_PKEY,
        props: *const c_char,
    ) -> *mut EVP_PKEY_CTX;
    fn EVP_PKEY_CTX_free(c: *mut EVP_PKEY_CTX);
    fn EVP_PKEY_paramgen_init(c: *mut EVP_PKEY_CTX) -> c_int;
    fn EVP_PKEY_paramgen(c: *mut EVP_PKEY_CTX, out: *mut *mut EVP_PKEY) -> c_int;
    fn EVP_PKEY_keygen_init(c: *mut EVP_PKEY_CTX) -> c_int;
    fn EVP_PKEY_keygen(c: *mut EVP_PKEY_CTX, out: *mut *mut EVP_PKEY) -> c_int;
    fn EVP_PKEY_generate(c: *mut EVP_PKEY_CTX, out: *mut *mut EVP_PKEY) -> c_int;
    fn EVP_PKEY_CTX_set_dsa_paramgen_bits(c: *mut EVP_PKEY_CTX, bits: c_int) -> c_int;
    fn EVP_PKEY_CTX_set_dh_paramgen_prime_len(c: *mut EVP_PKEY_CTX, bits: c_int) -> c_int;
    fn EVP_PKEY_CTX_set_ec_paramgen_curve_nid(c: *mut EVP_PKEY_CTX, nid: c_int) -> c_int;
    fn EVP_PKEY_CTX_set_ec_param_enc(c: *mut EVP_PKEY_CTX, enc: c_int) -> c_int;
    fn EVP_PKEY_CTX_set_rsa_keygen_bits(c: *mut EVP_PKEY_CTX, bits: c_int) -> c_int;
    fn EVP_PKEY_CTX_set_rsa_padding(c: *mut EVP_PKEY_CTX, pad: c_int) -> c_int;
    fn EVP_PKEY_CTX_set_rsa_pss_saltlen(c: *mut EVP_PKEY_CTX, len: c_int) -> c_int;
    fn EVP_PKEY_CTX_set_rsa_oaep_md(c: *mut EVP_PKEY_CTX, md: *const EVP_MD) -> c_int;
    fn EVP_PKEY_CTX_set_params(c: *mut EVP_PKEY_CTX, p: *const OSSL_PARAM) -> c_int;
    fn EVP_PKEY_fromdata_init(c: *mut EVP_PKEY_CTX) -> c_int;
    fn EVP_PKEY_fromdata(
        c: *mut EVP_PKEY_CTX,
        out: *mut *mut EVP_PKEY,
        sel: c_int,
        p: *mut OSSL_PARAM,
    ) -> c_int;
    fn EVP_PKEY_check(c: *mut EVP_PKEY_CTX) -> c_int;
    fn EVP_PKEY_public_check_quick(c: *mut EVP_PKEY_CTX) -> c_int;
    fn EVP_PKEY_sign_init(c: *mut EVP_PKEY_CTX) -> c_int;
    fn EVP_PKEY_sign(
        c: *mut EVP_PKEY_CTX,
        out: *mut c_uchar,
        outlen: *mut usize,
        inp: *const c_uchar,
        inlen: usize,
    ) -> c_int;
    fn EVP_PKEY_encrypt_init(c: *mut EVP_PKEY_CTX) -> c_int;
    fn EVP_PKEY_encrypt(
        c: *mut EVP_PKEY_CTX,
        out: *mut c_uchar,
        outlen: *mut usize,
        inp: *const c_uchar,
        inlen: usize,
    ) -> c_int;
    fn EVP_PKEY_decrypt_init(c: *mut EVP_PKEY_CTX) -> c_int;
    fn EVP_PKEY_decrypt(
        c: *mut EVP_PKEY_CTX,
        out: *mut c_uchar,
        outlen: *mut usize,
        inp: *const c_uchar,
        inlen: usize,
    ) -> c_int;
    fn EVP_PKEY_verify_recover_init(c: *mut EVP_PKEY_CTX) -> c_int;
    fn EVP_PKEY_verify_recover(
        c: *mut EVP_PKEY_CTX,
        out: *mut c_uchar,
        outlen: *mut usize,
        inp: *const c_uchar,
        inlen: usize,
    ) -> c_int;
    fn EVP_PKEY_derive_init(c: *mut EVP_PKEY_CTX) -> c_int;
    fn EVP_PKEY_derive_set_peer(c: *mut EVP_PKEY_CTX, peer: *mut EVP_PKEY) -> c_int;
    fn EVP_PKEY_derive(c: *mut EVP_PKEY_CTX, key: *mut c_uchar, len: *mut usize) -> c_int;

    fn OSSL_PARAM_BLD_new() -> *mut OSSL_PARAM_BLD;
    fn OSSL_PARAM_BLD_free(b: *mut OSSL_PARAM_BLD);
    fn OSSL_PARAM_BLD_push_BN(
        b: *mut OSSL_PARAM_BLD,
        key: *const c_char,
        bn: *const BIGNUM,
    ) -> c_int;
    fn OSSL_PARAM_BLD_push_utf8_string(
        b: *mut OSSL_PARAM_BLD,
        key: *const c_char,
        s: *const c_char,
        len: usize,
    ) -> c_int;
    fn OSSL_PARAM_BLD_push_octet_string(
        b: *mut OSSL_PARAM_BLD,
        key: *const c_char,
        s: *const c_void,
        len: usize,
    ) -> c_int;
    fn OSSL_PARAM_BLD_to_param(b: *mut OSSL_PARAM_BLD) -> *mut OSSL_PARAM;
    fn OSSL_PARAM_free(p: *mut OSSL_PARAM);

    fn BN_new() -> *mut BIGNUM;
    fn BN_free(b: *mut BIGNUM);
    fn BN_bin2bn(s: *const c_uchar, len: c_int, ret: *mut BIGNUM) -> *mut BIGNUM;
    fn BN_bn2bin(b: *const BIGNUM, to: *mut c_uchar) -> c_int;
    fn BN_num_bits(b: *const BIGNUM) -> c_int;
    fn BN_bn2hex(b: *const BIGNUM) -> *mut c_char;
    fn BN_CTX_new() -> *mut BN_CTX;
    fn BN_CTX_free(c: *mut BN_CTX);
    fn BN_with_flags(dest: *mut BIGNUM, b: *const BIGNUM, flags: c_int);
    fn BN_mod_exp_mont(
        r: *mut BIGNUM,
        a: *const BIGNUM,
        p: *const BIGNUM,
        m: *const BIGNUM,
        ctx: *mut BN_CTX,
        mont: *mut c_void,
    ) -> c_int;

    fn EC_GROUP_new_by_curve_name(nid: c_int) -> *mut EC_GROUP;
    fn EC_GROUP_new_curve_GFp(
        p: *const BIGNUM,
        a: *const BIGNUM,
        b: *const BIGNUM,
        ctx: *mut BN_CTX,
    ) -> *mut EC_GROUP;
    fn EC_GROUP_free(g: *mut EC_GROUP);
    fn EC_GROUP_set_seed(g: *mut EC_GROUP, s: *const c_uchar, len: usize) -> usize;
    fn EC_GROUP_set_generator(
        g: *mut EC_GROUP,
        gen: *const EC_POINT,
        order: *const BIGNUM,
        cof: *const BIGNUM,
    ) -> c_int;
    fn EC_GROUP_check_named_curve(g: *const EC_GROUP, nist_only: c_int, ctx: *mut BN_CTX) -> c_int;
    fn EC_POINT_new(g: *const EC_GROUP) -> *mut EC_POINT;
    fn EC_POINT_free(p: *mut EC_POINT);
    fn EC_POINT_oct2point(
        g: *const EC_GROUP,
        p: *mut EC_POINT,
        buf: *const c_uchar,
        len: usize,
        ctx: *mut BN_CTX,
    ) -> c_int;
    fn EC_POINT_set_affine_coordinates(
        g: *const EC_GROUP,
        p: *mut EC_POINT,
        x: *const BIGNUM,
        y: *const BIGNUM,
        ctx: *mut BN_CTX,
    ) -> c_int;
    fn EC_POINT_point2buf(
        g: *const EC_GROUP,
        p: *const EC_POINT,
        form: c_int,
        buf: *mut *mut c_uchar,
        ctx: *mut BN_CTX,
    ) -> usize;
    fn EC_POINT_mul(
        g: *const EC_GROUP,
        r: *mut EC_POINT,
        n: *const BIGNUM,
        q: *const EC_POINT,
        m: *const BIGNUM,
        ctx: *mut BN_CTX,
    ) -> c_int;
}

/// `EVP_PKEY_*` type identifiers (NIDs).
pub mod pkey_id {
    /// RSA.
    pub const RSA: i32 = 6;
    /// RSA-PSS.
    pub const RSA_PSS: i32 = 912;
    /// DSA.
    pub const DSA: i32 = 116;
    /// DH.
    pub const DH: i32 = 28;
    /// EC.
    pub const EC: i32 = 408;
    /// X25519.
    pub const X25519: i32 = 1034;
    /// X448.
    pub const X448: i32 = 1035;
    /// Ed25519.
    pub const ED25519: i32 = 1087;
    /// Ed448.
    pub const ED448: i32 = 1088;
    /// Provider-only key (`EVP_PKEY_KEYMGMT`).
    pub const KEYMGMT: i32 = -1;
}

/// `NID_sm2`.
pub const NID_SM2: i32 = 1172;
const EVP_PKEY_KEYPAIR: c_int = 0x87;
const OPENSSL_EC_NAMED_CURVE: c_int = 0x001;
const POINT_CONVERSION_COMPRESSED: c_int = 2;
const BN_FLG_CONSTTIME: c_int = 0x04;

handle!(
    /// An `EVP_PKEY` (public or private).
    Pkey,
    EVP_PKEY,
    EVP_PKEY_free
);

impl Clone for Pkey {
    fn clone(&self) -> Self {
        // SAFETY: bumping the refcount of a live key; each clone frees once.
        unsafe { EVP_PKEY_up_ref(self.as_ptr()) };
        Pkey(self.0)
    }
}

struct PassCtx<'a> {
    pass: Option<&'a [u8]>,
}

unsafe extern "C" fn pem_password_cb(
    buf: *mut c_char,
    size: c_int,
    _rw: c_int,
    u: *mut c_void,
) -> c_int {
    // SAFETY: `u` is the `PassCtx` supplied by the caller for this call.
    let ctx = unsafe { &*u.cast::<PassCtx<'_>>() };
    let Some(pass) = ctx.pass else { return -1 };
    let n = pass.len().min(usize::try_from(size).unwrap_or(0));
    // SAFETY: OpenSSL provides `size` writable bytes at `buf`.
    unsafe { ptr::copy_nonoverlapping(pass.as_ptr(), buf.cast::<u8>(), n) };
    n as c_int
}

impl Pkey {
    /// `PEM_read_bio_PrivateKey` with PHP's password callback (`None`
    /// makes the callback fail instead of prompting).
    pub fn read_private(bio: &Bio<'_>, pass: Option<&[u8]>) -> Option<Pkey> {
        let mut ctx = PassCtx { pass };
        // SAFETY: valid BIO; callback data outlives the call.
        Self::from_raw(unsafe {
            PEM_read_bio_PrivateKey(
                bio.as_ptr(),
                ptr::null_mut(),
                Some(pem_password_cb),
                ptr::addr_of_mut!(ctx).cast(),
            )
        })
    }

    /// `PEM_read_bio_PUBKEY(bio, NULL, NULL, NULL)`.
    pub fn read_public(bio: &Bio<'_>) -> Option<Pkey> {
        let mut ctx = PassCtx { pass: None };
        // SAFETY: valid BIO.
        Self::from_raw(unsafe {
            PEM_read_bio_PUBKEY(
                bio.as_ptr(),
                ptr::null_mut(),
                Some(pem_password_cb),
                ptr::addr_of_mut!(ctx).cast(),
            )
        })
    }

    /// `PEM_write_bio_PUBKEY`.
    pub fn write_public(&self, bio: &Bio<'_>) -> bool {
        // SAFETY: valid BIO and key.
        unsafe { PEM_write_bio_PUBKEY(bio.as_ptr(), self.as_ptr()) == 1 }
    }

    /// `PEM_write_bio_PrivateKey(bio, key, cipher, pass, len, NULL, NULL)`.
    pub fn write_private(
        &self,
        bio: &Bio<'_>,
        cipher: Option<&Cipher>,
        pass: Option<&[u8]>,
    ) -> bool {
        let (p, l) = pass.map_or((ptr::null(), 0), |p| (p.as_ptr(), c_len(p.len())));
        // SAFETY: valid pointers; pass buffer has `l` bytes.
        unsafe {
            PEM_write_bio_PrivateKey(
                bio.as_ptr(),
                self.as_ptr(),
                cipher.map_or(ptr::null(), |c| c.as_ptr().cast_const()),
                p,
                l,
                None,
                ptr::null_mut(),
            ) == 1
        }
    }

    /// PEM `PUBLIC KEY` text of this key.
    pub fn public_pem(&self) -> Option<Vec<u8>> {
        let b = Bio::mem()?;
        self.write_public(&b).then(|| b.contents())
    }

    /// `EVP_PKEY_new()`.
    pub fn empty() -> Option<Pkey> {
        // SAFETY: constructor.
        Self::from_raw(unsafe { EVP_PKEY_new() })
    }

    /// `EVP_PKEY_get_bits`.
    pub fn bits(&self) -> i32 {
        // SAFETY: valid key.
        unsafe { EVP_PKEY_get_bits(self.as_ptr()) }
    }

    /// `EVP_PKEY_get_size`.
    pub fn size(&self) -> i32 {
        // SAFETY: valid key.
        unsafe { EVP_PKEY_get_size(self.as_ptr()) }
    }

    /// `EVP_PKEY_get_id`.
    pub fn id(&self) -> i32 {
        // SAFETY: valid key.
        unsafe { EVP_PKEY_get_id(self.as_ptr()) }
    }

    /// `EVP_PKEY_get_base_id`.
    pub fn base_id(&self) -> i32 {
        // SAFETY: valid key.
        unsafe { EVP_PKEY_get_base_id(self.as_ptr()) }
    }

    /// `EVP_PKEY_type(EVP_PKEY_get_id(key))`.
    pub fn type_of_id(&self) -> i32 {
        // SAFETY: plain lookup.
        unsafe { EVP_PKEY_type(self.id()) }
    }

    /// Base id the way `php_openssl_pkey_get_details` derives it.
    pub fn details_base_id(&self) -> i32 {
        if self.id() != pkey_id::KEYMGMT {
            return self.base_id();
        }
        // SAFETY: valid key; returned name is owned by the key.
        let name = cstr_opt(unsafe { EVP_PKEY_get0_type_name(self.as_ptr()) });
        match name {
            Some(n) => {
                let nid = txt2nid(n.as_bytes());
                // SAFETY: plain lookup.
                if nid == 0 {
                    0
                } else {
                    unsafe { EVP_PKEY_type(nid) }
                }
            }
            None => 0,
        }
    }

    /// `EVP_PKEY_get_bn_param` as big-endian bytes.
    pub fn bn_param(&self, name: &CStr) -> Option<Vec<u8>> {
        let mut bn: *mut BIGNUM = ptr::null_mut();
        // SAFETY: valid key/name; on success we own `bn`.
        if unsafe { EVP_PKEY_get_bn_param(self.as_ptr(), name.as_ptr(), &raw mut bn) } <= 0
            || bn.is_null()
        {
            return None;
        }
        let b = Bn::from_raw(bn)?;
        Some(b.to_vec())
    }

    /// `EVP_PKEY_get_octet_string_param` into a 64-byte buffer.
    pub fn octet_param(&self, name: &CStr) -> Option<Vec<u8>> {
        let mut buf = [0u8; 64];
        let mut len = 0usize;
        // SAFETY: valid buffer of 64 bytes.
        (unsafe {
            EVP_PKEY_get_octet_string_param(
                self.as_ptr(),
                name.as_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut len,
            )
        } > 0)
            .then(|| buf[..len].to_vec())
    }

    /// `EVP_PKEY_get_utf8_string_param` into a 64-byte buffer.
    pub fn utf8_param(&self, name: &CStr) -> Option<Vec<u8>> {
        let mut buf = [0 as c_char; 64];
        let mut len = 0usize;
        // SAFETY: valid buffer of 64 bytes.
        (unsafe {
            EVP_PKEY_get_utf8_string_param(
                self.as_ptr(),
                name.as_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut len,
            )
        } > 0)
            .then(|| buf[..len].iter().map(|&c| c as u8).collect())
    }

    /// `EVP_PKEY_copy_parameters(self, from)`.
    pub fn copy_parameters(&self, from: &Pkey) -> bool {
        // SAFETY: valid keys.
        unsafe { EVP_PKEY_copy_parameters(self.as_ptr(), from.as_ptr()) > 0 }
    }

    /// `EVP_PKEY_set1_encoded_public_key`.
    pub fn set_encoded_public_key(&self, data: &[u8]) -> bool {
        // SAFETY: valid buffer.
        unsafe { EVP_PKEY_set1_encoded_public_key(self.as_ptr(), data.as_ptr(), data.len()) > 0 }
    }
}

/// A borrowed `EVP_PKEY_CTX` (owned by a digest context).
#[derive(Debug)]
pub struct PkeyCtxRef(*mut EVP_PKEY_CTX);

impl PkeyCtxRef {
    /// `EVP_PKEY_CTX_set_rsa_padding`.
    pub fn set_rsa_padding(&self, pad: i32) -> i32 {
        // SAFETY: ctx pointer valid during the digest operation.
        unsafe { EVP_PKEY_CTX_set_rsa_padding(self.0, pad) }
    }

    /// `EVP_PKEY_CTX_set_rsa_pss_saltlen`.
    pub fn set_rsa_pss_saltlen(&self, len: i32) -> i32 {
        // SAFETY: ctx pointer valid during the digest operation.
        unsafe { EVP_PKEY_CTX_set_rsa_pss_saltlen(self.0, len) }
    }
}

handle!(
    /// An owned `EVP_PKEY_CTX`.
    PkeyCtx,
    EVP_PKEY_CTX,
    EVP_PKEY_CTX_free
);

/// Which one-shot `EVP_PKEY_*` operation to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkeyOp {
    /// `EVP_PKEY_sign`.
    Sign,
    /// `EVP_PKEY_encrypt`.
    Encrypt,
    /// `EVP_PKEY_decrypt`.
    Decrypt,
    /// `EVP_PKEY_verify_recover`.
    VerifyRecover,
}

impl PkeyCtx {
    /// `EVP_PKEY_CTX_new(pkey, NULL)`.
    pub fn new(key: &Pkey) -> Option<Self> {
        // SAFETY: valid key.
        Self::from_raw(unsafe { EVP_PKEY_CTX_new(key.as_ptr(), ptr::null_mut()) })
    }

    /// `EVP_PKEY_CTX_new_from_name(NULL, name, NULL)`.
    pub fn from_name(name: &str) -> Option<Self> {
        let c = to_cstring(name.as_bytes());
        // SAFETY: valid C string.
        Self::from_raw(unsafe {
            EVP_PKEY_CTX_new_from_name(ptr::null_mut(), c.as_ptr(), ptr::null())
        })
    }

    /// `EVP_PKEY_CTX_new_from_pkey(NULL, pkey, NULL)`.
    pub fn from_pkey(key: &Pkey) -> Option<Self> {
        // SAFETY: valid key.
        Self::from_raw(unsafe {
            EVP_PKEY_CTX_new_from_pkey(ptr::null_mut(), key.as_ptr(), ptr::null())
        })
    }

    /// `EVP_PKEY_paramgen_init`.
    pub fn paramgen_init(&self) -> bool {
        // SAFETY: valid ctx.
        unsafe { EVP_PKEY_paramgen_init(self.as_ptr()) > 0 }
    }
    /// `EVP_PKEY_CTX_set_dsa_paramgen_bits`.
    pub fn set_dsa_paramgen_bits(&self, bits: i32) -> bool {
        // SAFETY: valid ctx.
        unsafe { EVP_PKEY_CTX_set_dsa_paramgen_bits(self.as_ptr(), bits) > 0 }
    }
    /// `EVP_PKEY_CTX_set_dh_paramgen_prime_len`.
    pub fn set_dh_paramgen_prime_len(&self, bits: i32) -> bool {
        // SAFETY: valid ctx.
        unsafe { EVP_PKEY_CTX_set_dh_paramgen_prime_len(self.as_ptr(), bits) > 0 }
    }
    /// `EVP_PKEY_CTX_set_ec_paramgen_curve_nid` + named-curve encoding.
    pub fn set_ec_curve(&self, nid: i32) -> bool {
        // SAFETY: valid ctx.
        unsafe {
            EVP_PKEY_CTX_set_ec_paramgen_curve_nid(self.as_ptr(), nid) > 0
                && EVP_PKEY_CTX_set_ec_param_enc(self.as_ptr(), OPENSSL_EC_NAMED_CURVE) > 0
        }
    }
    /// `EVP_PKEY_paramgen`.
    pub fn paramgen(&self) -> Option<Pkey> {
        let mut k = ptr::null_mut();
        // SAFETY: valid ctx; on success we own `k`.
        (unsafe { EVP_PKEY_paramgen(self.as_ptr(), &raw mut k) } > 0)
            .then(|| Pkey::from_raw(k))
            .flatten()
    }
    /// `EVP_PKEY_keygen_init`.
    pub fn keygen_init(&self) -> bool {
        // SAFETY: valid ctx.
        unsafe { EVP_PKEY_keygen_init(self.as_ptr()) > 0 }
    }
    /// `EVP_PKEY_CTX_set_rsa_keygen_bits`.
    pub fn set_rsa_keygen_bits(&self, bits: i32) -> bool {
        // SAFETY: valid ctx.
        unsafe { EVP_PKEY_CTX_set_rsa_keygen_bits(self.as_ptr(), bits) > 0 }
    }
    /// `EVP_PKEY_keygen`.
    pub fn keygen(&self) -> Option<Pkey> {
        let mut k = ptr::null_mut();
        // SAFETY: valid ctx; on success we own `k`.
        (unsafe { EVP_PKEY_keygen(self.as_ptr(), &raw mut k) } > 0)
            .then(|| Pkey::from_raw(k))
            .flatten()
    }
    /// `EVP_PKEY_generate` (returns `== 1` semantics).
    pub fn generate(&self) -> Option<Pkey> {
        let mut k = ptr::null_mut();
        // SAFETY: valid ctx; on success we own `k`.
        (unsafe { EVP_PKEY_generate(self.as_ptr(), &raw mut k) } == 1)
            .then(|| Pkey::from_raw(k))
            .flatten()
    }
    /// `EVP_PKEY_CTX_set_params` (`== 1`).
    pub fn set_params(&self, p: &Params) -> bool {
        // SAFETY: valid ctx and params.
        unsafe { EVP_PKEY_CTX_set_params(self.as_ptr(), p.as_ptr()) == 1 }
    }
    /// `EVP_PKEY_fromdata_init` + `EVP_PKEY_fromdata(KEYPAIR)`.
    pub fn fromdata(&self, p: &Params) -> Option<Pkey> {
        let mut k = ptr::null_mut();
        // SAFETY: valid ctx/params; on success we own `k`.
        unsafe {
            if EVP_PKEY_fromdata_init(self.as_ptr()) <= 0
                || EVP_PKEY_fromdata(self.as_ptr(), &raw mut k, EVP_PKEY_KEYPAIR, p.as_ptr()) <= 0
            {
                return None;
            }
        }
        Pkey::from_raw(k)
    }
    /// `EVP_PKEY_check(ctx) || EVP_PKEY_public_check_quick(ctx)`.
    pub fn check_or_public_check_quick(&self) -> bool {
        // SAFETY: valid ctx.
        unsafe {
            EVP_PKEY_check(self.as_ptr()) > 0 || EVP_PKEY_public_check_quick(self.as_ptr()) > 0
        }
    }
    /// `*_init` for the given operation.
    pub fn op_init(&self, op: PkeyOp) -> bool {
        // SAFETY: valid ctx.
        let r = unsafe {
            match op {
                PkeyOp::Sign => EVP_PKEY_sign_init(self.as_ptr()),
                PkeyOp::Encrypt => EVP_PKEY_encrypt_init(self.as_ptr()),
                PkeyOp::Decrypt => EVP_PKEY_decrypt_init(self.as_ptr()),
                PkeyOp::VerifyRecover => EVP_PKEY_verify_recover_init(self.as_ptr()),
            }
        };
        r > 0
    }
    /// `EVP_PKEY_CTX_set_rsa_padding`.
    pub fn set_rsa_padding(&self, pad: i32) -> bool {
        // SAFETY: valid ctx.
        unsafe { EVP_PKEY_CTX_set_rsa_padding(self.as_ptr(), pad) > 0 }
    }
    /// `EVP_PKEY_CTX_set_rsa_oaep_md`.
    pub fn set_rsa_oaep_md(&self, md: &Md) -> bool {
        // SAFETY: valid ctx and md.
        unsafe { EVP_PKEY_CTX_set_rsa_oaep_md(self.as_ptr(), md.as_ptr()) > 0 }
    }
    /// Size query (`out = NULL`) for the operation; `None` on failure.
    pub fn op_size(&self, op: PkeyOp, input: &[u8]) -> Option<usize> {
        let mut n = 0usize;
        // SAFETY: NULL output requests the size.
        let r = unsafe { self.op_raw(op, ptr::null_mut(), &raw mut n, input) };
        (r > 0).then_some(n)
    }
    /// Runs the operation into a buffer of `cap` bytes.
    pub fn op_run(&self, op: PkeyOp, input: &[u8], cap: usize) -> Option<Vec<u8>> {
        let mut out = vec![0u8; cap.max(1)];
        let mut n = cap;
        // SAFETY: output buffer of `cap` bytes, as reported by op_size.
        let r = unsafe { self.op_raw(op, out.as_mut_ptr(), &raw mut n, input) };
        (r > 0).then(|| {
            out.truncate(n);
            out
        })
    }
    unsafe fn op_raw(&self, op: PkeyOp, out: *mut c_uchar, n: *mut usize, input: &[u8]) -> c_int {
        // SAFETY: forwarded from callers.
        unsafe {
            match op {
                PkeyOp::Sign => EVP_PKEY_sign(self.as_ptr(), out, n, input.as_ptr(), input.len()),
                PkeyOp::Encrypt => {
                    EVP_PKEY_encrypt(self.as_ptr(), out, n, input.as_ptr(), input.len())
                }
                PkeyOp::Decrypt => {
                    EVP_PKEY_decrypt(self.as_ptr(), out, n, input.as_ptr(), input.len())
                }
                PkeyOp::VerifyRecover => {
                    EVP_PKEY_verify_recover(self.as_ptr(), out, n, input.as_ptr(), input.len())
                }
            }
        }
    }
    /// `EVP_PKEY_derive_init` + `EVP_PKEY_derive_set_peer`.
    pub fn derive_init(&self, peer: &Pkey) -> bool {
        // SAFETY: valid ctx and key.
        unsafe {
            EVP_PKEY_derive_init(self.as_ptr()) > 0
                && EVP_PKEY_derive_set_peer(self.as_ptr(), peer.as_ptr()) > 0
        }
    }
    /// `EVP_PKEY_derive(ctx, NULL, &len)`.
    pub fn derive_size(&self) -> Option<usize> {
        let mut n = 0usize;
        // SAFETY: size query.
        (unsafe { EVP_PKEY_derive(self.as_ptr(), ptr::null_mut(), &raw mut n) } > 0).then_some(n)
    }
    /// `EVP_PKEY_derive` into `len` bytes.
    pub fn derive(&self, len: usize) -> Option<Vec<u8>> {
        let mut out = vec![0u8; len.max(1)];
        let mut n = len;
        // SAFETY: buffer of `len` bytes.
        (unsafe { EVP_PKEY_derive(self.as_ptr(), out.as_mut_ptr(), &raw mut n) } > 0).then(|| {
            out.truncate(n);
            out
        })
    }
}

handle!(
    /// `BIGNUM`.
    Bn,
    BIGNUM,
    BN_free
);

impl Bn {
    /// `BN_bin2bn`.
    pub fn from_bin(data: &[u8]) -> Option<Self> {
        // SAFETY: valid buffer.
        Self::from_raw(unsafe { BN_bin2bn(data.as_ptr(), c_len(data.len()), ptr::null_mut()) })
    }
    /// `BN_bn2bin` (big-endian, minimal length).
    pub fn to_vec(&self) -> Vec<u8> {
        // SAFETY: valid BIGNUM.
        let bits = unsafe { BN_num_bits(self.as_ptr()) };
        let len = ((bits + 7) / 8).max(0) as usize;
        let mut out = vec![0u8; len];
        // SAFETY: buffer of BN_num_bytes bytes.
        unsafe { BN_bn2bin(self.as_ptr(), out.as_mut_ptr()) };
        out
    }
    /// `BN_bn2hex`.
    pub fn to_hex(&self) -> Option<Vec<u8>> {
        // SAFETY: valid BIGNUM; result freed by take_cstr.
        take_cstr(unsafe { BN_bn2hex(self.as_ptr()) })
    }
}

/// `php_openssl_dh_pub_from_priv`: `g^priv mod p` in constant time.
pub fn dh_pub_from_priv(priv_key: &Bn, g: &Bn, p: &Bn) -> Option<Bn> {
    // SAFETY: constructors; all pointers valid for the duration.
    unsafe {
        let pub_key = Bn::from_raw(BN_new())?;
        let ct = Bn::from_raw(BN_new())?;
        let ctx = BnCtx::from_raw(BN_CTX_new())?;
        BN_with_flags(ct.as_ptr(), priv_key.as_ptr(), BN_FLG_CONSTTIME);
        let ok = BN_mod_exp_mont(
            pub_key.as_ptr(),
            g.as_ptr(),
            ct.as_ptr(),
            p.as_ptr(),
            ctx.as_ptr(),
            ptr::null_mut(),
        );
        // `ct` shares `priv_key`'s limbs (BN_with_flags makes a shallow
        // copy flagged BN_FLG_STATIC_DATA); freeing it only frees the
        // header, which is the intended use.
        (ok == 1).then_some(pub_key)
    }
}

handle!(
    /// `BN_CTX`.
    BnCtx,
    BN_CTX,
    BN_CTX_free
);

impl BnCtx {
    /// `BN_CTX_new`.
    pub fn new() -> Option<Self> {
        // SAFETY: constructor.
        Self::from_raw(unsafe { BN_CTX_new() })
    }
}

/// `OSSL_PARAM_BLD` that keeps the pushed data alive until `to_param`.
pub struct ParamBld {
    raw: NonNull<OSSL_PARAM_BLD>,
    bns: Vec<Bn>,
    bufs: Vec<Vec<u8>>,
}

impl std::fmt::Debug for ParamBld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ParamBld")
    }
}

impl Drop for ParamBld {
    fn drop(&mut self) {
        // SAFETY: owned builder.
        unsafe { OSSL_PARAM_BLD_free(self.raw.as_ptr()) }
    }
}

impl ParamBld {
    /// `OSSL_PARAM_BLD_new`.
    pub fn new() -> Option<Self> {
        // SAFETY: constructor.
        NonNull::new(unsafe { OSSL_PARAM_BLD_new() }).map(|raw| ParamBld {
            raw,
            bns: Vec::new(),
            bufs: Vec::new(),
        })
    }
    /// `OSSL_PARAM_BLD_push_BN`.
    pub fn push_bn(&mut self, key: &'static CStr, bn: Bn) -> bool {
        // SAFETY: the BIGNUM is kept alive in `self.bns` until drop.
        let ok =
            unsafe { OSSL_PARAM_BLD_push_BN(self.raw.as_ptr(), key.as_ptr(), bn.as_ptr()) } == 1;
        self.bns.push(bn);
        ok
    }
    /// `OSSL_PARAM_BLD_push_utf8_string`.
    pub fn push_utf8(&mut self, key: &'static CStr, s: &[u8]) -> bool {
        let mut v = s.to_vec();
        v.push(0);
        // SAFETY: data kept alive in `self.bufs`.
        let ok = unsafe {
            OSSL_PARAM_BLD_push_utf8_string(
                self.raw.as_ptr(),
                key.as_ptr(),
                v.as_ptr().cast(),
                s.len(),
            )
        } == 1;
        self.bufs.push(v);
        ok
    }
    /// `OSSL_PARAM_BLD_push_octet_string`.
    pub fn push_octet(&mut self, key: &'static CStr, s: &[u8]) -> bool {
        let v = s.to_vec();
        // SAFETY: data kept alive in `self.bufs`.
        let ok = unsafe {
            OSSL_PARAM_BLD_push_octet_string(
                self.raw.as_ptr(),
                key.as_ptr(),
                v.as_ptr().cast(),
                v.len(),
            )
        } == 1;
        self.bufs.push(v);
        ok
    }
    /// `OSSL_PARAM_BLD_to_param`.
    pub fn to_param(&self) -> Option<Params> {
        // SAFETY: builder valid; pushed data still alive.
        Params::from_raw(unsafe { OSSL_PARAM_BLD_to_param(self.raw.as_ptr()) })
    }
}

handle!(
    /// An `OSSL_PARAM` array.
    Params,
    OSSL_PARAM,
    OSSL_PARAM_free
);

handle!(
    /// `EC_GROUP`.
    EcGroup,
    EC_GROUP,
    EC_GROUP_free
);

handle!(
    /// `EC_POINT`.
    EcPoint,
    EC_POINT,
    EC_POINT_free
);

impl EcGroup {
    /// `EC_GROUP_new_by_curve_name`.
    pub fn by_curve_name(nid: i32) -> Option<Self> {
        // SAFETY: plain constructor.
        Self::from_raw(unsafe { EC_GROUP_new_by_curve_name(nid) })
    }
    /// `EC_GROUP_new_curve_GFp`.
    pub fn curve_gfp(p: &Bn, a: &Bn, b: &Bn, ctx: &BnCtx) -> Option<Self> {
        // SAFETY: valid BIGNUMs.
        Self::from_raw(unsafe {
            EC_GROUP_new_curve_GFp(p.as_ptr(), a.as_ptr(), b.as_ptr(), ctx.as_ptr())
        })
    }
    /// `EC_POINT_new`.
    pub fn new_point(&self) -> Option<EcPoint> {
        // SAFETY: valid group.
        EcPoint::from_raw(unsafe { EC_POINT_new(self.as_ptr()) })
    }
    /// `EC_POINT_oct2point`.
    pub fn oct2point(&self, p: &EcPoint, data: &[u8], ctx: &BnCtx) -> bool {
        // SAFETY: valid objects.
        unsafe {
            EC_POINT_oct2point(
                self.as_ptr(),
                p.as_ptr(),
                data.as_ptr(),
                data.len(),
                ctx.as_ptr(),
            ) == 1
        }
    }
    /// `EC_POINT_set_affine_coordinates`.
    pub fn set_affine(&self, p: &EcPoint, x: &Bn, y: &Bn, ctx: &BnCtx) -> bool {
        // SAFETY: valid objects.
        unsafe {
            EC_POINT_set_affine_coordinates(
                self.as_ptr(),
                p.as_ptr(),
                x.as_ptr(),
                y.as_ptr(),
                ctx.as_ptr(),
            ) == 1
        }
    }
    /// `EC_POINT_point2buf(…, POINT_CONVERSION_COMPRESSED, …)`.
    pub fn point2buf_compressed(&self, p: &EcPoint, ctx: &BnCtx) -> Option<Vec<u8>> {
        let mut buf: *mut c_uchar = ptr::null_mut();
        // SAFETY: valid objects; OpenSSL allocates `buf` which we free.
        let n = unsafe {
            EC_POINT_point2buf(
                self.as_ptr(),
                p.as_ptr(),
                POINT_CONVERSION_COMPRESSED,
                &raw mut buf,
                ctx.as_ptr(),
            )
        };
        if n == 0 || buf.is_null() {
            return None;
        }
        // SAFETY: `n` bytes at `buf`.
        let v = unsafe { std::slice::from_raw_parts(buf, n) }.to_vec();
        openssl_free(buf.cast());
        Some(v)
    }
    /// `EC_GROUP_set_seed`.
    pub fn set_seed(&self, seed: &[u8]) -> bool {
        // SAFETY: valid buffer.
        unsafe { EC_GROUP_set_seed(self.as_ptr(), seed.as_ptr(), seed.len()) != 0 }
    }
    /// `EC_GROUP_set_generator`.
    pub fn set_generator(&self, g: &EcPoint, order: &Bn, cofactor: Option<&Bn>) -> bool {
        // SAFETY: valid objects; NULL cofactor is allowed.
        unsafe {
            EC_GROUP_set_generator(
                self.as_ptr(),
                g.as_ptr(),
                order.as_ptr(),
                cofactor.map_or(ptr::null(), |c| c.as_ptr().cast_const()),
            ) == 1
        }
    }
    /// `EC_GROUP_check_named_curve(group, 0, ctx)`.
    pub fn check_named_curve(&self, ctx: &BnCtx) -> i32 {
        // SAFETY: valid objects.
        unsafe { EC_GROUP_check_named_curve(self.as_ptr(), 0, ctx.as_ptr()) }
    }
    /// `EC_POINT_mul(group, r, n, NULL, NULL, ctx)`.
    pub fn mul_generator(&self, r: &EcPoint, n: &Bn, ctx: &BnCtx) -> bool {
        // SAFETY: valid objects.
        unsafe {
            EC_POINT_mul(
                self.as_ptr(),
                r.as_ptr(),
                n.as_ptr(),
                ptr::null(),
                ptr::null(),
                ctx.as_ptr(),
            ) == 1
        }
    }
}

// ---------------------------------------------------------------------------
// X.509 certificates, names, extensions
// ---------------------------------------------------------------------------

#[allow(clippy::upper_case_acronyms, clashing_extern_declarations)]
extern "C" {
    fn X509_new() -> *mut X509;
    fn X509_free(x: *mut X509);
    fn X509_dup(x: *const X509) -> *mut X509;
    fn PEM_read_bio_X509(
        b: *mut BIO,
        x: *mut *mut X509,
        cb: Option<PemPasswordCb>,
        u: *mut c_void,
    ) -> *mut X509;
    fn PEM_write_bio_X509(b: *mut BIO, x: *const X509) -> c_int;
    fn X509_print(b: *mut BIO, x: *const X509) -> c_int;
    fn X509_get_subject_name(x: *const X509) -> *mut X509_NAME;
    fn X509_get_issuer_name(x: *const X509) -> *mut X509_NAME;
    fn X509_NAME_oneline(n: *const X509_NAME, buf: *mut c_char, size: c_int) -> *mut c_char;
    fn X509_subject_name_hash(x: *const X509) -> c_ulong;
    fn X509_get_version(x: *const X509) -> c_long;
    fn X509_get_serialNumber(x: *mut X509) -> *mut ASN1_INTEGER;
    fn ASN1_INTEGER_to_BN(a: *const ASN1_INTEGER, bn: *mut BIGNUM) -> *mut BIGNUM;
    fn ASN1_INTEGER_set_int64(a: *mut ASN1_INTEGER, v: i64) -> c_int;
    fn a2i_ASN1_INTEGER(b: *mut BIO, bs: *mut ASN1_INTEGER, buf: *mut c_char, size: c_int)
        -> c_int;
    fn i2s_ASN1_INTEGER(m: *mut X509V3_EXT_METHOD, a: *const ASN1_INTEGER) -> *mut c_char;
    fn X509_getm_notBefore(x: *const X509) -> *mut ASN1_TIME;
    fn X509_getm_notAfter(x: *const X509) -> *mut ASN1_TIME;
    fn X509_gmtime_adj(s: *mut ASN1_TIME, adj: c_long) -> *mut ASN1_TIME;
    fn ASN1_STRING_type(s: *const ASN1_STRING) -> c_int;
    fn ASN1_STRING_length(s: *const ASN1_STRING) -> c_int;
    fn ASN1_STRING_get0_data(s: *const ASN1_STRING) -> *const c_uchar;
    fn ASN1_STRING_to_UTF8(out: *mut *mut c_uchar, s: *const ASN1_STRING) -> c_int;
    fn ASN1_STRING_set(s: *mut ASN1_STRING, data: *const c_void, len: c_int) -> c_int;
    fn ASN1_STRING_set_default_mask_asc(p: *const c_char) -> c_int;
    fn X509_alias_get0(x: *mut X509, len: *mut c_int) -> *mut c_uchar;
    fn X509_get_signature_nid(x: *const X509) -> c_int;
    fn X509_PURPOSE_get_count() -> c_int;
    fn X509_PURPOSE_get0(idx: c_int) -> *mut X509_PURPOSE;
    fn X509_PURPOSE_get_id(p: *const X509_PURPOSE) -> c_int;
    fn X509_PURPOSE_get0_name(p: *const X509_PURPOSE) -> *mut c_char;
    fn X509_PURPOSE_get0_sname(p: *const X509_PURPOSE) -> *mut c_char;
    fn X509_check_purpose(x: *mut X509, id: c_int, ca: c_int) -> c_int;
    fn X509_get_ext_count(x: *const X509) -> c_int;
    fn X509_get_ext(x: *const X509, loc: c_int) -> *mut X509_EXTENSION;
    fn X509_EXTENSION_get_object(e: *mut X509_EXTENSION) -> *mut ASN1_OBJECT;
    fn X509_EXTENSION_get_critical(e: *const X509_EXTENSION) -> c_int;
    fn X509_EXTENSION_get_data(e: *mut X509_EXTENSION) -> *mut ASN1_STRING;
    fn X509V3_EXT_print(b: *mut BIO, e: *mut X509_EXTENSION, flag: c_ulong, indent: c_int)
        -> c_int;
    fn X509V3_EXT_get(e: *mut X509_EXTENSION) -> *const X509V3_EXT_METHOD;
    fn X509V3_EXT_d2i(e: *mut X509_EXTENSION) -> *mut c_void;
    fn GENERAL_NAME_print(b: *mut BIO, g: *mut GENERAL_NAME) -> c_int;
    fn GENERAL_NAME_free(g: *mut GENERAL_NAME);
    fn X509_NAME_entry_count(n: *const X509_NAME) -> c_int;
    fn X509_NAME_get_entry(n: *const X509_NAME, loc: c_int) -> *mut X509_NAME_ENTRY;
    fn X509_NAME_ENTRY_get_object(e: *const X509_NAME_ENTRY) -> *mut ASN1_OBJECT;
    fn X509_NAME_ENTRY_get_data(e: *const X509_NAME_ENTRY) -> *mut ASN1_STRING;
    fn X509_NAME_new() -> *mut X509_NAME;
    fn X509_NAME_free(n: *mut X509_NAME);
    fn X509_NAME_add_entry_by_NID(
        n: *mut X509_NAME,
        nid: c_int,
        t: c_int,
        b: *const c_uchar,
        len: c_int,
        loc: c_int,
        set: c_int,
    ) -> c_int;
    fn X509_NAME_add_entry_by_txt(
        n: *mut X509_NAME,
        f: *const c_char,
        t: c_int,
        b: *const c_uchar,
        len: c_int,
        loc: c_int,
        set: c_int,
    ) -> c_int;
    fn X509_NAME_get_index_by_NID(n: *const X509_NAME, nid: c_int, last: c_int) -> c_int;
    fn X509_digest(x: *const X509, md: *const EVP_MD, out: *mut c_uchar, len: *mut c_uint)
        -> c_int;
    fn X509_check_private_key(x: *const X509, k: *const EVP_PKEY) -> c_int;
    fn X509_verify(x: *mut X509, k: *mut EVP_PKEY) -> c_int;
    fn X509_get_pubkey(x: *mut X509) -> *mut EVP_PKEY;
    fn X509_set_version(x: *mut X509, v: c_long) -> c_int;
    fn X509_set_subject_name(x: *mut X509, n: *const X509_NAME) -> c_int;
    fn X509_set_issuer_name(x: *mut X509, n: *const X509_NAME) -> c_int;
    fn X509_set_pubkey(x: *mut X509, k: *mut EVP_PKEY) -> c_int;
    fn X509_sign(x: *mut X509, k: *mut EVP_PKEY, md: *const EVP_MD) -> c_int;
    fn X509_PUBKEY_get(k: *mut X509_PUBKEY) -> *mut EVP_PKEY;
    fn X509_CRL_free(c: *mut X509_CRL);
    fn PEM_write_bio_X509_CRL(b: *mut BIO, c: *const X509_CRL) -> c_int;

    fn X509_STORE_new() -> *mut X509_STORE;
    fn X509_STORE_free(s: *mut X509_STORE);
    fn X509_STORE_add_lookup(s: *mut X509_STORE, m: *mut X509_LOOKUP_METHOD) -> *mut X509_LOOKUP;
    fn X509_LOOKUP_file() -> *mut X509_LOOKUP_METHOD;
    fn X509_LOOKUP_hash_dir() -> *mut X509_LOOKUP_METHOD;
    fn X509_LOOKUP_ctrl(
        l: *mut X509_LOOKUP,
        cmd: c_int,
        argc: *const c_char,
        argl: c_long,
        ret: *mut *mut c_char,
    ) -> c_int;
    fn X509_STORE_CTX_new() -> *mut X509_STORE_CTX;
    fn X509_STORE_CTX_free(c: *mut X509_STORE_CTX);
    fn X509_STORE_CTX_init(
        c: *mut X509_STORE_CTX,
        s: *mut X509_STORE,
        x: *mut X509,
        chain: *mut OPENSSL_STACK,
    ) -> c_int;
    fn X509_STORE_CTX_set_purpose(c: *mut X509_STORE_CTX, p: c_int) -> c_int;
    fn X509_verify_cert(c: *mut X509_STORE_CTX) -> c_int;
    fn PEM_X509_INFO_read_bio(
        b: *mut BIO,
        sk: *mut OPENSSL_STACK,
        cb: Option<PemPasswordCb>,
        u: *mut c_void,
    ) -> *mut OPENSSL_STACK;
    fn X509_INFO_free(i: *mut X509_INFO);
}

/// `V_ASN1_UTF8STRING`.
pub const V_ASN1_UTF8STRING: i32 = 12;
/// `V_ASN1_UTCTIME`.
pub const V_ASN1_UTCTIME: i32 = 23;
/// `V_ASN1_GENERALIZEDTIME`.
pub const V_ASN1_GENERALIZEDTIME: i32 = 24;
/// `NID_subject_alt_name`.
pub const NID_SUBJECT_ALT_NAME: i32 = 85;
/// `MBSTRING_UTF8`.
const MBSTRING_UTF8: c_int = 0x1000;
const GEN_EMAIL: c_int = 1;
const GEN_DNS: c_int = 2;
const GEN_URI: c_int = 6;
const X509_L_FILE_LOAD: c_int = 1;
const X509_L_ADD_DIR: c_int = 2;
const X509_FILETYPE_PEM: c_long = 1;
const X509_FILETYPE_DEFAULT: c_long = 3;

unsafe extern "C" fn free_x509(p: *mut c_void) {
    // SAFETY: called by OPENSSL_sk_pop_free on X509 elements.
    unsafe { X509_free(p.cast()) }
}
unsafe extern "C" fn free_crl(p: *mut c_void) {
    // SAFETY: called by OPENSSL_sk_pop_free on X509_CRL elements.
    unsafe { X509_CRL_free(p.cast()) }
}
unsafe extern "C" fn free_general_name(p: *mut c_void) {
    // SAFETY: called by OPENSSL_sk_pop_free on GENERAL_NAME elements.
    unsafe { GENERAL_NAME_free(p.cast()) }
}

handle!(
    /// An `X509` certificate.
    X509Cert,
    X509,
    X509_free
);

/// A view of an `ASN1_STRING` (type and raw bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asn1Str {
    /// `ASN1_STRING_type`.
    pub kind: i32,
    /// `ASN1_STRING_get0_data` bytes.
    pub data: Vec<u8>,
}

/// # Safety
/// `s` must be a valid `ASN1_STRING`.
unsafe fn asn1_view(s: *const ASN1_STRING) -> Asn1Str {
    // SAFETY: per contract.
    unsafe {
        let len = ASN1_STRING_length(s).max(0) as usize;
        let p = ASN1_STRING_get0_data(s);
        let data = if p.is_null() || len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(p, len).to_vec()
        };
        Asn1Str {
            kind: ASN1_STRING_type(s),
            data,
        }
    }
}

/// One entry of an `X509_NAME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameEntry {
    /// NID of the attribute type.
    pub nid: i32,
    /// UTF-8 value (`None` when `ASN1_STRING_to_UTF8` failed).
    pub value: Option<Vec<u8>>,
}

/// Rendering of one certificate extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionView {
    /// Short name, or dotted OID for unknown extensions.
    pub name: String,
    /// NID (0 = unknown).
    pub nid: i32,
    /// Critical flag.
    pub critical: bool,
    /// Raw `extnValue` contents.
    pub data: Vec<u8>,
}

/// Outcome of rendering an extension's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtText {
    /// Printed text.
    Text(Vec<u8>),
    /// `X509V3_EXT_print` could not print it.
    Unprintable,
    /// Hard failure (PHP aborts `openssl_x509_parse`).
    Failed,
}

/// A borrowed `X509_NAME` (owned by its certificate/request).
#[derive(Debug, Clone, Copy)]
pub struct NameRef<'a>(*const X509_NAME, PhantomData<&'a ()>);

impl NameRef<'_> {
    /// `X509_NAME_oneline(name, NULL, 0)`.
    pub fn oneline(&self) -> Option<Vec<u8>> {
        // SAFETY: valid name; result allocated by OpenSSL.
        take_cstr(unsafe { X509_NAME_oneline(self.0, ptr::null_mut(), 0) })
    }

    /// Entries as in `php_openssl_add_assoc_name_entry`.
    pub fn entries(&self) -> Vec<NameEntry> {
        let mut out = Vec::new();
        // SAFETY: valid name; entries borrowed from it.
        unsafe {
            for i in 0..X509_NAME_entry_count(self.0) {
                let ne = X509_NAME_get_entry(self.0, i);
                let nid = OBJ_obj2nid(X509_NAME_ENTRY_get_object(ne));
                let s = X509_NAME_ENTRY_get_data(ne);
                let value = if ASN1_STRING_type(s) == V_ASN1_UTF8STRING {
                    Some(asn1_view(s).data)
                } else {
                    let mut buf: *mut c_uchar = ptr::null_mut();
                    let n = ASN1_STRING_to_UTF8(&raw mut buf, s);
                    if n < 0 {
                        None
                    } else {
                        let v = if buf.is_null() {
                            Vec::new()
                        } else {
                            std::slice::from_raw_parts(buf, n as usize).to_vec()
                        };
                        openssl_free(buf.cast());
                        Some(v)
                    }
                };
                out.push(NameEntry { nid, value });
            }
        }
        out
    }
}

impl X509Cert {
    /// `PEM_read_bio_X509(bio, NULL, NULL, NULL)`.
    pub fn read_pem(bio: &Bio<'_>) -> Option<Self> {
        let mut ctx = PassCtx { pass: None };
        // SAFETY: valid BIO.
        Self::from_raw(unsafe {
            PEM_read_bio_X509(
                bio.as_ptr(),
                ptr::null_mut(),
                Some(pem_password_cb),
                ptr::addr_of_mut!(ctx).cast(),
            )
        })
    }

    /// `X509_new()`.
    pub fn new() -> Option<Self> {
        // SAFETY: constructor.
        Self::from_raw(unsafe { X509_new() })
    }

    /// `X509_dup`.
    pub fn dup(&self) -> Option<Self> {
        // SAFETY: valid cert.
        Self::from_raw(unsafe { X509_dup(self.as_ptr()) })
    }

    /// `PEM_write_bio_X509`.
    pub fn write_pem(&self, bio: &Bio<'_>) -> bool {
        // SAFETY: valid objects.
        unsafe { PEM_write_bio_X509(bio.as_ptr(), self.as_ptr()) == 1 }
    }

    /// PEM text, or `None` on failure.
    pub fn to_pem(&self) -> Option<Vec<u8>> {
        let b = Bio::mem()?;
        self.write_pem(&b).then(|| b.contents())
    }

    /// `X509_print`.
    pub fn print(&self, bio: &Bio<'_>) -> bool {
        // SAFETY: valid objects.
        unsafe { X509_print(bio.as_ptr(), self.as_ptr()) == 1 }
    }

    /// Subject name.
    pub fn subject(&self) -> NameRef<'_> {
        // SAFETY: valid cert; the name is owned by it.
        NameRef(unsafe { X509_get_subject_name(self.as_ptr()) }, PhantomData)
    }

    /// Issuer name.
    pub fn issuer(&self) -> NameRef<'_> {
        // SAFETY: valid cert.
        NameRef(unsafe { X509_get_issuer_name(self.as_ptr()) }, PhantomData)
    }

    /// `X509_subject_name_hash`.
    pub fn subject_name_hash(&self) -> u64 {
        // SAFETY: valid cert.
        unsafe { X509_subject_name_hash(self.as_ptr()) }
    }

    /// `X509_get_version`.
    pub fn version(&self) -> i64 {
        // SAFETY: valid cert.
        unsafe { X509_get_version(self.as_ptr()) }
    }

    /// Serial as (`BN_bn2hex`, `i2s_ASN1_INTEGER`); `None` on failure.
    pub fn serial(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        // SAFETY: valid cert; temporary BIGNUM freed by the handle.
        unsafe {
            let a = X509_get_serialNumber(self.as_ptr());
            let bn = Bn::from_raw(ASN1_INTEGER_to_BN(a, ptr::null_mut()))?;
            let hex = bn.to_hex()?;
            let dec = take_cstr(i2s_ASN1_INTEGER(ptr::null_mut(), a))?;
            Some((hex, dec))
        }
    }

    /// `notBefore` as an ASN.1 string view.
    pub fn not_before(&self) -> Asn1Str {
        // SAFETY: valid cert; time owned by it.
        unsafe { asn1_view(X509_getm_notBefore(self.as_ptr())) }
    }

    /// `notAfter` as an ASN.1 string view.
    pub fn not_after(&self) -> Asn1Str {
        // SAFETY: valid cert.
        unsafe { asn1_view(X509_getm_notAfter(self.as_ptr())) }
    }

    /// `X509_alias_get0`.
    pub fn alias(&self) -> Option<Vec<u8>> {
        let mut len = 0;
        // SAFETY: valid cert; returned data owned by it.
        let p = unsafe { X509_alias_get0(self.as_ptr(), &raw mut len) };
        if p.is_null() {
            return None;
        }
        // SAFETY: PHP treats it as a C string.
        Some(unsafe { CStr::from_ptr(p.cast()) }.to_bytes().to_vec())
    }

    /// `X509_get_signature_nid`.
    pub fn signature_nid(&self) -> i32 {
        // SAFETY: valid cert.
        unsafe { X509_get_signature_nid(self.as_ptr()) }
    }

    /// `X509_check_purpose(cert, id, ca)`.
    pub fn check_purpose(&self, id: i32, ca: i32) -> i32 {
        // SAFETY: valid cert.
        unsafe { X509_check_purpose(self.as_ptr(), id, ca) }
    }

    /// Extensions with their names (as in `openssl_x509_parse`).
    pub fn extensions(&self) -> Vec<ExtensionView> {
        let mut out = Vec::new();
        // SAFETY: valid cert; extensions borrowed from it.
        unsafe {
            for i in 0..X509_get_ext_count(self.as_ptr()) {
                let ext = X509_get_ext(self.as_ptr(), i);
                let obj = X509_EXTENSION_get_object(ext);
                let nid = OBJ_obj2nid(obj);
                let name = if nid == 0 {
                    obj2txt(obj, true)
                } else {
                    nid2sn(nid).unwrap_or_default()
                };
                out.push(ExtensionView {
                    name,
                    nid,
                    critical: X509_EXTENSION_get_critical(ext) != 0,
                    data: asn1_view(X509_EXTENSION_get_data(ext)).data,
                });
            }
        }
        out
    }

    /// Renders extension `idx` like `openssl_x509_parse`: special
    /// subjectAltName printer, else `X509V3_EXT_print`.
    pub fn extension_text(&self, idx: usize) -> ExtText {
        let Some(bio) = Bio::mem() else {
            return ExtText::Failed;
        };
        // SAFETY: valid cert and index (< ext count, from `extensions`).
        unsafe {
            let ext = X509_get_ext(self.as_ptr(), idx as c_int);
            if ext.is_null() {
                return ExtText::Failed;
            }
            let nid = OBJ_obj2nid(X509_EXTENSION_get_object(ext));
            if nid == NID_SUBJECT_ALT_NAME {
                if X509V3_EXT_get(ext).is_null() {
                    return ExtText::Failed;
                }
                let names = X509V3_EXT_d2i(ext).cast::<OPENSSL_STACK>();
                if names.is_null() {
                    return ExtText::Failed;
                }
                let num = OPENSSL_sk_num(names);
                for i in 0..num {
                    let g = OPENSSL_sk_value(names, i).cast::<GENERAL_NAME>();
                    let prefix: Option<&CStr> = match (*g).type_ {
                        GEN_EMAIL => Some(c"email:"),
                        GEN_DNS => Some(c"DNS:"),
                        GEN_URI => Some(c"URI:"),
                        _ => None,
                    };
                    if let Some(p) = prefix {
                        BIO_puts(bio.as_ptr(), p.as_ptr());
                        let s = asn1_view((*g).d);
                        BIO_write(bio.as_ptr(), s.data.as_ptr().cast(), c_len(s.data.len()));
                    } else {
                        GENERAL_NAME_print(bio.as_ptr(), g);
                    }
                    if i < num - 1 {
                        BIO_puts(bio.as_ptr(), c", ".as_ptr());
                    }
                }
                OPENSSL_sk_pop_free(names, free_general_name);
                ExtText::Text(bio.contents())
            } else if X509V3_EXT_print(bio.as_ptr(), ext, 0, 0) > 0 {
                ExtText::Text(bio.contents())
            } else {
                ExtText::Unprintable
            }
        }
    }

    /// `X509_digest`.
    pub fn digest(&self, md: &Md) -> Option<Vec<u8>> {
        let mut buf = [0u8; 64];
        let mut n: c_uint = 0;
        // SAFETY: EVP_MAX_MD_SIZE buffer.
        (unsafe { X509_digest(self.as_ptr(), md.as_ptr(), buf.as_mut_ptr(), &raw mut n) } == 1)
            .then(|| buf[..n as usize].to_vec())
    }

    /// `X509_check_private_key`.
    pub fn check_private_key(&self, key: &Pkey) -> bool {
        // SAFETY: valid objects.
        unsafe { X509_check_private_key(self.as_ptr(), key.as_ptr()) == 1 }
    }

    /// `X509_verify`.
    pub fn verify(&self, key: &Pkey) -> i32 {
        // SAFETY: valid objects.
        unsafe { X509_verify(self.as_ptr(), key.as_ptr()) }
    }

    /// `X509_get_pubkey`.
    pub fn public_key(&self) -> Option<Pkey> {
        // SAFETY: valid cert; returns a new reference.
        Pkey::from_raw(unsafe { X509_get_pubkey(self.as_ptr()) })
    }

    /// `X509_set_version`.
    pub fn set_version(&self, v: i64) -> bool {
        // SAFETY: valid cert.
        unsafe { X509_set_version(self.as_ptr(), v as c_long) == 1 }
    }

    /// `a2i_ASN1_INTEGER` from `hex` into the serial number.
    pub fn set_serial_hex(&self, hex: &[u8]) -> bool {
        let Some(bio) = Bio::mem_buf(hex) else {
            return false;
        };
        let mut buf = [0 as c_char; 256];
        // SAFETY: valid BIO/cert/buffer.
        unsafe {
            a2i_ASN1_INTEGER(
                bio.as_ptr(),
                X509_get_serialNumber(self.as_ptr()),
                buf.as_mut_ptr(),
                buf.len() as c_int,
            ) == 1
        }
    }

    /// `ASN1_INTEGER_set_int64` on the serial number.
    pub fn set_serial(&self, v: i64) -> bool {
        // SAFETY: valid cert.
        unsafe { ASN1_INTEGER_set_int64(X509_get_serialNumber(self.as_ptr()), v) == 1 }
    }

    /// `X509_set_subject_name`.
    pub fn set_subject(&self, n: NameRef<'_>) -> bool {
        // SAFETY: valid objects (the name is copied).
        unsafe { X509_set_subject_name(self.as_ptr(), n.0) == 1 }
    }

    /// `X509_set_issuer_name`.
    pub fn set_issuer(&self, n: NameRef<'_>) -> bool {
        // SAFETY: valid objects (the name is copied).
        unsafe { X509_set_issuer_name(self.as_ptr(), n.0) == 1 }
    }

    /// `X509_gmtime_adj` on notBefore (0) and notAfter (`secs`).
    pub fn set_validity(&self, secs: i64) -> bool {
        // SAFETY: valid cert.
        unsafe {
            !X509_gmtime_adj(X509_getm_notBefore(self.as_ptr()), 0).is_null()
                && !X509_gmtime_adj(X509_getm_notAfter(self.as_ptr()), secs as c_long).is_null()
        }
    }

    /// `X509_set_pubkey`.
    pub fn set_pubkey(&self, k: &Pkey) -> bool {
        // SAFETY: valid objects.
        unsafe { X509_set_pubkey(self.as_ptr(), k.as_ptr()) == 1 }
    }

    /// `X509_sign`.
    pub fn sign(&self, k: &Pkey, md: Option<&Md>) -> bool {
        // SAFETY: valid objects.
        unsafe {
            X509_sign(
                self.as_ptr(),
                k.as_ptr(),
                md.map_or(ptr::null(), |m| m.as_ptr().cast_const()),
            ) > 0
        }
    }
}

/// `X509_PURPOSE_*` table entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Purpose {
    /// Purpose id.
    pub id: i32,
    /// Short name.
    pub sname: String,
    /// Long name.
    pub name: String,
}

/// All purposes in table order.
pub fn purposes() -> Vec<Purpose> {
    let mut v = Vec::new();
    // SAFETY: static table.
    unsafe {
        for i in 0..X509_PURPOSE_get_count() {
            let p = X509_PURPOSE_get0(i);
            if p.is_null() {
                continue;
            }
            v.push(Purpose {
                id: X509_PURPOSE_get_id(p),
                sname: cstr_opt(X509_PURPOSE_get0_sname(p)).unwrap_or_default(),
                name: cstr_opt(X509_PURPOSE_get0_name(p)).unwrap_or_default(),
            });
        }
    }
    v
}

/// `ASN1_STRING_set_default_mask_asc`.
pub fn set_default_string_mask(mask: &[u8]) -> bool {
    let c = to_cstring(mask);
    // SAFETY: valid C string.
    unsafe { ASN1_STRING_set_default_mask_asc(c.as_ptr()) == 1 }
}

/// An owned stack of certificates (`STACK_OF(X509)`, pop-freed).
pub struct X509Stack(NonNull<OPENSSL_STACK>);

impl std::fmt::Debug for X509Stack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "X509Stack(len={})", self.len())
    }
}

impl Drop for X509Stack {
    fn drop(&mut self) {
        // SAFETY: owned stack of owned certs.
        unsafe { OPENSSL_sk_pop_free(self.0.as_ptr(), free_x509) }
    }
}

impl X509Stack {
    /// `sk_X509_new_null`.
    pub fn new() -> Option<Self> {
        // SAFETY: constructor.
        NonNull::new(unsafe { OPENSSL_sk_new_null() }).map(X509Stack)
    }
    /// Pushes (taking ownership).
    pub fn push(&self, cert: X509Cert) -> bool {
        // SAFETY: stack takes ownership on success.
        let ok = unsafe { OPENSSL_sk_push(self.0.as_ptr(), cert.as_ptr().cast()) } > 0;
        if ok {
            std::mem::forget(cert);
        }
        ok
    }
    /// Number of certs.
    pub fn len(&self) -> usize {
        // SAFETY: valid stack.
        usize::try_from(unsafe { OPENSSL_sk_num(self.0.as_ptr()) }).unwrap_or(0)
    }
    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// PEM of each certificate.
    pub fn pems(&self) -> Vec<Vec<u8>> {
        // SAFETY: borrowed raw stack valid for the call.
        unsafe { stack_x509_pems(self.0.as_ptr()) }
    }
    fn as_ptr(&self) -> *mut OPENSSL_STACK {
        self.0.as_ptr()
    }
}

fn opt_stack(s: Option<&X509Stack>) -> *mut OPENSSL_STACK {
    s.map_or(ptr::null_mut(), X509Stack::as_ptr)
}

/// # Safety
/// `sk` must be a valid `STACK_OF(X509)` (or NULL).
unsafe fn stack_x509_pems(sk: *const OPENSSL_STACK) -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    if sk.is_null() {
        return v;
    }
    let Some(bio) = Bio::mem() else { return v };
    // SAFETY: per contract.
    unsafe {
        for i in 0..OPENSSL_sk_num(sk) {
            let x = OPENSSL_sk_value(sk, i).cast::<X509>();
            if PEM_write_bio_X509(bio.as_ptr(), x) == 1 {
                v.push(bio.contents());
            }
            bio.reset();
        }
    }
    v
}

/// # Safety
/// `sk` must be a valid `STACK_OF(X509_CRL)` (or NULL).
unsafe fn stack_crl_pems(sk: *const OPENSSL_STACK) -> Vec<Vec<u8>> {
    let mut v = Vec::new();
    if sk.is_null() {
        return v;
    }
    let Some(bio) = Bio::mem() else { return v };
    // SAFETY: per contract.
    unsafe {
        for i in 0..OPENSSL_sk_num(sk) {
            let x = OPENSSL_sk_value(sk, i).cast::<X509_CRL>();
            if PEM_write_bio_X509_CRL(bio.as_ptr(), x) == 1 {
                v.push(bio.contents());
            }
            bio.reset();
        }
    }
    v
}

/// `PEM_X509_INFO_read_bio` and extract the certificates (in order).
/// `None` when reading failed.
pub fn read_x509_infos(bio: &Bio<'_>) -> Option<X509Stack> {
    // SAFETY: valid BIO; we take every info's cert and free the infos.
    unsafe {
        let sk = PEM_X509_INFO_read_bio(bio.as_ptr(), ptr::null_mut(), None, ptr::null_mut());
        if sk.is_null() {
            return None;
        }
        let out = X509Stack::new();
        while OPENSSL_sk_num(sk) > 0 {
            let xi = OPENSSL_sk_shift(sk).cast::<X509_INFO>();
            if !(*xi).x509.is_null() {
                if let Some(s) = &out {
                    OPENSSL_sk_push(s.as_ptr(), (*xi).x509.cast());
                    (*xi).x509 = ptr::null_mut();
                }
            }
            X509_INFO_free(xi);
        }
        OPENSSL_sk_free(sk);
        out
    }
}

handle!(
    /// `X509_STORE`.
    X509Store,
    X509_STORE,
    X509_STORE_free
);

impl X509Store {
    /// `X509_STORE_new`.
    pub fn new() -> Option<Self> {
        // SAFETY: constructor.
        Self::from_raw(unsafe { X509_STORE_new() })
    }
    /// Adds a file lookup and loads `path` as PEM (`None` = default file).
    pub fn load_file(&self, path: Option<&[u8]>) -> bool {
        let c = path.map(to_cstring);
        // SAFETY: lookup owned by the store; path valid or NULL.
        unsafe {
            let l = X509_STORE_add_lookup(self.as_ptr(), X509_LOOKUP_file());
            !l.is_null()
                && X509_LOOKUP_ctrl(
                    l,
                    X509_L_FILE_LOAD,
                    c.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
                    if path.is_some() {
                        X509_FILETYPE_PEM
                    } else {
                        X509_FILETYPE_DEFAULT
                    },
                    ptr::null_mut(),
                ) == 1
        }
    }
    /// Adds a hash-dir lookup for `path` (`None` = default dir).
    pub fn add_dir(&self, path: Option<&[u8]>) -> bool {
        let c = path.map(to_cstring);
        // SAFETY: lookup owned by the store.
        unsafe {
            let l = X509_STORE_add_lookup(self.as_ptr(), X509_LOOKUP_hash_dir());
            !l.is_null()
                && X509_LOOKUP_ctrl(
                    l,
                    X509_L_ADD_DIR,
                    c.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
                    if path.is_some() {
                        X509_FILETYPE_PEM
                    } else {
                        X509_FILETYPE_DEFAULT
                    },
                    ptr::null_mut(),
                ) == 1
        }
    }
}

/// Failure points of [`check_cert`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckCert {
    /// `X509_STORE_CTX_new` failed.
    NoMemory,
    /// `X509_STORE_CTX_init` failed.
    InitFailed,
    /// Ran verification; `set_purpose_failed` reports the purpose call.
    Verified {
        /// `X509_verify_cert` result.
        result: i32,
        /// `X509_STORE_CTX_set_purpose` returned 0.
        set_purpose_failed: bool,
    },
}

/// `php_openssl_check_cert`.
pub fn check_cert(
    store: &X509Store,
    cert: &X509Cert,
    untrusted: Option<&X509Stack>,
    purpose: i32,
) -> CheckCert {
    // SAFETY: valid objects; ctx freed before return.
    unsafe {
        let csc = X509_STORE_CTX_new();
        if csc.is_null() {
            return CheckCert::NoMemory;
        }
        if X509_STORE_CTX_init(csc, store.as_ptr(), cert.as_ptr(), opt_stack(untrusted)) == 0 {
            X509_STORE_CTX_free(csc);
            return CheckCert::InitFailed;
        }
        let set_purpose_failed = purpose >= 0 && X509_STORE_CTX_set_purpose(csc, purpose) == 0;
        let result = X509_verify_cert(csc);
        X509_STORE_CTX_free(csc);
        CheckCert::Verified {
            result,
            set_purpose_failed,
        }
    }
}

// ---------------------------------------------------------------------------
// CSR, configuration, extensions
// ---------------------------------------------------------------------------

#[allow(clippy::upper_case_acronyms, clashing_extern_declarations)]
extern "C" {
    fn X509_REQ_new() -> *mut X509_REQ;
    fn X509_REQ_free(r: *mut X509_REQ);
    fn PEM_read_bio_X509_REQ(
        b: *mut BIO,
        x: *mut *mut X509_REQ,
        cb: Option<PemPasswordCb>,
        u: *mut c_void,
    ) -> *mut X509_REQ;
    fn PEM_write_bio_X509_REQ(b: *mut BIO, x: *const X509_REQ) -> c_int;
    fn X509_REQ_print(b: *mut BIO, x: *const X509_REQ) -> c_int;
    fn X509_REQ_set_version(r: *mut X509_REQ, v: c_long) -> c_int;
    fn X509_REQ_set_subject_name(r: *mut X509_REQ, n: *const X509_NAME) -> c_int;
    fn X509_REQ_get_subject_name(r: *const X509_REQ) -> *mut X509_NAME;
    fn X509_REQ_add1_attr_by_NID(
        r: *mut X509_REQ,
        nid: c_int,
        t: c_int,
        b: *const c_uchar,
        len: c_int,
    ) -> c_int;
    fn X509_REQ_add1_attr_by_txt(
        r: *mut X509_REQ,
        name: *const c_char,
        t: c_int,
        b: *const c_uchar,
        len: c_int,
    ) -> c_int;
    fn X509_REQ_get_attr_by_NID(r: *const X509_REQ, nid: c_int, last: c_int) -> c_int;
    fn X509_REQ_set_pubkey(r: *mut X509_REQ, k: *mut EVP_PKEY) -> c_int;
    fn X509_REQ_get_pubkey(r: *mut X509_REQ) -> *mut EVP_PKEY;
    fn X509_REQ_sign(r: *mut X509_REQ, k: *mut EVP_PKEY, md: *const EVP_MD) -> c_int;
    fn X509_REQ_verify(r: *mut X509_REQ, k: *mut EVP_PKEY) -> c_int;

    fn NCONF_new(m: *mut CONF_METHOD) -> *mut CONF;
    fn NCONF_free(c: *mut CONF);
    fn NCONF_load(c: *mut CONF, file: *const c_char, eline: *mut c_long) -> c_int;
    fn NCONF_get_string(c: *const CONF, group: *const c_char, name: *const c_char) -> *mut c_char;
    fn NCONF_get_number_e(
        c: *const CONF,
        group: *const c_char,
        name: *const c_char,
        out: *mut c_long,
    ) -> c_int;
    fn NCONF_get_section(c: *const CONF, section: *const c_char) -> *mut OPENSSL_STACK;
    fn X509V3_set_ctx(
        ctx: *mut X509V3_CTX,
        issuer: *mut X509,
        subject: *mut X509,
        req: *mut X509_REQ,
        crl: *mut c_void,
        flags: c_int,
    );
    fn X509V3_set_nconf(ctx: *mut X509V3_CTX, conf: *mut CONF);
    fn X509V3_EXT_add_nconf(
        conf: *mut CONF,
        ctx: *mut X509V3_CTX,
        section: *const c_char,
        cert: *mut X509,
    ) -> c_int;
    fn X509V3_EXT_REQ_add_nconf(
        conf: *mut CONF,
        ctx: *mut X509V3_CTX,
        section: *const c_char,
        req: *mut X509_REQ,
    ) -> c_int;
}

const X509V3_CTX_TEST: c_int = 0x1;

handle!(
    /// `X509_REQ`.
    X509Req,
    X509_REQ,
    X509_REQ_free
);

impl X509Req {
    /// `X509_REQ_new`.
    pub fn new() -> Option<Self> {
        // SAFETY: constructor.
        Self::from_raw(unsafe { X509_REQ_new() })
    }
    /// `PEM_read_bio_X509_REQ`.
    pub fn read_pem(bio: &Bio<'_>) -> Option<Self> {
        let mut ctx = PassCtx { pass: None };
        // SAFETY: valid BIO.
        Self::from_raw(unsafe {
            PEM_read_bio_X509_REQ(
                bio.as_ptr(),
                ptr::null_mut(),
                Some(pem_password_cb),
                ptr::addr_of_mut!(ctx).cast(),
            )
        })
    }
    /// `PEM_write_bio_X509_REQ`.
    pub fn write_pem(&self, bio: &Bio<'_>) -> bool {
        // SAFETY: valid objects.
        unsafe { PEM_write_bio_X509_REQ(bio.as_ptr(), self.as_ptr()) == 1 }
    }
    /// `X509_REQ_print`.
    pub fn print(&self, bio: &Bio<'_>) -> bool {
        // SAFETY: valid objects.
        unsafe { X509_REQ_print(bio.as_ptr(), self.as_ptr()) == 1 }
    }
    /// `X509_REQ_set_version`.
    pub fn set_version(&self, v: i64) -> bool {
        // SAFETY: valid request.
        unsafe { X509_REQ_set_version(self.as_ptr(), v as c_long) == 1 }
    }
    /// Subject.
    pub fn subject(&self) -> NameRef<'_> {
        // SAFETY: valid request; name owned by it.
        NameRef(
            unsafe { X509_REQ_get_subject_name(self.as_ptr()) },
            PhantomData,
        )
    }
    /// `X509_REQ_set_subject_name`.
    pub fn set_subject(&self, n: &X509Name) -> bool {
        // SAFETY: valid objects (copied).
        unsafe { X509_REQ_set_subject_name(self.as_ptr(), n.as_ptr()) == 1 }
    }
    /// `X509_REQ_add1_attr_by_NID(…, MBSTRING_UTF8, value, len)`.
    pub fn add_attr_by_nid(&self, nid: i32, value: &[u8]) -> bool {
        // SAFETY: valid buffer.
        unsafe {
            X509_REQ_add1_attr_by_NID(
                self.as_ptr(),
                nid,
                MBSTRING_UTF8,
                value.as_ptr(),
                c_len(value.len()),
            ) == 1
        }
    }
    /// `X509_REQ_add1_attr_by_txt(…, MBSTRING_UTF8, value, -1)`.
    pub fn add_attr_by_txt(&self, name: &[u8], value: &[u8]) -> bool {
        let n = to_cstring(name);
        let v = to_cstring(value);
        // SAFETY: valid C strings.
        unsafe {
            X509_REQ_add1_attr_by_txt(
                self.as_ptr(),
                n.as_ptr(),
                MBSTRING_UTF8,
                v.as_ptr().cast(),
                -1,
            ) == 1
        }
    }
    /// `X509_REQ_get_attr_by_NID(req, nid, -1)`.
    pub fn attr_index(&self, nid: i32) -> i32 {
        // SAFETY: valid request.
        unsafe { X509_REQ_get_attr_by_NID(self.as_ptr(), nid, -1) }
    }
    /// `X509_REQ_set_pubkey`.
    pub fn set_pubkey(&self, k: &Pkey) -> bool {
        // SAFETY: valid objects.
        unsafe { X509_REQ_set_pubkey(self.as_ptr(), k.as_ptr()) == 1 }
    }
    /// `X509_REQ_get_pubkey`.
    pub fn public_key(&self) -> Option<Pkey> {
        // SAFETY: valid request; new reference.
        Pkey::from_raw(unsafe { X509_REQ_get_pubkey(self.as_ptr()) })
    }
    /// `X509_REQ_sign`.
    pub fn sign(&self, k: &Pkey, md: Option<&Md>) -> bool {
        // SAFETY: valid objects.
        unsafe {
            X509_REQ_sign(
                self.as_ptr(),
                k.as_ptr(),
                md.map_or(ptr::null(), |m| m.as_ptr().cast_const()),
            ) > 0
        }
    }
    /// `X509_REQ_verify`.
    pub fn verify(&self, k: &Pkey) -> i32 {
        // SAFETY: valid objects.
        unsafe { X509_REQ_verify(self.as_ptr(), k.as_ptr()) }
    }
}

handle!(
    /// An owned `X509_NAME`.
    X509Name,
    X509_NAME,
    X509_NAME_free
);

impl X509Name {
    /// `X509_NAME_new`.
    pub fn new() -> Option<Self> {
        // SAFETY: constructor.
        Self::from_raw(unsafe { X509_NAME_new() })
    }
    /// `X509_NAME_add_entry_by_NID(…, MBSTRING_UTF8, value, -1, -1, 0)`.
    pub fn add_by_nid(&self, nid: i32, value: &[u8]) -> bool {
        let v = to_cstring(value);
        // SAFETY: valid C string (len -1 = strlen).
        unsafe {
            X509_NAME_add_entry_by_NID(
                self.as_ptr(),
                nid,
                MBSTRING_UTF8,
                v.as_ptr().cast(),
                -1,
                -1,
                0,
            ) == 1
        }
    }
    /// `X509_NAME_add_entry_by_txt(…, MBSTRING_UTF8, value, -1, -1, 0)`.
    pub fn add_by_txt(&self, field: &[u8], value: &[u8]) -> bool {
        let f = to_cstring(field);
        let v = to_cstring(value);
        // SAFETY: valid C strings.
        unsafe {
            X509_NAME_add_entry_by_txt(
                self.as_ptr(),
                f.as_ptr(),
                MBSTRING_UTF8,
                v.as_ptr().cast(),
                -1,
                -1,
                0,
            ) == 1
        }
    }
    /// `X509_NAME_get_index_by_NID(name, nid, -1)`.
    pub fn index_by_nid(&self, nid: i32) -> i32 {
        // SAFETY: valid name.
        unsafe { X509_NAME_get_index_by_NID(self.as_ptr(), nid, -1) }
    }
    /// `X509_NAME_entry_count`.
    pub fn entry_count(&self) -> i32 {
        // SAFETY: valid name.
        unsafe { X509_NAME_entry_count(self.as_ptr()) }
    }
}

handle!(
    /// An NCONF configuration.
    Conf,
    CONF,
    NCONF_free
);

/// One `name = value` line of a config section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfValue {
    /// Key.
    pub name: Vec<u8>,
    /// Value.
    pub value: Vec<u8>,
}

/// Which certificate/request an extension section is applied to.
#[derive(Debug)]
pub enum ExtTarget<'a> {
    /// Syntax check only (`X509V3_set_ctx_test`, no target).
    Test,
    /// Add to a certificate: issuer, subject cert.
    Cert {
        /// Issuer certificate (the subject itself when self-signed).
        issuer: Option<&'a X509Cert>,
        /// Certificate being built.
        subject: &'a X509Cert,
        /// Originating request.
        req: &'a X509Req,
    },
    /// Add to a request.
    Req(&'a X509Req),
}

impl Conf {
    /// `NCONF_new(NULL)`.
    pub fn new() -> Option<Self> {
        // SAFETY: constructor.
        Self::from_raw(unsafe { NCONF_new(ptr::null_mut()) })
    }
    /// `NCONF_load(conf, file, NULL)`.
    pub fn load(&self, file: &[u8]) -> bool {
        let c = to_cstring(file);
        // SAFETY: valid C string.
        unsafe { NCONF_load(self.as_ptr(), c.as_ptr(), ptr::null_mut()) == 1 }
    }
    /// `NCONF_get_string` (`None` → NULL group). Leaves errors queued.
    pub fn get_string(&self, group: Option<&[u8]>, name: &[u8]) -> Option<Vec<u8>> {
        let g = group.map(to_cstring);
        let n = to_cstring(name);
        // SAFETY: valid C strings; result owned by the CONF (copied).
        let p = unsafe {
            NCONF_get_string(
                self.as_ptr(),
                g.as_ref().map_or(ptr::null(), |g| g.as_ptr()),
                n.as_ptr(),
            )
        };
        // SAFETY: NUL-terminated string owned by the config.
        (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_bytes().to_vec())
    }
    /// `NCONF_get_number_e` (0 when absent).
    pub fn get_number(&self, group: Option<&[u8]>, name: &[u8]) -> i64 {
        let g = group.map(to_cstring);
        let n = to_cstring(name);
        let mut out: c_long = 0;
        // SAFETY: valid C strings.
        unsafe {
            NCONF_get_number_e(
                self.as_ptr(),
                g.as_ref().map_or(ptr::null(), |g| g.as_ptr()),
                n.as_ptr(),
                &raw mut out,
            )
        };
        out
    }
    /// `NCONF_get_section`.
    pub fn section(&self, name: &[u8]) -> Option<Vec<ConfValue>> {
        let n = to_cstring(name);
        // SAFETY: valid C string; values owned by the config.
        unsafe {
            let sk = NCONF_get_section(self.as_ptr(), n.as_ptr());
            if sk.is_null() {
                return None;
            }
            let mut v = Vec::new();
            for i in 0..OPENSSL_sk_num(sk) {
                let cv = OPENSSL_sk_value(sk, i).cast::<CONF_VALUE>();
                v.push(ConfValue {
                    name: CStr::from_ptr((*cv).name).to_bytes().to_vec(),
                    value: CStr::from_ptr((*cv).value).to_bytes().to_vec(),
                });
            }
            Some(v)
        }
    }
    /// `X509V3_set_ctx` + `X509V3_set_nconf` + `X509V3_EXT_(REQ_)add_nconf`.
    pub fn add_extensions(&self, section: &[u8], target: &ExtTarget<'_>) -> bool {
        let s = to_cstring(section);
        let mut ctx = X509V3_CTX { _storage: [0; 256] };
        // SAFETY: zeroed storage larger than X509V3_CTX, initialised by
        // X509V3_set_ctx; referenced objects outlive the call.
        unsafe {
            match target {
                ExtTarget::Test => {
                    X509V3_set_ctx(
                        &raw mut ctx,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        X509V3_CTX_TEST,
                    );
                    X509V3_set_nconf(&raw mut ctx, self.as_ptr());
                    X509V3_EXT_add_nconf(self.as_ptr(), &raw mut ctx, s.as_ptr(), ptr::null_mut())
                        == 1
                }
                ExtTarget::Cert {
                    issuer,
                    subject,
                    req,
                } => {
                    X509V3_set_ctx(
                        &raw mut ctx,
                        issuer.map_or(subject.as_ptr(), X509Cert::as_ptr),
                        subject.as_ptr(),
                        req.as_ptr(),
                        ptr::null_mut(),
                        0,
                    );
                    X509V3_set_nconf(&raw mut ctx, self.as_ptr());
                    X509V3_EXT_add_nconf(self.as_ptr(), &raw mut ctx, s.as_ptr(), subject.as_ptr())
                        == 1
                }
                ExtTarget::Req(req) => {
                    X509V3_set_ctx(
                        &raw mut ctx,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        req.as_ptr(),
                        ptr::null_mut(),
                        0,
                    );
                    X509V3_set_nconf(&raw mut ctx, self.as_ptr());
                    X509V3_EXT_REQ_add_nconf(self.as_ptr(), &raw mut ctx, s.as_ptr(), req.as_ptr())
                        == 1
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SPKI
// ---------------------------------------------------------------------------

#[allow(clippy::upper_case_acronyms, clashing_extern_declarations)]
extern "C" {
    fn NETSCAPE_SPKI_new() -> *mut NETSCAPE_SPKI;
    fn NETSCAPE_SPKI_free(s: *mut NETSCAPE_SPKI);
    fn NETSCAPE_SPKI_set_pubkey(s: *mut NETSCAPE_SPKI, k: *mut EVP_PKEY) -> c_int;
    fn NETSCAPE_SPKI_sign(s: *mut NETSCAPE_SPKI, k: *mut EVP_PKEY, md: *const EVP_MD) -> c_int;
    fn NETSCAPE_SPKI_verify(s: *mut NETSCAPE_SPKI, k: *mut EVP_PKEY) -> c_int;
    fn NETSCAPE_SPKI_b64_encode(s: *mut NETSCAPE_SPKI) -> *mut c_char;
    fn NETSCAPE_SPKI_b64_decode(s: *const c_char, len: c_int) -> *mut NETSCAPE_SPKI;
}

handle!(
    /// `NETSCAPE_SPKI`.
    Spki,
    NETSCAPE_SPKI,
    NETSCAPE_SPKI_free
);

impl Spki {
    /// `NETSCAPE_SPKI_new`.
    pub fn new() -> Option<Self> {
        // SAFETY: constructor.
        Self::from_raw(unsafe { NETSCAPE_SPKI_new() })
    }
    /// `NETSCAPE_SPKI_b64_decode`.
    pub fn decode(b64: &[u8]) -> Option<Self> {
        // SAFETY: buffer with explicit length.
        Self::from_raw(unsafe { NETSCAPE_SPKI_b64_decode(b64.as_ptr().cast(), c_len(b64.len())) })
    }
    /// Sets the challenge string.
    pub fn set_challenge(&self, c: &[u8]) -> bool {
        // SAFETY: spkac and challenge are always allocated by NETSCAPE_SPKI_new.
        unsafe {
            ASN1_STRING_set(
                (*(*self.as_ptr()).spkac).challenge,
                c.as_ptr().cast(),
                c_len(c.len()),
            ) == 1
        }
    }
    /// The challenge string.
    pub fn challenge(&self) -> Vec<u8> {
        // SAFETY: decoded SPKIs always carry spkac/challenge.
        unsafe { asn1_view((*(*self.as_ptr()).spkac).challenge).data }
    }
    /// `NETSCAPE_SPKI_set_pubkey`.
    pub fn set_pubkey(&self, k: &Pkey) -> bool {
        // SAFETY: valid objects.
        unsafe { NETSCAPE_SPKI_set_pubkey(self.as_ptr(), k.as_ptr()) == 1 }
    }
    /// `NETSCAPE_SPKI_sign`.
    pub fn sign(&self, k: &Pkey, md: &Md) -> bool {
        // SAFETY: valid objects.
        unsafe { NETSCAPE_SPKI_sign(self.as_ptr(), k.as_ptr(), md.as_ptr()) > 0 }
    }
    /// `NETSCAPE_SPKI_b64_encode`.
    pub fn encode(&self) -> Option<Vec<u8>> {
        // SAFETY: valid SPKI; result freed by take_cstr.
        take_cstr(unsafe { NETSCAPE_SPKI_b64_encode(self.as_ptr()) })
    }
    /// `X509_PUBKEY_get(spki->spkac->pubkey)`.
    pub fn public_key(&self) -> Option<Pkey> {
        // SAFETY: decoded SPKIs carry spkac; new key reference returned.
        Pkey::from_raw(unsafe { X509_PUBKEY_get((*(*self.as_ptr()).spkac).pubkey) })
    }
    /// `NETSCAPE_SPKI_verify`.
    pub fn verify(&self, k: &Pkey) -> i32 {
        // SAFETY: valid objects.
        unsafe { NETSCAPE_SPKI_verify(self.as_ptr(), k.as_ptr()) }
    }
}

// ---------------------------------------------------------------------------
// PKCS#12, PKCS#7, CMS
// ---------------------------------------------------------------------------

#[allow(clippy::upper_case_acronyms, clashing_extern_declarations)]
extern "C" {
    fn PKCS12_create(
        pass: *const c_char,
        name: *const c_char,
        pkey: *mut EVP_PKEY,
        cert: *mut X509,
        ca: *mut OPENSSL_STACK,
        nid_key: c_int,
        nid_cert: c_int,
        iter: c_int,
        mac_iter: c_int,
        keytype: c_int,
    ) -> *mut PKCS12;
    fn PKCS12_free(p: *mut PKCS12);
    fn i2d_PKCS12_bio(b: *mut BIO, p: *const PKCS12) -> c_int;
    fn d2i_PKCS12_bio(b: *mut BIO, p: *mut *mut PKCS12) -> *mut PKCS12;
    fn PKCS12_parse(
        p: *mut PKCS12,
        pass: *const c_char,
        pkey: *mut *mut EVP_PKEY,
        cert: *mut *mut X509,
        ca: *mut *mut OPENSSL_STACK,
    ) -> c_int;

    fn PKCS7_free(p: *mut PKCS7);
    fn SMIME_read_PKCS7(b: *mut BIO, bcont: *mut *mut BIO) -> *mut PKCS7;
    fn SMIME_write_PKCS7(b: *mut BIO, p: *mut PKCS7, data: *mut BIO, flags: c_int) -> c_int;
    fn PKCS7_verify(
        p: *mut PKCS7,
        certs: *mut OPENSSL_STACK,
        store: *mut X509_STORE,
        indata: *mut BIO,
        out: *mut BIO,
        flags: c_int,
    ) -> c_int;
    fn PKCS7_get0_signers(
        p: *mut PKCS7,
        certs: *mut OPENSSL_STACK,
        flags: c_int,
    ) -> *mut OPENSSL_STACK;
    fn PEM_write_bio_PKCS7(b: *mut BIO, p: *const PKCS7) -> c_int;
    fn PEM_read_bio_PKCS7(
        b: *mut BIO,
        x: *mut *mut PKCS7,
        cb: Option<PemPasswordCb>,
        u: *mut c_void,
    ) -> *mut PKCS7;
    fn PKCS7_encrypt(
        certs: *mut OPENSSL_STACK,
        inp: *mut BIO,
        cipher: *const EVP_CIPHER,
        flags: c_int,
    ) -> *mut PKCS7;
    fn PKCS7_sign(
        cert: *mut X509,
        pkey: *mut EVP_PKEY,
        certs: *mut OPENSSL_STACK,
        data: *mut BIO,
        flags: c_int,
    ) -> *mut PKCS7;
    fn PKCS7_decrypt(
        p: *mut PKCS7,
        pkey: *mut EVP_PKEY,
        cert: *mut X509,
        data: *mut BIO,
        flags: c_int,
    ) -> c_int;

    fn CMS_ContentInfo_free(c: *mut CMS_ContentInfo);
    fn PEM_read_bio_CMS(
        b: *mut BIO,
        x: *mut *mut CMS_ContentInfo,
        cb: Option<PemPasswordCb>,
        u: *mut c_void,
    ) -> *mut CMS_ContentInfo;
    fn PEM_write_bio_CMS(b: *mut BIO, c: *const CMS_ContentInfo) -> c_int;
    fn PEM_write_bio_CMS_stream(
        b: *mut BIO,
        c: *mut CMS_ContentInfo,
        inp: *mut BIO,
        flags: c_int,
    ) -> c_int;
    fn d2i_CMS_bio(b: *mut BIO, c: *mut *mut CMS_ContentInfo) -> *mut CMS_ContentInfo;
    fn i2d_CMS_bio(b: *mut BIO, c: *mut CMS_ContentInfo) -> c_int;
    fn SMIME_read_CMS(b: *mut BIO, bcont: *mut *mut BIO) -> *mut CMS_ContentInfo;
    fn SMIME_write_CMS(b: *mut BIO, c: *mut CMS_ContentInfo, data: *mut BIO, flags: c_int)
        -> c_int;
    fn CMS_verify(
        c: *mut CMS_ContentInfo,
        certs: *mut OPENSSL_STACK,
        store: *mut X509_STORE,
        dcont: *mut BIO,
        out: *mut BIO,
        flags: c_uint,
    ) -> c_int;
    fn CMS_get0_signers(c: *mut CMS_ContentInfo) -> *mut OPENSSL_STACK;
    fn CMS_encrypt(
        certs: *mut OPENSSL_STACK,
        inp: *mut BIO,
        cipher: *const EVP_CIPHER,
        flags: c_uint,
    ) -> *mut CMS_ContentInfo;
    fn CMS_sign(
        cert: *mut X509,
        pkey: *mut EVP_PKEY,
        certs: *mut OPENSSL_STACK,
        data: *mut BIO,
        flags: c_uint,
    ) -> *mut CMS_ContentInfo;
    fn CMS_final(c: *mut CMS_ContentInfo, data: *mut BIO, dcont: *mut BIO, flags: c_uint) -> c_int;
    fn CMS_decrypt(
        c: *mut CMS_ContentInfo,
        pkey: *mut EVP_PKEY,
        cert: *mut X509,
        dcont: *mut BIO,
        out: *mut BIO,
        flags: c_uint,
    ) -> c_int;
    fn CMS_get0_type(c: *const CMS_ContentInfo) -> *const ASN1_OBJECT;
    fn CMS_get1_certs(c: *mut CMS_ContentInfo) -> *mut OPENSSL_STACK;
    fn CMS_get1_crls(c: *mut CMS_ContentInfo) -> *mut OPENSSL_STACK;
}

/// `NID_pkcs7_signed`.
pub const NID_PKCS7_SIGNED: i32 = 22;
/// `NID_pkcs7_signedAndEnveloped`.
pub const NID_PKCS7_SIGNED_AND_ENVELOPED: i32 = 24;

/// Contents of a parsed PKCS#12 bundle.
#[derive(Debug)]
pub struct Pkcs12Parsed {
    /// The private key, if any.
    pub pkey: Option<Pkey>,
    /// The end-entity certificate, if any.
    pub cert: Option<X509Cert>,
    /// Extra certificates (raw stack; may be empty).
    pub ca: Option<X509Stack>,
}

/// `PKCS12_create` + `i2d_PKCS12_bio` into `out`. Returns
/// `Err(false)` when creation failed, `Err(true)` when writing failed.
pub fn pkcs12_create_write(
    out: &Bio<'_>,
    pass: &[u8],
    name: Option<&[u8]>,
    pkey: &Pkey,
    cert: &X509Cert,
    ca: Option<&X509Stack>,
) -> Result<(), bool> {
    let p = to_cstring(pass);
    let n = name.map(to_cstring);
    // SAFETY: valid objects; the PKCS12 is freed before returning.
    unsafe {
        let p12 = PKCS12_create(
            p.as_ptr(),
            n.as_ref().map_or(ptr::null(), |n| n.as_ptr()),
            pkey.as_ptr(),
            cert.as_ptr(),
            opt_stack(ca),
            0,
            0,
            0,
            0,
            0,
        );
        if p12.is_null() {
            return Err(false);
        }
        let ok = i2d_PKCS12_bio(out.as_ptr(), p12) != 0;
        PKCS12_free(p12);
        if ok {
            Ok(())
        } else {
            Err(true)
        }
    }
}

/// `d2i_PKCS12_bio` + `PKCS12_parse`.
pub fn pkcs12_parse(bio: &Bio<'_>, pass: &[u8]) -> Option<Pkcs12Parsed> {
    let p = to_cstring(pass);
    // SAFETY: valid BIO; out-pointers become owned handles.
    unsafe {
        let mut p12: *mut PKCS12 = ptr::null_mut();
        if d2i_PKCS12_bio(bio.as_ptr(), &raw mut p12).is_null() {
            return None;
        }
        let mut pkey = ptr::null_mut();
        let mut cert = ptr::null_mut();
        let mut ca: *mut OPENSSL_STACK = ptr::null_mut();
        let ok = PKCS12_parse(p12, p.as_ptr(), &raw mut pkey, &raw mut cert, &raw mut ca) == 1;
        PKCS12_free(p12);
        if !ok {
            return None;
        }
        Some(Pkcs12Parsed {
            pkey: Pkey::from_raw(pkey),
            cert: X509Cert::from_raw(cert),
            ca: NonNull::new(ca).map(X509Stack),
        })
    }
}

handle!(
    /// `PKCS7`.
    Pkcs7,
    PKCS7,
    PKCS7_free
);

/// Output of an S/MIME read: the structure and the detached content BIO.
#[derive(Debug)]
pub struct SmimeRead<T> {
    /// Parsed structure.
    pub obj: T,
    /// Content BIO for multipart/signed messages.
    pub content: Option<Bio<'static>>,
}

fn bio_ptr(b: Option<&Bio<'_>>) -> *mut BIO {
    b.map_or(ptr::null_mut(), Bio::as_ptr)
}

impl Pkcs7 {
    /// `SMIME_read_PKCS7`.
    pub fn smime_read(bio: &Bio<'_>) -> Option<SmimeRead<Self>> {
        let mut bcont: *mut BIO = ptr::null_mut();
        // SAFETY: valid BIO; returned objects owned by us.
        let p = unsafe { SMIME_read_PKCS7(bio.as_ptr(), &raw mut bcont) };
        let content = Bio::wrap(bcont);
        Self::from_raw(p).map(|obj| SmimeRead { obj, content })
    }
    /// `PEM_read_bio_PKCS7`.
    pub fn read_pem(bio: &Bio<'_>) -> Option<Self> {
        // SAFETY: valid BIO.
        Self::from_raw(unsafe {
            PEM_read_bio_PKCS7(bio.as_ptr(), ptr::null_mut(), None, ptr::null_mut())
        })
    }
    /// `PKCS7_verify`.
    pub fn verify(
        &self,
        certs: Option<&X509Stack>,
        store: &X509Store,
        indata: Option<&Bio<'_>>,
        out: Option<&Bio<'_>>,
        flags: i32,
    ) -> bool {
        // SAFETY: valid objects.
        unsafe {
            PKCS7_verify(
                self.as_ptr(),
                opt_stack(certs),
                store.as_ptr(),
                bio_ptr(indata),
                bio_ptr(out),
                flags,
            ) == 1
        }
    }
    /// PEMs of `PKCS7_get0_signers`; `None` if the call failed. The second
    /// element tells whether each write succeeded.
    pub fn signer_pems(
        &self,
        certs: Option<&X509Stack>,
        flags: i32,
    ) -> Option<Vec<Option<Vec<u8>>>> {
        // SAFETY: the returned stack is freed (not its elements).
        unsafe {
            let sk = PKCS7_get0_signers(self.as_ptr(), opt_stack(certs), flags);
            if sk.is_null() {
                return None;
            }
            let v = signer_list(sk);
            OPENSSL_sk_free(sk);
            Some(v)
        }
    }
    /// `PEM_write_bio_PKCS7`.
    pub fn write_pem(&self, bio: &Bio<'_>) -> bool {
        // SAFETY: valid objects.
        unsafe { PEM_write_bio_PKCS7(bio.as_ptr(), self.as_ptr()) == 1 }
    }
    /// `PKCS7_encrypt`.
    pub fn encrypt(
        certs: &X509Stack,
        input: &Bio<'_>,
        cipher: &Cipher,
        flags: i32,
    ) -> Option<Self> {
        // SAFETY: valid objects.
        Self::from_raw(unsafe {
            PKCS7_encrypt(certs.as_ptr(), input.as_ptr(), cipher.as_ptr(), flags)
        })
    }
    /// `PKCS7_sign`.
    pub fn sign(
        cert: &X509Cert,
        key: &Pkey,
        others: Option<&X509Stack>,
        data: &Bio<'_>,
        flags: i32,
    ) -> Option<Self> {
        // SAFETY: valid objects.
        Self::from_raw(unsafe {
            PKCS7_sign(
                cert.as_ptr(),
                key.as_ptr(),
                opt_stack(others),
                data.as_ptr(),
                flags,
            )
        })
    }
    /// `SMIME_write_PKCS7`.
    pub fn smime_write(&self, out: &Bio<'_>, data: &Bio<'_>, flags: i32) -> bool {
        // SAFETY: valid objects.
        unsafe { SMIME_write_PKCS7(out.as_ptr(), self.as_ptr(), data.as_ptr(), flags) == 1 }
    }
    /// `PKCS7_decrypt`.
    pub fn decrypt(&self, key: &Pkey, cert: &X509Cert, out: &Bio<'_>, flags: i32) -> bool {
        // SAFETY: valid objects.
        unsafe {
            PKCS7_decrypt(
                self.as_ptr(),
                key.as_ptr(),
                cert.as_ptr(),
                out.as_ptr(),
                flags,
            ) == 1
        }
    }
    /// Certificates and CRLs (as PEM) of signed / signedAndEnveloped data.
    pub fn certs_and_crls(&self) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        // SAFETY: reads public struct fields of a valid PKCS7; the union
        // member is only dereferenced for the two signed types, whose
        // layouts share the leading fields.
        unsafe {
            let head = &*self.as_ptr().cast::<PKCS7_HEAD>();
            let nid = OBJ_obj2nid(head.type_);
            if (nid == NID_PKCS7_SIGNED || nid == NID_PKCS7_SIGNED_AND_ENVELOPED)
                && !head.d.is_null()
            {
                let d = &*head.d;
                (stack_x509_pems(d.cert), stack_crl_pems(d.crl))
            } else {
                (Vec::new(), Vec::new())
            }
        }
    }
}

/// # Safety
/// `sk` must be a valid `STACK_OF(X509)`.
unsafe fn signer_list(sk: *mut OPENSSL_STACK) -> Vec<Option<Vec<u8>>> {
    let mut v = Vec::new();
    let Some(bio) = Bio::mem() else { return v };
    // SAFETY: per contract.
    unsafe {
        for i in 0..OPENSSL_sk_num(sk) {
            let x = OPENSSL_sk_value(sk, i).cast::<X509>();
            v.push((PEM_write_bio_X509(bio.as_ptr(), x) == 1).then(|| bio.contents()));
            bio.reset();
        }
    }
    v
}

handle!(
    /// `CMS_ContentInfo`.
    Cms,
    CMS_ContentInfo,
    CMS_ContentInfo_free
);

impl Cms {
    /// `PEM_read_bio_CMS`.
    pub fn read_pem(bio: &Bio<'_>) -> Option<Self> {
        // SAFETY: valid BIO.
        Self::from_raw(unsafe {
            PEM_read_bio_CMS(bio.as_ptr(), ptr::null_mut(), None, ptr::null_mut())
        })
    }
    /// `d2i_CMS_bio`.
    pub fn read_der(bio: &Bio<'_>) -> Option<Self> {
        // SAFETY: valid BIO.
        Self::from_raw(unsafe { d2i_CMS_bio(bio.as_ptr(), ptr::null_mut()) })
    }
    /// `SMIME_read_CMS`.
    pub fn smime_read(bio: &Bio<'_>) -> Option<SmimeRead<Self>> {
        let mut bcont: *mut BIO = ptr::null_mut();
        // SAFETY: valid BIO.
        let p = unsafe { SMIME_read_CMS(bio.as_ptr(), &raw mut bcont) };
        let content = Bio::wrap(bcont);
        Self::from_raw(p).map(|obj| SmimeRead { obj, content })
    }
    /// `CMS_verify`.
    pub fn verify(
        &self,
        certs: Option<&X509Stack>,
        store: &X509Store,
        dcont: Option<&Bio<'_>>,
        out: Option<&Bio<'_>>,
        flags: u32,
    ) -> bool {
        // SAFETY: valid objects.
        unsafe {
            CMS_verify(
                self.as_ptr(),
                opt_stack(certs),
                store.as_ptr(),
                bio_ptr(dcont),
                bio_ptr(out),
                flags,
            ) == 1
        }
    }
    /// PEMs of `CMS_get0_signers`.
    pub fn signer_pems(&self) -> Option<Vec<Option<Vec<u8>>>> {
        // SAFETY: returned stack freed (elements owned by the CMS).
        unsafe {
            let sk = CMS_get0_signers(self.as_ptr());
            if sk.is_null() {
                return None;
            }
            let v = signer_list(sk);
            OPENSSL_sk_free(sk);
            Some(v)
        }
    }
    /// `PEM_write_bio_CMS`.
    pub fn write_pem(&self, bio: &Bio<'_>) -> bool {
        // SAFETY: valid objects.
        unsafe { PEM_write_bio_CMS(bio.as_ptr(), self.as_ptr()) == 1 }
    }
    /// `PEM_write_bio_CMS_stream`.
    pub fn write_pem_stream(&self, bio: &Bio<'_>, input: &Bio<'_>, flags: i32) -> bool {
        // SAFETY: valid objects.
        unsafe { PEM_write_bio_CMS_stream(bio.as_ptr(), self.as_ptr(), input.as_ptr(), flags) == 1 }
    }
    /// `i2d_CMS_bio`.
    pub fn write_der(&self, bio: &Bio<'_>) -> bool {
        // SAFETY: valid objects.
        unsafe { i2d_CMS_bio(bio.as_ptr(), self.as_ptr()) == 1 }
    }
    /// `SMIME_write_CMS`.
    pub fn smime_write(&self, out: &Bio<'_>, data: &Bio<'_>, flags: i32) -> bool {
        // SAFETY: valid objects.
        unsafe { SMIME_write_CMS(out.as_ptr(), self.as_ptr(), data.as_ptr(), flags) == 1 }
    }
    /// `CMS_encrypt`.
    pub fn encrypt(
        certs: &X509Stack,
        input: &Bio<'_>,
        cipher: &Cipher,
        flags: u32,
    ) -> Option<Self> {
        // SAFETY: valid objects.
        Self::from_raw(unsafe {
            CMS_encrypt(certs.as_ptr(), input.as_ptr(), cipher.as_ptr(), flags)
        })
    }
    /// `CMS_sign`.
    pub fn sign(
        cert: &X509Cert,
        key: &Pkey,
        others: Option<&X509Stack>,
        data: &Bio<'_>,
        flags: u32,
    ) -> Option<Self> {
        // SAFETY: valid objects.
        Self::from_raw(unsafe {
            CMS_sign(
                cert.as_ptr(),
                key.as_ptr(),
                opt_stack(others),
                data.as_ptr(),
                flags,
            )
        })
    }
    /// `CMS_final(cms, data, NULL, flags)`.
    pub fn finalize(&self, data: &Bio<'_>, flags: u32) -> bool {
        // SAFETY: valid objects.
        unsafe { CMS_final(self.as_ptr(), data.as_ptr(), ptr::null_mut(), flags) == 1 }
    }
    /// `CMS_decrypt(cms, key, cert, NULL, out, 0)`.
    pub fn decrypt(&self, key: &Pkey, cert: &X509Cert, out: &Bio<'_>) -> bool {
        // SAFETY: valid objects.
        unsafe {
            CMS_decrypt(
                self.as_ptr(),
                key.as_ptr(),
                cert.as_ptr(),
                ptr::null_mut(),
                out.as_ptr(),
                0,
            ) == 1
        }
    }
    /// Certificates and CRLs (PEM) when the content type is signed.
    pub fn certs_and_crls(&self) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
        // SAFETY: get1 stacks are owned by us and pop-freed.
        unsafe {
            let nid = OBJ_obj2nid(CMS_get0_type(self.as_ptr()));
            if nid != NID_PKCS7_SIGNED && nid != NID_PKCS7_SIGNED_AND_ENVELOPED {
                return (Vec::new(), Vec::new());
            }
            let certs = CMS_get1_certs(self.as_ptr());
            let crls = CMS_get1_crls(self.as_ptr());
            let out = (stack_x509_pems(certs), stack_crl_pems(crls));
            if !certs.is_null() {
                OPENSSL_sk_pop_free(certs, free_x509);
            }
            if !crls.is_null() {
                OPENSSL_sk_pop_free(crls, free_crl);
            }
            out
        }
    }
}

/// `X509_V_ERR_CERT_CHAIN_TOO_LONG` as the `openssl` crate's verify result.
pub fn verify_result_chain_too_long() -> openssl::x509::X509VerifyResult {
    // SAFETY: 22 is the documented X509_V_ERR_CERT_CHAIN_TOO_LONG code.
    unsafe { openssl::x509::X509VerifyResult::from_raw(22) }
}
