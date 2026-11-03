//! Classification of `CURLOPT_*` options by how ext/curl's
//! `_php_curl_setopt()` coerces and applies the PHP value.

#[allow(clippy::wildcard_imports)]
use crate::consts::*;

/// The user callbacks ext/curl can install.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Callback {
    /// `CURLOPT_WRITEFUNCTION`.
    Write,
    /// `CURLOPT_HEADERFUNCTION`.
    Header,
    /// `CURLOPT_READFUNCTION`.
    Read,
    /// `CURLOPT_SEEKFUNCTION`.
    Seek,
    /// `CURLOPT_PROGRESSFUNCTION`.
    Progress,
    /// `CURLOPT_XFERINFOFUNCTION`.
    XferInfo,
    /// `CURLOPT_FNMATCH_FUNCTION`.
    Fnmatch,
    /// `CURLOPT_DEBUGFUNCTION`.
    Debug,
    /// `CURLOPT_PREREQFUNCTION`.
    Prereq,
    /// `CURLOPT_SSH_HOSTKEYFUNCTION`.
    SshHostKey,
}

impl Callback {
    /// The option name as used in error messages (`CURLOPT_WRITEFUNCTION`).
    pub fn option_name(self) -> &'static str {
        match self {
            Callback::Write => "CURLOPT_WRITEFUNCTION",
            Callback::Header => "CURLOPT_HEADERFUNCTION",
            Callback::Read => "CURLOPT_READFUNCTION",
            Callback::Seek => "CURLOPT_SEEKFUNCTION",
            Callback::Progress => "CURLOPT_PROGRESSFUNCTION",
            Callback::XferInfo => "CURLOPT_XFERINFOFUNCTION",
            Callback::Fnmatch => "CURLOPT_FNMATCH_FUNCTION",
            Callback::Debug => "CURLOPT_DEBUGFUNCTION",
            Callback::Prereq => "CURLOPT_PREREQFUNCTION",
            Callback::SshHostKey => "CURLOPT_SSH_HOSTKEYFUNCTION",
        }
    }

    /// The `*FUNCTION` option id.
    pub fn function_option(self) -> i64 {
        match self {
            Callback::Write => CURLOPT_WRITEFUNCTION,
            Callback::Header => CURLOPT_HEADERFUNCTION,
            Callback::Read => CURLOPT_READFUNCTION,
            Callback::Seek => CURLOPT_SEEKFUNCTION,
            Callback::Progress => CURLOPT_PROGRESSFUNCTION,
            Callback::XferInfo => CURLOPT_XFERINFOFUNCTION,
            Callback::Fnmatch => CURLOPT_FNMATCH_FUNCTION,
            Callback::Debug => CURLOPT_DEBUGFUNCTION,
            Callback::Prereq => CURLOPT_PREREQFUNCTION,
            Callback::SshHostKey => CURLOPT_SSH_HOSTKEYFUNCTION,
        }
    }

    /// The `*DATA` option id paired with the function.
    pub fn data_option(self) -> i64 {
        match self {
            Callback::Write => CURLOPT_WRITEDATA,
            Callback::Header => CURLOPT_HEADERDATA,
            Callback::Read => CURLOPT_READDATA,
            Callback::Seek => CURLOPT_SEEKDATA,
            Callback::Progress => CURLOPT_PROGRESSDATA,
            Callback::XferInfo => CURLOPT_XFERINFODATA,
            Callback::Fnmatch => CURLOPT_FNMATCH_DATA,
            Callback::Debug => CURLOPT_DEBUGDATA,
            Callback::Prereq => CURLOPT_PREREQDATA,
            Callback::SshHostKey => CURLOPT_SSH_HOSTKEYDATA,
        }
    }

    /// All callbacks.
    pub const ALL: [Callback; 10] = [
        Callback::Write,
        Callback::Header,
        Callback::Read,
        Callback::Seek,
        Callback::Progress,
        Callback::XferInfo,
        Callback::Fnmatch,
        Callback::Debug,
        Callback::Prereq,
        Callback::SshHostKey,
    ];
}

