//! The `convert.iconv.*` stream filter (`php_iconv_stream_filter_*`).

use crate::error::CSNMAXLEN;
use crate::ffi::Cd;

const STUB_SIZE: usize = 128;

/// State of one `convert.iconv.<from>.<to>` (or `<from>/<to>`) filter.
#[derive(Debug)]
pub struct IconvFilter {
    cd: Cd,
    to_charset: Vec<u8>,
    from_charset: Vec<u8>,
    stub: [u8; STUB_SIZE],
    stub_len: usize,
}

impl IconvFilter {
    /// The filter factory label.
    pub const LABEL: &'static str = "convert.iconv.*";

    /// `php_iconv_stream_filter_factory_create()`: parses
    /// `convert.iconv.FROM.TO` / `convert.iconv.FROM/TO`.  Returns `None`
    /// when the name is malformed, a charset is too long, or `iconv_open`
    /// fails (PHP then reports that the filter could not be created).
    pub fn from_filter_name(name: &[u8]) -> Option<Self> {
        let name = crate::error::c_prefix(name);
        let first = name.iter().position(|&b| b == b'.')? + 1;
        let second = first + name[first..].iter().position(|&b| b == b'.')? + 1;
        let rest = &name[second..];
        let delim = rest.iter().position(|&b| b == b'/' || b == b'.')?;
        let from = &rest[..delim];
        let to = &rest[delim + 1..];
        if from.len() >= CSNMAXLEN || to.len() >= CSNMAXLEN {
            return None;
        }
        Self::new(from, to)
    }

    /// `php_iconv_stream_filter_ctor()`.
    pub fn new(from_charset: &[u8], to_charset: &[u8]) -> Option<Self> {
        let cd = Cd::open(to_charset, from_charset).ok()?;
        Some(Self {
            cd,
            to_charset: to_charset.to_vec(),
            from_charset: from_charset.to_vec(),
            stub: [0; STUB_SIZE],
            stub_len: 0,
        })
    }

    fn warning(&self, what: &str) -> String {
        format!(
            "iconv stream filter (\"{}\"=>\"{}\"): {what}",
            String::from_utf8_lossy(&self.from_charset),
            String::from_utf8_lossy(&self.to_charset)
        )
    }

    fn grow(out: &mut Vec<u8>) {
        let n = (out.len() << 1).max(64);
        out.resize(n, 0);
    }

    /// `php_iconv_stream_filter_append_bucket()`.  `None` input flushes
    /// the converter (closing).  On failure returns the warning text (the
    /// filter then fails with `PSFS_ERR_FATAL`).
    pub fn append_bucket(&mut self, ps: Option<&[u8]>) -> Result<Vec<u8>, String> {
        let (initial, mut icnt) = match ps {
            None => (64, 1),
            Some(b) => (b.len(), b.len()),
        };
        let mut input: &[u8] = ps.unwrap_or(&[]);
        let mut out = vec![0u8; initial];
        let mut pos = 0usize;

        if self.stub_len > 0 {
            let mut off = 0usize;
            let mut tcnt = self.stub_len;
            while tcnt > 0 {
                let mut pt: &[u8] = &self.stub[off..self.stub_len];
                let r = self.cd.conv(&mut pt, &mut out, &mut pos);
                off = self.stub_len - pt.len();
                tcnt = pt.len();
                if let Err(e) = r {
                    match e {
                        libc::EILSEQ => return Err(self.warning("invalid multibyte sequence")),
                        libc::EINVAL => {
                            if ps.is_some() {
                                if icnt > 0 {
                                    if self.stub_len >= STUB_SIZE {
                                        return Err(self.warning("insufficient buffer"));
                                    }
                                    self.stub[self.stub_len] = input[0];
                                    self.stub_len += 1;
                                    input = &input[1..];
                                    icnt -= 1;
                                    off = 0;
                                    tcnt = self.stub_len;
                                } else {
                                    tcnt = 0;
                                }
                            } else {
                                return Err(self.warning("invalid multibyte sequence"));
                            }
                        }
                        libc::E2BIG => Self::grow(&mut out),
                        _ => return Err(self.warning("unknown error")),
                    }
                }
            }
            self.stub.copy_within(off..off + tcnt, 0);
            self.stub_len = tcnt;
        }

        while icnt > 0 {
            let r = if ps.is_none() {
                self.cd.flush(&mut out, &mut pos)
            } else {
                let r = self.cd.conv(&mut input, &mut out, &mut pos);
                icnt = input.len();
                r
            };
            match r {
                Err(libc::EILSEQ) => return Err(self.warning("invalid multibyte sequence")),
                Err(libc::EINVAL) => {
                    if ps.is_some() {
                        if icnt > STUB_SIZE {
                            return Err(self.warning("insufficient buffer"));
                        }
                        self.stub[..icnt].copy_from_slice(input);
                        self.stub_len = icnt;
                        input = &[];
                        icnt = 0;
                    } else {
                        return Err(self.warning("unexpected octet values"));
                    }
                }
                Err(libc::E2BIG) => Self::grow(&mut out),
                Err(_) => return Err(self.warning("unknown error")),
                Ok(_) => {
                    if ps.is_none() {
                        break;
                    }
                }
            }
        }
        out.truncate(pos);
        Ok(out)
    }

    /// Filters a set of input buckets (`php_iconv_stream_filter_do_filter`);
    /// `closing` (any flag other than `PSFS_FLAG_NORMAL`) flushes the shift
    /// state afterwards.
    pub fn filter_buckets(&mut self, buckets: &[&[u8]], closing: bool) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        for b in buckets {
            out.extend(self.append_bucket(Some(b))?);
        }
        if closing {
            out.extend(self.append_bucket(None)?);
        }
        Ok(out)
    }

    /// Filters `input` as a single bucket (no bucket when empty).
    pub fn filter(&mut self, input: &[u8], closing: bool) -> Result<Vec<u8>, String> {
        if input.is_empty() {
            self.filter_buckets(&[], closing)
        } else {
            self.filter_buckets(&[input], closing)
        }
    }

    /// `php_iconv_stream_filter_seek()`: resets the stub and the converter.
    pub fn seek(&mut self) -> Result<(), String> {
        self.stub_len = 0;
        match Cd::open(&self.to_charset, &self.from_charset) {
            Ok(cd) => {
                self.cd = cd;
                Ok(())
            }
            Err(_) => Err(self.warning("failed to reset conversion state")),
        }
    }
}
