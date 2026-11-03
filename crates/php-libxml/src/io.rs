//! I/O hooks: the stream layer libxml2 reads and writes files through,
//! the user external entity loader and the entity-loader kill switch.
//!
//! PHP channels every file libxml2 opens (documents, DTDs, external
//! entities, `XMLWriter::openUri()` targets) through PHP streams so that
//! wrappers, `open_basedir` and stream contexts apply. Here the VM installs
//! [`StreamHooks`] (two closures) with [`set_stream_hooks`]; without hooks
//! libxml2's own file/HTTP I/O is used.
//!
//! `libxml_set_external_entity_loader()` maps to
//! [`set_external_entity_loader`]: the closure receives the public and
//! system identifiers plus the parser's context strings and answers with an
//! [`EntityLoaderResult`].

use std::any::Any;
use std::cell::RefCell;
use std::io::{Read, Write};
use std::rc::Rc;

/// An opened input stream handed to libxml2.
pub struct InputStream {
    /// The byte source; dropped when libxml2 closes the input.
    pub reader: Box<dyn Read>,
    /// Charset sniffed from transport metadata (e.g. an HTTP
    /// `Content-Type`), used when libxml2 has no encoding hint.
    pub charset: Option<Vec<u8>>,
}

impl std::fmt::Debug for InputStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputStream")
            .field("charset", &self.charset)
            .finish_non_exhaustive()
    }
}

/// Opens a path for reading: receives the resolved path (URI-unescaped
/// for `file:` and scheme-less URIs) and returns `None` when the stream
/// cannot be opened. PHP checks `url_stat` quietly first for read-only
/// opens so that missing DTDs do not raise stream warnings.
pub type OpenReadFn = dyn Fn(&[u8]) -> Option<InputStream>;
/// Opens a path for writing (mode `"wb"`).
pub type OpenWriteFn = dyn Fn(&[u8]) -> Option<Box<dyn Write>>;

/// The stream layer installed by the VM.
#[derive(Default)]
pub struct StreamHooks {
    /// Reader factory (`php_libxml_streams_IO_open_read_wrapper`).
    pub open_read: Option<Box<OpenReadFn>>,
    /// Writer factory (`php_libxml_streams_IO_open_write_wrapper`).
    pub open_write: Option<Box<OpenWriteFn>>,
}

impl std::fmt::Debug for StreamHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamHooks")
            .field("open_read", &self.open_read.is_some())
            .field("open_write", &self.open_write.is_some())
            .finish()
    }
}

/// The context array passed as third argument to a user entity loader.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EntityLoaderContext {
    /// `ctxt->directory`.
    pub directory: Option<Vec<u8>>,
    /// `ctxt->intSubName`.
    pub int_sub_name: Option<Vec<u8>>,
    /// `ctxt->extSubURI`.
    pub ext_sub_uri: Option<Vec<u8>>,
    /// `ctxt->extSubSystem`.
    pub ext_sub_system: Option<Vec<u8>>,
}

impl EntityLoaderContext {
    /// The keys of the PHP array, in order, with their values.
    pub fn entries(&self) -> [(&'static str, Option<&[u8]>); 4] {
        [
            ("directory", self.directory.as_deref()),
            ("intSubName", self.int_sub_name.as_deref()),
            ("extSubURI", self.ext_sub_uri.as_deref()),
            ("extSubSystem", self.ext_sub_system.as_deref()),
        ]
    }
}

/// A request to the user entity loader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityRequest {
    /// Public identifier (`$public_id`), `None` = PHP `null`.
    pub public_id: Option<Vec<u8>>,
    /// System identifier / URL (`$system_id`), `None` = PHP `null`.
    pub system_id: Option<Vec<u8>>,
    /// Parser context strings (`$context`).
    pub context: EntityLoaderContext,
}

/// What the user entity loader returned.
pub enum EntityLoaderResult {
    /// The call failed (no return value, e.g. an exception was thrown).
    /// Carries the callback's function name for the error message.
    Failed {
        /// Function name used in "Call to user entity loader callback '%s' has failed".
        function_name: Vec<u8>,
    },
    /// `null`, or a resource that is not a stream (after the VM has thrown
    /// its `TypeError`).
    Null,
    /// A string: a path/URL libxml2 opens itself (`xmlNewInputFromFile`).
    Path(Vec<u8>),
    /// A stream resource: its contents are parsed.
    Stream(Box<dyn Read>),
}

