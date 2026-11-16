// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Module globals (`zend_soap_globals`) and the SOAP error handler
//! (`soap_error_handler`).
//!
//! While a `SoapClient`/`SoapServer` method runs, PHP routes *all* engine
//! errors through `soap_error_handler`. The fatal ones raised inside this
//! crate are converted where they surface (see [`crate::client`] and
//! [`crate::server`]); for diagnostics raised by user code the engine asks
//! [`error_disposition`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use php_runtime::{ErrorLevel, Obj};

use crate::consts::SOAP_1_1;
use crate::sdl::Sdl;

/// Which kind of object owns the active SOAP context.
#[derive(Debug, Clone)]
pub enum ErrorObject {
    /// A `SoapClient` (with its `exceptions` option).
    Client(Obj, bool),
    /// A `SoapServer`.
    Server(Obj),
}

/// The module globals.
#[derive(Debug)]
pub struct SoapGlobals {
    /// `use_soap_error_handler`.
    pub use_soap_error_handler: bool,
    /// `error_code`.
    pub error_code: Option<&'static str>,
    /// `error_object`.
    pub error_object: Option<ErrorObject>,
    /// `soap_version`.
    pub soap_version: i64,
    /// The in-memory WSDL cache: `uri → (time, sdl)`.
    pub mem_cache: HashMap<String, (i64, Rc<Sdl>)>,
    /// `SOAP_GLOBAL(features)` of the active call.
    pub features: i64,
}

impl Default for SoapGlobals {
    fn default() -> Self {
        Self {
            use_soap_error_handler: false,
            error_code: None,
            error_object: None,
            soap_version: SOAP_1_1,
            mem_cache: HashMap::new(),
            features: 0,
        }
    }
}

thread_local! {
    static GLOBALS: RefCell<SoapGlobals> = RefCell::new(SoapGlobals::default());
}

/// Runs `f` with the globals.
pub fn with<R>(f: impl FnOnce(&mut SoapGlobals) -> R) -> R {
    GLOBALS.with(|g| f(&mut g.borrow_mut()))
}

/// `SOAP_GLOBAL(soap_version)`.
#[must_use]
pub fn soap_version() -> i64 {
    with(|g| g.soap_version)
}

/// Sets `SOAP_GLOBAL(soap_version)`.
pub fn set_soap_version(v: i64) {
    with(|g| g.soap_version = v);
}

/// `PHP_RINIT_FUNCTION(soap)`: resets the per-request globals (the memory
/// cache survives).
pub fn request_init() {
    with(|g| {
        g.use_soap_error_handler = false;
        g.error_code = None;
        g.error_object = None;
        g.soap_version = SOAP_1_1;
    });
}

/// `use_soap_error_handler(bool $enable = true): bool`.
pub fn use_soap_error_handler(enable: bool) -> bool {
    with(|g| std::mem::replace(&mut g.use_soap_error_handler, enable))
}

/// Saved handler state (`SOAP_*_BEGIN_CODE` / `END_CODE`).
#[derive(Debug, Clone)]
pub struct SavedContext {
    use_handler: bool,
    code: Option<&'static str>,
    object: Option<ErrorObject>,
    version: i64,
}

/// Enters a client/server method: installs the handler state.
pub fn begin(code: &'static str, object: ErrorObject) -> SavedContext {
    with(|g| {
        let saved = SavedContext {
            use_handler: g.use_soap_error_handler,
            code: g.error_code,
            object: g.error_object.clone(),
            version: g.soap_version,
        };
        g.use_soap_error_handler = true;
        g.error_code = Some(code);
        g.error_object = Some(object);
        saved
    })
}

/// Leaves a client/server method.
pub fn end(saved: SavedContext) {
    with(|g| {
        g.use_soap_error_handler = saved.use_handler;
        g.error_code = saved.code;
        g.error_object = saved.object;
        g.soap_version = saved.version;
    });
}

/// What the engine should do with a diagnostic raised while a SOAP
/// context is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Report it normally.
    Normal,
    /// Do not display it (logged only): libxml warnings while loading a
    /// WSDL for a client with exceptions, and everything inside a server.
    Suppress,
}

/// The non-fatal half of `soap_real_error_handler`.
#[must_use]
pub fn error_disposition(level: ErrorLevel) -> Disposition {
    let _ = level;
    with(|g| {
        if !g.use_soap_error_handler {
            return Disposition::Normal;
        }
        match &g.error_object {
            Some(ErrorObject::Client(_, exceptions)) => {
                if *exceptions && g.error_code == Some("WSDL") {
                    Disposition::Suppress
                } else {
                    Disposition::Normal
                }
            }
            Some(ErrorObject::Server(_)) => Disposition::Suppress,
            None => Disposition::Normal,
        }
    })
}

/// Sets `SOAP_GLOBAL(error_code)`, returning the previous value.
pub fn set_error_code(code: Option<&'static str>) -> Option<&'static str> {
    with(|g| std::mem::replace(&mut g.error_code, code))
}

/// `SOAP_GLOBAL(error_code)`.
#[must_use]
pub fn error_code() -> Option<&'static str> {
    with(|g| g.error_code)
}
