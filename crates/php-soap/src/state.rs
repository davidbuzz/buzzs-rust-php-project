// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Per-object extension state (the C structs wrapping `zend_object`).

use std::cell::RefCell;
use std::rc::Rc;

use crate::encoding::Typemap;
use crate::sdl::Sdl;
use crate::server::Service;

/// State attached to objects of the SOAP classes.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // one per object; boxing buys nothing
pub enum SoapObject {
    /// `soap_client_object`: the typemap built by the constructor.
    Client(RefCell<Option<Rc<Typemap>>>),
    /// `Soap\Sdl`: the loaded service description.
    Sdl(RefCell<Option<Rc<Sdl>>>),
    /// `soap_server_object`: the service.
    Server(RefCell<Option<Service>>),
}

/// Creates the state for a new object whose closest internal SOAP class is
/// `class` (`soapclient`, `soapserver`, `soap\sdl`, `soap\url`; any case).
#[must_use]
pub fn create_state(class: &str) -> Option<Rc<SoapObject>> {
    match class.to_ascii_lowercase().as_str() {
        "soapclient" => Some(Rc::new(SoapObject::Client(RefCell::new(None)))),
        "soap\\sdl" => Some(Rc::new(SoapObject::Sdl(RefCell::new(None)))),
        "soapserver" => Some(Rc::new(SoapObject::Server(RefCell::new(None)))),
        _ => None,
    }
}
