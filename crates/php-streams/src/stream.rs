//! The core stream object (`php_stream`, `main/streams/streams.c`).
//!
//! A [`Stream`] owns a [`StreamOps`] implementation (the C `php_stream_ops`
//! plus its `abstract` data) and the generic machinery around it: the read
//! buffer with chunk-size semantics, position tracking, EOF state, and the
//! read/write filter chains. PHP writes through: there is no write buffer.

use std::any::Any;
use std::fmt;
use std::os::fd::RawFd;

use crate::context::ContextRef;
use crate::diag::{self, Severity, StreamErrorCode};
use crate::filter::{
    Brigade, Bucket, ChainKind, FilterChain, FilterEnv, FilterFlags, FilterId, FilterStatus,
    Seekable, StreamFilter,
};
use crate::stat::StatBuf;
use crate::value::Value;

/// Converts a byte count to a stream offset (saturating).
fn to_i64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Default read chunk size (`CHUNK_SIZE`).
pub const CHUNK_SIZE: usize = 8192;

/// `SEEK_SET`.
pub const SEEK_SET: i32 = 0;
/// `SEEK_CUR`.
pub const SEEK_CUR: i32 = 1;
/// `SEEK_END`.
pub const SEEK_END: i32 = 2;

/// `PHP_STREAM_OPTION_RETURN_OK`.
pub const OPT_OK: i64 = 0;
/// `PHP_STREAM_OPTION_RETURN_ERR`.
pub const OPT_ERR: i64 = -1;
/// `PHP_STREAM_OPTION_RETURN_NOTIMPL`.
pub const OPT_NOTIMPL: i64 = -2;

/// Stream flags (`PHP_STREAM_FLAG_*`).
pub mod flags {
    /// Not seekable.
    pub const NO_SEEK: u32 = 0x1;
    /// Unbuffered reads.
    pub const NO_BUFFER: u32 = 0x2;
    /// `auto_detect_line_endings`: detect EOL on first line.
    pub const DETECT_EOL: u32 = 0x4;
    /// Mac (`\r`) line endings detected.
    pub const EOL_MAC: u32 = 0x8;
    /// Avoid blocking reads.
    pub const AVOID_BLOCKING: u32 = 0x10;
    /// Do not close the underlying handle.
    pub const NO_CLOSE: u32 = 0x20;
    /// Directory stream.
    pub const IS_DIR: u32 = 0x40;
    /// `fclose()` is not allowed (stream in use by a callback).
    pub const NO_FCLOSE: u32 = 0x80;
    /// Suppress read/write errors.
    pub const SUPPRESS_ERRORS: u32 = 0x100;
    /// Data was written since the last flush.
    pub const WAS_WRITTEN: u32 = 0x8000_0000;
}

/// Options passed to [`StreamOps::set_option`] (`PHP_STREAM_OPTION_*`).
#[derive(Debug)]
pub enum StreamOption<'a> {
    /// `PHP_STREAM_OPTION_BLOCKING`.
    Blocking(bool),
    /// `PHP_STREAM_OPTION_READ_TIMEOUT` (seconds, microseconds).
    ReadTimeout(i64, i64),
    /// `PHP_STREAM_OPTION_READ_BUFFER` (mode, size).
    ReadBuffer(BufferMode, Option<usize>),
    /// `PHP_STREAM_OPTION_WRITE_BUFFER` (mode, size).
    WriteBuffer(BufferMode, Option<usize>),
    /// `PHP_STREAM_OPTION_SET_CHUNK_SIZE`.
    ChunkSize(usize),
    /// `PHP_STREAM_OPTION_LOCKING` with an `flock()` operation (0 = query).
    Locking(i32),
    /// `PHP_STREAM_OPTION_LOCKING` with `PHP_STREAM_LOCK_SUPPORTED`.
    LockSupported,
    /// `PHP_STREAM_OPTION_CHECK_LIVENESS`.
    CheckLiveness,
    /// `PHP_STREAM_TRUNCATE_SUPPORTED`.
    TruncateSupported,
    /// `PHP_STREAM_TRUNCATE_SET_SIZE`.
    TruncateSetSize(i64),
    /// `PHP_STREAM_SYNC_SUPPORTED`.
    SyncSupported,
    /// `PHP_STREAM_SYNC_FSYNC` / `FDSYNC` (data only).
    Sync(bool),
    /// `PHP_STREAM_OPTION_META_DATA_API`: append entries.
    MetaData(&'a mut Vec<(String, Value)>),
}

/// `PHP_STREAM_BUFFER_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferMode {
    /// `PHP_STREAM_BUFFER_NONE`.
    None,
    /// `PHP_STREAM_BUFFER_LINE`.
    Line,
    /// `PHP_STREAM_BUFFER_FULL`.
    Full,
}

/// Cast targets (`PHP_STREAM_AS_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastAs {
    /// `PHP_STREAM_AS_STDIO`.
    Stdio,
    /// `PHP_STREAM_AS_FD`.
    Fd,
    /// `PHP_STREAM_AS_SOCKETD`.
    SocketFd,
    /// `PHP_STREAM_AS_FD_FOR_SELECT`.
    FdForSelect,
}

/// An operation failure (`-1` in C).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpFailed;

impl fmt::Display for OpFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("stream operation failed")
    }
}

impl std::error::Error for OpFailed {}

/// The per-stream state an ops implementation may read or change.
#[derive(Debug)]
pub struct StreamState {
    /// Unique stream id.
    pub id: u64,
    /// EOF flag (`stream->eof`).
    pub eof: bool,
    /// Fatal filter error flag.
    pub fatal_error: bool,
    /// `PHP_STREAM_FLAG_*`.
    pub flags: u32,
    /// The wrapper label used in error reports (`"stream"` when none).
    pub wrapper_label: &'static str,
    /// `stream->has_buffered_data`: data was already returned by the
    /// running `read()` before this low-level read (socket ops then avoid
    /// blocking).
    pub has_buffered_data: bool,
    /// `errno` of the last failed operation that PHP inspects afterwards
    /// (e.g. `EWOULDBLOCK` after a failed non-blocking `flock()`).
    pub last_errno: i32,
}

