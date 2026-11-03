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

/// A callback receiving diagnostics the moment mysqlnd raises them.
pub type Sink = Box<dyn FnMut(&Diag)>;

thread_local! {
    static QUEUE: RefCell<Vec<Diag>> = const { RefCell::new(Vec::new()) };
    static SINK: RefCell<Option<Sink>> = const { RefCell::new(None) };
}

/// Installs (or removes) a callback that receives every diagnostic
/// immediately, in the order mysqlnd raises it (so PHP warnings interleave
/// with network traffic exactly like PHP). Without a sink, diagnostics are
/// queued for [`take`]. Returns the previous sink.
pub fn set_sink(sink: Option<Sink>) -> Option<Sink> {
    SINK.with(|s| std::mem::replace(&mut *s.borrow_mut(), sink))
}

fn emit(d: Diag) {
    // take the sink out while calling it, so a re-entrant warning is queued
    let taken = SINK.with(|s| s.borrow_mut().take());
    match taken {
        Some(mut f) => {
            f(&d);
            SINK.with(|s| {
                let mut slot = s.borrow_mut();
                if slot.is_none() {
                    *slot = Some(f);
                }
            });
        }
        None => QUEUE.with(|q| q.borrow_mut().push(d)),
    }
}

/// Raises a `php_error_docref(NULL, E_WARNING, ...)`.
pub fn warn(message: impl Into<String>) {
    emit(Diag {
        docref: true,
        fatal: false,
        message: message.into(),
    });
}

/// Raises a `php_error(E_WARNING, ...)` (no function prefix).
pub fn warn_plain(message: impl Into<String>) {
    emit(Diag {
        docref: false,
        fatal: false,
        message: message.into(),
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

    #[test]
    fn sink_receives_immediately() {
        use std::rc::Rc;
        let seen = Rc::new(RefCell::new(Vec::new()));
        let s2 = seen.clone();
        let _ = set_sink(Some(Box::new(move |d: &Diag| {
            s2.borrow_mut().push(d.message.clone());
        })));
        warn("x");
        assert_eq!(*seen.borrow(), vec!["x".to_string()]);
        assert!(take().is_empty());
        let _ = set_sink(None);
        warn("y");
        assert_eq!(take().len(), 1);
    }
}