/// How `curl_setopt()` treats an option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptKind {
    /// A user callback (`callable|null`).
    Callable(Callback),
    /// `zval_get_long()` then `curl_easy_setopt(long)`.
    Long,
    /// `CURLOPT_SSL_VERIFYHOST` (1 is mapped to 2 with a notice).
    SslVerifyHost,
    /// `CURLOPT_SAFE_UPLOAD` (only `true` accepted).
    SafeUpload,
    /// String option (`zval_get_string()`, NUL bytes rejected).
    Str,
    /// String option where `null` resets to libcurl's default.
    NullableStr,
    /// `CURLOPT_PRIVATE` (any value kept on the PHP side).
    Private,
    /// `CURLOPT_URL`.
    Url,
    /// Stream resource options (`CURLOPT_FILE`, `INFILE`, `STDERR`,
    /// `WRITEHEADER`).
    File,
    /// Array of strings turned into a `curl_slist`.
    Slist,
    /// Accepted and ignored (`CURLOPT_BINARYTRANSFER`,
    /// `CURLOPT_DNS_USE_GLOBAL_CACHE`).
    Ignored,
    /// `CURLOPT_POSTFIELDS` (string or array → MIME).
    PostFields,
    /// `CURLOPT_RETURNTRANSFER`.
    ReturnTransfer,
    /// `curl_off_t` option.
    OffT,
    /// `CURLOPT_POSTREDIR` (masked with `CURL_REDIR_POST_ALL`).
    PostRedir,
    /// String naming a file (subject to `open_basedir`).
    FileStr,
    /// `CURLINFO_HEADER_OUT` used as an option.
    HeaderOut,
    /// `CURLOPT_SHARE`.
    Share,
    /// `struct curl_blob` option.
    Blob,
}

