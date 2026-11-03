//! User-space filters (`php_user_filter` subclasses, `user_filters.c`).

use std::fmt;
use std::rc::Rc;

use super::{
    Brigade, ChainKind, FilterEnv, FilterFactory, FilterFlags, FilterStatus, Seekable, StreamFilter,
};
use crate::diag;
use crate::value::{UserCall, Value};

/// A `php_user_filter` instance, implemented by the VM.
///
/// The VM exposes `input`/`output` to PHP code as the bucket brigade
/// resources passed to `filter()` (for instance by moving them into
/// resources for the duration of the call and back afterwards) and
/// implements `stream_bucket_make_writeable()` etc. with the helpers in this
/// module ([`make_writeable`], [`append`], [`prepend`]).
pub trait UserFilterObject: fmt::Debug {
    /// Calls `filter($in, $out, &$consumed, $closing)`.
    ///
    /// `stream_id` is the stream to expose as `$this->stream` (the VM sets
    /// the property, if it exists, before the call and resets it to null
    /// afterwards). `consumed` holds the by-reference `$consumed` (`None` is
    /// passed as PHP `null`); the VM writes back the final value.
    fn filter(
        &mut self,
        stream_id: u64,
        input: &mut Brigade,
        output: &mut Brigade,
        consumed: &mut Option<i64>,
        closing: bool,
    ) -> UserCall;

    /// Whether the class declares a `seek()` method.
    fn has_seek(&self) -> bool {
        false
    }

    /// Calls `seek($offset, $whence, $chain)`.
    fn seek(&mut self, _stream_id: u64, _offset: i64, _whence: i32, _chain: ChainKind) -> UserCall {
        UserCall::NotImplemented
    }

    /// Calls `onClose()`.
    fn on_close(&mut self) {}
}

/// A registered user filter class, implemented by the VM.
pub trait UserFilterClass: fmt::Debug {
    /// Instantiates the class, sets `filtername` and `params` and calls
    /// `onCreate()`. Returns the object and the `onCreate()` result.
    fn instantiate(
        &self,
        filtername: &str,
        params: Option<&Value>,
    ) -> Option<(Box<dyn UserFilterObject>, UserCall)>;
}

/// The factory registered by `stream_filter_register()`.
#[derive(Debug)]
pub struct UserFilterFactory {
    class: Rc<dyn UserFilterClass>,
}

impl UserFilterFactory {
    /// Wraps a user filter class.
    pub fn new(class: Rc<dyn UserFilterClass>) -> Self {
        UserFilterFactory { class }
    }
}

impl FilterFactory for UserFilterFactory {
    fn create(
        &self,
        name: &str,
        params: Option<&Value>,
        persistent: bool,
    ) -> Option<Box<dyn StreamFilter>> {
        if persistent {
            diag::warning("Cannot use a user-space filter with a persistent stream");
            return None;
        }
        let (obj, created) = self.class.instantiate(name, params)?;
        if matches!(created, UserCall::Returned(Value::Bool(false))) {
            return None;
        }
        Some(Box::new(UserFilter { obj: Some(obj) }))
    }
}

/// A user filter attached to a stream (`userfilter_ops`).
#[derive(Debug)]
pub struct UserFilter {
    obj: Option<Box<dyn UserFilterObject>>,
}

impl UserFilter {
    /// Wraps an already-created object.
    pub fn new(obj: Box<dyn UserFilterObject>) -> Self {
        UserFilter { obj: Some(obj) }
    }
}

impl StreamFilter for UserFilter {
    fn label(&self) -> &str {
        "user-filter"
    }

