// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Call-backs into the embedding PHP engine.

use std::rc::Rc;

use crate::error::DomError;
use crate::object::Node;

/// An opaque, already-validated PHP callable owned by the embedder.
pub type HostCallable = Rc<dyn std::any::Any>;

/// An argument passed to an `XPath` PHP function.
#[derive(Debug, Clone)]
pub enum XPathArg {
    /// `string`.
    Str(Vec<u8>),
    /// `bool`.
    Bool(bool),
    /// `float`.
    Float(f64),
    /// A node-set (array of node objects).
    Nodes(Vec<Node>),
}

/// The value returned by an `XPath` PHP function.
#[derive(Debug, Clone)]
pub enum XPathReturn {
    /// No value (the call produced nothing, e.g. after an exception).
    Undef,
    /// A DOM node of the right family.
    Node(Node),
    /// `true`/`false`.
    Bool(bool),
    /// Any other object (not a DOM node of the right family): a
    /// `TypeError` is raised.
    NonNodeObject,
    /// Any other value, already converted with `(string)`.
    Str(Vec<u8>),
}

/// A stream opened for reading by the host (`php_stream_open_wrapper_ex`).
pub struct HostStream {
    /// The byte source.
    pub reader: Box<dyn std::io::Read>,
    /// Charset sniffed from transport metadata (HTTP `Content-Type`).
    pub charset: Option<Vec<u8>>,
    /// The resolved path when this is a plain file (`opened_path` with the
    /// plain-files wrapper); used for the document URI.
    pub plain_file_path: Option<Vec<u8>>,
}

impl std::fmt::Debug for HostStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostStream")
            .field("charset", &self.charset)
            .field("plain_file_path", &self.plain_file_path)
            .finish_non_exhaustive()
    }
}

fn os_error_text(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".into(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".into(),
        _ => {
            let s = e.to_string();
            s.split(" (os error").next().unwrap_or(&s).to_string()
        }
    }
}

/// The engine services the DOM extension needs.
pub trait DomHost {
    /// `instanceof`: whether class `class` (case-insensitive) is `base` or a
    /// subclass of it. Unknown classes return false.
    fn is_subclass_of(&self, class: &str, base: &str) -> bool;

    /// Looks a class up as a `C` parameter does (`zend_lookup_class`): the
    /// autoloader runs for a class that is not declared yet. `Ok(false)`:
    /// there is no such class; an exception the autoloader throws is the
    /// error. The default knows every class (a host without autoloading).
    fn load_class(&self, class: &str) -> Result<bool, DomError> {
        let _ = class;
        Ok(true)
    }

    /// Whether the class is abstract.
    fn is_abstract(&self, class: &str) -> bool {
        let _ = class;
        false
    }

    /// The declared spelling of a class name, if the class exists.
    fn canonical_name(&self, class: &str) -> Option<String> {
        Some(class.to_string())
    }

    /// `zend_update_class_constants()`: evaluates the class's constant
    /// expressions (an undefined constant throws).
    ///
    /// # Errors
    /// The exception the evaluation throws.
    fn update_class_constants(&self, class: &str) -> Result<(), DomError> {
        let _ = class;
        Ok(())
    }

    /// Resolves a callable given by name (`zend_is_callable`): the error
    /// is the text PHP appends after "must be a callable, ".
    fn resolve_callable(&self, name: &[u8]) -> Result<HostCallable, String> {
        Ok(Rc::new(name.to_vec()))
    }

    /// Calls a registered callable with `XPath` arguments.
    fn call(&self, callable: &HostCallable, args: Vec<XPathArg>) -> Result<XPathReturn, DomError>;

    /// Calls a PHP function by name (`registerPhpFunctions()` without a
    /// restriction list).
    fn call_by_name(&self, name: &[u8], args: Vec<XPathArg>) -> Result<XPathReturn, DomError>;

    /// Whether `node` is an instance of the DOM node class of the given
    /// family (always true for nodes produced by this crate).
    fn is_dom_node(&self, node: &Node, modern: bool) -> bool {
        node.family_is_modern() == modern
    }

    /// Opens `path` for reading through the stream layer with
    /// `REPORT_ERRORS` (the host emits "Failed to open stream" warnings
    /// itself). `Err` carries an exception thrown by a user wrapper. The
    /// default opens plain files.
    fn open_read_stream(&self, path: &[u8]) -> Result<Option<HostStream>, DomError> {
        let p = std::path::PathBuf::from(String::from_utf8_lossy(path).into_owned());
        let p = p
            .strip_prefix("file://")
            .map(std::path::Path::to_path_buf)
            .unwrap_or(p);
        match std::fs::File::open(&p) {
            Ok(f) => {
                let abs = std::fs::canonicalize(&p).unwrap_or(p);
                Ok(Some(HostStream {
                    reader: Box::new(f),
                    charset: None,
                    plain_file_path: Some(abs.to_string_lossy().into_owned().into_bytes()),
                }))
            }
            Err(e) => {
                crate::error::warning(format!("Failed to open stream: {}", os_error_text(&e)));
                Ok(None)
            }
        }
    }

    /// Opens `path` for writing (mode `"wb"`) with `REPORT_ERRORS`.
    fn open_write_stream(&self, path: &[u8]) -> Result<Option<Box<dyn std::io::Write>>, DomError> {
        let p = String::from_utf8_lossy(path).into_owned();
        match std::fs::File::create(&p) {
            Ok(f) => Ok(Some(Box::new(f))),
            Err(e) => {
                crate::error::warning(format!("Failed to open stream: {}", os_error_text(&e)));
                Ok(None)
            }
        }
    }
}

/// A host with no PHP callbacks (useful for tests and tools).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHost;

impl DomHost for NoHost {
    fn is_subclass_of(&self, class: &str, base: &str) -> bool {
        class.eq_ignore_ascii_case(base)
    }

    fn call(
        &self,
        _callable: &HostCallable,
        _args: Vec<XPathArg>,
    ) -> Result<XPathReturn, DomError> {
        Ok(XPathReturn::Undef)
    }

    fn call_by_name(&self, name: &[u8], _args: Vec<XPathArg>) -> Result<XPathReturn, DomError> {
        Err(DomError::Error(format!(
            "Invalid callback {}, function \"{}\" not found or invalid function name",
            String::from_utf8_lossy(name),
            String::from_utf8_lossy(name)
        )))
    }
}
