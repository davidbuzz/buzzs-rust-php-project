// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `string.rot13`, `string.toupper` and `string.tolower`
//! (`ext/standard/filters.c`).

use super::{Brigade, FilterEnv, FilterFactory, FilterFlags, FilterStatus, StreamFilter};
use crate::value::Value;

/// Which byte mapping a [`StringFilter`] applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringOp {
    /// `string.rot13`.
    Rot13,
    /// `string.toupper`.
    ToUpper,
    /// `string.tolower`.
    ToLower,
}

impl StringOp {
    /// The filter label.
    pub fn label(self) -> &'static str {
        match self {
            StringOp::Rot13 => "string.rot13",
            StringOp::ToUpper => "string.toupper",
            StringOp::ToLower => "string.tolower",
        }
    }

    /// Maps one byte (`php_strtr` with the filter's tables).
    pub fn map(self, b: u8) -> u8 {
        match self {
            StringOp::Rot13 => match b {
                b'a'..=b'z' => (b - b'a' + 13) % 26 + b'a',
                b'A'..=b'Z' => (b - b'A' + 13) % 26 + b'A',
                _ => b,
            },
            StringOp::ToUpper => b.to_ascii_uppercase(),
            StringOp::ToLower => b.to_ascii_lowercase(),
        }
    }
}

/// A stateless byte-mapping filter.
#[derive(Debug, Clone, Copy)]
pub struct StringFilter {
    op: StringOp,
}

impl StringFilter {
    /// Creates the filter.
    pub fn new(op: StringOp) -> Self {
        StringFilter { op }
    }
}

impl StreamFilter for StringFilter {
    fn label(&self) -> &str {
        self.op.label()
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
        while let Some(mut bucket) = input.pop_front() {
            for b in &mut bucket.data {
                *b = self.op.map(*b);
            }
            n += bucket.data.len();
            output.push_back(bucket);
        }
        if let Some(c) = consumed {
            *c = n;
        }
        FilterStatus::PassOn
    }
}

/// Factory for one of the `string.*` filters (registered under its exact
/// name; the C factories ignore the requested name).
#[derive(Debug, Clone, Copy)]
pub struct StringFilterFactory(pub StringOp);

impl FilterFactory for StringFilterFactory {
    fn create(
        &self,
        _name: &str,
        _params: Option<&Value>,
        _persistent: bool,
    ) -> Option<Box<dyn StreamFilter>> {
        Some(Box::new(StringFilter::new(self.0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Bucket, ChainKind};

    fn run(op: StringOp, data: &[u8]) -> Vec<u8> {
        let mut f = StringFilter::new(op);
        let mut env = FilterEnv::detached(ChainKind::Read);
        let mut input: Brigade = data.chunks(3).map(Bucket::new).collect();
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
        assert_eq!(consumed, data.len());
        out.into_iter().flat_map(|b| b.data).collect()
    }

    #[test]
    fn mappings() {
        assert_eq!(
            run(StringOp::Rot13, b"Hello, World! \xff"),
            b"Uryyb, Jbeyq! \xff"
        );
        assert_eq!(run(StringOp::ToUpper, b"abc\xe9XyZ"), b"ABC\xe9XYZ");
        assert_eq!(run(StringOp::ToLower, b"ABC\xc9xYz"), b"abc\xc9xyz");
        assert_eq!(StringOp::Rot13.label(), "string.rot13");
    }
}
