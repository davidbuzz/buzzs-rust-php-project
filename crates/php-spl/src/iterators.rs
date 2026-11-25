// Derived from php-src ext/spl/spl_iterators.c. Similarity metric to original: 18%
//   Copyright © The PHP Group and Contributors. Authors: Marcus Boerger <helly@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The iterator classes of `spl_iterators.c`: `IteratorIterator` and the
//! classes built on `spl_dual_it_object` (filters, `LimitIterator`,
//! `CachingIterator`, `NoRewindIterator`, `AppendIterator`,
//! `InfiniteIterator`, `RegexIterator`, ...), `RecursiveIteratorIterator`
//! and `RecursiveTreeIterator`, and `EmptyIterator`.

use crate::args::Args;
use crate::exceptions::{
    bad_method_call, error, invalid_argument, logic, out_of_bounds, unexpected_value,
};
use crate::host::{ObjectIterator, SplHost};
use crate::object::{state_of, SplObject};
use crate::util::{this_obj, to_long};
use php_runtime::{
    ArrayKey, ErrorLevel, HashTable, Obj, RtResult, RuntimeError, Value, ZArray, ZStr,
};
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

/// A shared native iterator (`zend_object_iterator *`).
pub type ItRef = Rc<RefCell<Box<dyn ObjectIterator>>>;

fn it_call<R>(
    it: &ItRef,
    host: &mut dyn SplHost,
    f: impl FnOnce(&mut dyn ObjectIterator, &mut dyn SplHost) -> RtResult<R>,
) -> RtResult<R> {
    // User iterators are driven through their methods without holding the
    // borrow, so that their code may re-enter the outer iterator.
    let method_it = it.try_borrow().ok().and_then(|b| b.method_iter());
    if let Some(mut m) = method_it {
        return f(&mut m, host);
    }
    let mut b = it
        .try_borrow_mut()
        .map_err(|_| error("Iterator is already in use"))?;
    f(b.as_mut(), host)
}

fn new_it(host: &mut dyn SplHost, obj: &Obj) -> RtResult<ItRef> {
    let it = crate::iter::get_iterator(host, obj, false)?;
    Ok(Rc::new(RefCell::new(it)))
}

/// `CachingIterator::CALL_TOSTRING`.
pub const CIT_CALL_TOSTRING: i64 = 1;
/// `CachingIterator::TOSTRING_USE_KEY`.
pub const CIT_TOSTRING_USE_KEY: i64 = 2;
/// `CachingIterator::TOSTRING_USE_CURRENT`.
pub const CIT_TOSTRING_USE_CURRENT: i64 = 4;
/// `CachingIterator::TOSTRING_USE_INNER`.
pub const CIT_TOSTRING_USE_INNER: i64 = 8;
/// `CachingIterator::CATCH_GET_CHILD` / `RecursiveIteratorIterator::CATCH_GET_CHILD`.
pub const CIT_CATCH_GET_CHILD: i64 = 16;
/// `CachingIterator::FULL_CACHE`.
pub const CIT_FULL_CACHE: i64 = 256;
const CIT_PUBLIC: i64 = 0xFFFF;
const CIT_VALID: i64 = 0x0001_0000;

/// `RegexIterator::USE_KEY`.
pub const REGIT_USE_KEY: i64 = 1;
/// `RegexIterator::INVERT_MATCH`.
pub const REGIT_INVERTED: i64 = 2;
/// `RegexIterator::MATCH` .. `REPLACE`.
pub const REGIT_MODE_MATCH: i64 = 0;
const REGIT_MODE_GET_MATCH: i64 = 1;
const REGIT_MODE_ALL_MATCHES: i64 = 2;
const REGIT_MODE_SPLIT: i64 = 3;
const REGIT_MODE_REPLACE: i64 = 4;
const REGIT_MODE_MAX: i64 = 5;

/// `RecursiveIteratorIterator::LEAVES_ONLY`.
pub const RIT_LEAVES_ONLY: i64 = 0;
/// `RecursiveIteratorIterator::SELF_FIRST`.
pub const RIT_SELF_FIRST: i64 = 1;
/// `RecursiveIteratorIterator::CHILD_FIRST`.
pub const RIT_CHILD_FIRST: i64 = 2;
/// `RecursiveTreeIterator::BYPASS_CURRENT`.
pub const RTIT_BYPASS_CURRENT: i64 = 4;
/// `RecursiveTreeIterator::BYPASS_KEY`.
pub const RTIT_BYPASS_KEY: i64 = 8;

/// The variant-specific part of a dual iterator (`dit_type` + union).
#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub enum DualKind {
    /// Not constructed yet (`DIT_Unknown`).
    Unknown,
    /// `FilterIterator`, `RecursiveFilterIterator`, `ParentIterator`.
    Default,
    /// `IteratorIterator`.
    IteratorIterator,
    /// `NoRewindIterator`.
    NoRewind,
    /// `InfiniteIterator`.
    Infinite,
    /// `LimitIterator`.
    Limit {
        /// Start offset.
        offset: i64,
        /// Maximum count (-1 = all).
        count: i64,
    },
    /// `CachingIterator` / `RecursiveCachingIterator`.
    Caching {
        /// `CIT_*` flags (public and internal bits).
        flags: i64,
        /// The cached string representation.
        zstr: Option<ZStr>,
        /// The children (`RecursiveCachingIterator`).
        children: Option<Value>,
        /// The full cache.
        cache: ZArray,
        /// Whether this is a `RecursiveCachingIterator`.
        recursive: bool,
    },
    /// `AppendIterator`.
    Append {
        /// The native iterator of `array_it` (declared first: it is
        /// released before the `ArrayIterator`, as in
        /// `spl_dual_it_free_storage`).
        it: ItRef,
        /// The `ArrayIterator` holding the iterators.
        array_it: Obj,
    },
    /// `RegexIterator` / `RecursiveRegexIterator`.
    Regex {
        /// `USE_KEY`/`INVERT_MATCH`.
        flags: i64,
        /// PREG flags.
        preg_flags: i64,
        /// The pattern source.
        regex: ZStr,
        /// The compiled pattern.
        compiled: php_pcre::Regex,
        /// The mode.
        mode: i64,
    },
    /// `CallbackFilterIterator` / `RecursiveCallbackFilterIterator`.
    CallbackFilter {
        /// The callback.
        callback: Value,
    },
}

/// The state of `IteratorIterator` and its subclasses
/// (`spl_dual_it_object`).
///
/// The fields are declared in `spl_dual_it_free_storage`'s release order
/// (Rust drops them in declaration order): the current element, the inner
/// iterator (whose object handle is freed first), the inner object, then
/// the variant's parts, so freed handles are reused in php-src's order.
#[derive(Clone)]
pub struct DualIt {
    /// Current value (`None` = `UNDEF`).
    pub data: Option<Value>,
    /// Current key (`None` = `UNDEF`).
    pub key: Option<Value>,
    /// The inner native iterator.
    pub inner_it: Option<ItRef>,
    /// The inner object.
    pub inner: Option<Obj>,
    /// The class used for the inner object (downcast for `IteratorIterator`).
    pub inner_class: Option<ZStr>,
    /// The variant.
    pub kind: DualKind,
    /// Position.
    pub pos: i64,
}

impl fmt::Debug for DualIt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DualIt")
            .field("inner", &self.inner)
            .field("data", &self.data)
            .field("key", &self.key)
            .field("pos", &self.pos)
            .finish_non_exhaustive()
    }
}

impl DualIt {
    /// A new, unconstructed state.
    #[must_use]
    pub fn new(base: &str) -> Self {
        let _ = base;
        DualIt {
            kind: DualKind::Unknown,
            inner: None,
            inner_class: None,
            inner_it: None,
            data: None,
            key: None,
            pos: 0,
        }
    }
}

fn not_initialized() -> RuntimeError {
    error("The object is in an invalid state as the parent constructor was not called")
}

/// `SPL_FETCH_AND_CHECK_DUAL_IT`.
fn check(st: &SplObject) -> RtResult<()> {
    if st.dual(|d| matches!(d.kind, DualKind::Unknown))? {
        return Err(not_initialized());
    }
    Ok(())
}

fn inner_it(st: &SplObject) -> RtResult<Option<ItRef>> {
    st.dual(|d| d.inner_it.clone())
}

/// `spl_dual_it_free`.
fn dual_free(st: &SplObject) -> RtResult<()> {
    // `invalidate_current` of the inner iterator (drops the value a
    // `zend_user_iterator` cached).
    if let Some(it) = inner_it(st)? {
        let cached = it.try_borrow().ok().and_then(|b| b.method_iter());
        if let Some(m) = cached {
            m.invalidate_current();
        }
    }
    let garbage = st.dual(|d| {
        let g = (d.data.take(), d.key.take());
        if let DualKind::Caching { zstr, children, .. } = &mut d.kind {
            *zstr = None;
            let c = children.take();
            return (g, c);
        }
        (g, None)
    })?;
    drop(garbage);
    Ok(())
}

/// `spl_dual_it_rewind`.
fn dual_rewind(host: &mut dyn SplHost, st: &SplObject) -> RtResult<()> {
    dual_free(st)?;
    st.dual(|d| d.pos = 0)?;
    if let Some(it) = inner_it(st)? {
        it_call(&it, host, |i, h| i.rewind(h))?;
    }
    Ok(())
}

