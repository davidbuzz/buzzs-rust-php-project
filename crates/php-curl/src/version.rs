//! `curl_version_info()`.

use crate::consts::{
    CURL_VERSION_ALTSVC, CURL_VERSION_ASYNCHDNS, CURL_VERSION_BROTLI, CURL_VERSION_CONV, CURL_VERSION_DEBUG,
    CURL_VERSION_GSASL, CURL_VERSION_GSSAPI, CURL_VERSION_GSSNEGOTIATE, CURL_VERSION_HSTS, CURL_VERSION_HTTP2,
    CURL_VERSION_HTTP3, CURL_VERSION_HTTPS_PROXY, CURL_VERSION_IDN, CURL_VERSION_IPV6, CURL_VERSION_KERBEROS4,
    CURL_VERSION_KERBEROS5, CURL_VERSION_LARGEFILE, CURL_VERSION_LIBZ, CURL_VERSION_MULTI_SSL, CURL_VERSION_NTLM,
    CURL_VERSION_NTLM_WB, CURL_VERSION_PSL, CURL_VERSION_SPNEGO, CURL_VERSION_SSL, CURL_VERSION_SSPI,
    CURL_VERSION_TLSAUTH_SRP, CURL_VERSION_UNICODE, CURL_VERSION_UNIX_SOCKETS, CURL_VERSION_ZSTD,
};
use crate::ffi;
use std::ffi::CStr;
use std::os::raw::c_char;

/// `php_curl_features`: the `feature_list` entries of `curl_version()`.
pub static FEATURES: &[(&str, i64)] = &[
    ("AsynchDNS", CURL_VERSION_ASYNCHDNS),
    ("CharConv", CURL_VERSION_CONV),
    ("Debug", CURL_VERSION_DEBUG),
    ("GSS-Negotiate", CURL_VERSION_GSSNEGOTIATE),
    ("IDN", CURL_VERSION_IDN),
    ("IPv6", CURL_VERSION_IPV6),
    ("krb4", CURL_VERSION_KERBEROS4),
    ("Largefile", CURL_VERSION_LARGEFILE),
    ("libz", CURL_VERSION_LIBZ),
    ("NTLM", CURL_VERSION_NTLM),
    ("NTLMWB", CURL_VERSION_NTLM_WB),
    ("SPNEGO", CURL_VERSION_SPNEGO),
    ("SSL", CURL_VERSION_SSL),
    ("SSPI", CURL_VERSION_SSPI),
    ("TLS-SRP", CURL_VERSION_TLSAUTH_SRP),
    ("HTTP2", CURL_VERSION_HTTP2),
    ("GSSAPI", CURL_VERSION_GSSAPI),
    ("KERBEROS5", CURL_VERSION_KERBEROS5),
    ("UNIX_SOCKETS", CURL_VERSION_UNIX_SOCKETS),
    ("PSL", CURL_VERSION_PSL),
    ("HTTPS_PROXY", CURL_VERSION_HTTPS_PROXY),
    ("MULTI_SSL", CURL_VERSION_MULTI_SSL),
    ("BROTLI", CURL_VERSION_BROTLI),
    ("ALTSVC", CURL_VERSION_ALTSVC),
    ("HTTP3", CURL_VERSION_HTTP3),
    ("UNICODE", CURL_VERSION_UNICODE),
    ("ZSTD", CURL_VERSION_ZSTD),
    ("HSTS", CURL_VERSION_HSTS),
    ("GSASL", CURL_VERSION_GSASL),
];

/// The fields of `curl_version_info_data` ext/curl reports (NULL strings
/// become empty).
#[derive(Debug, Clone, Default)]
pub struct VersionInfo {
    pub age: i32,
    pub version: Vec<u8>,
    pub version_num: u32,
    pub host: Vec<u8>,
    pub features: i32,
    pub ssl_version: Vec<u8>,
    pub ssl_version_num: i64,
    pub libz_version: Vec<u8>,
    pub protocols: Vec<Vec<u8>>,
    pub ares: Vec<u8>,
    pub ares_num: i32,
    pub libidn: Vec<u8>,
    pub iconv_ver_num: i32,
    pub libssh_version: Vec<u8>,
    pub brotli_ver_num: u32,
    pub brotli_version: Vec<u8>,
}

fn s(p: *const c_char) -> Vec<u8> {
    if p.is_null() {
        Vec::new()
    } else {
        // SAFETY: libcurl's version strings are static and NUL-terminated.
        unsafe { CStr::from_ptr(p) }.to_bytes().to_vec()
    }
}

/// `curl_version_info(CURLVERSION_NOW)`.
pub fn version_info() -> Option<VersionInfo> {
    // SAFETY: returns a pointer to static data (or NULL).
    let p = unsafe { ffi::curl_version_info(ffi::CURLVERSION_NOW) };
    if p.is_null() {
        return None;
    }
    // SAFETY: valid static struct of at least the requested age.
    let d = unsafe { &*p };
    let mut protocols = Vec::new();
    let mut pp = d.protocols;
    // SAFETY: the protocol list is NULL-terminated.
    unsafe {
        while !pp.is_null() && !(*pp).is_null() {
            protocols.push(s(*pp));
            pp = pp.add(1);
        }
    }
    Some(VersionInfo {
        age: d.age,
        version: s(d.version),
        version_num: d.version_num,
        host: s(d.host),
        features: d.features,
        ssl_version: s(d.ssl_version),
        ssl_version_num: d.ssl_version_num,
        libz_version: s(d.libz_version),
        protocols,
        ares: s(d.ares),
        ares_num: d.ares_num,
        libidn: s(d.libidn),
        iconv_ver_num: d.iconv_ver_num,
        libssh_version: s(d.libssh_version),
        brotli_ver_num: d.brotli_ver_num,
        brotli_version: s(d.brotli_version),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version() {
        let v = version_info().expect("info");
        assert!(v.age >= 4);
        assert!(v.version.starts_with(b"8."));
        assert!(v.protocols.iter().any(|p| p == b"http"));
    }
}