    fn filter(
        &mut self,
        env: &mut FilterEnv,
        input: &mut Brigade,
        output: &mut Brigade,
        consumed: Option<&mut usize>,
        flags: FilterFlags,
    ) -> FilterStatus {
        let Some(obj) = self.obj.as_mut() else {
            return FilterStatus::ErrFatal;
        };
        let has_consumed = consumed.is_some();
        let mut c = consumed
            .as_deref()
            .map(|c| i64::try_from(*c).unwrap_or(i64::MAX));
        let call = obj.filter(env.stream_id, input, output, &mut c, flags.is_closing());
        let status = match &call {
            UserCall::Returned(v) => FilterStatus::from_long(v.to_long()),
            UserCall::NotImplemented => {
                diag::warning("Failed to call filter function");
                FilterStatus::ErrFatal
            }
            UserCall::Threw => FilterStatus::ErrFatal,
        };
        if has_consumed {
            if let Some(out) = consumed {
                *out = usize::try_from(c.unwrap_or(0)).unwrap_or(0);
            }
        }
        if !input.is_empty() {
            diag::warning("Unprocessed filter buckets remaining on input brigade");
        }
        status
    }

    fn seek(&mut self, env: &mut FilterEnv, offset: i64, whence: i32) -> bool {
        let Some(obj) = self.obj.as_mut() else {
            return false;
        };
        if !obj.has_seek() {
            return true;
        }
        obj.seek(env.stream_id, offset, whence, env.chain).is_true()
    }

    fn read_seekable(&self) -> Seekable {
        Seekable::Check
    }

    fn write_seekable(&self) -> Seekable {
        Seekable::Check
    }

    fn close(&mut self) {
        if let Some(mut obj) = self.obj.take() {
            obj.on_close();
        }
    }
}

/// `stream_bucket_make_writeable()`: detaches the head bucket.
pub fn make_writeable(brigade: &mut Brigade) -> Option<super::Bucket> {
    brigade.pop_front()
}

/// `stream_bucket_append()`.
pub fn append(brigade: &mut Brigade, bucket: super::Bucket) {
    brigade.push_back(bucket);
}

/// `stream_bucket_prepend()`.
pub fn prepend(brigade: &mut Brigade, bucket: super::Bucket) {
    brigade.push_front(bucket);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Bucket, FilterRegistry};

    #[derive(Debug)]
    struct Obj;
    impl UserFilterObject for Obj {
        fn filter(
            &mut self,
            _stream_id: u64,
            input: &mut Brigade,
            output: &mut Brigade,
            consumed: &mut Option<i64>,
            _closing: bool,
        ) -> UserCall {
            while let Some(mut b) = make_writeable(input) {
                b.data.reverse();
                *consumed = Some(consumed.unwrap_or(0) + i64::try_from(b.data.len()).unwrap());
                append(output, b);
            }
            UserCall::Returned(Value::Int(2))
        }
    }

    #[derive(Debug)]
    struct Class(bool);
    impl UserFilterClass for Class {
        fn instantiate(
            &self,
            _filtername: &str,
            _params: Option<&Value>,
        ) -> Option<(Box<dyn UserFilterObject>, UserCall)> {
            Some((Box::new(Obj), UserCall::Returned(Value::Bool(self.0))))
        }
    }

    #[test]
    fn user_filter_roundtrip() {
        let _ = diag::take();
        let mut reg = FilterRegistry::new();
        reg.register("rev", Rc::new(UserFilterFactory::new(Rc::new(Class(true)))));
        reg.register(
            "bad",
            Rc::new(UserFilterFactory::new(Rc::new(Class(false)))),
        );
        let mut f = reg.create("rev", None, false).unwrap();
        let mut env = FilterEnv::detached(ChainKind::Read);
        let mut input = Brigade::from(vec![Bucket::new("abc")]);
        let mut out = Brigade::new();
        let mut consumed = 0;
        let st = f.filter(
            &mut env,
            &mut input,
            &mut out,
            Some(&mut consumed),
            FilterFlags::Normal,
        );
        assert_eq!(st, FilterStatus::PassOn);
        assert_eq!(out[0].data, b"cba");
        assert_eq!(consumed, 3);
        assert!(reg.create("bad", None, false).is_none());
        assert!(reg.create("rev", None, true).is_none());
        let d = diag::take();
        assert_eq!(d[0].text(), "Unable to create or locate filter \"bad\"");
        assert_eq!(
            d[1].text(),
            "Cannot use a user-space filter with a persistent stream"
        );
    }
}