/// `spl_dual_it_valid`.
fn dual_valid(host: &mut dyn SplHost, st: &SplObject) -> RtResult<bool> {
    match inner_it(st)? {
        Some(it) => it_call(&it, host, |i, h| i.valid(h)),
        None => Ok(false),
    }
}

/// `spl_dual_it_fetch`: whether an element was fetched.
fn dual_fetch(host: &mut dyn SplHost, st: &SplObject, check_more: bool) -> RtResult<bool> {
    dual_free(st)?;
    if check_more && !dual_valid(host, st)? {
        return Ok(false);
    }
    let Some(it) = inner_it(st)? else {
        return Ok(false);
    };
    let data = it_call(&it, host, |i, h| i.current(h))?;
    st.dual(|d| d.data = data.map(|v| v.deref()))?;
    let key = it_call(&it, host, |i, h| i.key(h))?;
    st.dual(|d| d.key = Some(key.unwrap_or(Value::Int(d.pos))))?;
    Ok(true)
}

/// `spl_dual_it_next`.
fn dual_next(host: &mut dyn SplHost, st: &SplObject, do_free: bool) -> RtResult<()> {
    if do_free {
        dual_free(st)?;
    }
    let Some(it) = inner_it(st)? else {
        return Err(error(
            "The inner constructor wasn't initialized with an iterator instance",
        ));
    };
    it_call(&it, host, |i, h| i.next(h))?;
    st.dual(|d| d.pos += 1)
}

/// `CachingIterator::*` flag check (`spl_cit_check_flags`).
fn cit_flags_ok(flags: i64) -> bool {
    [
        CIT_CALL_TOSTRING,
        CIT_TOSTRING_USE_KEY,
        CIT_TOSTRING_USE_CURRENT,
        CIT_TOSTRING_USE_INNER,
    ]
    .iter()
    .filter(|f| flags & **f != 0)
    .count()
        <= 1
}

const CIT_FLAGS_MSG: &str = "must contain only one of CachingIterator::CALL_TOSTRING, CachingIterator::TOSTRING_USE_KEY, CachingIterator::TOSTRING_USE_CURRENT, or CachingIterator::TOSTRING_USE_INNER";
const REGIT_MODE_MSG: &str = "must be RegexIterator::MATCH, RegexIterator::GET_MATCH, RegexIterator::ALL_MATCHES, RegexIterator::SPLIT, or RegexIterator::REPLACE";

/// `spl_get_iterator_from_aggregate`.
fn iterator_from_aggregate(host: &mut dyn SplHost, obj: &Obj, class: &ZStr) -> RtResult<Obj> {
    let r = host.call_method(obj, "getIterator", &[])?;
    match r.deref() {
        Value::Object(o) if host.instance_of(&o, "Traversable") => Ok(o),
        _ => Err(logic(format!(
            "{class}::getIterator() must return an object that implements Traversable"
        ))),
    }
}

