//! The PHP-level functions `json_encode()`, `json_decode()`,
//! `json_validate()`, `json_last_error()` and `json_last_error_msg()`
//! (`PHP_FUNCTION`s of `ext/json/json.c`), including argument validation
//! (`ValueError`s), `JSON_THROW_ON_ERROR` and the last-error state.
//!
//! The VM keeps one [`JsonState`] per request (`JSON_G(error_details)`,
//! cleared at request start) and calls these after parsing the arguments.

use crate::decode::{decode, validate};
use crate::encode::Encoder;
use crate::error::{DecodeError, ErrorCode, JsonError};
use crate::flags::{INVALID_UTF8_IGNORE, PARTIAL_OUTPUT_ON_ERROR, THROW_ON_ERROR};
use crate::host::JsonHost;
use php_runtime::{RtResult, RuntimeError, Value, ZStr};

/// The per-request JSON state: the details of the last error
/// (`JSON_G(error_details)`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JsonState {
    /// The last error (`JsonError::NONE` when there is none).
    pub last_error: JsonError,
}

impl JsonState {
    /// A fresh state (`PHP_RINIT_FUNCTION(json)`).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `php_json_error_details_clear`.
    pub fn clear(&mut self) {
        self.last_error = JsonError::NONE;
    }
}

const INT_MAX: i64 = i32::MAX as i64;
const INT_MIN: i64 = i32::MIN as i64;

/// `json_encode(mixed $value, int $flags = 0, int $depth = 512): string|false`.
///
/// Returns the JSON string, or `false` on error (unless
/// `JSON_PARTIAL_OUTPUT_ON_ERROR`). Throws a `ValueError` for a depth outside
/// the C `int` range, a `JsonException` (via [`JsonHost::json_exception`]) on
/// error with `JSON_THROW_ON_ERROR` (without `JSON_PARTIAL_OUTPUT_ON_ERROR`),
/// and propagates exceptions from `jsonSerialize()`.
pub fn json_encode(
    state: &mut JsonState,
    host: &mut dyn JsonHost,
    value: &Value,
    flags: i64,
    depth: i64,
) -> RtResult<Value> {
    if !(INT_MIN..=INT_MAX).contains(&depth) {
        return Err(RuntimeError::ValueError(format!(
            "json_encode(): Argument #3 ($depth) must be between {INT_MIN} and {INT_MAX}"
        )));
    }
    let mut buf = Vec::new();
    let mut encoder = Encoder::new(host, depth);
    let result = encoder.encode_zval(&mut buf, value, flags);
    let code = encoder.error;
    let records = flags & THROW_ON_ERROR == 0 || flags & PARTIAL_OUTPUT_ON_ERROR != 0;
    if let Err(e) = result {
        if records {
            state.last_error = JsonError::new(code);
        }
        return Err(e);
    }
    if records {
        state.last_error = JsonError::new(code);
        if code != ErrorCode::None && flags & PARTIAL_OUTPUT_ON_ERROR == 0 {
            return Ok(Value::Bool(false));
        }
    } else if code != ErrorCode::None {
        return Err(host.json_exception(code.message(), code.code()));
    }
    Ok(Value::Str(ZStr::from(buf)))
}

/// `json_decode(string $json, ?bool $associative = null, int $depth = 512,
/// int $flags = 0): mixed`.
///
/// Returns the decoded value, or `null` on error. Throws `ValueError`s for
/// `$depth <= 0` ("must be greater than 0") and `$depth > INT_MAX` ("must be
/// less than 2147483647"), and a `JsonException` with the located message on
/// error with `JSON_THROW_ON_ERROR` (in which case the last error is left
/// untouched).
pub fn json_decode(
    state: &mut JsonState,
    host: &mut dyn JsonHost,
    json: &[u8],
    assoc: Option<bool>,
    depth: i64,
    flags: i64,
) -> RtResult<Value> {
    let throw = flags & THROW_ON_ERROR != 0;
    if !throw {
        state.clear();
    }
    if json.is_empty() {
        let code = ErrorCode::Syntax;
        if !throw {
            state.last_error = JsonError::new(code);
            return Ok(Value::Null);
        }
        return Err(host.json_exception(code.message(), code.code()));
    }
    if depth <= 0 {
        return Err(RuntimeError::ValueError(
            "json_decode(): Argument #3 ($depth) must be greater than 0".to_string(),
        ));
    }
    if depth > INT_MAX {
        return Err(RuntimeError::ValueError(format!(
            "json_decode(): Argument #3 ($depth) must be less than {INT_MAX}"
        )));
    }
    match decode(json, assoc, depth, flags, host) {
        Ok(v) => Ok(v),
        Err(DecodeError::Json(e)) => {
            if throw {
                Err(host.json_exception(&e.msg(), e.code()))
            } else {
                state.last_error = e;
                Ok(Value::Null)
            }
        }
        Err(DecodeError::Exception(ex)) => Err(ex),
    }
}

/// `json_validate(string $json, int $depth = 512, int $flags = 0): bool`.
///
/// Throws `ValueError`s for flags other than `0`/`JSON_INVALID_UTF8_IGNORE`
/// (argument #3) and for `$depth <= 0` / `$depth > INT_MAX` (argument #2).
pub fn json_validate(state: &mut JsonState, json: &[u8], depth: i64, flags: i64) -> RtResult<bool> {
    if flags != 0 && flags != INVALID_UTF8_IGNORE {
        return Err(RuntimeError::ValueError(
            "json_validate(): Argument #3 ($flags) must be a valid flag (allowed flags: JSON_INVALID_UTF8_IGNORE)"
                .to_string(),
        ));
    }
    if json.is_empty() {
        state.last_error = JsonError::new(ErrorCode::Syntax);
        return Ok(false);
    }
    state.clear();
    if depth <= 0 {
        return Err(RuntimeError::ValueError(
            "json_validate(): Argument #2 ($depth) must be greater than 0".to_string(),
        ));
    }
    if depth > INT_MAX {
        return Err(RuntimeError::ValueError(format!(
            "json_validate(): Argument #2 ($depth) must be less than {INT_MAX}"
        )));
    }
    match validate(json, depth, flags) {
        Ok(()) => Ok(true),
        Err(e) => {
            state.last_error = e;
            Ok(false)
        }
    }
}

/// `json_last_error(): int`.
#[must_use]
pub fn json_last_error(state: &JsonState) -> i64 {
    state.last_error.code()
}

/// `json_last_error_msg(): string` (with `" near location L:C"` for decode
/// errors).
#[must_use]
pub fn json_last_error_msg(state: &JsonState) -> String {
    state.last_error.msg()
}
