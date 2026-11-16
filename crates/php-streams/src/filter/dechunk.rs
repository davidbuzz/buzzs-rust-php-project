// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `dechunk` filter: HTTP chunked transfer decoding
//! (`php_dechunk()` in `ext/standard/filters.c`).

use super::{Brigade, FilterEnv, FilterFactory, FilterFlags, FilterStatus, Seekable, StreamFilter};
use crate::value::Value;

/// `php_chunked_filter_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    SizeStart,
    Size,
    SizeExt,
    SizeCr,
    SizeLf,
    Body,
    BodyCr,
    BodyLf,
    Trailer,
    Error,
}

/// The `dechunk` filter.
#[derive(Debug, Clone)]
pub struct DechunkFilter {
    chunk_size: usize,
    state: State,
}

impl Default for DechunkFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl DechunkFilter {
    /// Creates the filter in its initial state.
    pub fn new() -> Self {
        DechunkFilter {
            chunk_size: 0,
            state: State::SizeStart,
        }
    }

    /// `php_dechunk()`: decodes `buf` in place and returns the output length.
    ///
    /// The C code is a `switch` with fall-through between cases; `case_`
    /// tracks the case being executed while `self.state` is only updated
    /// where the C code assigns `data->state` (a `continue` re-dispatches on
    /// the stored state).
    #[allow(clippy::too_many_lines)]
    pub fn dechunk(&mut self, buf: &mut Vec<u8>) -> usize {
        let end = buf.len();
        let mut p = 0usize;
        let mut out = 0usize;
        let mut out_len = 0usize;

        'outer: while p < end {
            let mut case_ = self.state;
            loop {
                match case_ {
                    State::SizeStart | State::Size => {
                        if case_ == State::SizeStart {
                            self.chunk_size = 0;
                        }
                        while p < end {
                            let c = buf[p];
                            let digit = match c {
                                b'0'..=b'9' => usize::from(c - b'0'),
                                b'A'..=b'F' => usize::from(c - b'A' + 10),
                                b'a'..=b'f' => usize::from(c - b'a' + 10),
                                _ => {
                                    if self.state == State::SizeStart {
                                        self.state = State::Error;
                                    } else {
                                        self.state = State::SizeExt;
                                    }
                                    break;
                                }
                            };
                            if self.chunk_size > usize::MAX / 16 {
                                self.state = State::Error;
                                break;
                            }
                            self.chunk_size = self.chunk_size * 16 + digit;
                            self.state = State::Size;
                            p += 1;
                        }
                        if self.state == State::Error {
                            continue 'outer;
                        } else if p == end {
                            break 'outer;
                        }
                        case_ = State::SizeExt;
                    }
                    State::SizeExt => {
                        while p < end && buf[p] != b'\r' && buf[p] != b'\n' {
                            p += 1;
                        }
                        if p == end {
                            break 'outer;
                        }
                        case_ = State::SizeCr;
                    }
                    State::SizeCr => {
                        if buf[p] == b'\r' {
                            p += 1;
                            if p == end {
                                self.state = State::SizeLf;
                                break 'outer;
                            }
                        }
                        case_ = State::SizeLf;
                    }
                    State::SizeLf => {
                        if buf[p] == b'\n' {
                            p += 1;
                            if self.chunk_size == 0 {
                                self.state = State::Trailer;
                                continue 'outer;
                            } else if p == end {
                                self.state = State::Body;
                                break 'outer;
                            }
                        } else {
                            self.state = State::Error;
                            continue 'outer;
                        }
                        case_ = State::Body;
                    }
                    State::Body => {
                        if end - p >= self.chunk_size {
                            buf.copy_within(p..p + self.chunk_size, out);
                            out += self.chunk_size;
                            out_len += self.chunk_size;
                            p += self.chunk_size;
                            if p == end {
                                self.state = State::BodyCr;
                                break 'outer;
                            }
                        } else {
                            buf.copy_within(p..end, out);
                            self.chunk_size -= end - p;
                            self.state = State::Body;
                            out_len += end - p;
                            break 'outer;
                        }
                        case_ = State::BodyCr;
                    }
                    State::BodyCr => {
                        if buf[p] == b'\r' {
                            p += 1;
                            if p == end {
                                self.state = State::BodyLf;
                                break 'outer;
                            }
                        }
                        case_ = State::BodyLf;
                    }
                    State::BodyLf => {
                        if buf[p] == b'\n' {
                            p += 1;
                            self.state = State::SizeStart;
                        } else {
                            self.state = State::Error;
                        }
                        continue 'outer;
                    }
                    State::Trailer => {
                        p = end;
                        continue 'outer;
                    }
                    State::Error => {
                        buf.copy_within(p..end, out);
                        out_len += end - p;
                        break 'outer;
                    }
                }
            }
        }
        buf.truncate(out_len);
        out_len
    }
}

impl StreamFilter for DechunkFilter {
    fn label(&self) -> &'static str {
        "dechunk"
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
            n += bucket.data.len();
            self.dechunk(&mut bucket.data);
            output.push_back(bucket);
        }
        if let Some(c) = consumed {
            *c = n;
        }
        FilterStatus::PassOn
    }

    fn seek(&mut self, _env: &mut FilterEnv, _offset: i64, _whence: i32) -> bool {
        self.state = State::SizeStart;
        self.chunk_size = 0;
        true
    }

    fn read_seekable(&self) -> Seekable {
        Seekable::Start
    }

    fn write_seekable(&self) -> Seekable {
        Seekable::Always
    }
}

/// Factory for `dechunk` (`chunked_filter_create`).
#[derive(Debug, Clone, Copy, Default)]
pub struct DechunkFilterFactory;

impl FilterFactory for DechunkFilterFactory {
    fn create(
        &self,
        name: &str,
        _params: Option<&Value>,
        _persistent: bool,
    ) -> Option<Box<dyn StreamFilter>> {
        if !name.eq_ignore_ascii_case("dechunk") {
            return None;
        }
        Some(Box::new(DechunkFilter::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(data: &[u8], split: usize) -> Vec<u8> {
        let mut f = DechunkFilter::new();
        let mut out = Vec::new();
        for chunk in data.chunks(split.max(1)) {
            let mut b = chunk.to_vec();
            f.dechunk(&mut b);
            out.extend(b);
        }
        out
    }

    #[test]
    fn valid_chunks_any_split() {
        let data = b"5\r\nhello\r\n7;ext=1\r\n, world\r\n0\r\nTrailer: x\r\n\r\n";
        for split in [1, 2, 3, 5, 7, 100] {
            assert_eq!(run(data, split), b"hello, world", "split {split}");
        }
    }

    #[test]
    fn lf_only_and_errors() {
        assert_eq!(run(b"3\nabc\n0\n", 100), b"abc");
        // Not a hex size at the start: the rest passes through unchanged.
        assert_eq!(run(b"xyz", 100), b"xyz");
        // Missing CRLF after body: error state passes remaining data.
        assert_eq!(run(b"2\r\nabXY", 100), b"abXY");
        assert_eq!(run(b"a\r\n0123456789\r\n0\r\n", 4), b"0123456789");
    }

    #[test]
    fn factory_checks_name() {
        assert!(DechunkFilterFactory
            .create("dechunk", None, false)
            .is_some());
        assert!(DechunkFilterFactory
            .create("DeChunk", None, false)
            .is_some());
        assert!(DechunkFilterFactory
            .create("dechunk2", None, false)
            .is_none());
    }
}
