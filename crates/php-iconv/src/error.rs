//! `php_iconv_err_t` and `_php_iconv_show_error()`.

use std::fmt;

/// Maximum charset name length (`ICONV_CSNMAXLEN`); names must be shorter.
pub const CSNMAXLEN: usize = 64;

/// Internal error codes (`php_iconv_err_t`, minus `SUCCESS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconvErr {
    /// `PHP_ICONV_ERR_CONVERTER`: `iconv_open` failed for a reason other
    /// than an unsupported conversion.
    Converter,
    /// `PHP_ICONV_ERR_WRONG_CHARSET`: unsupported conversion.
    WrongCharset,
    /// `PHP_ICONV_ERR_TOO_BIG`.
    TooBig,
    /// `PHP_ICONV_ERR_ILLEGAL_SEQ` (`EILSEQ`).
    IllegalSeq,
    /// `PHP_ICONV_ERR_ILLEGAL_CHAR` (`EINVAL`: incomplete sequence).
    IllegalChar,
    /// `PHP_ICONV_ERR_UNKNOWN`, carrying the `errno` PHP would print.
    Unknown(i32),
    /// `PHP_ICONV_ERR_MALFORMED`.
    Malformed,
    /// `PHP_ICONV_ERR_ALLOC`: a buffer of this many bytes could not be
    /// allocated (memory limit).
    Alloc(usize),
    /// `PHP_ICONV_ERR_OUT_BY_BOUNDS`.
    OutOfBounds,
}

impl IconvErr {
    /// Maps an `iconv_open` failure errno.
    pub(crate) fn from_open_errno(errno: i32) -> Self {
        if errno == libc::EINVAL {
            Self::WrongCharset
        } else {
            Self::Converter
        }
    }
}

/// A diagnostic the VM must raise.  Messages exclude the `"func(): "`
/// prefix that `php_error_docref` / `zend_argument_value_error` add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    /// `E_WARNING`.
    Warning(String),
    /// `E_NOTICE`.
    Notice(String),
    /// `E_DEPRECATED`.
    Deprecated(String),
    /// Throw `ValueError`.
    ValueError(String),
    /// Fatal "Allowed memory size of N bytes exhausted (tried to allocate
    /// `requested` bytes)".
    OutOfMemory {
        /// The allocation size that exceeded the limit.
        requested: usize,
    },
    /// Any other fatal error, full message text.
    Fatal(String),
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Warning(m)
            | Self::Notice(m)
            | Self::Deprecated(m)
            | Self::ValueError(m)
            | Self::Fatal(m) => f.write_str(m),
            Self::OutOfMemory { requested } => {
                write!(f, "memory exhausted (tried to allocate {requested} bytes)")
            }
        }
    }
}

/// `_php_iconv_show_error(err, out_charset, in_charset)`.
pub fn show_error(err: IconvErr, out_charset: &[u8], in_charset: &[u8]) -> Diagnostic {
    match err {
        IconvErr::Converter => Diagnostic::Warning("Cannot open converter".into()),
        IconvErr::WrongCharset => Diagnostic::Warning(format!(
            "Wrong encoding, conversion from \"{}\" to \"{}\" is not allowed",
            String::from_utf8_lossy(c_prefix(in_charset)),
            String::from_utf8_lossy(c_prefix(out_charset))
        )),
        IconvErr::IllegalChar => {
            Diagnostic::Notice("Detected an incomplete multibyte character in input string".into())
        }
        IconvErr::IllegalSeq => {
            Diagnostic::Notice("Detected an illegal character in input string".into())
        }
        IconvErr::TooBig => Diagnostic::Warning("Buffer length exceeded".into()),
        IconvErr::Malformed => Diagnostic::Warning("Malformed string".into()),
        IconvErr::OutOfBounds => Diagnostic::ValueError(
            "Argument #3 ($offset) must be contained in argument #1 ($haystack)".into(),
        ),
        IconvErr::Alloc(requested) => Diagnostic::OutOfMemory { requested },
        IconvErr::Unknown(errno) => Diagnostic::Notice(format!("Unknown error ({errno})")),
    }
}

/// The warning for over-long charset parameters.
pub fn charset_too_long() -> Diagnostic {
    Diagnostic::Warning(format!(
        "Encoding parameter exceeds the maximum allowed length of {CSNMAXLEN} characters"
    ))
}

/// What C sees of a byte string: everything before the first NUL.
pub(crate) fn c_prefix(s: &[u8]) -> &[u8] {
    s.iter().position(|&b| b == 0).map_or(s, |n| &s[..n])
}
