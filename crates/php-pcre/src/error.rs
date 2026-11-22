// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Error values: `preg_last_error()` codes and pattern compilation warnings.

use std::fmt;

use crate::ffi;

/// The value of `preg_last_error()` (`PHP_PCRE_*_ERROR` in `php_pcre.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(i64)]
pub enum PregError {
    /// `PREG_NO_ERROR`
    #[default]
    NoError = 0,
    /// `PREG_INTERNAL_ERROR`
    Internal = 1,
    /// `PREG_BACKTRACK_LIMIT_ERROR`
    BacktrackLimit = 2,
    /// `PREG_RECURSION_LIMIT_ERROR`
    RecursionLimit = 3,
    /// `PREG_BAD_UTF8_ERROR`
    BadUtf8 = 4,
    /// `PREG_BAD_UTF8_OFFSET_ERROR`
    BadUtf8Offset = 5,
    /// `PREG_JIT_STACKLIMIT_ERROR`
    JitStackLimit = 6,
}

impl PregError {
    /// The integer returned by `preg_last_error()`.
    pub fn code(self) -> i64 {
        self as i64
    }

    /// Inverse of [`PregError::code`].
    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::NoError,
            1 => Self::Internal,
            2 => Self::BacktrackLimit,
            3 => Self::RecursionLimit,
            4 => Self::BadUtf8,
            5 => Self::BadUtf8Offset,
            6 => Self::JitStackLimit,
            _ => return None,
        })
    }

    /// The string returned by `preg_last_error_msg()`.
    pub fn message(self) -> &'static str {
        match self {
            Self::NoError => "No error",
            Self::Internal => "Internal error",
            Self::BadUtf8 => "Malformed UTF-8 characters, possibly incorrectly encoded",
            Self::BadUtf8Offset => {
                "The offset did not correspond to the beginning of a valid UTF-8 code point"
            }
            Self::BacktrackLimit => "Backtrack limit exhausted",
            Self::RecursionLimit => "Recursion limit exhausted",
            Self::JitStackLimit => "JIT stack limit exhausted",
        }
    }

    /// The name of the PHP constant for this code, e.g. `"PREG_INTERNAL_ERROR"`.
    pub fn constant_name(self) -> &'static str {
        match self {
            Self::NoError => "PREG_NO_ERROR",
            Self::Internal => "PREG_INTERNAL_ERROR",
            Self::BacktrackLimit => "PREG_BACKTRACK_LIMIT_ERROR",
            Self::RecursionLimit => "PREG_RECURSION_LIMIT_ERROR",
            Self::BadUtf8 => "PREG_BAD_UTF8_ERROR",
            Self::BadUtf8Offset => "PREG_BAD_UTF8_OFFSET_ERROR",
            Self::JitStackLimit => "PREG_JIT_STACKLIMIT_ERROR",
        }
    }

    /// Maps a negative PCRE2 match result to PHP's code
    /// (`pcre_handle_exec_error`).
    pub(crate) fn from_pcre2(rc: i32) -> Self {
        match rc {
            ffi::PCRE2_ERROR_MATCHLIMIT => Self::BacktrackLimit,
            ffi::PCRE2_ERROR_RECURSIONLIMIT => Self::RecursionLimit,
            ffi::PCRE2_ERROR_BADUTFOFFSET => Self::BadUtf8Offset,
            ffi::PCRE2_ERROR_JIT_STACKLIMIT => Self::JitStackLimit,
            rc if (ffi::PCRE2_ERROR_UTF8_ERR21..=ffi::PCRE2_ERROR_UTF8_ERR1).contains(&rc) => {
                Self::BadUtf8
            }
            _ => Self::Internal,
        }
    }
}

impl fmt::Display for PregError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for PregError {}

/// Why a pattern string could not be turned into a [`crate::Regex`].
///
/// Every variant corresponds to an `E_WARNING` PHP emits from
/// `pcre_get_compiled_regex_cache()`; after it, PHP sets
/// `preg_last_error()` to [`PregError::Internal`] (see
/// [`CompileError::last_error`]).  The warning text is [`CompileError::message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// Only whitespace (or nothing) was given.
    EmptyRegex,
    /// The first non-blank byte is alphanumeric, a backslash or NUL.
    BadDelimiter,
    /// No closing delimiter (same start/end delimiter).
    NoEndingDelimiter(u8),
    /// No closing bracket-style delimiter (`(`, `[`, `{`, `<`).
    NoEndingMatchingDelimiter(u8),
    /// A modifier letter PHP does not know (including `e`).
    UnknownModifier(u8),
    /// A NUL byte in the modifier list.
    NulModifier,
    /// `pcre2_compile()` failed.
    Compilation {
        /// PCRE2's message (or PHP's replacement for `\C` under `/u`).
        message: Vec<u8>,
        /// Byte offset into the pattern body (between the delimiters).
        offset: usize,
    },
    /// `\C` in a pattern an in-pattern verb (`(*UTF)`) put in UTF mode,
    /// where it can match half a character (GH-21134).
    BackslashCInUtf,
    /// `pcre2_maketables()` failed for a locale-aware pattern.
    LocaleTables,
    /// `pcre2_pattern_info()` failed (should never happen).
    PatternInfo(i32),
}