/// `spl_dual_it_construct` for the class `base`.
#[allow(clippy::too_many_lines)]
fn dual_construct(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &SplObject,
    base: &str,
    args: &[Value],
) -> RtResult<()> {
    let func = format!("{base}::__construct");
    if !st.dual(|d| matches!(d.kind, DualKind::Unknown))? {
        return Err(bad_method_call(format!(
            "{base}::getIterator() must be called exactly once per instance"
        )));
    }
    let recursive_inner = matches!(
        base,
        "RecursiveCallbackFilterIterator"
            | "RecursiveFilterIterator"
            | "ParentIterator"
            | "RecursiveRegexIterator"
            | "RecursiveCachingIterator"
    );
    let ce_inner = if base == "IteratorIterator" {
        "Traversable"
    } else if recursive_inner {
        "RecursiveIterator"
    } else {
        "Iterator"
    };
    let (kind, inner, mut inner_class) = match base {
        "LimitIterator" => {
            let a = Args::new(&func, args, &["iterator", "offset", "limit"], 1, Some(3))?;
            let o = a.object_of(host, 0, ce_inner)?;
            let offset = a.opt_long(host, 1, 0)?;
            let count = a.opt_long(host, 2, -1)?;
            if offset < 0 {
                return Err(a.value_error(1, "must be greater than or equal to 0"));
            }
            if count < -1 {
                return Err(a.value_error(2, "must be greater than or equal to -1"));
            }
            (DualKind::Limit { offset, count }, o, None)
        }
        "CachingIterator" | "RecursiveCachingIterator" => {
            let a = Args::new(&func, args, &["iterator", "flags"], 1, Some(2))?;
            let o = a.object_of(host, 0, ce_inner)?;
            let flags = a.opt_long(host, 1, CIT_CALL_TOSTRING)?;
            if !cit_flags_ok(flags) {
                return Err(a.value_error(1, CIT_FLAGS_MSG));
            }
            (
                DualKind::Caching {
                    flags: flags & CIT_PUBLIC,
                    zstr: None,
                    children: None,
                    cache: ZArray::new(),
                    recursive: base == "RecursiveCachingIterator",
                },
                o,
                None,
            )
        }
        "IteratorIterator" => {
            let a = Args::new(&func, args, &["iterator", "class"], 1, Some(2))?;
            let mut o = a.object_of(host, 0, ce_inner)?;
            let class_name = a.str_or_null(host, 1)?;
            let mut ce = o.class_name();
            if !host.instance_of(&o, "Iterator") {
                if let Some(cn) = class_name {
                    let ok = host.class_is_a(&cn, "Traversable") == Some(true)
                        && host.instance_of(&o, &cn.to_string_lossy());
                    if !ok {
                        return Err(logic(
                            "Class to downcast to not found or not base class or does not implement Traversable",
                        ));
                    }
                    ce = cn;
                }
                if host.class_is_a(&ce, "IteratorAggregate") == Some(true)
                    || host.instance_of(&o, "IteratorAggregate")
                {
                    o = iterator_from_aggregate(host, &o, &ce)?;
                    ce = o.class_name();
                }
            }
            (DualKind::IteratorIterator, o, Some(ce))
        }
        "RegexIterator" | "RecursiveRegexIterator" => {
            let a = Args::new(
                &func,
                args,
                &["iterator", "pattern", "mode", "flags", "pregFlags"],
                2,
                Some(5),
            )?;
            let o = a.object_of(host, 0, ce_inner)?;
            let regex = a.str(host, 1)?;
            let mode = a.opt_long(host, 2, REGIT_MODE_MATCH)?;
            let flags = a.opt_long(host, 3, 0)?;
            let preg_flags = a.opt_long(host, 4, 0)?;
            if !(0..REGIT_MODE_MAX).contains(&mode) {
                return Err(a.value_error(2, REGIT_MODE_MSG));
            }
            let compiled = host.pcre().compile(regex.as_bytes());
            let warnings = host.pcre().take_warnings();
            if let Some(w) = warnings.first() {
                return Err(invalid_argument(format!(
                    "{func}(): {}",
                    String::from_utf8_lossy(w)
                )));
            }
            let compiled = match compiled {
                Ok(c) => c,
                Err(e) => {
                    return Err(invalid_argument(format!(
                        "{func}(): {}",
                        String::from_utf8_lossy(&e.message())
                    )))
                }
            };
            (
                DualKind::Regex {
                    flags,
                    preg_flags,
                    regex,
                    compiled,
                    mode,
                },
                o,
                None,
            )
        }
        "CallbackFilterIterator" | "RecursiveCallbackFilterIterator" => {
            let a = Args::new(&func, args, &["iterator", "callback"], 2, Some(2))?;
            let o = a.object_of(host, 0, ce_inner)?;
            let callback = a.callable(host, 1)?;
            (DualKind::CallbackFilter { callback }, o, None)
        }
        _ => {
            let a = Args::new(&func, args, &["iterator"], 1, Some(1))?;
            let o = a.object_of(host, 0, ce_inner)?;
            let kind = match base {
                "NoRewindIterator" => DualKind::NoRewind,
                "InfiniteIterator" => DualKind::Infinite,
                _ => DualKind::Default,
            };
            (kind, o, None)
        }
    };
    let _ = this;
    let it = new_it(host, &inner)?;
    if inner_class.is_none() {
        inner_class = Some(inner.class_name());
    }
    st.dual(|d| {
        d.kind = kind;
        d.inner = Some(inner);
        d.inner_class = inner_class;
        d.inner_it = Some(it);
    })
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

/// `spl_filter_it_fetch`.
fn filter_fetch(host: &mut dyn SplHost, this: &Obj, st: &SplObject) -> RtResult<()> {
    while dual_fetch(host, st, true)? {
        let r = host.call_method(this, "accept", &[])?;
        if r.to_bool(host)? {
            return Ok(());
        }
        if let Some(it) = inner_it(st)? {
            it_call(&it, host, |i, h| i.next(h))?;
        }
    }
    dual_free(st)
}

fn filter_rewind(host: &mut dyn SplHost, this: &Obj, st: &SplObject) -> RtResult<()> {
    dual_rewind(host, st)?;
    filter_fetch(host, this, st)
}

fn filter_next(host: &mut dyn SplHost, this: &Obj, st: &SplObject) -> RtResult<()> {
    dual_next(host, st, true)?;
    filter_fetch(host, this, st)
}

// ---------------------------------------------------------------------------
// LimitIterator
// ---------------------------------------------------------------------------

fn limit_params(st: &SplObject) -> RtResult<(i64, i64)> {
    st.dual(|d| match d.kind {
        DualKind::Limit { offset, count } => (offset, count),
        _ => (0, -1),
    })
}

fn limit_seek(host: &mut dyn SplHost, st: &SplObject, pos: i64) -> RtResult<()> {
    dual_free(st)?;
    let (offset, count) = limit_params(st)?;
    if pos < offset {
        return Err(out_of_bounds(format!(
            "Cannot seek to {pos} which is below the offset {offset}"
        )));
    }
    if pos - offset >= count && count != -1 {
        return Err(out_of_bounds(format!(
            "Cannot seek to {pos} which is behind offset {offset} plus count {count}"
        )));
    }
    let (cur, inner) = st.dual(|d| (d.pos, d.inner.clone()))?;
    let seekable = match &inner {
        Some(o) => host.instance_of(o, "SeekableIterator"),
        None => false,
    };
    if pos != cur && seekable {
        dual_free(st)?;
        if let Some(o) = &inner {
            host.call_method(o, "seek", &[Value::Int(pos)])?;
        }
        st.dual(|d| d.pos = pos)?;
        let valid = (count == -1 || pos - offset < count) && dual_valid(host, st)?;
        if valid {
            dual_fetch(host, st, false)?;
        }
    } else {
        if pos < cur {
            dual_rewind(host, st)?;
        }
        while pos > st.dual(|d| d.pos)? && dual_valid(host, st)? {
            dual_next(host, st, true)?;
        }
        if dual_valid(host, st)? {
            dual_fetch(host, st, true)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// CachingIterator
// ---------------------------------------------------------------------------

fn caching_flags(st: &SplObject) -> RtResult<i64> {
    st.dual(|d| match &d.kind {
        DualKind::Caching { flags, .. } => *flags,
        _ => 0,
    })
}

fn set_caching_flags(st: &SplObject, f: impl FnOnce(i64) -> i64) -> RtResult<()> {
    st.dual(|d| {
        if let DualKind::Caching { flags, .. } = &mut d.kind {
            *flags = f(*flags);
        }
    })
}

/// `spl_caching_it_next`.
fn caching_next(host: &mut dyn SplHost, st: &SplObject) -> RtResult<()> {
    if !dual_fetch(host, st, true)? {
        return set_caching_flags(st, |f| f & !CIT_VALID);
    }
    set_caching_flags(st, |f| f | CIT_VALID)?;
    let flags = caching_flags(st)?;
    if flags & CIT_FULL_CACHE != 0 {
        let (key, data) = st.dual(|d| (d.key.clone(), d.data.clone()))?;
        let mut cache = st.dual(|d| match &d.kind {
            DualKind::Caching { cache, .. } => cache.clone(),
            _ => ZArray::new(),
        })?;
        crate::iter::set_zval_key(
            host,
            cache.make_mut(),
            &key.unwrap_or_default(),
            data.unwrap_or_default().deref(),
        )?;
        st.dual(|d| {
            if let DualKind::Caching { cache: c, .. } = &mut d.kind {
                *c = cache;
            }
        })?;
    }
    let (recursive, inner) = st.dual(|d| {
        (
            matches!(
                d.kind,
                DualKind::Caching {
                    recursive: true,
                    ..
                }
            ),
            d.inner.clone(),
        )
    })?;
    if recursive {
        if let Some(inner) = inner.as_ref() {
            match recursive_caching_children(host, inner, flags) {
                Ok(children) => {
                    st.dual(|d| {
                        if let DualKind::Caching { children: c, .. } = &mut d.kind {
                            *c = children;
                        }
                    })?;
                }
                Err(e) => {
                    if !catches(flags, &e) {
                        return Err(e);
                    }
                }
            }
        }
    }
    if flags & (CIT_TOSTRING_USE_INNER | CIT_CALL_TOSTRING) != 0 {
        let v = if flags & CIT_TOSTRING_USE_INNER != 0 {
            inner.map(Value::Object).unwrap_or_default()
        } else {
            st.dual(|d| d.data.clone())?.unwrap_or_default()
        };
        let s = v.to_zstr(host)?;
        st.dual(|d| {
            if let DualKind::Caching { zstr, .. } = &mut d.kind {
                *zstr = Some(s);
            }
        })?;
    }
    dual_next(host, st, false)
}

fn recursive_caching_children(
    host: &mut dyn SplHost,
    inner: &Obj,
    flags: i64,
) -> RtResult<Option<Value>> {
    let r = host.call_method(inner, "hasChildren", &[])?;
    if !r.to_bool(host)? {
        return Ok(None);
    }
    let children = host.call_method(inner, "getChildren", &[])?;
    let o = host.instantiate(
        &ZStr::from("RecursiveCachingIterator"),
        &[children, Value::Int(flags & CIT_PUBLIC)],
    )?;
    Ok(Some(Value::Object(o)))
}

fn caching_rewind(host: &mut dyn SplHost, st: &SplObject) -> RtResult<()> {
    dual_rewind(host, st)?;
    st.dual(|d| {
        if let DualKind::Caching { cache, .. } = &mut d.kind {
            *cache = ZArray::new();
        }
    })?;
    caching_next(host, st)
}

fn full_cache_check(this: &Obj, st: &SplObject) -> RtResult<()> {
    if caching_flags(st)? & CIT_FULL_CACHE == 0 {
        return Err(bad_method_call(format!(
            "{} does not use a full cache (see CachingIterator::__construct)",
            this.class_name()
        )));
    }
    Ok(())
}

fn with_cache<R>(st: &SplObject, f: impl FnOnce(&mut HashTable) -> R) -> RtResult<R> {
    st.dual(|d| match &mut d.kind {
        DualKind::Caching { cache, .. } => f(cache.make_mut()),
        _ => f(&mut HashTable::new()),
    })
}

// ---------------------------------------------------------------------------
// AppendIterator
// ---------------------------------------------------------------------------

fn append_parts(st: &SplObject) -> RtResult<Option<(Obj, ItRef)>> {
    st.dual(|d| match &d.kind {
        DualKind::Append { array_it, it } => Some((array_it.clone(), it.clone())),
        _ => None,
    })
}

/// `spl_append_it_next_iterator`.
fn append_next_iterator(host: &mut dyn SplHost, st: &SplObject) -> RtResult<bool> {
    dual_free(st)?;
    let garbage = st.dual(|d| (d.inner.take(), d.inner_class.take(), d.inner_it.take()))?;
    drop(garbage);
    let Some((_, ait)) = append_parts(st)? else {
        return Ok(false);
    };
    if !it_call(&ait, host, |i, h| i.valid(h))? {
        return Ok(false);
    }
    let cur = it_call(&ait, host, |i, h| i.current(h))?.unwrap_or_default();
    let Value::Object(o) = cur.deref() else {
        return Ok(false);
    };
    let it = new_it(host, &o)?;
    let cls = o.class_name();
    st.dual(|d| {
        d.inner = Some(o);
        d.inner_class = Some(cls);
        d.inner_it = Some(it);
    })?;
    dual_rewind(host, st)?;
    Ok(true)
}

/// `spl_append_it_fetch`.
fn append_fetch(host: &mut dyn SplHost, st: &SplObject) -> RtResult<()> {
    while !dual_valid(host, st)? {
        let Some((_, ait)) = append_parts(st)? else {
            return Ok(());
        };
        it_call(&ait, host, |i, h| i.next(h))?;
        if !append_next_iterator(host, st)? {
            return Ok(());
        }
    }
    dual_fetch(host, st, false).map(|_| ())
}

fn append_next(host: &mut dyn SplHost, st: &SplObject) -> RtResult<()> {
    if dual_valid(host, st)? {
        dual_next(host, st, true)?;
    }
    append_fetch(host, st)
}

// ---------------------------------------------------------------------------
// RegexIterator
// ---------------------------------------------------------------------------

/// Converts a PCRE result array into a PHP array.
fn match_value(v: &php_pcre::Value) -> Value {
    match v {
        php_pcre::Value::Str(s) => Value::Str(ZStr::from(s.as_slice())),
        php_pcre::Value::Null => Value::Null,
        php_pcre::Value::Int(i) => Value::Int(*i),
        php_pcre::Value::Array(a) => Value::Array(match_array(a)),
    }
}

fn match_array(a: &php_pcre::MatchArray) -> ZArray {
    let mut t = HashTable::with_capacity(a.len());
    for (k, v) in a.iter() {
        let key = match k {
            php_pcre::Key::Index(i) => ArrayKey::Int(*i),
            php_pcre::Key::Name(n) => ArrayKey::from_bytes(n),
        };
        t.insert(key, match_value(v));
    }
    ZArray::from_table(t)
}

/// `RegexIterator::accept()`.
fn regex_accept(host: &mut dyn SplHost, this: &Obj, st: &SplObject) -> RtResult<bool> {
    let (data, key, kind) = st.dual(|d| (d.data.clone(), d.key.clone(), d.kind.clone()))?;
    let Some(data) = data else {
        return Ok(false);
    };
    let DualKind::Regex {
        flags,
        preg_flags,
        compiled,
        mode,
        ..
    } = kind
    else {
        return Ok(false);
    };
    let subject = if flags & REGIT_USE_KEY != 0 {
        key.unwrap_or_default().to_zstr(host)?
    } else {
        if matches!(data, Value::Array(_)) {
            return Ok(false);
        }
        data.to_zstr(host)?
    };
    let cfg = host.pcre().config();
    let result = match mode {
        REGIT_MODE_GET_MATCH | REGIT_MODE_ALL_MATCHES => {
            st.dual(|d| d.data = None)?;
            let out = if mode == REGIT_MODE_ALL_MATCHES {
                php_pcre::match_all(&compiled, &cfg, subject.as_bytes(), true, preg_flags, 0)
            } else {
                php_pcre::match_one(&compiled, &cfg, subject.as_bytes(), true, preg_flags, 0)
            };
            if let Some(e) = out.last_error {
                host.pcre().set_last_error(e);
            }
            if let Some(w) = out.warning {
                host.diagnostic(ErrorLevel::Warning, w)?;
            }
            let matches = out.matches.map(|m| Value::Array(match_array(&m)));
            st.dual(|d| d.data = Some(matches.unwrap_or_else(Value::empty_array)))?;
            matches!(out.result, Ok(n) if n > 0)
        }
        REGIT_MODE_SPLIT => {
            st.dual(|d| d.data = None)?;
            let parts = php_pcre::split(&compiled, &cfg, subject.as_bytes(), -1, preg_flags);
            let arr = match &parts {
                Ok(a) => match_array(a),
                Err(e) => {
                    host.pcre().set_last_error(*e);
                    ZArray::new()
                }
            };
            let n = arr.len();
            st.dual(|d| d.data = Some(Value::Array(arr)))?;
            n > 1
        }
        REGIT_MODE_REPLACE => {
            let repl = host
                .property_read(this, &ZStr::from("replacement"))?
                .unwrap_or_default();
            let repl = repl.to_zstr(host)?;
            let r = php_pcre::Replacement::parse(repl.as_bytes());
            let out = php_pcre::replace(
                &compiled,
                &cfg,
                subject.as_bytes(),
                &r,
                php_pcre::limit_from_php(-1),
            );
            host.pcre().set_last_error(out.last_error());
            let Ok(result) = out.result else {
                return Ok(false);
            };
            let v = Value::Str(ZStr::from(result));
            st.dual(|d| {
                if flags & REGIT_USE_KEY != 0 {
                    d.key = Some(v);
                } else {
                    d.data = Some(v);
                }
            })?;
            out.count > 0
        }
        _ => {
            let out = php_pcre::match_one(&compiled, &cfg, subject.as_bytes(), false, 0, 0);
            matches!(out.result, Ok(n) if n > 0)
        }
    };
    if flags & REGIT_INVERTED != 0 {
        return Ok(!result);
    }
    Ok(result)
}

fn regex_field<R>(
    st: &SplObject,
    f: impl FnOnce(&mut i64, &mut i64, &mut i64, &ZStr) -> R,
) -> RtResult<R> {
    st.dual(|d| match &mut d.kind {
        DualKind::Regex {
            flags,
            preg_flags,
            mode,
            regex,
            ..
        } => Ok(f(flags, preg_flags, mode, regex)),
        _ => Err(not_initialized()),
    })?
}

// ---------------------------------------------------------------------------
// Method dispatch for the dual iterators
// ---------------------------------------------------------------------------

fn method_name(class: &str, method: &str) -> Option<&'static str> {
    crate::classes::find_class(class)?
        .methods
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(method))
        .map(|m| m.name)
}

/// Methods of all iterator classes (declaring class `class`).
#[allow(clippy::too_many_lines)]
pub(crate) fn call(
    host: &mut dyn SplHost,
    class: &str,
    method: &str,
    this: Option<&Obj>,
    args: &[Value],
) -> Option<RtResult<Value>> {
    let this = match this_obj(this) {
        Ok(t) => t,
        Err(e) => return Some(Err(e)),
    };
    let name = method_name(class, method)?;
    let func = format!("{class}::{name}");
    if class == "EmptyIterator" {
        return Some(Args::none(&func, args).and_then(|()| match method {
            "valid" => Ok(Value::Bool(false)),
            "key" => Err(bad_method_call("Accessing the key of an EmptyIterator")),
            "current" => Err(bad_method_call("Accessing the value of an EmptyIterator")),
            _ => Ok(Value::Null),
        }));
    }
    if class == "RecursiveIteratorIterator" || class == "RecursiveTreeIterator" {
        return Some(recursive_call(host, class, method, &func, this, args));
    }
    Some((|| -> RtResult<Value> {
        let st = state_of(host, this)?;
        if method == "__construct" {
            if class == "AppendIterator" {
                Args::none(&func, args)?;
                if !st.dual(|d| matches!(d.kind, DualKind::Unknown))? {
                    return Err(bad_method_call(
                        "AppendIterator::getIterator() must be called exactly once per instance",
                    ));
                }
                let ait = host.instantiate(&ZStr::from("ArrayIterator"), &[])?;
                let it = new_it(host, &ait)?;
                st.dual(|d| d.kind = DualKind::Append { array_it: ait, it })?;
                return Ok(Value::Null);
            }
            dual_construct(host, this, &st, class, args)?;
            return Ok(Value::Null);
        }
        // parameter-less methods share the zpp check
        let takes_args = matches!(
            (class, method),
            ("LimitIterator", "seek")
                | (
                    "CachingIterator",
                    "setflags" | "offsetget" | "offsetset" | "offsetunset" | "offsetexists"
                )
                | ("AppendIterator", "append")
                | ("RegexIterator", "setmode" | "setflags" | "setpregflags")
        );
        if !takes_args {
            Args::none(&func, args)?;
        }
        if !(class == "CachingIterator" && method == "offsetunset") {
            check(&st)?;
        }
        match (class, method) {
            ("IteratorIterator", "getinneriterator") => Ok(st
                .dual(|d| d.inner.clone())?
                .map_or(Value::Null, Value::Object)),
            ("IteratorIterator", "rewind") => {
                dual_rewind(host, &st)?;
                dual_fetch(host, &st, true)?;
                Ok(Value::Null)
            }
            ("IteratorIterator" | "AppendIterator", "valid") => {
                st.dual(|d| Value::Bool(d.data.is_some()))
            }
            ("IteratorIterator", "key") => {
                st.dual(|d| d.key.clone().map_or(Value::Null, |v| v.deref()))
            }
            ("IteratorIterator", "current") => {
                st.dual(|d| d.data.clone().map_or(Value::Null, |v| v.deref()))
            }
            ("IteratorIterator", "next") => {
                dual_next(host, &st, true)?;
                dual_fetch(host, &st, true)?;
                Ok(Value::Null)
            }
            ("FilterIterator", "rewind") => {
                filter_rewind(host, this, &st)?;
                Ok(Value::Null)
            }
            ("FilterIterator", "next") => {
                filter_next(host, this, &st)?;
                Ok(Value::Null)
            }
            ("CallbackFilterIterator", "accept") => {
                let (data, key, inner, kind) = st.dual(|d| {
                    (
                        d.data.clone(),
                        d.key.clone(),
                        d.inner.clone(),
                        d.kind.clone(),
                    )
                })?;
                let (Some(data), Some(key)) = (data, key) else {
                    return Ok(Value::Bool(false));
                };
                let DualKind::CallbackFilter { callback } = kind else {
                    return Ok(Value::Bool(false));
                };
                let inner = inner.map_or(Value::Null, Value::Object);
                let r = host.call_callable(&callback, &[data, key, inner])?;
                Ok(r.deref())
            }
            (
                "RecursiveFilterIterator"
                | "RecursiveCallbackFilterIterator"
                | "ParentIterator"
                | "RecursiveRegexIterator",
                "haschildren",
            )
            | ("ParentIterator", "accept") => {
                let inner = st.dual(|d| d.inner.clone())?.ok_or_else(not_initialized)?;
                host.call_method(&inner, "hasChildren", &[])
            }
            ("RecursiveFilterIterator", "getchildren") => {
                let inner = st.dual(|d| d.inner.clone())?.ok_or_else(not_initialized)?;
                let children = host.call_method(&inner, "getChildren", &[])?;
                let o = host.instantiate(&this.class_name(), &[children])?;
                Ok(Value::Object(o))
            }
            ("RecursiveCallbackFilterIterator", "getchildren") => {
                let (inner, kind) = st.dual(|d| (d.inner.clone(), d.kind.clone()))?;
                let inner = inner.ok_or_else(not_initialized)?;
                let children = host.call_method(&inner, "getChildren", &[])?;
                let cb = match kind {
                    DualKind::CallbackFilter { callback } => callback,
                    _ => Value::Null,
                };
                let o = host.instantiate(&this.class_name(), &[children, cb])?;
                Ok(Value::Object(o))
            }
            ("LimitIterator", "rewind") => {
                dual_rewind(host, &st)?;
                let (offset, _) = limit_params(&st)?;
                limit_seek(host, &st, offset)?;
                Ok(Value::Null)
            }
            ("LimitIterator", "valid") => {
                let (offset, count) = limit_params(&st)?;
                st.dual(|d| {
                    Value::Bool((count == -1 || d.pos - offset < count) && d.data.is_some())
                })
            }
            ("LimitIterator", "next") => {
                dual_next(host, &st, true)?;
                let (offset, count) = limit_params(&st)?;
                let pos = st.dual(|d| d.pos)?;
                if count == -1 || pos - offset < count {
                    dual_fetch(host, &st, true)?;
                }
                Ok(Value::Null)
            }
            ("LimitIterator", "seek") => {
                let a = Args::new(&func, args, &["offset"], 1, Some(1))?;
                let pos = a.long(host, 0)?;
                limit_seek(host, &st, pos)?;
                st.dual(|d| Value::Int(d.pos))
            }
            ("LimitIterator", "getposition") => st.dual(|d| Value::Int(d.pos)),
            ("CachingIterator", "rewind") => {
                caching_rewind(host, &st)?;
                Ok(Value::Null)
            }
            ("CachingIterator", "valid") => Ok(Value::Bool(caching_flags(&st)? & CIT_VALID != 0)),
            ("CachingIterator", "next") => {
                caching_next(host, &st)?;
                Ok(Value::Null)
            }
            ("CachingIterator", "hasnext") => dual_valid(host, &st).map(Value::Bool),
            ("CachingIterator", "__tostring") => {
                let flags = caching_flags(&st)?;
                if flags
                    & (CIT_CALL_TOSTRING
                        | CIT_TOSTRING_USE_KEY
                        | CIT_TOSTRING_USE_CURRENT
                        | CIT_TOSTRING_USE_INNER)
                    == 0
                {
                    return Err(bad_method_call(format!(
                        "{} does not fetch string value (see CachingIterator::__construct)",
                        this.class_name()
                    )));
                }
                if flags & CIT_TOSTRING_USE_KEY != 0 {
                    let k = st.dual(|d| d.key.clone())?.unwrap_or_default();
                    return k.to_zstr(host).map(Value::Str);
                }
                if flags & CIT_TOSTRING_USE_CURRENT != 0 {
                    let v = st.dual(|d| d.data.clone())?.unwrap_or_default();
                    return v.to_zstr(host).map(Value::Str);
                }
                st.dual(|d| match &d.kind {
                    DualKind::Caching { zstr: Some(s), .. } => Value::Str(s.clone()),
                    _ => Value::from(""),
                })
            }
            ("CachingIterator", "offsetset") => {
                let a = Args::new(&func, args, &["key", "value"], 2, Some(2))?;
                let key = a.str(host, 0)?;
                let value = a.raw(1);
                full_cache_check(this, &st)?;
                with_cache(&st, |c| {
                    c.insert(ArrayKey::from_zstr(&key), value);
                })?;
                Ok(Value::Null)
            }
            ("CachingIterator", "offsetget") => {
                let a = Args::new(&func, args, &["key"], 1, Some(1))?;
                let key = a.str(host, 0)?;
                full_cache_check(this, &st)?;
                if let Some(v) = with_cache(&st, |c| c.get(&ArrayKey::from_zstr(&key)).cloned())? {
                    Ok(v.deref())
                } else {
                    host.diagnostic(
                        ErrorLevel::Warning,
                        &format!("Undefined array key \"{}\"", key.to_string_lossy()),
                    )?;
                    Ok(Value::Null)
                }
            }
            ("CachingIterator", "offsetunset") => {
                check(&st)?;
                let a = Args::new(&func, args, &["key"], 1, Some(1))?;
                let key = a.str(host, 0)?;
                full_cache_check(this, &st)?;
                let g = with_cache(&st, |c| c.remove(&ArrayKey::from_zstr(&key)))?;
                drop(g);
                Ok(Value::Null)
            }
            ("CachingIterator", "offsetexists") => {
                let a = Args::new(&func, args, &["key"], 1, Some(1))?;
                let key = a.str(host, 0)?;
                full_cache_check(this, &st)?;
                with_cache(&st, |c| {
                    Value::Bool(c.contains_key(&ArrayKey::from_zstr(&key)))
                })
            }
            ("CachingIterator", "getcache") => {
                full_cache_check(this, &st)?;
                st.dual(|d| match &d.kind {
                    DualKind::Caching { cache, .. } => Value::Array(cache.clone()),
                    _ => Value::empty_array(),
                })
            }
            ("CachingIterator", "getflags") => caching_flags(&st).map(Value::Int),
            ("CachingIterator", "setflags") => {
                let a = Args::new(&func, args, &["flags"], 1, Some(1))?;
                let flags = a.long(host, 0)?;
                if !cit_flags_ok(flags) {
                    return Err(a.value_error(0, CIT_FLAGS_MSG));
                }
                let cur = caching_flags(&st)?;
                if cur & CIT_CALL_TOSTRING != 0 && flags & CIT_CALL_TOSTRING == 0 {
                    return Err(invalid_argument(
                        "Unsetting flag CALL_TO_STRING is not possible",
                    ));
                }
                if cur & CIT_TOSTRING_USE_INNER != 0 && flags & CIT_TOSTRING_USE_INNER == 0 {
                    return Err(invalid_argument(
                        "Unsetting flag TOSTRING_USE_INNER is not possible",
                    ));
                }
                if flags & CIT_FULL_CACHE != 0 && cur & CIT_FULL_CACHE == 0 {
                    with_cache(&st, HashTable::clear)?;
                }
                set_caching_flags(&st, |f| (f & !CIT_PUBLIC) | (flags & CIT_PUBLIC))?;
                Ok(Value::Null)
            }
            ("CachingIterator", "count") => {
                full_cache_check(this, &st)?;
                with_cache(&st, |c| Value::Int(to_long(c.len())))
            }
            ("RecursiveCachingIterator", "haschildren") => st.dual(|d| {
                Value::Bool(matches!(
                    &d.kind,
                    DualKind::Caching {
                        children: Some(_),
                        ..
                    }
                ))
            }),
            ("RecursiveCachingIterator", "getchildren") => st.dual(|d| match &d.kind {
                DualKind::Caching {
                    children: Some(c), ..
                } => c.clone(),
                _ => Value::Null,
            }),
            ("NoRewindIterator", "rewind") => Ok(Value::Null),
            ("NoRewindIterator", "valid") => {
                let it = inner_it(&st)?.ok_or_else(not_initialized)?;
                it_call(&it, host, |i, h| i.valid(h)).map(Value::Bool)
            }
            ("NoRewindIterator", "key") => {
                let it = inner_it(&st)?.ok_or_else(not_initialized)?;
                it_call(&it, host, |i, h| i.key(h)).map(Option::unwrap_or_default)
            }
            ("NoRewindIterator", "current") => {
                let it = inner_it(&st)?.ok_or_else(not_initialized)?;
                it_call(&it, host, |i, h| i.current(h))
                    .map(|v| v.map_or(Value::Null, |v| v.deref()))
            }
            ("NoRewindIterator", "next") => {
                let it = inner_it(&st)?.ok_or_else(not_initialized)?;
                it_call(&it, host, |i, h| i.next(h))?;
                Ok(Value::Null)
            }
            ("InfiniteIterator", "next") => {
                dual_next(host, &st, true)?;
                if dual_valid(host, &st)? {
                    dual_fetch(host, &st, false)?;
                } else {
                    dual_rewind(host, &st)?;
                    if dual_valid(host, &st)? {
                        dual_fetch(host, &st, false)?;
                    }
                }
                Ok(Value::Null)
            }
            ("AppendIterator", "append") => {
                let a = Args::new(&func, args, &["iterator"], 1, Some(1))?;
                let it_obj = a.object_of(host, 0, "Iterator")?;
                let Some((aobj, ait)) = append_parts(&st)? else {
                    return Err(not_initialized());
                };
                let ait_valid = it_call(&ait, host, |i, h| i.valid(h))?;
                let inner_valid = dual_valid(host, &st)?;
                host.call_method(&aobj, "append", &[Value::Object(it_obj.clone())])?;
                if ait_valid && !inner_valid {
                    it_call(&ait, host, |i, h| i.next(h))?;
                }
                let has_inner = inner_it(&st)?.is_some();
                if !has_inner || !dual_valid(host, &st)? {
                    if !it_call(&ait, host, |i, h| i.valid(h))? {
                        it_call(&ait, host, |i, h| i.rewind(h))?;
                    }
                    loop {
                        let moved = append_next_iterator(host, &st)?;
                        let same =
                            st.dual(|d| d.inner.as_ref().is_some_and(|o| Obj::ptr_eq(o, &it_obj)))?;
                        if same || !moved {
                            break;
                        }
                    }
                    append_fetch(host, &st)?;
                }
                Ok(Value::Null)
            }
            ("AppendIterator", "current") => {
                dual_fetch(host, &st, true)?;
                st.dual(|d| d.data.clone().map_or(Value::Null, |v| v.deref()))
            }
            ("AppendIterator", "rewind") => {
                let Some((_, ait)) = append_parts(&st)? else {
                    return Err(not_initialized());
                };
                it_call(&ait, host, |i, h| i.rewind(h))?;
                if append_next_iterator(host, &st)? {
                    append_fetch(host, &st)?;
                }
                Ok(Value::Null)
            }
            ("AppendIterator", "next") => {
                append_next(host, &st)?;
                Ok(Value::Null)
            }
            ("AppendIterator", "getiteratorindex") => {
                let Some((aobj, _)) = append_parts(&st)? else {
                    return Err(bad_method_call(
                        "Classes derived from AppendIterator must call AppendIterator::__construct()",
                    ));
                };
                host.call_method(&aobj, "key", &[])
            }
            ("AppendIterator", "getarrayiterator") => {
                let Some((aobj, _)) = append_parts(&st)? else {
                    return Err(not_initialized());
                };
                Ok(Value::Object(aobj))
            }
            ("RegexIterator", "accept") => regex_accept(host, this, &st).map(Value::Bool),
            ("RecursiveRegexIterator", "accept") => {
                let data = st.dual(|d| d.data.clone())?;
                match data {
                    None => Ok(Value::Bool(false)),
                    Some(Value::Array(a)) => Ok(Value::Bool(!a.is_empty())),
                    Some(_) => regex_accept(host, this, &st).map(Value::Bool),
                }
            }
            ("RecursiveRegexIterator", "getchildren") => {
                let inner = st.dual(|d| d.inner.clone())?.ok_or_else(not_initialized)?;
                let children = host.call_method(&inner, "getChildren", &[])?;
                let (regex, mode, flags, pf) =
                    regex_field(&st, |f, p, m, r| (r.clone(), *m, *f, *p))?;
                let o = host.instantiate(
                    &this.class_name(),
                    &[
                        children,
                        Value::Str(regex),
                        Value::Int(mode),
                        Value::Int(flags),
                        Value::Int(pf),
                    ],
                )?;
                Ok(Value::Object(o))
            }
            ("RegexIterator", "getregex") => regex_field(&st, |_, _, _, r| Value::Str(r.clone())),
            ("RegexIterator", "getmode") => regex_field(&st, |_, _, m, _| Value::Int(*m)),
            ("RegexIterator", "getflags") => regex_field(&st, |f, _, _, _| Value::Int(*f)),
            ("RegexIterator", "getpregflags") => regex_field(&st, |_, p, _, _| Value::Int(*p)),
            ("RegexIterator", "setmode") => {
                let a = Args::new(&func, args, &["mode"], 1, Some(1))?;
                let mode = a.long(host, 0)?;
                if !(0..REGIT_MODE_MAX).contains(&mode) {
                    return Err(a.value_error(0, REGIT_MODE_MSG));
                }
                regex_field(&st, |_, _, m, _| *m = mode)?;
                Ok(Value::Null)
            }
            ("RegexIterator", "setflags") => {
                let a = Args::new(&func, args, &["flags"], 1, Some(1))?;
                let v = a.long(host, 0)?;
                regex_field(&st, |f, _, _, _| *f = v)?;
                Ok(Value::Null)
            }
            ("RegexIterator", "setpregflags") => {
                let a = Args::new(&func, args, &["pregFlags"], 1, Some(1))?;
                let v = a.long(host, 0)?;
                regex_field(&st, |_, p, _, _| *p = v)?;
                Ok(Value::Null)
            }
            _ => Err(RuntimeError::Error(format!(
                "Call to undefined method {func}()"
            ))),
        }
    })())
}

// ---------------------------------------------------------------------------
// RecursiveIteratorIterator / RecursiveTreeIterator
// ---------------------------------------------------------------------------

/// The state of one level (`RecursiveIteratorState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsState {
    /// `RS_NEXT`.
    Next,
    /// `RS_TEST`.
    Test,
    /// `RS_SELF`.
    SelfState,
    /// `RS_CHILD`.
    Child,
    /// `RS_START`.
    Start,
}

/// One level of a recursive iteration (`spl_sub_iterator`).
///
/// `it` is declared before `obj` so that dropping a level releases the
/// native iterator (and its object handle) before the level's object, as
/// `zend_iterator_dtor(sub_iter); zval_ptr_dtor(&zobject)` do in
/// `spl_recursive_it_rewind_ex` and `spl_RecursiveIteratorIterator_free_iterators`.
#[derive(Clone)]
#[allow(missing_debug_implementations)]
pub struct SubIter {
    /// Its native iterator.
    pub it: ItRef,
    /// The iterator object of this level.
    pub obj: Obj,
    /// The state machine state.
    pub state: RsState,
}

/// The state of `RecursiveIteratorIterator` (`spl_recursive_it_object`).
#[derive(Clone)]
pub struct RecursiveIt {
    /// The levels; `None` before the constructor ran.
    pub iterators: Option<Vec<SubIter>>,
    /// `LEAVES_ONLY` / `SELF_FIRST` / `CHILD_FIRST`.
    pub mode: i64,
    /// Flags (`CATCH_GET_CHILD`, tree flags).
    pub flags: i64,
    /// Maximum depth (-1 = unlimited).
    pub max_depth: i64,
    /// Whether `beginIteration()` ran and `endIteration()` has not.
    pub in_iteration: bool,
    /// `RecursiveTreeIterator` prefix parts.
    pub prefix: [ZStr; 6],
    /// `RecursiveTreeIterator` postfix.
    pub postfix: ZStr,
}

impl Drop for RecursiveIt {
    /// `spl_RecursiveIteratorIterator_free_iterators`: the levels are
    /// released from the deepest one up.
    fn drop(&mut self) {
        if let Some(levels) = self.iterators.as_mut() {
            while let Some(level) = levels.pop() {
                drop(level);
            }
        }
    }
}

impl fmt::Debug for RecursiveIt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecursiveIt")
            .field("levels", &self.iterators.as_ref().map(Vec::len))
            .field("mode", &self.mode)
            .field("flags", &self.flags)
            .finish_non_exhaustive()
    }
}

