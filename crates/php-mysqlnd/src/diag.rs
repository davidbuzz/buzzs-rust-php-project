//! PHP-level diagnostics raised by mysqlnd itself (`php_error_docref` /
//! `php_error` calls deep in the protocol code). They are queued in a
//! thread-local list; the binding drains them with [`take`] after each call
//! and emits them as `E_WARNING`s.

use std::cell::RefCell;

/// One queued diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    /// `true` for `php_error_docref` (message prefixed with the current
    /// function, e.g. `mysqli_query(): ...`), `false` for plain `php_error`.
    pub docref: bool,
    /// `true` for `E_ERROR` (fatal), `false` for `E_WARNING`.
    pub fatal: bool,
    /// The message text.
    pub message: String,
}

thread_local! {
    static QUEUE: RefCell<Vec<Diag>> = const { RefCell::new(Vec::new()) };
}

/// Queues a `php_error_docref(NULL, E_WARNING, ...)`.
pub fn warn(message: impl Into<String>) {
    QUEUE.with(|q| {
        q.borrow_mut().push(Diag {
            docref: true,
            fatal: false,
            message: message.into(),
        });
    });
}

/// Queues a `php_error(E_WARNING, ...)` (no function prefix).
pub fn warn_plain(message: impl Into<String>) {
    QUEUE.with(|q| {
        q.borrow_mut().push(Diag {
            docref: false,
            fatal: false,
            message: message.into(),
        });
    });
}

/// Drains the queued diagnostics.
#[must_use]
pub fn take() -> Vec<Diag> {
    QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// The process id as printed in mysqlnd messages (`PID=%d`).
#[must_use]
pub fn pid() -> u32 {
    std::process::id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_roundtrip() {
        let _ = take();
        warn("a");
        warn_plain("b");
        let d = take();
        assert_eq!(d.len(), 2);
        assert!(d[0].docref && !d[1].docref);
        assert!(take().is_empty());
    }
}
