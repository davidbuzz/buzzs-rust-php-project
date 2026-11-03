//! JSON error codes (`php_json_error_code`), error details with location and
//! the error types returned by [`crate::encode()`] / [`crate::decode()`].

use php_runtime::RuntimeError;
use std::fmt;

/// A JSON error code: the value of `json_last_error()` and of the
/// `JSON_ERROR_*` constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(i64)]
pub enum ErrorCode {
    /// `JSON_ERROR_NONE`.
    #[default]
    None = 0,
    /// `JSON_ERROR_DEPTH`.
    Depth = 1,
    /// `JSON_ERROR_STATE_MISMATCH`.
    StateMismatch = 2,
    /// `JSON_ERROR_CTRL_CHAR`.
    CtrlChar = 3,
    /// `JSON_ERROR_SYNTAX`.
    Syntax = 4,
    /// `JSON_ERROR_UTF8`.
    Utf8 = 5,
    /// `JSON_ERROR_RECURSION`.
    Recursion = 6,
    /// `JSON_ERROR_INF_OR_NAN`.
    InfOrNan = 7,
    /// `JSON_ERROR_UNSUPPORTED_TYPE`.
    UnsupportedType = 8,
    /// `JSON_ERROR_INVALID_PROPERTY_NAME`.
    InvalidPropertyName = 9,
    /// `JSON_ERROR_UTF16`.
    Utf16 = 10,
    /// `JSON_ERROR_NON_BACKED_ENUM`.
    NonBackedEnum = 11,
}

impl ErrorCode {
    /// All error codes, in numeric order.
    pub const ALL: [ErrorCode; 12] = [
        ErrorCode::None,
        ErrorCode::Depth,
        ErrorCode::StateMismatch,
        ErrorCode::CtrlChar,
        ErrorCode::Syntax,
        ErrorCode::Utf8,
        ErrorCode::Recursion,
        ErrorCode::InfOrNan,
        ErrorCode::UnsupportedType,
        ErrorCode::InvalidPropertyName,
        ErrorCode::Utf16,
        ErrorCode::NonBackedEnum,
    ];

    /// The numeric value (`JSON_ERROR_*`).
    #[must_use]
    pub fn code(self) -> i64 {
        self as i64
    }

    /// Converts a numeric value back to a code.
    #[must_use]
    pub fn from_code(code: i64) -> Option<Self> {
        usize::try_from(code)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
    }

    /// The name of the `JSON_ERROR_*` constant.
    #[must_use]
    pub fn constant_name(self) -> &'static str {
        match self {
            ErrorCode::None => "JSON_ERROR_NONE",
            ErrorCode::Depth => "JSON_ERROR_DEPTH",
            ErrorCode::StateMismatch => "JSON_ERROR_STATE_MISMATCH",
            ErrorCode::CtrlChar => "JSON_ERROR_CTRL_CHAR",
            ErrorCode::Syntax => "JSON_ERROR_SYNTAX",
            ErrorCode::Utf8 => "JSON_ERROR_UTF8",
            ErrorCode::Recursion => "JSON_ERROR_RECURSION",
            ErrorCode::InfOrNan => "JSON_ERROR_INF_OR_NAN",
            ErrorCode::UnsupportedType => "JSON_ERROR_UNSUPPORTED_TYPE",
            ErrorCode::InvalidPropertyName => "JSON_ERROR_INVALID_PROPERTY_NAME",
            ErrorCode::Utf16 => "JSON_ERROR_UTF16",
            ErrorCode::NonBackedEnum => "JSON_ERROR_NON_BACKED_ENUM",
        }
    }

    /// The message of `php_json_get_error_msg` (without location).
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            ErrorCode::None => "No error",
            ErrorCode::Depth => "Maximum stack depth exceeded",
            ErrorCode::StateMismatch => "State mismatch (invalid or malformed JSON)",
            ErrorCode::CtrlChar => "Control character error, possibly incorrectly encoded",
            ErrorCode::Syntax => "Syntax error",
            ErrorCode::Utf8 => "Malformed UTF-8 characters, possibly incorrectly encoded",
            ErrorCode::Recursion => "Recursion detected",
            ErrorCode::InfOrNan => "Inf and NaN cannot be JSON encoded",
            ErrorCode::UnsupportedType => "Type is not supported",
            ErrorCode::InvalidPropertyName => "The decoded property name is invalid",
            ErrorCode::Utf16 => "Single unpaired UTF-16 surrogate in unicode escape",
            ErrorCode::NonBackedEnum => "Non-backed enums have no default serialization",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