impl RecursiveIt {
    /// A new, unconstructed state.
    #[must_use]
    pub fn new(base: &str) -> Self {
        let tree = base == "RecursiveTreeIterator";
        let e = ZStr::empty;
        RecursiveIt {
            iterators: None,
            mode: 0,
            flags: 0,
            max_depth: -1,
            in_iteration: false,
            prefix: if tree {
                [
                    e(),
                    ZStr::from("| "),
                    ZStr::from("  "),
                    ZStr::from("|-"),
                    ZStr::from("\\-"),
                    e(),
                ]
            } else {
                [e(), e(), e(), e(), e(), e()]
            },
            postfix: e(),
        }
    }
}

fn level_of(st: &SplObject) -> RtResult<Option<usize>> {
    st.recursive(|r| r.iterators.as_ref().map(|v| v.len() - 1))
}

fn sub(st: &SplObject, level: usize) -> RtResult<Option<SubIter>> {
    st.recursive(|r| r.iterators.as_ref().and_then(|v| v.get(level).cloned()))
}

fn set_state(st: &SplObject, level: usize, s: RsState) -> RtResult<()> {
    st.recursive(|r| {
        if let Some(sub) = r.iterators.as_mut().and_then(|v| v.get_mut(level)) {
            sub.state = s;
        }
    })
}

