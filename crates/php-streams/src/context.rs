// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Stream contexts (`php_stream_context`).
//!
//! Options are kept as an insertion-ordered `wrapper => option => value`
//! map so `stream_context_get_options()` returns them in PHP's order. The
//! notification callback and other `params` are VM-side concerns; the VM
//! may store arbitrary parameters in [`StreamContext::params`].

use std::cell::RefCell;
use std::rc::Rc;

use crate::value::{Key, Value};

/// A stream context.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamContext {
    options: Vec<(String, Vec<(String, Value)>)>,
    /// Context parameters other than `options` (e.g. `notification`).
    pub params: Vec<(String, Value)>,
    /// `context->notifier`: present when a notification callback is set
    /// (the callback itself lives VM-side; see [`set_notify_hook`]).
    pub notifier: Option<Notifier>,
}

/// `PHP_STREAM_NOTIFIER_PROGRESS`.
pub const NOTIFIER_PROGRESS: u32 = 1;

/// `STREAM_NOTIFY_*` codes.
pub mod notify {
    /// `STREAM_NOTIFY_RESOLVE`.
    pub const RESOLVE: i64 = 1;
    /// `STREAM_NOTIFY_CONNECT`.
    pub const CONNECT: i64 = 2;
    /// `STREAM_NOTIFY_AUTH_REQUIRED`.
    pub const AUTH_REQUIRED: i64 = 3;
    /// `STREAM_NOTIFY_MIME_TYPE_IS`.
    pub const MIME_TYPE_IS: i64 = 4;
    /// `STREAM_NOTIFY_FILE_SIZE_IS`.
    pub const FILE_SIZE_IS: i64 = 5;
    /// `STREAM_NOTIFY_REDIRECTED`.
    pub const REDIRECTED: i64 = 6;
    /// `STREAM_NOTIFY_PROGRESS`.
    pub const PROGRESS: i64 = 7;
    /// `STREAM_NOTIFY_COMPLETED`.
    pub const COMPLETED: i64 = 8;
    /// `STREAM_NOTIFY_FAILURE`.
    pub const FAILURE: i64 = 9;
    /// `STREAM_NOTIFY_AUTH_RESULT`.
    pub const AUTH_RESULT: i64 = 10;
    /// `STREAM_NOTIFY_SEVERITY_INFO`.
    pub const SEVERITY_INFO: i64 = 0;
    /// `STREAM_NOTIFY_SEVERITY_ERR`.
    pub const SEVERITY_ERR: i64 = 2;
}

/// The library side of `php_stream_notifier`: progress bookkeeping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Notifier {
    /// `progress`.
    pub progress: u64,
    /// `progress_max`.
    pub progress_max: u64,
    /// `mask` (`NOTIFIER_PROGRESS`).
    pub mask: u32,
}

/// One `php_stream_notification_notify()` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// `$notification_code`.
    pub code: i64,
    /// `$severity`.
    pub severity: i64,
    /// `$message` (`null` when absent).
    pub message: Option<Vec<u8>>,
    /// `$message_code`.
    pub message_code: i64,
    /// `$bytes_transferred`.
    pub bytes_sofar: u64,
    /// `$bytes_max`.
    pub bytes_max: u64,
}

/// Calls the context's notification callback (installed by the VM).
pub type NotifyHook = Rc<dyn Fn(&ContextRef, &Notification)>;

thread_local! {
    static NOTIFY_HOOK: RefCell<Option<NotifyHook>> = const { RefCell::new(None) };
}

/// Installs the hook that runs notification callbacks.
pub fn set_notify_hook(hook: Option<NotifyHook>) {
    NOTIFY_HOOK.with(|h| *h.borrow_mut() = hook);
}

fn has_notifier(ctx: &ContextRef) -> bool {
    ctx.try_borrow().is_ok_and(|c| c.notifier.is_some())
}

/// `php_stream_notification_notify()`: runs the callback when the context
/// has a notifier.
pub fn notify_raw(ctx: Option<&ContextRef>, n: &Notification) {
    let Some(ctx) = ctx.filter(|c| has_notifier(c)) else {
        return;
    };
    // Streams may be closed while thread-locals are being destroyed.
    let hook = NOTIFY_HOOK
        .try_with(|h| h.try_borrow().ok().and_then(|h| h.clone()))
        .ok()
        .flatten();
    if let Some(hook) = hook {
        hook(ctx, n);
    }
}

fn notify(
    ctx: Option<&ContextRef>,
    code: i64,
    severity: i64,
    message: Option<&[u8]>,
    message_code: i64,
    sofar: u64,
    max: u64,
) {
    notify_raw(
        ctx,
        &Notification {
            code,
            severity,
            message: message.map(<[u8]>::to_vec),
            message_code,
            bytes_sofar: sofar,
            bytes_max: max,
        },
    );
}

/// `php_stream_notify_info()`.
pub fn notify_info(ctx: Option<&ContextRef>, code: i64, message: Option<&[u8]>, xcode: i64) {
    notify(ctx, code, notify::SEVERITY_INFO, message, xcode, 0, 0);
}

/// `php_stream_notify_error()`.
pub fn notify_error(ctx: Option<&ContextRef>, code: i64, message: Option<&[u8]>, xcode: i64) {
    notify(ctx, code, notify::SEVERITY_ERR, message, xcode, 0, 0);
}

/// `php_stream_notify_file_size()`.
pub fn notify_file_size(ctx: Option<&ContextRef>, size: u64, message: &[u8], xcode: i64) {
    notify(
        ctx,
        notify::FILE_SIZE_IS,
        notify::SEVERITY_INFO,
        Some(message),
        xcode,
        0,
        size,
    );
}