/// The kind of option `opt`, `None` for "not a valid cURL option".
#[allow(clippy::too_many_lines)]
pub fn option_kind(opt: i64) -> Option<OptKind> {
    use OptKind as K;
    Some(match opt {
        CURLOPT_WRITEFUNCTION => K::Callable(Callback::Write),
        CURLOPT_HEADERFUNCTION => K::Callable(Callback::Header),
        CURLOPT_READFUNCTION => K::Callable(Callback::Read),
        CURLOPT_SEEKFUNCTION => K::Callable(Callback::Seek),
        CURLOPT_PROGRESSFUNCTION => K::Callable(Callback::Progress),
        CURLOPT_XFERINFOFUNCTION => K::Callable(Callback::XferInfo),
        CURLOPT_FNMATCH_FUNCTION => K::Callable(Callback::Fnmatch),
        CURLOPT_DEBUGFUNCTION => K::Callable(Callback::Debug),
        CURLOPT_PREREQFUNCTION => K::Callable(Callback::Prereq),
        CURLOPT_SSH_HOSTKEYFUNCTION => K::Callable(Callback::SshHostKey),

        CURLOPT_SSL_VERIFYHOST => K::SslVerifyHost,
        CURLOPT_AUTOREFERER
        | CURLOPT_BUFFERSIZE
        | CURLOPT_CONNECTTIMEOUT
        | CURLOPT_COOKIESESSION
        | CURLOPT_CRLF
        | CURLOPT_DNS_CACHE_TIMEOUT
        | CURLOPT_FAILONERROR
        | CURLOPT_FILETIME
        | CURLOPT_FORBID_REUSE
        | CURLOPT_FRESH_CONNECT
        | CURLOPT_FTP_USE_EPRT
        | CURLOPT_FTP_USE_EPSV
        | CURLOPT_HEADER
        | CURLOPT_HTTPGET
        | CURLOPT_HTTPPROXYTUNNEL
        | CURLOPT_HTTP_VERSION
        | CURLOPT_INFILESIZE
        | CURLOPT_LOW_SPEED_LIMIT
        | CURLOPT_LOW_SPEED_TIME
        | CURLOPT_MAXCONNECTS
        | CURLOPT_MAXREDIRS
        | CURLOPT_NETRC
        | CURLOPT_NOBODY
        | CURLOPT_NOPROGRESS
        | CURLOPT_NOSIGNAL
        | CURLOPT_PORT
        | CURLOPT_POST
        | CURLOPT_PROXYPORT
        | CURLOPT_PROXYTYPE
        | CURLOPT_PUT
        | CURLOPT_RESUME_FROM
        | CURLOPT_SSLVERSION
        | CURLOPT_SSL_VERIFYPEER
        | CURLOPT_TIMECONDITION
        | CURLOPT_TIMEOUT
        | CURLOPT_TIMEVALUE
        | CURLOPT_TRANSFERTEXT
        | CURLOPT_UNRESTRICTED_AUTH
        | CURLOPT_UPLOAD
        | CURLOPT_VERBOSE
        | CURLOPT_HTTPAUTH
        | CURLOPT_FTP_CREATE_MISSING_DIRS
        | CURLOPT_PROXYAUTH
        | CURLOPT_SERVER_RESPONSE_TIMEOUT
        | CURLOPT_IPRESOLVE
        | CURLOPT_MAXFILESIZE
        | CURLOPT_TCP_NODELAY
        | CURLOPT_FTPSSLAUTH
        | CURLOPT_IGNORE_CONTENT_LENGTH
        | CURLOPT_FTP_SKIP_PASV_IP
        | CURLOPT_FTP_FILEMETHOD
        | CURLOPT_CONNECT_ONLY
        | CURLOPT_LOCALPORT
        | CURLOPT_LOCALPORTRANGE
        | CURLOPT_SSL_SESSIONID_CACHE
        | CURLOPT_FTP_SSL_CCC
        | CURLOPT_SSH_AUTH_TYPES
        | CURLOPT_CONNECTTIMEOUT_MS
        | CURLOPT_HTTP_CONTENT_DECODING
        | CURLOPT_HTTP_TRANSFER_DECODING
        | CURLOPT_TIMEOUT_MS
        | CURLOPT_NEW_DIRECTORY_PERMS
        | CURLOPT_NEW_FILE_PERMS
        | CURLOPT_USE_SSL
        | CURLOPT_APPEND
        | CURLOPT_DIRLISTONLY
        | CURLOPT_PROXY_TRANSFER_MODE
        | CURLOPT_ADDRESS_SCOPE
        | CURLOPT_CERTINFO
        | CURLOPT_PROTOCOLS
        | CURLOPT_REDIR_PROTOCOLS
        | CURLOPT_SOCKS5_GSSAPI_NEC
        | CURLOPT_TFTP_BLKSIZE
        | CURLOPT_FTP_USE_PRET
        | CURLOPT_RTSP_CLIENT_CSEQ
        | CURLOPT_RTSP_REQUEST
        | CURLOPT_RTSP_SERVER_CSEQ
        | CURLOPT_WILDCARDMATCH
        | CURLOPT_GSSAPI_DELEGATION
        | CURLOPT_ACCEPTTIMEOUT_MS
        | CURLOPT_SSL_OPTIONS
        | CURLOPT_TCP_KEEPALIVE
        | CURLOPT_TCP_KEEPIDLE
        | CURLOPT_TCP_KEEPINTVL
        | CURLOPT_SASL_IR
        | CURLOPT_EXPECT_100_TIMEOUT_MS
        | CURLOPT_SSL_ENABLE_ALPN
        | CURLOPT_SSL_ENABLE_NPN
        | CURLOPT_HEADEROPT
        | CURLOPT_SSL_VERIFYSTATUS
        | CURLOPT_PATH_AS_IS
        | CURLOPT_SSL_FALSESTART
        | CURLOPT_PIPEWAIT
        | CURLOPT_STREAM_WEIGHT
        | CURLOPT_TFTP_NO_OPTIONS
        | CURLOPT_TCP_FASTOPEN
        | CURLOPT_KEEP_SENDING_ON_ERROR
        | CURLOPT_PROXY_SSL_OPTIONS
        | CURLOPT_PROXY_SSL_VERIFYHOST
        | CURLOPT_PROXY_SSL_VERIFYPEER
        | CURLOPT_PROXY_SSLVERSION
        | CURLOPT_SUPPRESS_CONNECT_HEADERS
        | CURLOPT_SOCKS5_AUTH
        | CURLOPT_SSH_COMPRESSION
        | CURLOPT_HAPPY_EYEBALLS_TIMEOUT_MS
        | CURLOPT_DNS_SHUFFLE_ADDRESSES
        | CURLOPT_HAPROXYPROTOCOL
        | CURLOPT_DISALLOW_USERNAME_IN_URL
        | CURLOPT_UPKEEP_INTERVAL_MS
        | CURLOPT_UPLOAD_BUFFERSIZE
        | CURLOPT_HTTP09_ALLOWED
        | CURLOPT_ALTSVC_CTRL
        | CURLOPT_MAXAGE_CONN
        | CURLOPT_MAIL_RCPT_ALLLOWFAILS
        | CURLOPT_HSTS_CTRL
        | CURLOPT_DOH_SSL_VERIFYHOST
        | CURLOPT_DOH_SSL_VERIFYPEER
        | CURLOPT_DOH_SSL_VERIFYSTATUS
        | CURLOPT_MAXLIFETIME_CONN
        | CURLOPT_MIME_OPTIONS
        | CURLOPT_WS_OPTIONS
        | CURLOPT_CA_CACHE_TIMEOUT
        | CURLOPT_QUICK_EXIT
        | CURLOPT_FOLLOWLOCATION => K::Long,
        CURLOPT_SAFE_UPLOAD => K::SafeUpload,

        CURLOPT_CAINFO
        | CURLOPT_CAPATH
        | CURLOPT_COOKIE
        | CURLOPT_EGDSOCKET
        | CURLOPT_INTERFACE
        | CURLOPT_PROXY
        | CURLOPT_PROXYUSERPWD
        | CURLOPT_REFERER
        | CURLOPT_SSLCERTTYPE
        | CURLOPT_SSLENGINE
        | CURLOPT_SSLENGINE_DEFAULT
        | CURLOPT_SSLKEY
        | CURLOPT_SSLKEYPASSWD
        | CURLOPT_SSLKEYTYPE
        | CURLOPT_SSL_CIPHER_LIST
        | CURLOPT_USERAGENT
        | CURLOPT_COOKIELIST
        | CURLOPT_FTP_ALTERNATIVE_TO_USER
        | CURLOPT_SSH_HOST_PUBLIC_KEY_MD5
        | CURLOPT_PROXYPASSWORD
        | CURLOPT_PROXYUSERNAME
        | CURLOPT_NOPROXY
        | CURLOPT_SOCKS5_GSSAPI_SERVICE
        | CURLOPT_MAIL_FROM
        | CURLOPT_RTSP_STREAM_URI
        | CURLOPT_RTSP_TRANSPORT
        | CURLOPT_TLSAUTH_TYPE
        | CURLOPT_TLSAUTH_PASSWORD
        | CURLOPT_TLSAUTH_USERNAME
        | CURLOPT_TRANSFER_ENCODING
        | CURLOPT_DNS_SERVERS
        | CURLOPT_MAIL_AUTH
        | CURLOPT_LOGIN_OPTIONS
        | CURLOPT_PINNEDPUBLICKEY
        | CURLOPT_PROXY_SERVICE_NAME
        | CURLOPT_SERVICE_NAME
        | CURLOPT_DEFAULT_PROTOCOL
        | CURLOPT_PRE_PROXY
        | CURLOPT_PROXY_CAINFO
        | CURLOPT_PROXY_CAPATH
        | CURLOPT_PROXY_CRLFILE
        | CURLOPT_PROXY_KEYPASSWD
        | CURLOPT_PROXY_PINNEDPUBLICKEY
        | CURLOPT_PROXY_SSL_CIPHER_LIST
        | CURLOPT_PROXY_SSLCERT
        | CURLOPT_PROXY_SSLCERTTYPE
        | CURLOPT_PROXY_SSLKEY
        | CURLOPT_PROXY_SSLKEYTYPE
        | CURLOPT_PROXY_TLSAUTH_PASSWORD
        | CURLOPT_PROXY_TLSAUTH_TYPE
        | CURLOPT_PROXY_TLSAUTH_USERNAME
        | CURLOPT_ABSTRACT_UNIX_SOCKET
        | CURLOPT_REQUEST_TARGET
        | CURLOPT_PROXY_TLS13_CIPHERS
        | CURLOPT_TLS13_CIPHERS
        | CURLOPT_ALTSVC
        | CURLOPT_SASL_AUTHZID
        | CURLOPT_PROXY_ISSUERCERT
        | CURLOPT_SSL_EC_CURVES
        | CURLOPT_AWS_SIGV4
        | CURLOPT_SSH_HOST_PUBLIC_KEY_SHA256
        | CURLOPT_PROTOCOLS_STR
        | CURLOPT_REDIR_PROTOCOLS_STR => K::Str,

        CURLOPT_CUSTOMREQUEST
        | CURLOPT_FTPPORT
        | CURLOPT_RANGE
        | CURLOPT_FTP_ACCOUNT
        | CURLOPT_RTSP_SESSION_ID
        | CURLOPT_ACCEPT_ENCODING
        | CURLOPT_DNS_INTERFACE
        | CURLOPT_DNS_LOCAL_IP4
        | CURLOPT_DNS_LOCAL_IP6
        | CURLOPT_XOAUTH2_BEARER
        | CURLOPT_UNIX_SOCKET_PATH
        | CURLOPT_DOH_URL
        | CURLOPT_HSTS
        | CURLOPT_KRBLEVEL
        | CURLOPT_USERPWD
        | CURLOPT_USERNAME
        | CURLOPT_PASSWORD => K::NullableStr,

        CURLOPT_PRIVATE => K::Private,
        CURLOPT_URL => K::Url,
        CURLOPT_FILE | CURLOPT_INFILE | CURLOPT_STDERR | CURLOPT_WRITEHEADER => K::File,

        CURLOPT_HTTP200ALIASES
        | CURLOPT_HTTPHEADER
        | CURLOPT_POSTQUOTE
        | CURLOPT_PREQUOTE
        | CURLOPT_QUOTE
        | CURLOPT_TELNETOPTIONS
        | CURLOPT_MAIL_RCPT
        | CURLOPT_RESOLVE
        | CURLOPT_PROXYHEADER
        | CURLOPT_CONNECT_TO => K::Slist,

        CURLOPT_BINARYTRANSFER | CURLOPT_DNS_USE_GLOBAL_CACHE => K::Ignored,
        CURLOPT_POSTFIELDS => K::PostFields,
        CURLOPT_RETURNTRANSFER => K::ReturnTransfer,

        CURLOPT_INFILESIZE_LARGE
        | CURLOPT_MAX_RECV_SPEED_LARGE
        | CURLOPT_MAX_SEND_SPEED_LARGE
        | CURLOPT_MAXFILESIZE_LARGE
        | CURLOPT_TIMEVALUE_LARGE => K::OffT,
        CURLOPT_POSTREDIR => K::PostRedir,

        CURLOPT_COOKIEFILE
        | CURLOPT_COOKIEJAR
        | CURLOPT_RANDOM_FILE
        | CURLOPT_SSLCERT
        | CURLOPT_NETRC_FILE
        | CURLOPT_SSH_PRIVATE_KEYFILE
        | CURLOPT_SSH_PUBLIC_KEYFILE
        | CURLOPT_CRLFILE
        | CURLOPT_ISSUERCERT
        | CURLOPT_SSH_KNOWNHOSTS => K::FileStr,

        CURLINFO_HEADER_OUT => K::HeaderOut,
        CURLOPT_SHARE => K::Share,

        CURLOPT_ISSUERCERT_BLOB
        | CURLOPT_PROXY_ISSUERCERT_BLOB
        | CURLOPT_PROXY_SSLCERT_BLOB
        | CURLOPT_PROXY_SSLKEY_BLOB
        | CURLOPT_SSLCERT_BLOB
        | CURLOPT_SSLKEY_BLOB
        | CURLOPT_CAINFO_BLOB
        | CURLOPT_PROXY_CAINFO_BLOB => K::Blob,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(option_kind(CURLOPT_URL), Some(OptKind::Url));
        assert_eq!(option_kind(CURLOPT_HTTPHEADER), Some(OptKind::Slist));
        assert_eq!(
            option_kind(CURLOPT_RETURNTRANSFER),
            Some(OptKind::ReturnTransfer)
        );
        assert_eq!(option_kind(CURLOPT_FOLLOWLOCATION), Some(OptKind::Long));
        assert_eq!(option_kind(CURLINFO_HEADER_OUT), Some(OptKind::HeaderOut));
        assert_eq!(option_kind(1234), None);
        assert_eq!(option_kind(-1), Some(OptKind::SafeUpload));
    }

    #[test]
    fn callbacks() {
        for cb in Callback::ALL {
            assert_eq!(
                option_kind(cb.function_option()),
                Some(OptKind::Callable(cb))
            );
            assert!(cb.option_name().starts_with("CURLOPT_"));
        }
    }
}
