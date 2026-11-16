// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The mysqlnd error model: per-connection / per-statement
//! [`ErrorInfo`] (errno, SQLSTATE, message, error list) and the [`Error`]
//! value returned by failing operations (ref: `mysqlnd_connection.c`
//! `mysqlnd_error_info`, `mysqlnd_structs.h` `SET_CLIENT_ERROR`).

use std::fmt;

use crate::consts::{
    CR_OUT_OF_MEMORY, ERRMSG_SIZE, MSG_OUT_OF_MEMORY, SQLSTATE_NULL, UNKNOWN_SQLSTATE,
};

/// One element of `mysqli_error_list()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorEntry {
    /// Error number.
    pub errno: u32,
    /// Five-character SQLSTATE.
    pub sqlstate: String,
    /// Message bytes.
    pub error: Vec<u8>,
}

/// A failed operation: a snapshot of the error that was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// Error number (server `ER_*` or client `CR_*`; 0 when the failure
    /// carried no error, e.g. a protocol warning only).
    pub errno: u32,
    /// SQLSTATE.
    pub sqlstate: String,
    /// Message bytes.
    pub message: Vec<u8>,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "({}/{}): {}",
            self.sqlstate,
            self.errno,
            String::from_utf8_lossy(&self.message)
        )
    }
}

impl std::error::Error for Error {}

/// Result type of fallible mysqlnd operations.
pub type Result<T> = std::result::Result<T, Error>;

/// `MYSQLND_ERROR_INFO`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorInfo {
    /// `error_no`
    pub errno: u32,
    /// `sqlstate` (empty after a reset, reported as "00000").
    pub sqlstate: String,
    /// `error`
    pub error: Vec<u8>,
    /// `error_list`
    pub list: Vec<ErrorEntry>,
}

/// `strlcpy` into a `MYSQLND_ERRMSG_SIZE` buffer: stops at NUL, keeps at
/// most 511 bytes.
fn clamp_message(msg: &[u8]) -> Vec<u8> {
    let end = msg
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(msg.len())
        .min(ERRMSG_SIZE - 1);
    msg[..end].to_vec()
}

fn clamp_sqlstate(s: &str) -> String {
    let end = s
        .as_bytes()
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(s.len())
        .min(5);
    s[..end].to_string()
}

impl ErrorInfo {
    /// `SET_EMPTY_ERROR`
    pub fn reset(&mut self) {
        self.errno = 0;
        self.error.clear();
        self.sqlstate.clear();
        self.list.clear();
    }

    /// `SET_CLIENT_ERROR`: records an error (or resets when `errno == 0`).
    pub fn set(&mut self, errno: u32, sqlstate: &str, error: &[u8]) {
        if errno == 0 {
            self.reset();
            return;
        }
        self.errno = errno;
        self.sqlstate = clamp_sqlstate(sqlstate);
        self.error = clamp_message(error);
        self.list.push(ErrorEntry {
            errno,
            sqlstate: self.sqlstate.clone(),
            error: self.error.clone(),
        });
    }

    /// Convenience: a client error with SQLSTATE HY000.
    pub fn set_client(&mut self, errno: u32, msg: &str) {
        self.set(errno, UNKNOWN_SQLSTATE, msg.as_bytes());
    }

    /// `SET_OOM_ERROR`
    pub fn set_oom(&mut self) {
        self.set_client(CR_OUT_OF_MEMORY, MSG_OUT_OF_MEMORY);
    }

    /// `COPY_CLIENT_ERROR(self, other)`
    pub fn copy_from(&mut self, other: &ErrorInfo) {
        let (errno, sqlstate, error) = (other.errno, other.sqlstate.clone(), other.error.clone());
        self.set(errno, &sqlstate, &error);
    }

    /// The SQLSTATE as reported to users (`"00000"` when unset).
    #[must_use]
    pub fn sqlstate_or_null(&self) -> &str {
        if self.sqlstate.is_empty() {
            SQLSTATE_NULL
        } else {
            &self.sqlstate
        }
    }

    /// Snapshot as an [`Error`].
    #[must_use]
    pub fn to_error(&self) -> Error {
        Error {
            errno: self.errno,
            sqlstate: self.sqlstate_or_null().to_string(),
            message: self.error.clone(),
        }
    }
}

/// A server error packet body (after the 0xFF marker), as decoded by
/// `php_mysqlnd_read_error_from_line`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerError {
    /// Error number (defaults to `CR_UNKNOWN_ERROR` if the packet is too short).
    pub errno: u32,
    /// SQLSTATE (defaults to HY000).
    pub sqlstate: String,
    /// Message.
    pub message: Vec<u8>,
}

impl ServerError {
    /// Decodes `buf` = the bytes after the 0xFF marker.
    #[must_use]
    pub fn parse(buf: &[u8]) -> Self {
        let mut errno = crate::consts::CR_UNKNOWN_ERROR;
        let mut sqlstate = UNKNOWN_SQLSTATE.to_string();
        let mut message = Vec::new();
        if buf.len() > 2 {
            errno = u32::from(u16::from_le_bytes([buf[0], buf[1]]));
            let mut p = 2;
            let mut ok = true;
            if buf[p] == b'#' {
                p += 1;
                if buf.len() - p >= 5 {
                    sqlstate = String::from_utf8_lossy(&buf[p..p + 5]).into_owned();
                    p += 5;
                } else {
                    ok = false;
                }
            }
            if ok && buf.len() > p {
                let n = (buf.len() - p).min(ERRMSG_SIZE - 1);
                message = buf[p..p + n].to_vec();
            }
        }
        Self {
            errno,
            sqlstate,
            message,
        }
    }

    /// Records this error into an [`ErrorInfo`].
    pub fn apply(&self, info: &mut ErrorInfo) {
        info.set(self.errno, &self.sqlstate, &self.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_info_lifecycle() {
        let mut e = ErrorInfo::default();
        assert_eq!(e.sqlstate_or_null(), "00000");
        e.set_client(2014, "x");
        e.set(1064, "42000", b"syntax");
        assert_eq!(e.errno, 1064);
        assert_eq!(e.list.len(), 2);
        e.set(0, "", b"");
        assert_eq!(e.errno, 0);
        assert!(e.list.is_empty());
        let long = vec![b'a'; 1000];
        e.set(1, "HY000", &long);
        assert_eq!(e.error.len(), 511);
        e.set(1, "HY000", b"ab\0cd");
        assert_eq!(e.error, b"ab");
    }

    #[test]
    fn parse_server_error() {
        let pkt = b"\x15\x04#28000Access denied";
        let e = ServerError::parse(pkt);
        assert_eq!(e.errno, 1045);
        assert_eq!(e.sqlstate, "28000");
        assert_eq!(e.message, b"Access denied");
        let e = ServerError::parse(b"\x10\x04Too many connections");
        assert_eq!(e.errno, 1040);
        assert_eq!(e.sqlstate, "HY000");
        assert_eq!(e.message, b"Too many connections");
        let e = ServerError::parse(b"\x01");
        assert_eq!(e.errno, 2000);
        let e = ServerError::parse(b"\x01\x00#12");
        assert_eq!(e.errno, 1);
        assert!(e.message.is_empty());
    }
}