fn update_notifier(ctx: &ContextRef, f: impl FnOnce(&mut Notifier) -> bool) -> Option<Notifier> {
    let mut c = ctx.try_borrow_mut().ok()?;
    let n = c.notifier.as_mut()?;
    f(n).then_some(*n)
}

/// `php_stream_notify_progress_init()`.
pub fn notify_progress_init(ctx: Option<&ContextRef>, sofar: u64, max: u64) {
    let Some(c) = ctx else { return };
    let Some(n) = update_notifier(c, |n| {
        n.progress = sofar;
        n.progress_max = max;
        n.mask |= NOTIFIER_PROGRESS;
        true
    }) else {
        return;
    };
    notify(
        ctx,
        notify::PROGRESS,
        notify::SEVERITY_INFO,
        None,
        0,
        n.progress,
        n.progress_max,
    );
}

/// `php_stream_notify_progress_increment()`.
pub fn notify_progress_increment(ctx: Option<&ContextRef>, dsofar: u64, dmax: u64) {
    let Some(c) = ctx else { return };
    let Some(n) = update_notifier(c, |n| {
        if n.mask & NOTIFIER_PROGRESS == 0 {
            return false;
        }
        n.progress = n.progress.wrapping_add(dsofar);
        n.progress_max = n.progress_max.wrapping_add(dmax);
        true
    }) else {
        return;
    };
    notify(
        ctx,
        notify::PROGRESS,
        notify::SEVERITY_INFO,
        None,
        0,
        n.progress,
        n.progress_max,
    );
}

/// `php_stream_notify_completed()`.
pub fn notify_completed(ctx: Option<&ContextRef>) {
    let Some(c) = ctx else { return };
    let Some(n) = update_notifier(c, |_| true) else {
        return;
    };
    notify(
        ctx,
        notify::COMPLETED,
        notify::SEVERITY_INFO,
        None,
        0,
        n.progress,
        n.progress_max,
    );
}

/// A shared context handle (contexts are resources shared by streams).
pub type ContextRef = Rc<RefCell<StreamContext>>;

impl StreamContext {
    /// An empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wraps a context in a shared handle.
    pub fn shared(self) -> ContextRef {
        Rc::new(RefCell::new(self))
    }

    /// `php_stream_context_get_option()`.
    pub fn get(&self, wrapper: &str, option: &str) -> Option<&Value> {
        self.options
            .iter()
            .find(|(w, _)| w == wrapper)
            .and_then(|(_, opts)| opts.iter().find(|(o, _)| o == option))
            .map(|(_, v)| v)
    }

    /// `php_stream_context_set_option()`.
    pub fn set(&mut self, wrapper: &str, option: &str, value: Value) {
        let idx = if let Some(i) = self.options.iter().position(|(w, _)| w == wrapper) {
            i
        } else {
            self.options.push((wrapper.to_string(), Vec::new()));
            self.options.len() - 1
        };
        let opts = &mut self.options[idx].1;
        if let Some(slot) = opts.iter_mut().find(|(o, _)| o == option) {
            slot.1 = value;
        } else {
            opts.push((option.to_string(), value));
        }
    }

    /// `php_stream_context_unset_option()`.
    pub fn unset(&mut self, wrapper: &str, option: &str) {
        if let Some((_, opts)) = self.options.iter_mut().find(|(w, _)| w == wrapper) {
            opts.retain(|(o, _)| o != option);
        }
    }

    /// Sets every option of a `[wrapper => [option => value]]` array, as
    /// `stream_context_create()` / `stream_context_set_option($ctx, array)`
    /// do. Returns false (PHP: `ValueError`/warning in the VM) when an entry
    /// is not an array.
    pub fn set_from_array(&mut self, options: &Value) -> bool {
        let Some(wrappers) = options.as_array() else {
            return false;
        };
        for (wkey, wval) in wrappers {
            let Some(opts) = wval.as_array() else {
                return false;
            };
            let wname = wkey.to_string();
            for (okey, oval) in opts {
                if let Key::Str(_) = okey {
                    self.set(&wname, &okey.to_string(), oval.clone());
                }
            }
        }
        true
    }

    /// All options (`stream_context_get_options()`).
    pub fn options(&self) -> &[(String, Vec<(String, Value)>)] {
        &self.options
    }

    /// All options as a PHP array value.
    pub fn options_value(&self) -> Value {
        Value::Array(
            self.options
                .iter()
                .map(|(w, opts)| {
                    (
                        Key::str(w.as_bytes()),
                        Value::Array(
                            opts.iter()
                                .map(|(o, v)| (Key::str(o.as_bytes()), v.clone()))
                                .collect(),
                        ),
                    )
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_order() {
        let mut c = StreamContext::new();
        c.set("http", "method", Value::str("POST"));
        c.set("file", "x", Value::Int(1));
        c.set("http", "header", Value::str("A: b"));
        c.set("http", "method", Value::str("GET"));
        assert_eq!(c.get("http", "method"), Some(&Value::str("GET")));
        assert_eq!(c.options()[0].1.len(), 2);
        assert_eq!(c.options()[1].0, "file");
        c.unset("http", "method");
        assert!(c.get("http", "method").is_none());
    }

    #[test]
    fn from_array() {
        let mut c = StreamContext::new();
        let v = Value::Array(vec![(
            Key::str("filter"),
            Value::Array(vec![(Key::str("max_filter_count"), Value::Int(3))]),
        )]);
        assert!(c.set_from_array(&v));
        assert_eq!(c.get("filter", "max_filter_count"), Some(&Value::Int(3)));
        assert!(!c.set_from_array(&Value::Array(vec![(Key::str("x"), Value::Int(1))])));
        assert_eq!(c.options_value().as_array().unwrap().len(), 1);
    }
}