impl StreamState {
    /// Reports a stream error (`php_stream_error`).
    pub fn error(
        &self,
        severity: Severity,
        code: StreamErrorCode,
        terminating: bool,
        msg: impl Into<Vec<u8>>,
    ) {
        diag::stream_error(severity, self.wrapper_label, code, terminating, msg);
    }

    /// `php_stream_warn`.
    pub fn warn(&self, code: StreamErrorCode, msg: impl Into<Vec<u8>>) {
        self.error(Severity::Warning, code, true, msg);
    }

    /// `php_stream_warn_nt`.
    pub fn warn_nt(&self, code: StreamErrorCode, msg: impl Into<Vec<u8>>) {
        self.error(Severity::Warning, code, false, msg);
    }

    /// `php_stream_notice`.
    pub fn notice(&self, code: StreamErrorCode, msg: impl Into<Vec<u8>>) {
        self.error(Severity::Notice, code, false, msg);
    }
}

/// The operations of a stream implementation (`php_stream_ops`).
pub trait StreamOps: fmt::Debug + Any {
    /// The ops label shown as `stream_type` (`"STDIO"`, `"MEMORY"`, …).
    fn label(&self) -> &'static str;

    /// Reads into `buf`. `Ok(0)` means no data (EOF or would block); set
    /// `st.eof` at end of file.
    fn read(&mut self, st: &mut StreamState, buf: &mut [u8]) -> Result<usize, OpFailed>;

    /// Whether the stream has a write operation at all.
    fn can_write(&self) -> bool {
        true
    }

    /// Writes `buf`, returning the number of bytes written.
    fn write(&mut self, _st: &mut StreamState, _buf: &[u8]) -> Result<usize, OpFailed> {
        Err(OpFailed)
    }

    /// Closes the stream; returns the close result (0 on success; `pclose`
    /// status for process pipes).
    fn close(&mut self, _st: &mut StreamState, _preserve_handle: bool) -> i32 {
        0
    }

    /// Flushes; returns 0 on success.
    fn flush(&mut self, _st: &mut StreamState) -> i32 {
        0
    }

    /// Whether the ops have a seek operation.
    fn can_seek(&self) -> bool {
        false
    }

    /// Seeks; on success stores the new offset in `newpos` and returns true.
    /// Implementations may store into `newpos` on failure too (memory
    /// streams store `-1`, which PHP then reports through `ftell()`).
    fn seek(
        &mut self,
        _st: &mut StreamState,
        _offset: i64,
        _whence: i32,
        _newpos: &mut i64,
    ) -> bool {
        false
    }

    /// Stats the open stream.
    fn stat(&mut self, _st: &mut StreamState) -> Option<StatBuf> {
        None
    }

    /// Sets an option; returns `OPT_OK`, `OPT_ERR`, `OPT_NOTIMPL` or an
    /// option-specific value.
    fn set_option(&mut self, _st: &mut StreamState, _opt: &mut StreamOption<'_>) -> i64 {
        OPT_NOTIMPL
    }

    /// Casts to a file descriptor. `report_errors` is false for capability
    /// checks.
    fn cast(
        &mut self,
        _st: &mut StreamState,
        _kind: CastAs,
        _report_errors: bool,
    ) -> Option<RawFd> {
        None
    }

    /// Reads the next directory entry (directory streams only).
    fn readdir(&mut self, _st: &mut StreamState) -> Option<Vec<u8>> {
        None
    }

    /// Whether this is a user-space stream (`PHP_STREAM_IS_USERSPACE`).
    fn is_userspace(&self) -> bool {
        false
    }

    /// Downcasting support.
    fn as_any(&self) -> &dyn Any;

    /// Mutable downcasting support.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Describes the wrapper a stream was opened through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapperInfo {
    /// The wrapper ops label (`wrapper_type` in the meta data).
    pub label: &'static str,
    /// `is_url`.
    pub is_url: bool,
    /// Whether this is the plain files wrapper (enables greedy reads).
    pub is_plain_files: bool,
}

/// Called with the stream id when a stream is closed, before its handle is
/// released (`php_io_poll_stream_notify_close()`).
pub type CloseHook = std::rc::Rc<dyn Fn(u64)>;

thread_local! {
    static NEXT_STREAM_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    static CLOSE_HOOK: std::cell::RefCell<Option<CloseHook>> = const { std::cell::RefCell::new(None) };
}

/// Installs the stream close hook.
pub fn set_close_hook(hook: Option<CloseHook>) {
    CLOSE_HOOK.with(|h| *h.borrow_mut() = hook);
}

fn fresh_id() -> u64 {
    NEXT_STREAM_ID.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    })
}

/// A PHP stream.
pub struct Stream {
    ops: Box<dyn StreamOps>,
    /// Ops-visible state (eof, flags, …).
    pub state: StreamState,
    /// The open mode string.
    pub mode: String,
    /// The path the stream was opened with (`uri`).
    pub orig_path: Option<Vec<u8>>,
    /// The wrapper, if opened through one.
    pub wrapper: Option<WrapperInfo>,
    /// The attached context.
    pub context: Option<ContextRef>,
    /// Logical position (`stream->position`).
    pub position: i64,
    /// Read chunk size.
    pub chunk_size: usize,
    readbuf: Vec<u8>,
    readpos: usize,
    writepos: usize,
    /// The read filter chain.
    pub read_filters: FilterChain,
    /// The write filter chain.
    pub write_filters: FilterChain,
    closed: bool,
    /// Last lock operation applied (`flock()` bookkeeping).
    pub lock_flag: i32,
    /// Whether the stream was opened for `include` (`fclose()` refuses it).
    pub is_include: bool,
    /// `stream->wrapperdata` set by a built-in wrapper (the `http://`
    /// response headers); user-space wrapper objects are VM-side.
    pub wrapper_data: Option<Value>,
}

impl fmt::Debug for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Stream")
            .field("id", &self.state.id)
            .field("ops", &self.ops.label())
            .field("mode", &self.mode)
            .field("position", &self.position)
            .field("eof", &self.state.eof)
            .finish_non_exhaustive()
    }
}

/// The result of `stream_get_meta_data()`, in PHP's key order.
pub type MetaData = Vec<(String, Value)>;

impl Stream {
    /// `php_stream_alloc()`.
    pub fn new(ops: Box<dyn StreamOps>, mode: &str) -> Self {
        let detect = crate::state::settings().auto_detect_line_endings;
        let mut s = Self::alloc(ops, mode);
        if detect {
            s.state.flags |= flags::DETECT_EOL;
        }
        s
    }

