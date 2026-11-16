// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The native state behind `CurlHandle`, `CurlMultiHandle` and
//! `CurlShareHandle` objects (`php_curl`, `php_curlm`, `php_curlsh`).

use php_curl::{Easy, Multi, Share};
use php_runtime::{Obj, Value};
use php_vm::object;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// What a write/header/read handler does (`PHP_CURL_STDOUT`...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Method {
    /// Write to the output (`PHP_CURL_STDOUT`).
    #[default]
    Stdout,
    /// Write to a stream (`PHP_CURL_FILE`).
    File,
    /// Call a user function (`PHP_CURL_USER`).
    User,
    /// Read from a stream (`PHP_CURL_DIRECT`).
    Direct,
    /// Collect for `curl_exec()`'s return value (`PHP_CURL_RETURN`).
    Return,
    /// Discard (`PHP_CURL_IGNORE`).
    Ignore,
}

/// `php_curl_write` / `php_curl_read`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Io {
    pub(crate) method: Method,
    pub(crate) fcc: Option<Value>,
    pub(crate) stream: Option<Value>,
    /// The `RETURNTRANSFER` buffer (`smart_str`; `None` = never written).
    pub(crate) buf: Option<Vec<u8>>,
}

/// The PHP-side handler state of an easy handle.
#[derive(Debug, Clone, Default)]
pub(crate) struct State {
    pub(crate) write: Io,
    pub(crate) write_header: Io,
    pub(crate) read: Io,
    pub(crate) std_err: Option<Value>,
    pub(crate) seek: Option<Value>,
    pub(crate) progress: Option<Value>,
    pub(crate) xferinfo: Option<Value>,
    pub(crate) fnmatch: Option<Value>,
    pub(crate) debug: Option<Value>,
    pub(crate) prereq: Option<Value>,
    pub(crate) sshhostkey: Option<Value>,
    /// `ch->header.str` (`CURLINFO_HEADER_OUT`).
    pub(crate) header_out: Option<Vec<u8>>,
    pub(crate) postfields: Option<Value>,
    pub(crate) private: Option<Value>,
    pub(crate) share: Option<Value>,
}

impl State {
    /// The state of a fresh handle (`curl_init()`).
    pub(crate) fn fresh() -> State {
        let mut s = State::default();
        s.write.method = Method::Stdout;
        s.read.method = Method::Direct;
        s.write_header.method = Method::Ignore;
        s
    }
}

/// `php_curl`: the libcurl handle plus its PHP-side state.
#[derive(Debug)]
pub(crate) struct Curl {
    /// Declared first: cleaned up before the state (which keeps the share
    /// handle alive) is released.
    pub(crate) easy: Easy,
    pub(crate) st: RefCell<State>,
    pub(crate) err_no: Cell<i32>,
    pub(crate) in_callback: Cell<bool>,
}

impl Curl {
    pub(crate) fn new(easy: Easy, st: State) -> Rc<Curl> {
        Rc::new(Curl {
            easy,
            st: RefCell::new(st),
            err_no: Cell::new(0),
            in_callback: Cell::new(false),
        })
    }
}

/// `php_curlm`.
#[derive(Debug)]
pub(crate) struct CurlMulti {
    /// Declared first: `curl_multi_cleanup()` runs before the easy handles
    /// are released.
    pub(crate) multi: Multi,
    pub(crate) easyh: RefCell<Vec<Obj>>,
    pub(crate) push: RefCell<Option<Value>>,
    pub(crate) err_no: Cell<i32>,
}

/// `php_curlsh`.
#[derive(Debug)]
pub(crate) struct CurlShare {
    pub(crate) share: Share,
    pub(crate) err_no: Cell<i32>,
}

/// The easy state of a `CurlHandle` object.
pub(crate) fn curl_of(o: &Obj) -> Option<Rc<Curl>> {
    object::internal::<Rc<Curl>, _>(o, |c| Rc::clone(c))
}

/// The multi state of a `CurlMultiHandle` object.
pub(crate) fn multi_of(o: &Obj) -> Option<Rc<CurlMulti>> {
    object::internal::<Rc<CurlMulti>, _>(o, |c| Rc::clone(c))
}

/// The share state of a `CurlShareHandle`/`CurlSharePersistentHandle`.
pub(crate) fn share_of(o: &Obj) -> Option<Rc<CurlShare>> {
    object::internal::<Rc<CurlShare>, _>(o, |c| Rc::clone(c))
}
