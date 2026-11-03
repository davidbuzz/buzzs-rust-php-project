//! Stream filters (`main/streams/filter.c`, `ext/standard/user_filters.c`).
//!
//! A filter transforms bucket brigades. The read chain is wound when the
//! stream's read buffer is filled; the write chain when data is written or
//! flushed. Bundled filters live in submodules; extensions (zlib, bz2, iconv)
//! and user-space filters (`php_user_filter` subclasses) register factories
//! through [`FilterRegistry`].

use std::collections::VecDeque;
use std::fmt;
use std::rc::Rc;

use crate::diag;
use crate::value::Value;

pub mod consumed;
pub mod convert;
pub mod dechunk;
pub mod standard;
pub mod string;
pub mod user;

pub use user::{UserFilter, UserFilterFactory, UserFilterObject};

/// One bucket of data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bucket {
    /// The bucket contents.
    pub data: Vec<u8>,
}

impl Bucket {
    /// Creates a bucket.
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        Bucket { data: data.into() }
    }
}

/// A bucket brigade (`php_stream_bucket_brigade`).
pub type Brigade = VecDeque<Bucket>;

/// `php_stream_filter_status_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterStatus {
    /// `PSFS_ERR_FATAL`: error in the data stream.
    ErrFatal,
    /// `PSFS_FEED_ME`: the filter needs more data.
    FeedMe,
    /// `PSFS_PASS_ON`: the filter produced output buckets.
    PassOn,
}

impl FilterStatus {
    /// Maps the integer a user filter returns (`PSFS_*` constants).
    pub fn from_long(v: i64) -> Self {
        match v {
            2 => FilterStatus::PassOn,
            1 => FilterStatus::FeedMe,
            _ => FilterStatus::ErrFatal,
        }
    }

    /// The `PSFS_*` integer value.
    pub fn as_long(self) -> i64 {
        match self {
            FilterStatus::ErrFatal => 0,
            FilterStatus::FeedMe => 1,
            FilterStatus::PassOn => 2,
        }
    }
}

/// `PSFS_FLAG_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterFlags {
    /// `PSFS_FLAG_NORMAL`.
    Normal,
    /// `PSFS_FLAG_FLUSH_INC`.
    FlushInc,
    /// `PSFS_FLAG_FLUSH_CLOSE`.
    FlushClose,
}

impl FilterFlags {
    /// Whether this is the closing flush (`$closing` for user filters).
    pub fn is_closing(self) -> bool {
        self == FilterFlags::FlushClose
    }

    /// Whether any flush was requested.
    pub fn is_flush(self) -> bool {
        self != FilterFlags::Normal
    }
}

/// `php_stream_filter_seekable_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seekable {
    /// `PSFS_SEEKABLE_NEVER`.
    Never,
    /// `PSFS_SEEKABLE_START`.
    Start,
    /// `PSFS_SEEKABLE_CHECK`.
    Check,
    /// `PSFS_SEEKABLE_ALWAYS`.
    Always,
}

/// Which chain a filter is attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainKind {
    /// The read chain (`PHP_STREAM_FILTER_READ` = 1).
    Read,
    /// The write chain (`PHP_STREAM_FILTER_WRITE` = 2).
    Write,
}

impl ChainKind {
    /// The `PHP_STREAM_FILTER_*` constant.
    pub fn as_long(self) -> i64 {
        match self {
            ChainKind::Read => 1,
            ChainKind::Write => 2,
        }
    }
}

/// Information about the owning stream passed to filters.
///
/// Filters cannot borrow the stream (it is being read or written), so stream
/// operations a C filter would perform directly are expressed as requests
/// applied by the stream once the chain has run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterEnv {
    /// The owning stream's id (`$this->stream` for user filters).
    pub stream_id: u64,
    /// The stream position (`php_stream_tell()`).
    pub position: i64,
    /// The chain being run.
    pub chain: ChainKind,
    /// A seek the filter asks for (`php_stream_seek(stream, pos, SEEK_SET)`).
    pub seek_request: Option<i64>,
}

impl FilterEnv {
    /// Creates an environment for a detached run (tests, `stream_filter_*`
    /// helpers).
    pub fn detached(chain: ChainKind) -> Self {
        FilterEnv {
            stream_id: 0,
            position: 0,
            chain,
            seek_request: None,
        }
    }
}

/// A stream filter instance (`php_stream_filter` + its ops).
pub trait StreamFilter: fmt::Debug {
    /// The filter ops label (`"string.rot13"`, `"user-filter"`, …).
    fn label(&self) -> &str;

    /// Processes `input` into `output`. `consumed`, when present, receives
    /// the number of input bytes consumed.
    fn filter(
        &mut self,
        env: &mut FilterEnv,
        input: &mut Brigade,
        output: &mut Brigade,
        consumed: Option<&mut usize>,
        flags: FilterFlags,
    ) -> FilterStatus;

    /// Seek notification (`fops->seek`); returns false on failure.
    fn seek(&mut self, _env: &mut FilterEnv, _offset: i64, _whence: i32) -> bool {
        true
    }

