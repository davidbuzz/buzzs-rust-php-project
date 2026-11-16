// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `consumed` filter (`ext/standard/filters.c`).
//!
//! It passes data through unchanged while counting the bytes it saw; on the
//! closing flush it seeks the stream to the position just after the consumed
//! data. Since a filter cannot borrow its stream, the seek is requested
//! through [`FilterEnv::seek_request`].

use super::{Brigade, FilterEnv, FilterFactory, FilterFlags, FilterStatus, Seekable, StreamFilter};
use crate::value::Value;

/// The `consumed` filter.
#[derive(Debug, Clone, Default)]
pub struct ConsumedFilter {
    consumed: i64,
    /// Stream position when the filter first ran (`offset == ~0` in C means
    /// "not yet known").
    offset: Option<i64>,
}

impl ConsumedFilter {
    /// Creates the filter.
    pub fn new() -> Self {
        Self::default()
    }
}

impl StreamFilter for ConsumedFilter {
    fn label(&self) -> &'static str {
        "consumed"
    }

    fn filter(
        &mut self,
        env: &mut FilterEnv,
        input: &mut Brigade,
        output: &mut Brigade,
        consumed: Option<&mut usize>,
        flags: FilterFlags,
    ) -> FilterStatus {
        let offset = *self.offset.get_or_insert(env.position);
        let mut n = 0usize;
        while let Some(bucket) = input.pop_front() {
            n += bucket.data.len();
            output.push_back(bucket);
        }
        if let Some(c) = consumed {
            *c = n;
        }
        if flags.is_closing() {
            env.seek_request = Some(offset + self.consumed);
        }
        self.consumed += i64::try_from(n).unwrap_or(i64::MAX);
        FilterStatus::PassOn
    }

    fn seek(&mut self, _env: &mut FilterEnv, _offset: i64, _whence: i32) -> bool {
        self.consumed = 0;
        self.offset = None;
        true
    }

    fn read_seekable(&self) -> Seekable {
        Seekable::Start
    }

    fn write_seekable(&self) -> Seekable {
        Seekable::Always
    }
}

/// Factory for `consumed` (`consumed_filter_create`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ConsumedFilterFactory;

impl FilterFactory for ConsumedFilterFactory {
    fn create(
        &self,
        name: &str,
        _params: Option<&Value>,
        _persistent: bool,
    ) -> Option<Box<dyn StreamFilter>> {
        if !name.eq_ignore_ascii_case("consumed") {
            return None;
        }
        Some(Box::new(ConsumedFilter::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Bucket, ChainKind};

    #[test]
    fn counts_and_requests_seek_on_close() {
        let mut f = ConsumedFilter::new();
        let mut env = FilterEnv::detached(ChainKind::Read);
        env.position = 10;
        let mut input = Brigade::from(vec![Bucket::new("abc"), Bucket::new("de")]);
        let mut out = Brigade::new();
        let mut c = 0;
        let st = f.filter(
            &mut env,
            &mut input,
            &mut out,
            Some(&mut c),
            FilterFlags::Normal,
        );
        assert_eq!(st, FilterStatus::PassOn);
        assert_eq!(c, 5);
        assert_eq!(out.len(), 2);
        assert_eq!(env.seek_request, None);
        env.position = 99;
        let mut input = Brigade::from(vec![Bucket::new("f")]);
        f.filter(
            &mut env,
            &mut input,
            &mut out,
            None,
            FilterFlags::FlushClose,
        );
        // offset (10, captured on first run) + bytes consumed before this call
        assert_eq!(env.seek_request, Some(15));
        assert!(f.seek(&mut env, 0, 0));
        assert!(ConsumedFilterFactory
            .create("consumed", None, false)
            .is_some());
        assert!(ConsumedFilterFactory.create("x", None, false).is_none());
    }
}