impl CompileError {
    /// The exact text of PHP's warning (without the `"preg_xxx(): "` prefix).
    pub fn message(&self) -> Vec<u8> {
        match self {
            Self::EmptyRegex => b"Empty regular expression".to_vec(),
            Self::BadDelimiter => {
                b"Delimiter must not be alphanumeric, backslash, or NUL byte".to_vec()
            }
            Self::NoEndingDelimiter(c) => {
                let mut m = b"No ending delimiter '".to_vec();
                m.push(*c);
                m.extend_from_slice(b"' found");
                m
            }
            Self::NoEndingMatchingDelimiter(c) => {
                let mut m = b"No ending matching delimiter '".to_vec();
                m.push(*c);
                m.extend_from_slice(b"' found");
                m
            }
            Self::UnknownModifier(c) => {
                let mut m = b"Unknown modifier '".to_vec();
                m.push(*c);
                m.push(b'\'');
                m
            }
            Self::NulModifier => b"NUL byte is not a valid modifier".to_vec(),
            Self::Compilation { message, offset } => {
                let mut m = b"Compilation failed: ".to_vec();
                m.extend_from_slice(message);
                m.extend_from_slice(format!(" at offset {offset}").as_bytes());
                m
            }
            Self::BackslashCInUtf => {
                b"Compilation failed: using \\C is incompatible with UTF-8 mode".to_vec()
            }
            Self::LocaleTables => b"Failed to generate locale character tables".to_vec(),
            Self::PatternInfo(rc) => {
                format!("Internal pcre_pattern_info() error {rc}").into_bytes()
            }
        }
    }

    /// The `preg_last_error()` value PHP leaves behind after this failure.
    pub fn last_error(&self) -> PregError {
        PregError::Internal
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for CompileError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip() {
        for code in 0..=6 {
            let e = PregError::from_code(code).unwrap();
            assert_eq!(e.code(), code);
        }
        assert_eq!(PregError::from_code(7), None);
    }

    #[test]
    fn messages_match_php() {
        assert_eq!(PregError::NoError.message(), "No error");
        assert_eq!(PregError::Internal.message(), "Internal error");
        assert_eq!(
            PregError::BacktrackLimit.message(),
            "Backtrack limit exhausted"
        );
        assert_eq!(
            PregError::RecursionLimit.message(),
            "Recursion limit exhausted"
        );
        assert_eq!(
            PregError::BadUtf8.message(),
            "Malformed UTF-8 characters, possibly incorrectly encoded"
        );
        assert_eq!(
            PregError::BadUtf8Offset.message(),
            "The offset did not correspond to the beginning of a valid UTF-8 code point"
        );
        assert_eq!(
            PregError::JitStackLimit.message(),
            "JIT stack limit exhausted"
        );
    }

    #[test]
    fn pcre2_mapping() {
        assert_eq!(
            PregError::from_pcre2(ffi::PCRE2_ERROR_MATCHLIMIT),
            PregError::BacktrackLimit
        );
        assert_eq!(
            PregError::from_pcre2(ffi::PCRE2_ERROR_RECURSIONLIMIT),
            PregError::RecursionLimit
        );
        assert_eq!(
            PregError::from_pcre2(ffi::PCRE2_ERROR_UTF8_ERR1),
            PregError::BadUtf8
        );
        assert_eq!(
            PregError::from_pcre2(ffi::PCRE2_ERROR_UTF8_ERR21),
            PregError::BadUtf8
        );
        assert_eq!(
            PregError::from_pcre2(ffi::PCRE2_ERROR_BADUTFOFFSET),
            PregError::BadUtf8Offset
        );
        assert_eq!(
            PregError::from_pcre2(ffi::PCRE2_ERROR_JIT_STACKLIMIT),
            PregError::JitStackLimit
        );
        assert_eq!(
            PregError::from_pcre2(ffi::PCRE2_ERROR_BADOFFSET),
            PregError::Internal
        );
        assert_eq!(
            PregError::from_pcre2(ffi::PCRE2_ERROR_INTERNAL),
            PregError::Internal
        );
    }

    #[test]
    fn compile_error_texts() {
        assert_eq!(
            CompileError::EmptyRegex.to_string(),
            "Empty regular expression"
        );
        assert_eq!(
            CompileError::BadDelimiter.to_string(),
            "Delimiter must not be alphanumeric, backslash, or NUL byte"
        );
        assert_eq!(
            CompileError::NoEndingDelimiter(b'~').to_string(),
            "No ending delimiter '~' found"
        );
        assert_eq!(
            CompileError::NoEndingMatchingDelimiter(b'}').to_string(),
            "No ending matching delimiter '}' found"
        );
        assert_eq!(
            CompileError::UnknownModifier(b'z').to_string(),
            "Unknown modifier 'z'"
        );
        assert_eq!(
            CompileError::NulModifier.to_string(),
            "NUL byte is not a valid modifier"
        );
        assert_eq!(
            CompileError::Compilation {
                message: b"missing closing parenthesis".to_vec(),
                offset: 1
            }
            .to_string(),
            "Compilation failed: missing closing parenthesis at offset 1"
        );
        assert_eq!(
            CompileError::UnknownModifier(0xE9).message(),
            b"Unknown modifier '\xE9'"
        );
    }
}
