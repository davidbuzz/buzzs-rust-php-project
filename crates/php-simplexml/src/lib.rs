//! PHP's `ext/simplexml`: `SimpleXMLElement`, `SimpleXMLIterator` and the
//! `simplexml_*()` functions over the system libxml2 (through
//! `php-libxml`'s reference-counted tree).
//!
//! * [`Sxe`] — the state of one object (`php_sxe_object`): document, node
//!   and the iteration mode with its name/namespace filter. Its associated
//!   functions are the object handlers (property/dimension read, write,
//!   isset, unset, `get_property_ptr_ptr`, casts, property tables,
//!   comparison, count, clone) and the class methods.
//! * [`SxeHost`] — what the VM provides: object creation for result
//!   elements and access to the state behind an object, plus
//!   [`php_runtime::RuntimeContext`] for diagnostics and conversions.
//! * [`class::class_table`] / [`class::FUNCTIONS`] — declarations for
//!   generic registration.
//!
//! Errors are [`php_runtime::RuntimeError`]s (aliased as [`SxeError`]);
//! warnings raised with `php_error_docref` go through
//! [`php_runtime::RuntimeContext::diagnostic`] without the
//! `"SimpleXMLElement::method(): "` prefix, which the VM adds. libxml2's
//! own parse errors are reported through `php_libxml::error`.

mod ffi_extra;
mod sxe;
mod xnode;

pub mod class;

pub use sxe::{CheckKind, FetchMode, IterType, PropertyPtr, Sxe, SxeHost, SxeRef};
pub use xnode::XNode;

/// The error type of `SimpleXML` operations.
pub type SxeError = php_runtime::RuntimeError;