fn hook(host: &mut dyn SplHost, this: &Obj, name: &str) -> RtResult<()> {
    if host.has_user_method(this, name) {
        host.call_method(this, name, &[])?;
    }
    Ok(())
}

/// Whether `CATCH_GET_CHILD` swallows `e`: exceptions are, while an
/// unwind exit or graceful exit (`exit`, fatal errors, a destroyed fiber:
/// [`RuntimeError::Fatal`]) always propagates (GH-24264).
fn catches(flags: i64, e: &RuntimeError) -> bool {
    flags & CIT_CATCH_GET_CHILD != 0 && !matches!(e, RuntimeError::Fatal(_))
}

/// Runs `f`; with `CATCH_GET_CHILD` an error is swallowed (`Ok(None)`).
fn catchable<R>(flags: i64, r: RtResult<R>) -> RtResult<Option<R>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if catches(flags, &e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// `spl_recursive_it_valid_ex`.
fn rit_valid(host: &mut dyn SplHost, this: &Obj, st: &SplObject) -> RtResult<bool> {
    let Some(levels) = st.recursive(|r| r.iterators.clone())? else {
        return Ok(false);
    };
    for s in levels.iter().rev() {
        if it_call(&s.it, host, |i, h| i.valid(h))? {
            return Ok(true);
        }
    }
    let in_it = st.recursive(|r| r.in_iteration)?;
    if in_it && host.has_user_method(this, "endIteration") {
        host.call_method(this, "endIteration", &[])?;
    }
    st.recursive(|r| r.in_iteration = false)?;
    Ok(false)
}

/// `spl_recursive_it_move_forward_ex`.
#[allow(clippy::too_many_lines)]
fn rit_move_forward(host: &mut dyn SplHost, this: &Obj, st: &SplObject) -> RtResult<()> {
    if level_of(st)?.is_none() {
        return Err(not_initialized());
    }
    let (mode, flags, max_depth) = st.recursive(|r| (r.mode, r.flags, r.max_depth))?;
    loop {
        let Some(level) = level_of(st)? else {
            return Ok(());
        };
        let Some(cur) = sub(st, level)? else {
            return Ok(());
        };
        let mut state = cur.state;
        if state == RsState::Next {
            let r = it_call(&cur.it, host, |i, h| i.next(h));
            if catchable(flags, r)?.is_none() {
                // exception swallowed
            }
            state = RsState::Start;
        }
        if state == RsState::Start {
            let valid = it_call(&cur.it, host, |i, h| i.valid(h))?;
            let reentered = level_of(st)? != Some(level)
                || sub(st, level)?.is_none_or(|s| !Rc::ptr_eq(&s.it, &cur.it));
            if reentered {
                return Ok(());
            }
            if !valid {
                // no more elements at this level
                if level > 0 {
                    if host.has_user_method(this, "endChildren") {
                        let r = host.call_method(this, "endChildren", &[]);
                        catchable(flags, r)?;
                    }
                    let still_same = level_of(st)? == Some(level)
                        && sub(st, level)?.is_some_and(|s| Rc::ptr_eq(&s.it, &cur.it));
                    if still_same {
                        let garbage = st.recursive(|r| r.iterators.as_mut().and_then(Vec::pop))?;
                        // `zval_ptr_dtor(&garbage); zend_iterator_dtor(iterator)`:
                        // here the level's object is released before its
                        // iterator (which then frees the object, if it held
                        // the last reference, and then its own handle).
                        let SubIter {
                            it: cur_it,
                            obj: cur_obj,
                            ..
                        } = cur;
                        let (garbage_it, garbage_obj) = garbage.map(|g| (g.it, g.obj)).unzip();
                        drop((garbage_obj, cur_obj));
                        drop((garbage_it, cur_it));
                        // `zval_ptr_dtor(&garbage)`: the child iterator is
                        // destructed right here, and an exception from its
                        // destructor ends the walk (`while (!EG(exception))`).
                        host.run_pending_destructors()?;
                    }
                    continue;
                }
                set_state(st, level, RsState::Start)?;
                return Ok(());
            }
            set_state(st, level, RsState::Test)?;
            state = RsState::Test;
        }
        match state {
            RsState::Test => {
                let r = if host.has_user_method(this, "callHasChildren") {
                    host.call_method(this, "callHasChildren", &[])
                } else {
                    host.call_method(&cur.obj, "hasChildren", &[])
                };
                let r = match r {
                    Ok(v) => Some(v),
                    Err(e) => {
                        if catches(flags, &e) {
                            None
                        } else {
                            set_state(st, level, RsState::Next)?;
                            return Err(e);
                        }
                    }
                };
                if let Some(v) = r {
                    if v.to_bool(host)? {
                        let lvl = to_long(level);
                        if max_depth == -1 || max_depth > lvl {
                            match mode {
                                RIT_LEAVES_ONLY | RIT_CHILD_FIRST => {
                                    set_state(st, level, RsState::Child)?;
                                    continue;
                                }
                                _ => {
                                    set_state(st, level, RsState::SelfState)?;
                                    continue;
                                }
                            }
                        } else if mode == RIT_LEAVES_ONLY {
                            set_state(st, level, RsState::Next)?;
                            continue;
                        }
                    }
                }
                let r = if host.has_user_method(this, "nextElement") {
                    host.call_method(this, "nextElement", &[]).map(|_| ())
                } else {
                    Ok(())
                };
                set_state(st, level, RsState::Next)?;
                catchable(flags, r)?;
                return Ok(());
            }
            RsState::SelfState => {
                if (mode == RIT_SELF_FIRST || mode == RIT_CHILD_FIRST)
                    && host.has_user_method(this, "nextElement")
                {
                    host.call_method(this, "nextElement", &[])?;
                }
                if mode == RIT_SELF_FIRST {
                    set_state(st, level, RsState::Child)?;
                } else {
                    set_state(st, level, RsState::Next)?;
                }
                return Ok(());
            }
            RsState::Child => {
                let r = if host.has_user_method(this, "callGetChildren") {
                    host.call_method(this, "callGetChildren", &[])
                } else {
                    host.call_method(&cur.obj, "getChildren", &[])
                };
                let child = match r {
                    Ok(c) => c,
                    Err(e) => {
                        if catches(flags, &e) {
                            set_state(st, level, RsState::Next)?;
                            continue;
                        }
                        return Err(e);
                    }
                };
                let child_obj = match child.deref() {
                    Value::Object(o) if host.instance_of(&o, "RecursiveIterator") => o,
                    _ => {
                        return Err(unexpected_value(
                            "Objects returned by RecursiveIterator::getChildren() must implement RecursiveIterator",
                        ))
                    }
                };
                if mode == RIT_CHILD_FIRST {
                    set_state(st, level, RsState::SelfState)?;
                } else {
                    set_state(st, level, RsState::Next)?;
                }
                let it = new_it(host, &child_obj)?;
                let new_sub = SubIter {
                    obj: child_obj,
                    it: it.clone(),
                    state: RsState::Start,
                };
                st.recursive(|r| {
                    if let Some(v) = r.iterators.as_mut() {
                        v.push(new_sub);
                    }
                })?;
                it_call(&it, host, |i, h| i.rewind(h))?;
                if host.has_user_method(this, "beginChildren") {
                    let r = host.call_method(this, "beginChildren", &[]);
                    catchable(flags, r)?;
                }
            }
            RsState::Next | RsState::Start => {}
        }
    }
}

/// `spl_recursive_it_rewind_ex`.
fn rit_rewind(host: &mut dyn SplHost, this: &Obj, st: &SplObject) -> RtResult<()> {
    if level_of(st)?.is_none() {
        return Err(not_initialized());
    }
    while level_of(st)?.unwrap_or(0) > 0 {
        let garbage = st.recursive(|r| r.iterators.as_mut().and_then(Vec::pop))?;
        drop(garbage);
        hook(host, this, "endChildren")?;
    }
    let first = sub(st, 0)?.ok_or_else(not_initialized)?;
    set_state(st, 0, RsState::Start)?;
    it_call(&first.it, host, |i, h| i.rewind(h))?;
    let in_it = st.recursive(|r| r.in_iteration)?;
    if !in_it {
        hook(host, this, "beginIteration")?;
    }
    st.recursive(|r| r.in_iteration = true)?;
    rit_move_forward(host, this, st)
}

fn current_sub(st: &SplObject) -> RtResult<SubIter> {
    let level = level_of(st)?.ok_or_else(not_initialized)?;
    sub(st, level)?.ok_or_else(not_initialized)
}

/// `RecursiveIteratorIterator::__construct` / `RecursiveTreeIterator::__construct`.
fn rit_construct(
    host: &mut dyn SplHost,
    this: &Obj,
    st: &SplObject,
    class: &str,
    func: &str,
    args: &[Value],
) -> RtResult<()> {
    let tree = class == "RecursiveTreeIterator";
    let (iterator, mode, flags) = if tree {
        let a = Args::new(
            func,
            args,
            &["iterator", "flags", "cachingIteratorFlags", "mode"],
            1,
            Some(4),
        )?;
        let mut it = a.object(0)?;
        let flags = a.opt_long(host, 1, RTIT_BYPASS_KEY)?;
        let cit_flags = a.opt_long(host, 2, CIT_CATCH_GET_CHILD)?;
        let mode = a.opt_long(host, 3, RIT_SELF_FIRST)?;
        if host.instance_of(&it, "IteratorAggregate") {
            let cls = it.class_name();
            it = iterator_from_aggregate(host, &it, &cls)?;
        }
        let caching = host.instantiate(
            &ZStr::from("RecursiveCachingIterator"),
            &[Value::Object(it), Value::Int(cit_flags)],
        )?;
        (caching, mode, flags)
    } else {
        let a = Args::new(func, args, &["iterator", "mode", "flags"], 1, Some(3))?;
        let mut it = a.object(0)?;
        let mode = a.opt_long(host, 1, RIT_LEAVES_ONLY)?;
        let flags = a.opt_long(host, 2, 0)?;
        if host.instance_of(&it, "IteratorAggregate") {
            let cls = it.class_name();
            it = iterator_from_aggregate(host, &it, &cls)?;
        }
        (it, mode, flags)
    };
    if !host.instance_of(&iterator, "RecursiveIterator") {
        return Err(invalid_argument(
            "An instance of RecursiveIterator or IteratorAggregate creating it is required",
        ));
    }
    let _ = this;
    let it = new_it(host, &iterator)?;
    st.recursive(|r| {
        r.iterators = Some(vec![SubIter {
            obj: iterator,
            it,
            state: RsState::Start,
        }]);
        r.mode = mode;
        r.flags = flags;
        r.max_depth = -1;
        r.in_iteration = false;
    })
}

fn tree_prefix(host: &mut dyn SplHost, st: &SplObject) -> RtResult<ZStr> {
    let (levels, prefix) =
        st.recursive(|r| (r.iterators.clone().unwrap_or_default(), r.prefix.clone()))?;
    let mut out = prefix[0].as_bytes().to_vec();
    let n = levels.len();
    for (i, s) in levels.iter().enumerate() {
        let has_next = host.call_method(&s.obj, "hasNext", &[])?;
        let is_true = matches!(has_next, Value::Bool(true));
        let part = if i + 1 < n {
            if is_true {
                &prefix[1]
            } else {
                &prefix[2]
            }
        } else if is_true {
            &prefix[3]
        } else {
            &prefix[4]
        };
        out.extend_from_slice(part.as_bytes());
    }
    out.extend_from_slice(prefix[5].as_bytes());
    Ok(ZStr::from(out))
}

fn tree_entry(host: &mut dyn SplHost, st: &SplObject) -> RtResult<Option<ZStr>> {
    let cur = current_sub(st)?;
    let Some(data) = it_call(&cur.it, host, |i, h| i.current(h))? else {
        return Ok(None);
    };
    match data.deref() {
        Value::Array(_) => Ok(Some(ZStr::from("Array"))),
        v => v.to_zstr(host).map(Some),
    }
}

#[allow(clippy::too_many_lines)]
fn recursive_call(
    host: &mut dyn SplHost,
    class: &str,
    method: &str,
    func: &str,
    this: &Obj,
    args: &[Value],
) -> RtResult<Value> {
    let st = state_of(host, this)?;
    match method {
        "__construct" => {
            rit_construct(host, this, &st, class, func, args)?;
            return Ok(Value::Null);
        }
        "setmaxdepth" => {
            let a = Args::new(func, args, &["maxDepth"], 0, Some(1))?;
            let d = a.opt_long(host, 0, -1)?;
            if d < -1 {
                return Err(a.value_error(0, "must be greater than or equal to -1"));
            }
            let d = d.min(i64::from(i32::MAX));
            st.recursive(|r| r.max_depth = d)?;
            return Ok(Value::Null);
        }
        "getsubiterator" => {
            let a = Args::new(func, args, &["level"], 0, Some(1))?;
            let level = a.long_or_null(host, 0)?;
            let cur = level_of(&st)?;
            let level = match level {
                None => match cur {
                    Some(l) => to_long(l),
                    None => return Err(not_initialized()),
                },
                Some(l) => {
                    if l < 0 || cur.is_none_or(|c| l > to_long(c)) {
                        return Ok(Value::Null);
                    }
                    l
                }
            };
            let s = sub(&st, usize::try_from(level).unwrap_or(0))?.ok_or_else(not_initialized)?;
            return Ok(Value::Object(s.obj));
        }
        "setprefixpart" => {
            let a = Args::new(func, args, &["part", "value"], 2, Some(2))?;
            let part = a.long(host, 0)?;
            let v = a.str(host, 1)?;
            if !(0..=5).contains(&part) {
                return Err(a.value_error(0, "must be a RecursiveTreeIterator::PREFIX_* constant"));
            }
            st.recursive(|r| r.prefix[usize::try_from(part).unwrap_or(0)] = v)?;
            return Ok(Value::Null);
        }
        "setpostfix" => {
            let a = Args::new(func, args, &["postfix"], 1, Some(1))?;
            let v = a.str(host, 0)?;
            st.recursive(|r| r.postfix = v)?;
            return Ok(Value::Null);
        }
        _ => Args::none(func, args)?,
    }
    match (class, method) {
        (_, "rewind") => {
            rit_rewind(host, this, &st)?;
            Ok(Value::Null)
        }
        (_, "valid") => rit_valid(host, this, &st).map(Value::Bool),
        ("RecursiveIteratorIterator", "key") => {
            let cur = current_sub(&st)?;
            it_call(&cur.it, host, |i, h| i.key(h)).map(Option::unwrap_or_default)
        }
        ("RecursiveIteratorIterator", "current") => {
            let cur = current_sub(&st)?;
            it_call(&cur.it, host, |i, h| i.current(h))
                .map(|v| v.map_or(Value::Null, |v| v.deref()))
        }
        (_, "next") => {
            rit_move_forward(host, this, &st)?;
            Ok(Value::Null)
        }
        (_, "getdepth") => Ok(Value::Int(level_of(&st)?.map_or(0, to_long))),
        (_, "getinneriterator") => Ok(Value::Object(current_sub(&st)?.obj)),
        (
            _,
            "beginiteration" | "enditeration" | "beginchildren" | "endchildren" | "nextelement",
        ) => Ok(Value::Null),
        (_, "callhaschildren") => {
            let Some(level) = level_of(&st)? else {
                return Ok(Value::Bool(false));
            };
            let s = sub(&st, level)?.ok_or_else(not_initialized)?;
            host.call_method(&s.obj, "hasChildren", &[])
        }
        (_, "callgetchildren") => {
            let s = current_sub(&st)?;
            host.call_method(&s.obj, "getChildren", &[])
        }
        (_, "getmaxdepth") => st.recursive(|r| {
            if r.max_depth == -1 {
                Value::Bool(false)
            } else {
                Value::Int(r.max_depth)
            }
        }),
        ("RecursiveTreeIterator", "getprefix") => {
            current_sub(&st)?;
            tree_prefix(host, &st).map(Value::Str)
        }
        ("RecursiveTreeIterator", "getentry") => {
            current_sub(&st)?;
            Ok(tree_entry(host, &st)?.map_or(Value::Null, Value::Str))
        }
        ("RecursiveTreeIterator", "getpostfix") => {
            current_sub(&st)?;
            st.recursive(|r| Value::Str(r.postfix.clone()))
        }
        ("RecursiveTreeIterator", "current") => {
            let flags = st.recursive(|r| r.flags)?;
            let cur = current_sub(&st)?;
            if flags & RTIT_BYPASS_CURRENT != 0 {
                return it_call(&cur.it, host, |i, h| i.current(h))
                    .map(|v| v.map_or(Value::Null, |v| v.deref()));
            }
            let Some(entry) = tree_entry(host, &st)? else {
                return Ok(Value::Null);
            };
            let prefix = tree_prefix(host, &st)?;
            let postfix = st.recursive(|r| r.postfix.clone())?;
            Ok(Value::Str(ZStr::from(
                [prefix.as_bytes(), entry.as_bytes(), postfix.as_bytes()].concat(),
            )))
        }
        ("RecursiveTreeIterator", "key") => {
            let flags = st.recursive(|r| r.flags)?;
            let cur = current_sub(&st)?;
            let key = it_call(&cur.it, host, |i, h| i.key(h))?.unwrap_or_default();
            if flags & RTIT_BYPASS_KEY != 0 {
                return Ok(key);
            }
            let ks = key.to_zstr(host)?;
            let prefix = tree_prefix(host, &st)?;
            let postfix = st.recursive(|r| r.postfix.clone())?;
            Ok(Value::Str(ZStr::from(
                [prefix.as_bytes(), ks.as_bytes(), postfix.as_bytes()].concat(),
            )))
        }
        _ => Err(RuntimeError::Error(format!(
            "Call to undefined method {func}()"
        ))),
    }
}

/// The `foreach` iterator of `RecursiveIteratorIterator`
/// (`spl_recursive_it_iterator_funcs`).
#[derive(Debug)]
pub struct RecursiveItIter {
    obj: Obj,
    st: Rc<SplObject>,
}

impl RecursiveItIter {
    /// Creates it over the object and its SPL state.
    #[must_use]
    pub fn new(obj: Obj, st: Rc<SplObject>) -> Self {
        RecursiveItIter { obj, st }
    }
}

impl Drop for RecursiveItIter {
    /// `spl_recursive_it_dtor`: releasing the `foreach` iterator releases
    /// every level below the top one (deepest first), leaving the object
    /// at depth 0.
    fn drop(&mut self) {
        let below = self.st.recursive(|r| {
            r.iterators
                .as_mut()
                .filter(|v| v.len() > 1)
                .map(|v| v.split_off(1))
        });
        if let Ok(Some(mut levels)) = below {
            while let Some(level) = levels.pop() {
                drop(level);
            }
        }
    }
}

impl ObjectIterator for RecursiveItIter {
    fn rewind(&mut self, host: &mut dyn SplHost) -> RtResult<()> {
        let st = state_of(host, &self.obj)?;
        rit_rewind(host, &self.obj, &st)
    }
    fn valid(&mut self, host: &mut dyn SplHost) -> RtResult<bool> {
        let st = state_of(host, &self.obj)?;
        rit_valid(host, &self.obj, &st)
    }
    fn current(&mut self, host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        let st = state_of(host, &self.obj)?;
        let cur = current_sub(&st)?;
        it_call(&cur.it, host, |i, h| i.current(h))
    }
    fn key(&mut self, host: &mut dyn SplHost) -> RtResult<Option<Value>> {
        let st = state_of(host, &self.obj)?;
        let cur = current_sub(&st)?;
        it_call(&cur.it, host, |i, h| i.key(h))
    }
    fn next(&mut self, host: &mut dyn SplHost) -> RtResult<()> {
        let st = state_of(host, &self.obj)?;
        rit_move_forward(host, &self.obj, &st)
    }
}

/// Checks that a `RecursiveIteratorIterator` was constructed (used when
/// creating its `foreach` iterator: "Object is not initialized").
pub fn check_recursive_initialized(st: &SplObject) -> RtResult<()> {
    if level_of(st)?.is_none() {
        return Err(error("Object is not initialized"));
    }
    Ok(())
}

/// `get_method` handlers: dual iterators and `RecursiveIteratorIterator`
/// forward unknown methods to the inner (current sub-) iterator. Returns
/// the object to call `method` on instead, if any.
pub fn method_target(host: &mut dyn SplHost, obj: &Obj) -> RtResult<Option<Obj>> {
    let Some(st) = host.spl_state(obj) else {
        return Ok(None);
    };
    if let Ok(inner) = st.dual(|d| d.inner.clone()) {
        return Ok(inner);
    }
    if let Ok(level) = st.recursive(|r| {
        r.iterators
            .as_ref()
            .and_then(|v| v.last().map(|s| s.obj.clone()))
    }) {
        if level.is_none() && st.recursive(|r| r.iterators.is_none())? {
            return Err(error(format!(
                "The {} instance wasn't initialized properly",
                obj.class_name()
            )));
        }
        return Ok(level);
    }
    Ok(None)
}

/// Whether the given value is a tree-iterator `Array` placeholder (helper
/// for tests).
#[must_use]
pub fn is_array_entry(v: &Value) -> bool {
    matches!(v, Value::Array(_))
}

#[cfg(test)]
mod tests {
    use super::{catchable, catches, CIT_CATCH_GET_CHILD};
    use php_runtime::RuntimeError;

    #[test]
    fn catch_get_child_never_swallows_unwind_exits() {
        let exception = RuntimeError::Error("boom".to_string());
        let exit = RuntimeError::Fatal(String::new());
        assert!(catches(CIT_CATCH_GET_CHILD, &exception));
        assert!(!catches(0, &exception));
        assert!(!catches(CIT_CATCH_GET_CHILD, &exit));
        assert!(matches!(
            catchable::<()>(CIT_CATCH_GET_CHILD, Err(exit)),
            Err(RuntimeError::Fatal(_))
        ));
        assert!(matches!(
            catchable::<()>(CIT_CATCH_GET_CHILD, Err(exception)),
            Ok(None)
        ));
    }
}