    /// Seekability on a read chain.
    fn read_seekable(&self) -> Seekable {
        Seekable::Always
    }

    /// Seekability on a write chain.
    fn write_seekable(&self) -> Seekable {
        Seekable::Always
    }

    /// Destructor (`fops->dtor`; calls `onClose()` for user filters).
    fn close(&mut self) {}
}

/// Creates filter instances (`php_stream_filter_factory`).
pub trait FilterFactory: fmt::Debug {
    /// Creates a filter for `name` (the full requested name, even when the
    /// factory was found through a wildcard), or `None` when it cannot.
    fn create(
        &self,
        name: &str,
        params: Option<&Value>,
        persistent: bool,
    ) -> Option<Box<dyn StreamFilter>>;
}

/// An identifier for a filter attached to a stream (the filter resource
/// returned by `stream_filter_append()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FilterId(pub u64);

thread_local! {
    static NEXT_FILTER_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

impl FilterId {
    fn fresh() -> Self {
        NEXT_FILTER_ID.with(|n| {
            let id = n.get();
            n.set(id + 1);
            FilterId(id)
        })
    }
}

/// A filter attached to a chain.
#[derive(Debug)]
pub struct AttachedFilter {
    /// Its id.
    pub id: FilterId,
    /// The filter.
    pub filter: Box<dyn StreamFilter>,
}

/// A filter chain (`php_stream_filter_chain`).
#[derive(Debug, Default)]
pub struct FilterChain {
    /// The filters, head first.
    pub filters: Vec<AttachedFilter>,
}

impl FilterChain {
    /// Whether the chain is empty.
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// Number of filters (`num_filters`).
    pub fn len(&self) -> usize {
        self.filters.len()
    }

    /// Appends a filter, returning its id.
    pub fn push_back(&mut self, filter: Box<dyn StreamFilter>) -> FilterId {
        let id = FilterId::fresh();
        self.filters.push(AttachedFilter { id, filter });
        id
    }

    /// Prepends a filter, returning its id.
    pub fn push_front(&mut self, filter: Box<dyn StreamFilter>) -> FilterId {
        let id = FilterId::fresh();
        self.filters.insert(0, AttachedFilter { id, filter });
        id
    }

    /// Removes a filter by id.
    pub fn remove(&mut self, id: FilterId) -> Option<Box<dyn StreamFilter>> {
        let pos = self.filters.iter().position(|f| f.id == id)?;
        Some(self.filters.remove(pos).filter)
    }

    /// Position of a filter.
    pub fn position(&self, id: FilterId) -> Option<usize> {
        self.filters.iter().position(|f| f.id == id)
    }

    /// Runs the chain from `start` on `input` with `flags`. Returns the
    /// final status and the output brigade (for `PassOn`). `consumed` gets
    /// the byte count consumed by the first filter run.
    pub fn run(
        &mut self,
        start: usize,
        env: &mut FilterEnv,
        input: Brigade,
        mut consumed: Option<&mut usize>,
        flags: FilterFlags,
    ) -> (FilterStatus, Brigade) {
        let mut inp = input;
        let mut status = FilterStatus::ErrFatal;
        for (i, f) in self.filters.iter_mut().enumerate().skip(start) {
            let mut out = Brigade::new();
            let c = if i == start {
                consumed.as_deref_mut()
            } else {
                None
            };
            status = f.filter.filter(env, &mut inp, &mut out, c, flags);
            if status != FilterStatus::PassOn {
                return (status, Brigade::new());
            }
            inp = out;
        }
        (status, inp)
    }
}

/// `php_stream_filter_parse_write_seek_mode()`: the `write_seek_mode`
/// filter parameter. Emits PHP's warning and returns `None` for an invalid
/// value.
pub fn parse_write_seek_mode(params: Option<&Value>) -> Option<Seekable> {
    let Some(v) = params.and_then(|p| p.get("write_seek_mode")) else {
        return Some(Seekable::Always);
    };
    match v.to_bytes().unwrap_or_default().as_slice() {
        b"preserve" => Some(Seekable::Always),
        b"reset" => Some(Seekable::Start),
        b"strict" => Some(Seekable::Never),
        _ => {
            diag::warning(
                "\"write_seek_mode\" filter parameter must be one of \"preserve\", \"reset\", or \"strict\"",
            );
            None
        }
    }
}

/// The filter factory registry (`stream_filters_hash` plus the per-request
/// volatile copy).
#[derive(Debug, Default, Clone)]
pub struct FilterRegistry {
    factories: Vec<(String, Rc<dyn FilterFactory>)>,
}

impl FilterRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a factory; fails when the pattern already exists.
    pub fn register(&mut self, pattern: &str, factory: Rc<dyn FilterFactory>) -> bool {
        if self.factories.iter().any(|(n, _)| n == pattern) {
            return false;
        }
        self.factories.push((pattern.to_string(), factory));
        true
    }

    /// Unregisters a factory.
    pub fn unregister(&mut self, pattern: &str) -> bool {
        let before = self.factories.len();
        self.factories.retain(|(n, _)| n != pattern);
        before != self.factories.len()
    }

    /// Whether a pattern is registered.
    pub fn contains(&self, pattern: &str) -> bool {
        self.factories.iter().any(|(n, _)| n == pattern)
    }

    /// Registered patterns in registration order (`stream_get_filters()`).
    pub fn names(&self) -> Vec<String> {
        self.factories.iter().map(|(n, _)| n.clone()).collect()
    }

    fn find(&self, name: &str) -> Option<Rc<dyn FilterFactory>> {
        self.factories
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, f)| Rc::clone(f))
    }

    /// `php_stream_filter_create()`: exact lookup, then wildcard patterns
    /// (`a.b.c` tries `a.b.*` then `a.*`). Emits PHP's warnings on failure.
    pub fn create(
        &self,
        name: &str,
        params: Option<&Value>,
        persistent: bool,
    ) -> Option<Box<dyn StreamFilter>> {
        let mut factory = self.find(name);
        let mut filter = None;
        if let Some(f) = &factory {
            filter = f.create(name, params, persistent);
        } else if let Some(mut period) = name.rfind('.') {
            loop {
                let wild = format!("{}.*", &name[..period]);
                if let Some(f) = self.find(&wild) {
                    filter = f.create(name, params, persistent);
                    factory = Some(f);
                }
                if filter.is_some() {
                    break;
                }
                match name[..period].rfind('.') {
                    Some(p) => period = p,
                    None => break,
                }
            }
        }
        if filter.is_none() {
            if factory.is_none() {
                diag::warning(format!("Unable to locate filter \"{name}\""));
            } else {
                diag::warning(format!("Unable to create or locate filter \"{name}\""));
            }
        }
        filter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Upper;
    impl StreamFilter for Upper {
        fn label(&self) -> &str {
            "t.upper"
        }
        fn filter(
            &mut self,
            _env: &mut FilterEnv,
            input: &mut Brigade,
            output: &mut Brigade,
            consumed: Option<&mut usize>,
            _flags: FilterFlags,
        ) -> FilterStatus {
            let mut n = 0;
            while let Some(mut b) = input.pop_front() {
                n += b.data.len();
                b.data.make_ascii_uppercase();
                output.push_back(b);
            }
            if let Some(c) = consumed {
                *c = n;
            }
            FilterStatus::PassOn
        }
    }

    #[derive(Debug)]
    struct UpperFactory;
    impl FilterFactory for UpperFactory {
        fn create(&self, name: &str, _: Option<&Value>, _: bool) -> Option<Box<dyn StreamFilter>> {
            (name.starts_with("t.")).then(|| Box::new(Upper) as Box<dyn StreamFilter>)
        }
    }

    #[test]
    fn wildcard_lookup_and_errors() {
        let _ = diag::take();
        let mut reg = FilterRegistry::new();
        assert!(reg.register("t.*", Rc::new(UpperFactory)));
        assert!(!reg.register("t.*", Rc::new(UpperFactory)));
        assert!(reg.create("t.upper.x", None, false).is_some());
        assert!(reg.create("nope", None, false).is_none());
        assert_eq!(diag::take()[0].text(), "Unable to locate filter \"nope\"");
        reg.register("u.*", Rc::new(UpperFactory));
        assert!(reg.create("u.x", None, false).is_none());
        assert_eq!(
            diag::take()[0].text(),
            "Unable to create or locate filter \"u.x\""
        );
        assert_eq!(reg.names(), vec!["t.*".to_string(), "u.*".to_string()]);
    }

    #[test]
    fn chain_run() {
        let mut chain = FilterChain::default();
        chain.push_back(Box::new(Upper));
        let id = chain.push_front(Box::new(Upper));
        assert_eq!(chain.len(), 2);
        let mut env = FilterEnv::detached(ChainKind::Read);
        let mut consumed = 0;
        let (st, out) = chain.run(
            0,
            &mut env,
            Brigade::from(vec![Bucket::new("ab")]),
            Some(&mut consumed),
            FilterFlags::Normal,
        );
        assert_eq!(st, FilterStatus::PassOn);
        assert_eq!(out[0].data, b"AB");
        assert_eq!(consumed, 2);
        assert!(chain.remove(id).is_some());
        assert_eq!(chain.len(), 1);
    }

    #[test]
    fn write_seek_mode() {
        let _ = diag::take();
        assert_eq!(parse_write_seek_mode(None), Some(Seekable::Always));
        let p = Value::Array(vec![(
            crate::value::Key::str("write_seek_mode"),
            Value::str("reset"),
        )]);
        assert_eq!(parse_write_seek_mode(Some(&p)), Some(Seekable::Start));
        let p = Value::Array(vec![(
            crate::value::Key::str("write_seek_mode"),
            Value::str("x"),
        )]);
        assert_eq!(parse_write_seek_mode(Some(&p)), None);
        assert_eq!(diag::take().len(), 1);
    }
}
