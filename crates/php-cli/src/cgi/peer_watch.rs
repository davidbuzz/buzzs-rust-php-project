// buzzs-rust-php-project - Copyright (c) 2026 Aleks <al-git001@none.at>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The thread that watches a `FastCGI` connection while its request runs.
//!
//! php-src reads nothing from the web server until the script ends, so a
//! client that went away shows only when a write fails; a script that
//! sleeps, waits on a database or computes runs on. Here a thread waits
//! (in `poll`, without reading) for the connection to become readable or
//! closed: an `FCGI_ABORT_REQUEST`, a closed connection, or another record.
//! It then asks the VM for an interrupt and waits; at its next safe point
//! the VM reads what came ([`crate::fastcgi::Connection::check_peer`]) and
//! either treats the request as aborted (`php_handle_aborted_connection()`)
//! or lets the thread watch again ([`rearm`]).
//!
//! The thread lives for one request: a worker between requests has one
//! thread, so a pre-forking parent still forks a single-threaded process.
//! No signal is used: `SIGIO` would also fire when the socket's send buffer
//! drains, and cut a script's `sleep()` short.

use crate::{fastcgi, sys};
use std::cell::RefCell;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};

/// The byte that tells the thread to watch the socket again.
const REARM: u8 = b'r';

/// The thread saw the connection readable or closed, and waits for the VM.
static READY: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The write end of the running watch's control pipe. Closing it ends
    /// the thread.
    static CONTROL: RefCell<Option<File>> = const { RefCell::new(None) };
    /// The socket of the request about to run ([`arm_for_request`]).
    static PENDING: std::cell::Cell<Option<RawFd>> = const { std::cell::Cell::new(None) };
    /// The watch of the running request.
    static RUNNING: RefCell<Option<Watch>> = const { RefCell::new(None) };
}

/// The request about to run has its connection on `socket`: the watch
/// starts once the request started ([`start_for_request`]).
pub fn arm_for_request(socket: Option<RawFd>) {
    PENDING.with(|p| p.set(socket));
}

/// The VM's startup hook: starts watching the request's connection. It runs
/// after `Vm::activate()`, which resets the connection status, so an abort
/// the watch sees is not reset away.
pub fn start_for_request(_vm: &mut php_vm::Vm) {
    let watch = PENDING.with(std::cell::Cell::take).and_then(start);
    RUNNING.with(|r| *r.borrow_mut() = watch);
}

/// The request ended: its watch stops.
pub fn end_request() {
    PENDING.with(|p| p.set(None));
    let watch = RUNNING.with(|r| r.borrow_mut().take());
    drop(watch);
}

/// A running watch; dropping it ends and joins the thread.
#[derive(Debug)]
pub struct Watch {
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Starts watching `socket` for the request that runs now; `None` when the
/// thread could not be started (logged), then a gone client shows only
/// when a write fails, as in php-src.
pub fn start(socket: RawFd) -> Option<Watch> {
    READY.store(false, Ordering::Release);
    let (read, write) = match sys::pipe() {
        Ok(ends) => ends,
        Err(err) => {
            fastcgi::log(&format!(
                "Cannot watch the FastCGI connection for aborts: pipe: {err}\n"
            ));
            return None;
        }
    };
    let control = File::from(read);
    let spawned = std::thread::Builder::new()
        .name("fcgi-peer".into())
        .spawn(move || watch(&control, socket));
    match spawned {
        Ok(thread) => {
            CONTROL.with(|c| *c.borrow_mut() = Some(File::from(write)));
            Some(Watch {
                thread: Some(thread),
            })
        }
        Err(err) => {
            fastcgi::log(&format!(
                "Cannot watch the FastCGI connection for aborts: thread: {err}\n"
            ));
            None
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // Closing the write end wakes the thread with end of file.
        CONTROL.with(|c| c.borrow_mut().take());
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                fastcgi::log("The FastCGI connection watch thread panicked\n");
            }
        }
    }
}

/// Whether the thread saw the connection readable or closed since the last
/// call; it then waits for [`rearm`].
pub fn take_ready() -> bool {
    READY.swap(false, Ordering::AcqRel)
}

/// Lets the thread watch the socket again: what it saw was read and the
/// request goes on.
pub fn rearm() {
    let written = CONTROL.with(|c| {
        c.borrow_mut()
            .as_mut()
            .map(|control| control.write_all(&[REARM]))
    });
    if let Some(Err(err)) = written {
        fastcgi::log(&format!(
            "Cannot watch the FastCGI connection for aborts any more: {err}\n"
        ));
    }
}

/// The thread: waits for the socket while armed, and for control bytes.
fn watch(control: &File, socket: RawFd) {
    let mut armed = true;
    let mut reader = control;
    loop {
        match sys::wait_either(control.as_raw_fd(), armed.then_some(socket)) {
            sys::Woke::Socket => {
                armed = false;
                READY.store(true, Ordering::Release);
                php_vm::interrupt::request_interrupt();
            }
            sys::Woke::Control => {
                let mut byte = [0u8; 1];
                match reader.read(&mut byte) {
                    // End of file: the request ended.
                    Ok(0) => return,
                    Ok(1) if byte[0] == REARM => armed = true,
                    Ok(count) => {
                        fastcgi::log(&format!(
                            "The FastCGI connection watch stopped: {count} unexpected control byte(s), {:#04x}\n",
                            byte[0]
                        ));
                        return;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(err) => {
                        fastcgi::log(&format!(
                            "The FastCGI connection watch stopped: read: {err}\n"
                        ));
                        return;
                    }
                }
            }
            sys::Woke::Failed(errno) => {
                fastcgi::log(&format!(
                    "The FastCGI connection watch stopped: poll: {}\n",
                    std::io::Error::from_raw_os_error(errno)
                ));
                return;
            }
        }
    }
}
