// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `convert.iconv.*` stream filter, registered with the `php-streams`
//! filter registry (`php_iconv_stream_filter_register_factory()`).

use std::rc::Rc;

use php_iconv::IconvFilter;
use php_streams::diag;
use php_streams::filter::{
    parse_write_seek_mode, Brigade, Bucket, FilterEnv, FilterFactory, FilterFlags, FilterStatus,
    Seekable, StreamFilter,
};
use php_streams::value::Value;

/// One attached `convert.iconv.FROM.TO` filter.
#[derive(Debug)]
struct IconvStreamFilter {
    inner: IconvFilter,
    write_seekable: Seekable,
}

impl StreamFilter for IconvStreamFilter {
    fn label(&self) -> &'static str {
        IconvFilter::LABEL
    }

    /// `php_iconv_stream_filter_do_filter()`: every input bucket is
    /// converted into one output bucket; any flush also flushes the
    /// converter's shift state.
    fn filter(
        &mut self,
        _env: &mut FilterEnv,
        input: &mut Brigade,
        output: &mut Brigade,
        consumed: Option<&mut usize>,
        flags: FilterFlags,
    ) -> FilterStatus {
        let mut total = 0usize;
        while let Some(bucket) = input.pop_front() {
            total += bucket.data.len();
            match self.inner.append_bucket(Some(&bucket.data)) {
                Ok(out) => {
                    if !out.is_empty() {
                        output.push_back(Bucket::new(out));
                    }
                }
                Err(msg) => {
                    diag::warning(msg);
                    return FilterStatus::ErrFatal;
                }
            }
        }
        if flags.is_flush() {
            match self.inner.append_bucket(None) {
                Ok(out) => {
                    if !out.is_empty() {
                        output.push_back(Bucket::new(out));
                    }
                }
                Err(msg) => {
                    diag::warning(msg);
                    return FilterStatus::ErrFatal;
                }
            }
        }
        if let Some(c) = consumed {
            *c = total;
        }
        FilterStatus::PassOn
    }

    fn seek(&mut self, _env: &mut FilterEnv, _offset: i64, _whence: i32) -> bool {
        match self.inner.seek() {
            Ok(()) => true,
            Err(msg) => {
                diag::warning(msg);
                false
            }
        }
    }

    fn read_seekable(&self) -> Seekable {
        Seekable::Start
    }

    fn write_seekable(&self) -> Seekable {
        self.write_seekable
    }
}

/// The `convert.iconv.*` factory (`php_iconv_stream_filter_factory_create`).
#[derive(Debug, Clone, Copy, Default)]
struct IconvFilterFactory;

impl FilterFactory for IconvFilterFactory {
    fn create(
        &self,
        name: &str,
        params: Option<&Value>,
        _persistent: bool,
    ) -> Option<Box<dyn StreamFilter>> {
        let write_seekable = parse_write_seek_mode(params)?;
        let inner = IconvFilter::from_filter_name(name.as_bytes())?;
        Some(Box::new(IconvStreamFilter {
            inner,
            write_seekable,
        }))
    }
}

/// Registers the factory (`MINIT`).
pub(crate) fn register() {
    php_streams::wrapper::register_filter_global(IconvFilter::LABEL, Rc::new(IconvFilterFactory));
}

#[cfg(test)]
mod tests {
    use super::*;
    use php_streams::filter::ChainKind;

    #[test]
    fn converts_buckets() {
        let mut f = IconvFilterFactory
            .create("convert.iconv.ISO-8859-1/UTF-8", None, false)
            .expect("filter");
        let mut env = FilterEnv::detached(ChainKind::Read);
        let mut input = Brigade::new();
        input.push_back(Bucket::new(vec![0xE9]));
        let mut output = Brigade::new();
        let st = f.filter(
            &mut env,
            &mut input,
            &mut output,
            None,
            FilterFlags::FlushClose,
        );
        assert_eq!(st, FilterStatus::PassOn);
        let data: Vec<u8> = output.into_iter().flat_map(|b| b.data).collect();
        assert_eq!(data, "é".as_bytes());
    }

    #[test]
    fn rejects_malformed_names() {
        assert!(IconvFilterFactory
            .create("convert.iconv.UTF-8", None, false)
            .is_none());
    }
}