impl std::fmt::Debug for EntityLoaderResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed { function_name } => f
                .debug_struct("Failed")
                .field("function_name", function_name)
                .finish(),
            Self::Null => f.write_str("Null"),
            Self::Path(p) => f.debug_tuple("Path").field(p).finish(),
            Self::Stream(_) => f.write_str("Stream(..)"),
        }
    }
}

/// A user entity loader.
pub type EntityLoader = dyn Fn(&EntityRequest) -> EntityLoaderResult;

#[derive(Default)]
struct IoState {
    hooks: Option<Rc<StreamHooks>>,
    loader: Option<Rc<EntityLoader>>,
    loader_disabled: bool,
    stream_context: Option<Rc<dyn Any>>,
}

thread_local! {
    static IO: RefCell<IoState> = RefCell::new(IoState::default());
}

/// Installs the stream layer used for all libxml2 file I/O on this
/// thread; `None` restores libxml2's native I/O.
pub fn set_stream_hooks(hooks: Option<StreamHooks>) {
    crate::raw::ensure_init();
    IO.with(|s| s.borrow_mut().hooks = hooks.map(Rc::new));
}

pub(crate) fn stream_hooks() -> Option<Rc<StreamHooks>> {
    IO.with(|s| s.borrow().hooks.clone())
}

/// `libxml_set_external_entity_loader()`: `None` restores the default.
pub fn set_external_entity_loader(loader: Option<Rc<EntityLoader>>) {
    crate::raw::ensure_init();
    IO.with(|s| s.borrow_mut().loader = loader);
}

/// Whether a user entity loader is installed.
pub fn has_external_entity_loader() -> bool {
    IO.with(|s| s.borrow().loader.is_some())
}

pub(crate) fn external_entity_loader() -> Option<Rc<EntityLoader>> {
    IO.with(|s| s.borrow().loader.clone())
}

/// `libxml_disable_entity_loader()` / `php_libxml_disable_entity_loader`:
/// returns the previous setting.
pub fn disable_entity_loader(disable: bool) -> bool {
    IO.with(|s| std::mem::replace(&mut s.borrow_mut().loader_disabled, disable))
}

/// Whether file loading through the input-buffer factory is disabled.
pub fn entity_loader_disabled() -> bool {
    IO.with(|s| s.borrow().loader_disabled)
}

/// `libxml_set_streams_context()`: stores an opaque stream context that
/// the VM's [`StreamHooks`] can retrieve with [`streams_context`].
pub fn set_streams_context(context: Option<Rc<dyn Any>>) {
    IO.with(|s| s.borrow_mut().stream_context = context);
}

/// The context stored by [`set_streams_context`].
pub fn streams_context() -> Option<Rc<dyn Any>> {
    IO.with(|s| s.borrow().stream_context.clone())
}

pub(crate) fn reset_request_state() {
    IO.with(|s| {
        let mut s = s.borrow_mut();
        s.loader = None;
        s.loader_disabled = false;
        s.stream_context = None;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disable_entity_loader_returns_previous() {
        reset_request_state();
        assert!(!disable_entity_loader(true));
        assert!(entity_loader_disabled());
        assert!(disable_entity_loader(false));
        assert!(!entity_loader_disabled());
    }

    #[test]
    fn streams_context_is_opaque_storage() {
        set_streams_context(Some(Rc::new(42_i32)));
        let ctx = streams_context().expect("stored");
        assert_eq!(ctx.downcast_ref::<i32>(), Some(&42));
        reset_request_state();
        assert!(streams_context().is_none());
    }

    #[test]
    fn context_entries_order() {
        let ctx = EntityLoaderContext {
            directory: Some(b"/d".to_vec()),
            ..Default::default()
        };
        let keys: Vec<_> = ctx.entries().iter().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            ["directory", "intSubName", "extSubURI", "extSubSystem"]
        );
    }
}
