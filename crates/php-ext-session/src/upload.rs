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

/// `php_session_rfc1867_callback()`: `false` is `FAILURE` (the upload was
/// cancelled through `$_SESSION[...]['cancel_upload']`).
pub(crate) fn callback(vm: &mut Vm, event: Rfc1867Event<'_>, post_bytes_processed: i64) -> bool {
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
