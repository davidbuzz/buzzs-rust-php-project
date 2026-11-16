// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Session upload progress: the module's `php_rfc1867_callback`
//! (`php_session_rfc1867_callback` in `session.c`), forwarding the SAPI's
//! multipart parser events to [`php_session::rfc1867`].
//!
//! The in-flight progress record lives here between events, as
//! `PS(rfc1867_progress)` does in PHP.

use std::cell::RefCell;

use php_session::rfc1867::{self, UploadProgress};
use php_vm::ext::Rfc1867Event;
use php_vm::Vm;

use crate::host::{set_phase, with_host, Phase};

thread_local! {
    /// `PS(rfc1867_progress)`: set by the start event, consumed by the end.
    static PROGRESS: RefCell<Option<UploadProgress>> = const { RefCell::new(None) };
}

/// Runs `f` on the in-flight record; events without one succeed.
fn with_progress(f: impl FnOnce(&mut UploadProgress) -> bool) -> bool {
    // Taken out of the slot while `f` runs: save handlers execute user
    // code.
    let Some(mut p) = PROGRESS.with(|c| c.borrow_mut().take()) else {
        return true;
    };
    let ok = f(&mut p);
    PROGRESS.with(|c| *c.borrow_mut() = Some(p));
    ok
}

/// Whether an upload progress record is in flight.
fn has_progress() -> bool {
    PROGRESS.with(|c| c.borrow().is_some())
}

/// For an in-flight record that is not tracked, what a data event
/// returns: not cancelled.
fn untracked_verdict() -> Option<bool> {
    PROGRESS.with(|c| {
        c.borrow()
            .as_ref()
            .filter(|p| !p.tracking())
            .map(|p| !p.cancelled())
    })
}

/// `php_session_rfc1867_callback()`: `false` is `FAILURE` (the upload was
/// cancelled through `$_SESSION[...]['cancel_upload']`).
pub(crate) fn callback(vm: &mut Vm, event: Rfc1867Event<'_>, post_bytes_processed: i64) -> bool {
    // Without an in-flight record (upload progress disabled) every event
    // but the start is a no-op:
    // skip the host set-up and INI synchronisation, which would otherwise
    // run once per 5 KiB chunk of an uploaded file.
    if !matches!(event, Rfc1867Event::Start { .. }) && !has_progress() {
        return true;
    }
    // An untracked upload's data event changes nothing either
    // (`rfc1867::file_data` returns before touching the session).
    if matches!(event, Rfc1867Event::FileData { .. }) {
        if let Some(ok) = untracked_verdict() {
            return ok;
        }
    }
    // The parser runs during request startup, before `RINIT`.
    set_phase(Phase::RequestStartup);
    let ok = with_host(vm, |h| {
        let ok = match event {
            Rfc1867Event::Start { content_length } => {
                let p = rfc1867::start(h, content_length);
                PROGRESS.with(|c| *c.borrow_mut() = p);
                true
            }
            Rfc1867Event::FormData { name, value } => {
                with_progress(|p| rfc1867::formdata(&mut *h, p, name, value))
            }
            Rfc1867Event::FileStart { name, filename } => with_progress(|p| {
                rfc1867::file_start(&mut *h, p, name, filename, post_bytes_processed)
            }),
            Rfc1867Event::FileData { offset, length } => with_progress(|p| {
                rfc1867::file_data(&mut *h, p, offset, length, post_bytes_processed)
            }),
            Rfc1867Event::FileEnd {
                temp_filename,
                cancel_upload,
            } => with_progress(|p| {
                rfc1867::file_end(
                    &mut *h,
                    p,
                    temp_filename,
                    cancel_upload,
                    post_bytes_processed,
                )
            }),
            Rfc1867Event::End => {
                if let Some(p) = PROGRESS.with(|c| c.borrow_mut().take()) {
                    rfc1867::end(h, p, post_bytes_processed);
                }
                true
            }
        };
        // Exceptions cannot escape request startup.
        h.state().take_exception();
        ok
    });
    vm.pending_throw = None;
    set_phase(Phase::Running);
    ok
}