/// `php_json_error_details`: an error code plus the 1-based location of the
/// failing token for decode errors (`line == 0`/`column == 0` when there is no
/// location, e.g. for encode errors and the empty-input syntax error).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct JsonError {
    /// The error code.
    pub code: ErrorCode,
    /// 1-based line of the failing token (0 = unknown).
    pub line: u64,
    /// 1-based column (in UTF-8 characters) of the failing token (0 = unknown).
    pub column: u64,
}

impl JsonError {
    /// The "no error" state.
    pub const NONE: JsonError = JsonError {
        code: ErrorCode::None,
        line: 0,
        column: 0,
    };

    /// An error without location.
    #[must_use]
    pub fn new(code: ErrorCode) -> Self {
        JsonError {
            code,
            line: 0,
            column: 0,
        }
    }

    /// An error at a location.
    #[must_use]
    pub fn at(code: ErrorCode, line: u64, column: u64) -> Self {
        JsonError { code, line, column }
    }

    /// The numeric code (`json_last_error()`).
    #[must_use]
    pub fn code(&self) -> i64 {
        self.code.code()
    }

    /// Whether this is the "no error" state.
    #[must_use]
    pub fn is_none(&self) -> bool {
        self.code == ErrorCode::None
    }

    /// The message of `json_last_error_msg()` / `JsonException::getMessage()`
    /// for decode errors (`php_json_get_error_msg_with_location`): the base
    /// message followed by `" near location LINE:COLUMN"` when known.
    #[must_use]
    pub fn msg(&self) -> String {
        let base = self.code.message();
        if self.line > 0 && self.column > 0 {
            format!("{base} near location {}:{}", self.line, self.column)
        } else {
            base.to_string()
        }
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg())
    }
}

impl std::error::Error for JsonError {}

impl From<ErrorCode> for JsonError {
    fn from(code: ErrorCode) -> Self {
        JsonError::new(code)
    }
}

/// Why [`crate::encode()`] failed.
#[derive(Debug, Clone)]
pub enum EncodeError {
    /// A JSON error (`json_encode()` returns `false`, or the partial output
    /// when `JSON_PARTIAL_OUTPUT_ON_ERROR` is set).
    Json {
        /// The last error recorded by the encoder.
        error: JsonError,
        /// The output produced with `JSON_PARTIAL_OUTPUT_ON_ERROR`
        /// (`None` when the flag is not set).
        partial_output: Option<Vec<u8>>,
    },
    /// An exception thrown while encoding (by `jsonSerialize()`, a property
    /// hook or the host). It propagates out of `json_encode()`.
    Exception(RuntimeError),
}

impl EncodeError {
    /// The JSON error, if this is not an exception.
    #[must_use]
    pub fn json_error(&self) -> Option<JsonError> {
        match self {
            EncodeError::Json { error, .. } => Some(*error),
            EncodeError::Exception(_) => None,
        }
    }
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::Json { error, .. } => write!(f, "{error}"),
            EncodeError::Exception(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Why [`crate::decode()`] failed.
#[derive(Debug, Clone)]
pub enum DecodeError {
    /// A JSON error (`json_decode()` returns `null`).
    Json(JsonError),
    /// An exception thrown by the host while building the result.
    Exception(RuntimeError),
}

impl DecodeError {
    /// The JSON error, if this is not an exception.
    #[must_use]
    pub fn json_error(&self) -> Option<JsonError> {
        match self {
            DecodeError::Json(e) => Some(*e),
            DecodeError::Exception(_) => None,
        }
    }
}

impl From<JsonError> for DecodeError {
    fn from(e: JsonError) -> Self {
        DecodeError::Json(e)
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Json(e) => write!(f, "{e}"),
            DecodeError::Exception(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_messages() {
        for (i, c) in ErrorCode::ALL.iter().enumerate() {
            assert_eq!(c.code(), i64::try_from(i).unwrap());
            assert_eq!(ErrorCode::from_code(c.code()), Some(*c));
        }
        assert_eq!(ErrorCode::from_code(12), None);
        assert_eq!(ErrorCode::from_code(-1), None);
        assert_eq!(ErrorCode::Syntax.message(), "Syntax error");
        assert_eq!(ErrorCode::Utf16.constant_name(), "JSON_ERROR_UTF16");
    }

    #[test]
    fn message_with_location() {
        assert_eq!(
            JsonError::at(ErrorCode::Syntax, 1, 3).msg(),
            "Syntax error near location 1:3"
        );
        assert_eq!(JsonError::new(ErrorCode::Syntax).msg(), "Syntax error");
        assert_eq!(JsonError::NONE.msg(), "No error");
        assert!(JsonError::default().is_none());
    }
}