    fn alloc(ops: Box<dyn StreamOps>, mode: &str) -> Self {
        Stream {
            ops,
            state: StreamState {
                id: fresh_id(),
                eof: false,
                fatal_error: false,
                flags: 0,
                wrapper_label: "stream",
                has_buffered_data: false,
                last_errno: 0,
            },
            mode: mode.to_string(),
            orig_path: None,
            wrapper: None,
            context: None,
            position: 0,
            chunk_size: CHUNK_SIZE,
            readbuf: Vec::new(),
            readpos: 0,
            writepos: 0,
            read_filters: FilterChain::default(),
            write_filters: FilterChain::default(),
            closed: false,
            lock_flag: 0,
            is_include: false,
            wrapper_data: None,
        }
    }

    /// The stream id (used for `Value::Stream`, user filter `$this->stream`).
    pub fn id(&self) -> u64 {
        self.state.id
    }

    /// Sets the wrapper the stream was opened with.
    pub fn set_wrapper(&mut self, wrapper: WrapperInfo) {
        self.state.wrapper_label = wrapper.label;
        self.wrapper = Some(wrapper);
    }

    /// The ops label (`stream_type`).
    pub fn ops_label(&self) -> &'static str {
        self.ops.label()
    }

    /// Access to the ops implementation.
    pub fn ops(&self) -> &dyn StreamOps {
        &*self.ops
    }

    /// Mutable access to the ops implementation.
    pub fn ops_mut(&mut self) -> &mut dyn StreamOps {
        &mut *self.ops
    }

    /// Downcasts the ops implementation.
    pub fn ops_as<T: StreamOps>(&self) -> Option<&T> {
        self.ops.as_any().downcast_ref::<T>()
    }

    /// Mutably downcasts the ops implementation.
    pub fn ops_as_mut<T: StreamOps>(&mut self) -> Option<&mut T> {
        self.ops.as_any_mut().downcast_mut::<T>()
    }

    /// Whether a flag is set.
    pub fn has_flag(&self, flag: u32) -> bool {
        self.state.flags & flag != 0
    }

    /// Sets flags.
    pub fn add_flags(&mut self, flag: u32) {
        self.state.flags |= flag;
    }

    /// Clears flags.
    pub fn clear_flags(&mut self, flag: u32) {
        self.state.flags &= !flag;
    }

    /// Whether the stream was closed.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Number of buffered unread bytes (`unread_bytes`).
    pub fn unread_bytes(&self) -> usize {
        self.writepos - self.readpos
    }

    /// Whether the stream is seekable (`seekable` meta data).
    pub fn is_seekable(&self) -> bool {
        self.ops.can_seek() && !self.has_flag(flags::NO_SEEK)
    }

    /// Whether any filter is attached (`php_stream_is_filtered`).
    pub fn is_filtered(&self) -> bool {
        !self.read_filters.is_empty() || !self.write_filters.is_empty()
    }

    /// Whether the read loop may be greedy (plain files, memory, temp).
    fn greedy(&self) -> bool {
        self.wrapper.as_ref().is_some_and(|w| w.is_plain_files)
            || matches!(self.ops.label(), "MEMORY" | "TEMP")
    }

    fn filter_env(&self, chain: ChainKind) -> FilterEnv {
        FilterEnv {
            stream_id: self.state.id,
            position: self.position,
            chain,
            seek_request: None,
            eof: self.state.eof,
        }
    }

    fn apply_filter_requests(&mut self, env: &FilterEnv) {
        if let Some(pos) = env.seek_request {
            self.seek(pos, SEEK_SET);
        }
    }

    fn compact(&mut self) {
        if self.writepos > self.readpos {
            self.readbuf.copy_within(self.readpos..self.writepos, 0);
        }
        self.writepos -= self.readpos;
        self.readpos = 0;
    }

    fn append_to_readbuf(&mut self, data: &[u8]) {
        if !self.readbuf.is_empty() && self.readbuf.len() - self.writepos < data.len() {
            self.compact();
        }
        if self.readbuf.len() - self.writepos < data.len() {
            let newlen = self.readbuf.len() + data.len();
            self.readbuf.resize(newlen, 0);
        }
        self.readbuf[self.writepos..self.writepos + data.len()].copy_from_slice(data);
        self.writepos += data.len();
    }

    /// `php_stream_fill_read_buffer()`.
    pub fn fill_read_buffer(&mut self, size: usize) -> Result<(), OpFailed> {
        if !self.read_filters.is_empty() {
            let to_read_now = size.min(self.chunk_size);
            if self.state.eof || self.writepos - self.readpos >= to_read_now {
                return Ok(());
            }
            let mut chunk = vec![0u8; self.chunk_size];
            while !self.state.eof && self.writepos - self.readpos < to_read_now {
                let justread = self.ops.read(&mut self.state, &mut chunk);
                if let Ok(n) = justread {
                    self.note_progress(n);
                }
                let mut input = Brigade::new();
                let flags = match justread {
                    Err(_) if self.writepos == self.readpos => return Err(OpFailed),
                    Ok(n) if n > 0 => {
                        input.push_back(Bucket::new(&chunk[..n]));
                        if self.state.eof {
                            FilterFlags::FlushClose
                        } else {
                            FilterFlags::Normal
                        }
                    }
                    _ => {
                        if self.state.eof {
                            FilterFlags::FlushClose
                        } else {
                            FilterFlags::FlushInc
                        }
                    }
                };
                let mut env = self.filter_env(ChainKind::Read);
                let (status, out) = self.read_filters.run(0, &mut env, input, None, flags);
                match status {
                    FilterStatus::PassOn => {
                        for b in out {
                            self.append_to_readbuf(&b.data);
                        }
                    }
                    FilterStatus::FeedMe => {}
                    FilterStatus::ErrFatal => {
                        self.state.eof = true;
                        self.state.fatal_error = true;
                        crate::context::notify_completed(self.context.as_ref());
                        return Err(OpFailed);
                    }
                }
                self.apply_filter_requests(&env);
                if !matches!(justread, Ok(n) if n > 0) {
                    break;
                }
            }
            return Ok(());
        }
        if self.writepos - self.readpos < size {
            if !self.readbuf.is_empty() && self.readbuf.len() - self.writepos < self.chunk_size {
                self.compact();
            }
            if self.readbuf.len() - self.writepos < self.chunk_size {
                let newlen = self.readbuf.len() + self.chunk_size;
                self.readbuf.resize(newlen, 0);
            }
            let wp = self.writepos;
            let old_eof = self.state.eof;
            let r = self.ops.read(&mut self.state, &mut self.readbuf[wp..]);
            if let Ok(n) = r {
                self.writepos += n;
                self.note_progress(n);
            }
            if old_eof != self.state.eof {
                crate::context::notify_completed(self.context.as_ref());
            }
            r?;
        }
        Ok(())
    }

    /// Whether the ops are a socket (`xp_socket` / `xp_ssl`), whose reads
    /// and writes advance the context's progress notifications.
    fn is_socket(&self) -> bool {
        let l = self.ops.label();
        l.ends_with("_socket") || l.ends_with("/ssl")
    }

    /// `php_stream_notify_progress_increment()` after socket I/O.
    fn note_progress(&self, n: usize) {
        if n > 0 && self.context.is_some() && self.is_socket() {
            crate::context::notify_progress_increment(
                self.context.as_ref(),
                u64::try_from(n).unwrap_or(u64::MAX),
                0,
            );
        }
    }

    /// `php_stream_read()`: returns the number of bytes read; `Err` when
    /// nothing could be read because of an error.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, OpFailed> {
        let mut size = buf.len();
        let mut off = 0usize;
        let mut didread = 0usize;
        while size > 0 {
            if self.writepos > self.readpos {
                let toread = (self.writepos - self.readpos).min(size);
                buf[off..off + toread]
                    .copy_from_slice(&self.readbuf[self.readpos..self.readpos + toread]);
                self.readpos += toread;
                size -= toread;
                self.state.has_buffered_data = true;
                off += toread;
                didread += toread;
            }
            if size == 0 {
                break;
            }
            let toread;
            if self.read_filters.is_empty()
                && (self.has_flag(flags::NO_BUFFER) || self.chunk_size == 1)
            {
                match self.ops.read(&mut self.state, &mut buf[off..off + size]) {
                    Ok(n) => {
                        self.note_progress(n);
                        toread = n;
                    }
                    Err(e) => {
                        if didread == 0 {
                            return Err(e);
                        }
                        break;
                    }
                }
            } else {
                if self.fill_read_buffer(size).is_err() {
                    if didread == 0 {
                        return Err(OpFailed);
                    }
                    break;
                }
                toread = (self.writepos - self.readpos).min(size);
                if toread > 0 {
                    buf[off..off + toread]
                        .copy_from_slice(&self.readbuf[self.readpos..self.readpos + toread]);
                    self.readpos += toread;
                }
            }
            if toread > 0 {
                didread += toread;
                off += toread;
                size -= toread;
                self.state.has_buffered_data = true;
            } else {
                break;
            }
            if !self.greedy() {
                break;
            }
        }
        if didread > 0 {
            self.position += to_i64(didread);
            self.state.has_buffered_data = false;
        }
        Ok(didread)
    }

    /// `php_stream_read_to_str()`: one read of up to `len` bytes.
    pub fn read_vec(&mut self, len: usize) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; len];
        let n = self.read(&mut buf).ok()?;
        buf.truncate(n);
        Some(buf)
    }

    /// `php_stream_eof()`.
    pub fn eof(&mut self) -> bool {
        if self.writepos > self.readpos {
            return false;
        }
        if !self.state.eof && self.set_option(&mut StreamOption::CheckLiveness) == OPT_ERR {
            self.state.eof = true;
        }
        self.state.eof
    }

    /// `php_stream_getc()`.
    pub fn getc(&mut self) -> Option<u8> {
        let mut b = [0u8; 1];
        match self.read(&mut b) {
            Ok(1) => Some(b[0]),
            _ => None,
        }
    }

    fn locate_eol(&mut self) -> Option<usize> {
        let avail = &self.readbuf[self.readpos..self.writepos];
        if self.has_flag(flags::DETECT_EOL) {
            let cr = avail.iter().position(|&c| c == b'\r');
            let lf = avail.iter().position(|&c| c == b'\n');
            if let Some(cr) = cr {
                if lf != Some(cr + 1) && lf.is_none_or(|lf| lf >= cr) {
                    self.state.flags ^= flags::DETECT_EOL;
                    self.state.flags |= flags::EOL_MAC;
                    return Some(cr);
                }
            }
            if lf.is_some() {
                self.state.flags ^= flags::DETECT_EOL;
                return lf;
            }
            None
        } else if self.has_flag(flags::EOL_MAC) {
            avail.iter().position(|&c| c == b'\r')
        } else {
            avail.iter().position(|&c| c == b'\n')
        }
    }

    /// `php_stream_get_line()`: `fgets()`. `maxlen` is the C buffer size
    /// (`fgets($fp, $length)` passes `$length`, so at most `maxlen - 1`
    /// bytes are returned); `None` reads a whole line.
    pub fn get_line(&mut self, maxlen: Option<usize>) -> Option<Vec<u8>> {
        let mut out: Vec<u8> = Vec::new();
        let mut maxlen = match maxlen {
            Some(0) => return None,
            Some(m) => Some(m),
            None => None,
        };
        loop {
            let avail = self.writepos - self.readpos;
            if avail > 0 {
                let mut done = false;
                let mut cpysz = if let Some(eol) = self.locate_eol() {
                    done = true;
                    eol + 1
                } else {
                    avail
                };
                if let Some(m) = maxlen {
                    if cpysz >= m - 1 {
                        cpysz = m - 1;
                        done = true;
                    }
                }
                out.extend_from_slice(&self.readbuf[self.readpos..self.readpos + cpysz]);
                self.position += to_i64(cpysz);
                self.readpos += cpysz;
                if let Some(m) = maxlen.as_mut() {
                    *m -= cpysz;
                }
                if done {
                    break;
                }
            } else if self.state.eof {
                break;
            } else {
                let toread = match maxlen {
                    None => self.chunk_size,
                    Some(m) => (m - 1).min(self.chunk_size),
                };
                if self.fill_read_buffer(toread).is_err() && self.state.fatal_error {
                    return None;
                }
                if self.writepos == self.readpos {
                    break;
                }
            }
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    fn search_delim(&self, maxlen: usize, skiplen: usize, delim: &[u8]) -> Option<usize> {
        let seek_len = (self.writepos - self.readpos).min(maxlen);
        if seek_len <= skiplen {
            return None;
        }
        let hay = &self.readbuf[self.readpos + skiplen..self.readpos + seek_len];
        crate::util::find_bytes(hay, delim).map(|p| p + skiplen)
    }

    /// `php_stream_get_record()`: `stream_get_line()`.
    pub fn get_record(&mut self, maxlen: usize, delim: &[u8]) -> Option<Vec<u8>> {
        if maxlen == 0 {
            return None;
        }
        let has_delim = !delim.is_empty();
        let mut found = if has_delim {
            self.search_delim(maxlen, 0, delim)
        } else {
            None
        };
        let mut buffered_len = self.writepos - self.readpos;
        while found.is_none() && buffered_len < maxlen {
            let to_read_now = (maxlen - buffered_len).min(self.chunk_size);
            if self.fill_read_buffer(buffered_len + to_read_now).is_err() && self.state.fatal_error
            {
                return None;
            }
            let just_read = (self.writepos - self.readpos) - buffered_len;
            if just_read == 0 {
                break;
            }
            if has_delim {
                let skip = buffered_len.saturating_sub(delim.len() - 1);
                found = self.search_delim(maxlen, skip, delim);
                if found.is_some() {
                    break;
                }
            }
            buffered_len += just_read;
        }
        let buffered = self.writepos - self.readpos;
        let tent = if let Some(f) = found {
            f
        } else if !has_delim && buffered >= maxlen {
            maxlen
        } else if (buffered < maxlen && !self.state.eof) || (buffered == 0 && self.state.eof) {
            return None;
        } else {
            buffered.min(maxlen)
        };
        let mut out = vec![0u8; tent];
        let n = self.read(&mut out).unwrap_or(0);
        out.truncate(n);
        if found.is_some() {
            self.readpos += delim.len();
            self.position += to_i64(delim.len());
        }
        Some(out)
    }

    fn write_buffer(&mut self, mut buf: &[u8]) -> Result<usize, OpFailed> {
        if self.ops.can_seek() && !self.has_flag(flags::NO_SEEK) && self.readpos != self.writepos {
            self.readpos = 0;
            self.writepos = 0;
            let mut pos = self.position;
            self.ops
                .seek(&mut self.state, self.position, SEEK_SET, &mut pos);
            self.position = pos;
        }
        let chunk = if self.ops.is_userspace() {
            self.chunk_size
        } else {
            buf.len()
        };
        let mut didwrite = 0usize;
        while !buf.is_empty() {
            let n = buf.len().min(chunk.max(1));
            let old_eof = self.state.eof;
            let r = self.ops.write(&mut self.state, &buf[..n]);
            if let Ok(w) = r {
                self.note_progress(w);
            }
            if old_eof != self.state.eof {
                crate::context::notify_completed(self.context.as_ref());
            }
            match r {
                Ok(w) if w > 0 => {
                    buf = &buf[w..];
                    didwrite += w;
                    self.position += to_i64(w);
                }
                other => {
                    if didwrite == 0 {
                        return other;
                    }
                    return Ok(didwrite);
                }
            }
        }
        Ok(didwrite)
    }

    fn write_filtered(
        &mut self,
        buf: Option<&[u8]>,
        flags: FilterFlags,
    ) -> Result<usize, OpFailed> {
        let mut input = Brigade::new();
        if let Some(b) = buf {
            input.push_back(Bucket::new(b));
        }
        let mut consumed = 0usize;
        let mut env = self.filter_env(ChainKind::Write);
        let (status, out) = self
            .write_filters
            .run(0, &mut env, input, Some(&mut consumed), flags);
        let mut result = Ok(consumed);
        match status {
            FilterStatus::PassOn => {
                for b in out {
                    if self.write_buffer(&b.data).is_err() {
                        result = Err(OpFailed);
                    }
                }
            }
            FilterStatus::ErrFatal => result = Err(OpFailed),
            FilterStatus::FeedMe => {}
        }
        self.apply_filter_requests(&env);
        result
    }

    /// `php_stream_write()`. Emits "Stream is not writable" for read-only
    /// ops.
    pub fn write(&mut self, buf: &[u8]) -> Result<usize, OpFailed> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.ops.can_write() {
            self.state
                .notice(StreamErrorCode::NotWritable, "Stream is not writable");
            return Err(OpFailed);
        }
        let r = if self.write_filters.is_empty() {
            self.write_buffer(buf)
        } else {
            self.write_filtered(Some(buf), FilterFlags::Normal)
        };
        if !matches!(r, Ok(0)) {
            self.state.flags |= flags::WAS_WRITTEN;
        }
        r
    }

    /// `php_stream_puts()`.
    pub fn puts(&mut self, s: &[u8]) -> bool {
        !s.is_empty()
            && matches!(self.write(s), Ok(n) if n > 0)
            && matches!(self.write(b"\n"), Ok(n) if n > 0)
    }

    fn flush_ex(&mut self, closing: bool) -> i32 {
        if !self.write_filters.is_empty() && self.ops.can_write() {
            let _ = self.write_filtered(
                None,
                if closing {
                    FilterFlags::FlushClose
                } else {
                    FilterFlags::FlushInc
                },
            );
        }
        self.state.flags &= !flags::WAS_WRITTEN;
        self.ops.flush(&mut self.state)
    }

    /// `php_stream_flush()`; returns 0 on success.
    pub fn flush(&mut self) -> i32 {
        self.flush_ex(false)
    }

    /// `php_stream_tell()`.
    pub fn tell(&self) -> i64 {
        self.position
    }

    fn filters_seekable(chain: &FilterChain, start: bool, kind: ChainKind) -> bool {
        for f in &chain.filters {
            let s = match kind {
                ChainKind::Read => f.filter.read_seekable(),
                ChainKind::Write => f.filter.write_seekable(),
            };
            if s == Seekable::Never {
                diag::warning(format!(
                    "Stream filter {} is never seekable",
                    f.filter.label()
                ));
                return false;
            }
            if !start && s == Seekable::Start {
                diag::warning(format!(
                    "Stream filter {} is seekable only to start position",
                    f.filter.label()
                ));
                return false;
            }
        }
        true
    }

    fn filters_seek(&mut self, start: bool, offset: i64, whence: i32) -> bool {
        for kind in [ChainKind::Write, ChainKind::Read] {
            let mut env = self.filter_env(kind);
            let chain = match kind {
                ChainKind::Read => &mut self.read_filters,
                ChainKind::Write => &mut self.write_filters,
            };
            for f in &mut chain.filters {
                let s = match kind {
                    ChainKind::Read => f.filter.read_seekable(),
                    ChainKind::Write => f.filter.write_seekable(),
                };
                if ((s == Seekable::Start && start) || s == Seekable::Check)
                    && !f.filter.seek(&mut env, offset, whence)
                {
                    diag::warning(format!(
                        "Stream filter seeking for {} failed",
                        f.filter.label()
                    ));
                    return false;
                }
            }
        }
        true
    }

    /// `php_stream_seek()`; returns 0 on success, -1 on failure.
    pub fn seek(&mut self, offset: i64, whence: i32) -> i32 {
        let mut offset = offset;
        let mut whence = whence;
        let start = whence == SEEK_SET && offset == 0;
        if !self.write_filters.is_empty() {
            self.flush();
            if !Self::filters_seekable(&self.write_filters, start, ChainKind::Write) {
                return -1;
            }
        }
        if !self.read_filters.is_empty()
            && !Self::filters_seekable(&self.read_filters, start, ChainKind::Read)
        {
            return -1;
        }
        let avail = to_i64(self.writepos - self.readpos);
        if !self.has_flag(flags::NO_BUFFER) {
            match whence {
                SEEK_CUR if offset > 0 && offset <= avail => {
                    self.readpos += usize::try_from(offset).unwrap_or(0);
                    self.position += offset;
                    self.state.eof = false;
                    self.state.fatal_error = false;
                    return if self.filters_seek(start, offset, whence) {
                        0
                    } else {
                        -1
                    };
                }
                SEEK_SET if offset > self.position && offset <= self.position + avail => {
                    self.readpos += usize::try_from(offset - self.position).unwrap_or(0);
                    self.position = offset;
                    self.state.eof = false;
                    self.state.fatal_error = false;
                    return if self.filters_seek(start, offset, whence) {
                        0
                    } else {
                        -1
                    };
                }
                _ => {}
            }
        }
        if self.ops.can_seek() && !self.has_flag(flags::NO_SEEK) {
            match whence {
                SEEK_CUR => {
                    offset = self.position.saturating_add(offset);
                    whence = SEEK_SET;
                }
                SEEK_SET if offset < 0 => return -1,
                _ => {}
            }
            let mut pos = self.position;
            let ok = self.ops.seek(&mut self.state, offset, whence, &mut pos);
            self.position = pos;
            if !self.has_flag(flags::NO_SEEK) || ok {
                if ok {
                    self.state.eof = false;
                    self.state.fatal_error = false;
                }
                self.readpos = 0;
                self.writepos = 0;
                let ret = if ok { 0 } else { -1 };
                return if self.filters_seek(start, offset, whence) {
                    ret
                } else {
                    -1
                };
            }
        }
        if whence == SEEK_CUR && offset >= 0 {
            let mut tmp = [0u8; 1024];
            while offset > 0 {
                let n = usize::try_from(offset).unwrap_or(usize::MAX).min(tmp.len());
                match self.read(&mut tmp[..n]) {
                    Ok(r) if r > 0 => offset -= to_i64(r),
                    _ => return -1,
                }
            }
            self.state.eof = false;
            self.state.fatal_error = false;
            return 0;
        }
        self.state.warn(
            StreamErrorCode::SeekNotSupported,
            "Stream does not support seeking",
        );
        -1
    }

    /// Calls the ops' seek directly (no buffer handling), returning the
    /// new offset on success.
    pub fn raw_seek(&mut self, offset: i64, whence: i32) -> Option<i64> {
        let mut p = self.position;
        self.ops
            .seek(&mut self.state, offset, whence, &mut p)
            .then_some(p)
    }

    /// `php_stream_rewind()`.
    pub fn rewind(&mut self) -> i32 {
        self.seek(0, SEEK_SET)
    }

    /// `php_stream_set_option()`.
    pub fn set_option(&mut self, opt: &mut StreamOption<'_>) -> i64 {
        let ret = self.ops.set_option(&mut self.state, opt);
        if ret == OPT_NOTIMPL {
            match opt {
                StreamOption::ChunkSize(size) => {
                    let old = i64::try_from(self.chunk_size.min(i32::MAX as usize)).unwrap_or(0);
                    self.chunk_size = *size;
                    return old;
                }
                StreamOption::ReadBuffer(mode, _) => {
                    if *mode == BufferMode::None {
                        self.state.flags |= flags::NO_BUFFER;
                    } else {
                        self.state.flags &= !flags::NO_BUFFER;
                    }
                    return OPT_OK;
                }
                _ => {}
            }
        }
        ret
    }

    /// `php_stream_set_chunk_size()`: returns the previous chunk size.
    pub fn set_chunk_size(&mut self, size: usize) -> i64 {
        self.set_option(&mut StreamOption::ChunkSize(size))
    }

    /// `php_stream_truncate_supported()`.
    pub fn truncate_supported(&mut self) -> bool {
        self.set_option(&mut StreamOption::TruncateSupported) == OPT_OK
    }

    /// `php_stream_truncate_set_size()`; returns 0 on success.
    pub fn truncate(&mut self, size: i64) -> i64 {
        self.set_option(&mut StreamOption::TruncateSetSize(size))
    }

    /// `php_stream_supports_lock()`.
    pub fn supports_lock(&mut self) -> bool {
        self.set_option(&mut StreamOption::LockSupported) == OPT_OK
    }

    /// `php_stream_lock()`; returns 0 on success.
    pub fn lock(&mut self, op: i32) -> i64 {
        self.set_option(&mut StreamOption::Locking(op))
    }

    /// `php_stream_sync()`.
    pub fn sync(&mut self, data_only: bool) -> i64 {
        self.set_option(&mut StreamOption::Sync(data_only))
    }

    /// `php_stream_sync_supported()`.
    pub fn sync_supported(&mut self) -> bool {
        self.set_option(&mut StreamOption::SyncSupported) == OPT_OK
    }

    /// `php_stream_stat()`.
    pub fn stat(&mut self) -> Option<StatBuf> {
        self.ops.stat(&mut self.state)
    }

    /// Casts to a file descriptor (`php_stream_cast`). With
    /// `report_errors` false this is a capability check.
    pub fn cast(&mut self, kind: CastAs, report_errors: bool) -> Option<RawFd> {
        if self.is_filtered() && kind != CastAs::FdForSelect {
            if report_errors {
                diag::warning("Cannot cast a filtered stream on this system");
            }
            return None;
        }
        let r = self.ops.cast(&mut self.state, kind, report_errors);
        match r {
            Some(_) => {
                let unread = self.writepos - self.readpos;
                if report_errors && unread > 0 && kind != CastAs::FdForSelect {
                    diag::warning(format!(
                        "{unread} bytes of buffered data lost during stream conversion!"
                    ));
                }
            }
            None if report_errors => {
                let name = match kind {
                    CastAs::Stdio => "STDIO FILE*",
                    CastAs::Fd => "File Descriptor",
                    CastAs::SocketFd => "Socket Descriptor",
                    CastAs::FdForSelect => "select()able descriptor",
                };
                diag::warning(format!(
                    "Cannot represent a stream of type {} as a {name}",
                    self.ops.label()
                ));
            }
            None => {}
        }
        r
    }

    /// `stream_isatty()` / `posix_isatty()` on the stream.
    pub fn isatty(&mut self) -> bool {
        let fd = self
            .cast(CastAs::FdForSelect, false)
            .or_else(|| self.cast(CastAs::Fd, false));
        fd.is_some_and(crate::sys::isatty)
    }

    /// Reads the next directory entry (`php_stream_readdir`).
    pub fn readdir(&mut self) -> Option<Vec<u8>> {
        self.ops.readdir(&mut self.state)
    }

    /// `rewinddir()`.
    pub fn rewinddir(&mut self) -> i32 {
        let mut pos = 0;
        if self.ops.seek(&mut self.state, 0, SEEK_SET, &mut pos) {
            self.position = 0;
            0
        } else {
            -1
        }
    }

    /// `_php_stream_passthru()`: copies the rest of the stream to `out`;
    /// returns the number of bytes passed (or `Err` when the first read
    /// failed).
    pub fn passthru(&mut self, out: &mut dyn FnMut(&[u8])) -> Result<usize, OpFailed> {
        let mut buf = vec![0u8; 8192];
        let mut count = 0;
        loop {
            match self.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    out(&buf[..n]);
                    count += n;
                }
                Err(e) => {
                    if count == 0 {
                        return Err(e);
                    }
                    break;
                }
            }
        }
        Ok(count)
    }

    /// `_php_stream_copy_to_mem()`: reads up to `maxlen` bytes (`None` for
    /// everything). Returns `None` when nothing was read (PHP returns an
    /// empty string in that case at the call sites).
    pub fn copy_to_mem(&mut self, maxlen: Option<usize>) -> Option<Vec<u8>> {
        if maxlen == Some(0) {
            return Some(Vec::new());
        }
        let mut result = Vec::new();
        if let Some(max) = maxlen.filter(|m| *m < 4 * CHUNK_SIZE) {
            let mut buf = vec![0u8; max];
            let mut len = 0;
            while len < max && !self.eof() {
                match self.read(&mut buf[len..]) {
                    Ok(n) if n > 0 => len += n,
                    _ => break,
                }
            }
            buf.truncate(len);
            return (len > 0).then_some(buf);
        }
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            let want = match maxlen {
                Some(m) => (m - result.len()).min(buf.len()),
                None => buf.len(),
            };
            if want == 0 {
                break;
            }
            match self.read(&mut buf[..want]) {
                Ok(n) if n > 0 => result.extend_from_slice(&buf[..n]),
                _ => break,
            }
        }
        (!result.is_empty()).then_some(result)
    }

    /// Attaches a filter. Appending to the read chain winds any buffered
    /// data through it (`php_stream_filter_append_ex`); on failure the
    /// filter is detached again and `None` returned.
    pub fn append_filter(
        &mut self,
        kind: ChainKind,
        filter: Box<dyn StreamFilter>,
    ) -> Option<FilterId> {
        match kind {
            ChainKind::Write => Some(self.write_filters.push_back(filter)),
            ChainKind::Read => {
                let id = self.read_filters.push_back(filter);
                if self.writepos > self.readpos {
                    let idx = self.read_filters.len() - 1;
                    let data = self.readbuf[self.readpos..self.writepos].to_vec();
                    let mut env = self.filter_env(ChainKind::Read);
                    let mut consumed = 0usize;
                    let mut input = Brigade::from(vec![Bucket::new(data.clone())]);
                    let mut out = Brigade::new();
                    let f = &mut self.read_filters.filters[idx].filter;
                    let mut status = f.filter(
                        &mut env,
                        &mut input,
                        &mut out,
                        Some(&mut consumed),
                        FilterFlags::Normal,
                    );
                    if self.readpos + consumed > self.writepos {
                        status = FilterStatus::ErrFatal;
                    }
                    match status {
                        FilterStatus::ErrFatal => {
                            self.state.warn(
                                StreamErrorCode::FilterFailed,
                                "Filter failed to process pre-buffered data",
                            );
                            if let Some(mut f) = self.read_filters.remove(id) {
                                f.close();
                            }
                            return None;
                        }
                        FilterStatus::FeedMe => {
                            self.readpos = 0;
                            self.writepos = 0;
                        }
                        FilterStatus::PassOn => {
                            self.readpos = 0;
                            self.writepos = 0;
                            for b in out {
                                self.append_to_readbuf(&b.data);
                            }
                        }
                    }
                }
                Some(id)
            }
        }
    }

    /// Prepends a filter (`php_stream_filter_prepend`).
    pub fn prepend_filter(&mut self, kind: ChainKind, filter: Box<dyn StreamFilter>) -> FilterId {
        match kind {
            ChainKind::Read => self.read_filters.push_front(filter),
            ChainKind::Write => self.write_filters.push_front(filter),
        }
    }

    /// Which chain a filter is on.
    pub fn filter_chain_of(&self, id: FilterId) -> Option<ChainKind> {
        if self.read_filters.position(id).is_some() {
            Some(ChainKind::Read)
        } else if self.write_filters.position(id).is_some() {
            Some(ChainKind::Write)
        } else {
            None
        }
    }

    /// `php_stream_filter_flush()` from the filter `id` onwards.
    pub fn flush_filter(&mut self, id: FilterId, finish: bool) -> bool {
        let Some(kind) = self.filter_chain_of(id) else {
            return false;
        };
        let flags = if finish {
            FilterFlags::FlushClose
        } else {
            FilterFlags::FlushInc
        };
        let mut env = self.filter_env(kind);
        let chain = match kind {
            ChainKind::Read => &mut self.read_filters,
            ChainKind::Write => &mut self.write_filters,
        };
        let Some(start) = chain.position(id) else {
            return false;
        };
        let mut inp = Brigade::new();
        let mut fl = flags;
        for f in chain.filters.iter_mut().skip(start) {
            let mut out = Brigade::new();
            match f.filter.filter(&mut env, &mut inp, &mut out, None, fl) {
                FilterStatus::FeedMe => return true,
                FilterStatus::ErrFatal => return false,
                FilterStatus::PassOn => {}
            }
            inp = out;
            fl = FilterFlags::Normal;
        }
        let total: usize = inp.iter().map(|b| b.data.len()).sum();
        if total == 0 {
            return true;
        }
        match kind {
            ChainKind::Read => {
                if self.readpos > 0 {
                    self.compact();
                }
                for b in inp {
                    self.append_to_readbuf(&b.data);
                }
            }
            ChainKind::Write => {
                for b in inp {
                    if let Ok(n) = self.ops.write(&mut self.state, &b.data) {
                        self.position += to_i64(n);
                    }
                }
            }
        }
        self.apply_filter_requests(&env);
        true
    }

    /// `stream_filter_remove()`: flushes the filter (finishing) and removes
    /// it. Returns false when the flush failed (PHP warns "Unable to flush
    /// filter, not removing" in the VM glue) or the filter is unknown.
    pub fn remove_filter(&mut self, id: FilterId) -> bool {
        if !self.flush_filter(id, true) {
            return false;
        }
        let removed = self
            .read_filters
            .remove(id)
            .or_else(|| self.write_filters.remove(id));
        match removed {
            Some(mut f) => {
                f.close();
                true
            }
            None => false,
        }
    }

    /// Builds `stream_get_meta_data()`. `wrapper_data` is VM-side (the
    /// user-space wrapper object, see [`crate::user::UserStreamOps::object`]);
    /// when present the VM inserts it immediately before `wrapper_type`.
    pub fn meta_data(&mut self) -> MetaData {
        let mut out = Vec::new();
        if self.set_option(&mut StreamOption::MetaData(&mut out)) != OPT_OK {
            out.clear();
            out.push(("timed_out".into(), Value::Bool(false)));
            out.push(("blocked".into(), Value::Bool(true)));
            let eof = self.eof();
            out.push(("eof".into(), Value::Bool(eof)));
        }
        if let Some(d) = &self.wrapper_data {
            out.push(("wrapper_data".into(), d.clone()));
        }
        if let Some(w) = &self.wrapper {
            out.push(("wrapper_type".into(), Value::str(w.label)));
        }
        out.push(("stream_type".into(), Value::str(self.ops.label())));
        out.push(("mode".into(), Value::str(self.mode.as_bytes())));
        out.push((
            "unread_bytes".into(),
            Value::Int(to_i64(self.writepos - self.readpos)),
        ));
        out.push(("seekable".into(), Value::Bool(self.is_seekable())));
        if let Some(p) = &self.orig_path {
            out.push(("uri".into(), Value::Str(p.clone())));
        }
        out
    }

    /// `php_stream_free()` with `PHP_STREAM_FREE_CLOSE`: flushes (closing
    /// the write filters), closes the ops, and destroys the filters.
    /// Returns the close result (0 on success). Idempotent.
    pub fn close(&mut self) -> i32 {
        self.close_ex(false)
    }

    /// Closes, optionally preserving the underlying handle.
    pub fn close_ex(&mut self, preserve_handle: bool) -> i32 {
        if self.closed {
            return 0;
        }
        self.closed = true;
        let preserve = preserve_handle || self.has_flag(flags::NO_CLOSE);
        let flush_result = if self.has_flag(flags::WAS_WRITTEN) || !self.write_filters.is_empty() {
            self.flush_ex(true)
        } else {
            0
        };
        // `try_with`: streams owned by thread-locals close during their
        // destruction, after the hook slot may be gone.
        let hook = CLOSE_HOOK
            .try_with(|h| h.try_borrow().ok().and_then(|h| h.clone()))
            .ok()
            .flatten();
        if let Some(hook) = hook {
            hook(self.state.id);
        }
        let mut ret = self.ops.close(&mut self.state, preserve);
        if ret == 0 {
            ret = flush_result;
        }
        for mut f in self.read_filters.filters.drain(..) {
            f.filter.close();
        }
        for mut f in self.write_filters.filters.drain(..) {
            f.filter.close();
        }
        self.readbuf = Vec::new();
        self.readpos = 0;
        self.writepos = 0;
        ret
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.close();
    }
}

/// `_php_stream_copy_to_stream_ex()`: copies up to `maxlen` bytes (`None`
/// for all). Returns `(success, bytes_copied)`.
pub fn copy_to_stream(src: &mut Stream, dest: &mut Stream, maxlen: Option<usize>) -> (bool, usize) {
    if maxlen == Some(0) {
        return (true, 0);
    }
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut haveread = 0usize;
    loop {
        let mut readchunk = buf.len();
        if let Some(m) = maxlen {
            readchunk = readchunk.min(m - haveread);
        }
        let didread = match src.read(&mut buf[..readchunk]) {
            Ok(0) => return (true, haveread),
            Ok(n) => n,
            Err(_) => return (false, haveread),
        };
        haveread += didread;
        let mut towrite = &buf[..didread];
        while !towrite.is_empty() {
            match dest.write(towrite) {
                Ok(n) if n > 0 => towrite = &towrite[n..],
                _ => return (false, haveread - towrite.len()),
            }
        }
        if maxlen == Some(haveread) {
            break;
        }
    }
    (true, haveread)
}

#[cfg(test)]
mod tests;
