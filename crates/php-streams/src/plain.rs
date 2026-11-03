//! The plain files wrapper and STDIO stream ops (`plain_wrapper.c`).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;

use crate::context::ContextRef;
use crate::diag::{self, Severity, StreamErrorCode};
use crate::mode::{parse_fopen_mode, FopenFlags};
use crate::paths;
use crate::stat::StatBuf;
use crate::state;
use crate::stream::{
    flags, CastAs, OpFailed, Stream, StreamOps, StreamOption, StreamState, WrapperInfo, OPT_ERR,
    OPT_NOTIMPL, OPT_OK, SEEK_CUR, SEEK_END, SEEK_SET,
};
use crate::sys;
use crate::util::{os_path, starts_with_ci};
use crate::wrapper::{
    wrapper_error, wrapper_warn, MetadataOp, OpenError, Wrapper, MKDIR_RECURSIVE, REPORT_ERRORS,
    STREAM_ASSUME_REALPATH, STREAM_DISABLE_OPEN_BASEDIR, STREAM_OPEN_FOR_INCLUDE,
    STREAM_USE_GLOB_DIR_OPEN, URL_STAT_IGNORE_OPEN_BASEDIR, URL_STAT_LINK, URL_STAT_QUIET,
};

/// The wrapper label of the plain files wrapper.
pub const LABEL: &str = "plainfile";

/// The [`WrapperInfo`] attached to plain file streams.
pub fn wrapper_info() -> WrapperInfo {
    WrapperInfo {
        label: LABEL,
        is_url: false,
        is_plain_files: true,
    }
}

/// STDIO stream ops over a file descriptor (`php_stream_stdio_ops`).
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // mirrors php_stdio_stream_data flags
pub struct StdioOps {
    file: Option<File>,
    fd: RawFd,
    is_seekable: bool,
    is_pipe: bool,
    is_stdio_file: bool,
    temp_name: Option<Vec<u8>>,
    cached_stat: Option<StatBuf>,
    no_forced_fstat: bool,
}

impl StdioOps {
    fn new(file: File) -> Self {
        let fd = file.as_raw_fd();
        StdioOps {
            file: Some(file),
            fd,
            is_seekable: true,
            is_pipe: false,
            is_stdio_file: false,
            temp_name: None,
            cached_stat: None,
            no_forced_fstat: false,
        }
    }

    /// The underlying descriptor (-1 once closed).
    pub fn fd(&self) -> RawFd {
        if self.file.is_some() {
            self.fd
        } else {
            -1
        }
    }

    /// Whether the descriptor refers to a pipe.
    pub fn is_pipe(&self) -> bool {
        self.is_pipe
    }

    fn do_fstat(&mut self, force: bool) -> Option<StatBuf> {
        if self.cached_stat.is_none() || (force && !self.no_forced_fstat) {
            let file = self.file.as_ref()?;
            self.cached_stat = file.metadata().ok().map(|m| StatBuf::from_metadata(&m));
        }
        self.cached_stat
    }

    fn detect_is_seekable(&mut self) {
        if let Some(sb) = self.do_fstat(false) {
            if sb.is_char() {
                #[allow(clippy::cast_sign_loss)]
                let rdev = sb.rdev as u64;
                self.is_seekable = sys::dev_major(rdev) == 1
                    && matches!(sys::dev_minor(rdev), 1 | 2 | 3 | 4 | 5 | 7);
            } else {
                self.is_seekable = !sb.is_fifo();
            }
            self.is_pipe = sb.is_fifo();
        }
    }
}

/// `_php_stream_fopen_from_fd()`: wraps an open file in a STDIO stream.
/// `zero_position` asserts the descriptor is at offset 0.
pub fn stream_from_file(file: File, mode: &str, zero_position: bool) -> Stream {
    let mut ops = StdioOps::new(file);
    ops.detect_is_seekable();
    let seekable = ops.is_seekable;
    let pos = if seekable && !zero_position {
        ops.file
            .as_mut()
            .and_then(|f| f.stream_position().ok())
            .map(|p| i64::try_from(p).unwrap_or(i64::MAX))
    } else {
        Some(0)
    };
    let mut s = Stream::new(Box::new(ops), mode);
    if !seekable {
        s.add_flags(flags::NO_SEEK);
        s.position = -1;
    } else if let Some(p) = pos {
        s.position = p;
    } else {
        s.add_flags(flags::NO_SEEK);
        if let Some(o) = s.ops_as_mut::<StdioOps>() {
            o.is_seekable = false;
        }
    }
    s
}

/// Like [`stream_from_file`] for the process' standard streams opened via
/// `FILE*` (`php_stream_fopen_from_file`): `stream_set_write_buffer()`
/// succeeds on them.
pub fn stream_from_stdio_file(file: File, mode: &str) -> Stream {
    let mut s = stream_from_file(file, mode, false);
    if let Some(o) = s.ops_as_mut::<StdioOps>() {
        o.is_stdio_file = true;
    }
    s
}

impl StreamOps for StdioOps {
    fn label(&self) -> &'static str {
        "STDIO"
    }

    fn read(&mut self, st: &mut StreamState, buf: &mut [u8]) -> Result<usize, OpFailed> {
        let Some(file) = self.file.as_mut() else {
            return Err(OpFailed);
        };
        let mut r = file.read(buf);
        if matches!(&r, Err(e) if e.kind() == io::ErrorKind::Interrupted) {
            r = file.read(buf);
        }
        state::clear_stat_cache();
        match r {
            Ok(0) => {
                st.eof = true;
                Ok(0)
            }
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Err(OpFailed),
            Err(e) => {
                let errno = sys::errno_of(&e);
                if st.flags & flags::SUPPRESS_ERRORS == 0 {
                    st.notice(
                        StreamErrorCode::ReadFailed,
                        format!(
                            "Read of {} bytes failed with errno={errno} {}",
                            buf.len(),
                            sys::strerror(errno)
                        ),
                    );
                }
                if errno != libc::EBADF {
                    st.eof = true;
                }
                Err(OpFailed)
            }
        }
    }

    fn write(&mut self, st: &mut StreamState, buf: &[u8]) -> Result<usize, OpFailed> {
        let Some(file) = self.file.as_mut() else {
            return Err(OpFailed);
        };
        let r = file.write(buf);
        state::clear_stat_cache();
        match r {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Err(OpFailed),
            Err(e) => {
                let errno = sys::errno_of(&e);
                if st.flags & flags::SUPPRESS_ERRORS == 0 {
                    st.notice(
                        StreamErrorCode::WriteFailed,
                        format!(
                            "Write of {} bytes failed with errno={errno} {}",
                            buf.len(),
                            sys::strerror(errno)
                        ),
                    );
                }
                Err(OpFailed)
            }
        }
    }

    fn close(&mut self, _st: &mut StreamState, preserve_handle: bool) -> i32 {
        let Some(file) = self.file.take() else {
            return 0;
        };
        if preserve_handle {
            let _ = file.into_raw_fd();
            return 0;
        }
        drop(file);
        if let Some(name) = self.temp_name.take() {
            let _ = std::fs::remove_file(os_path(&name));
        }
        0
    }

    fn can_seek(&self) -> bool {
        true
    }

    fn seek(&mut self, _st: &mut StreamState, offset: i64, whence: i32, newpos: &mut i64) -> bool {
        if !self.is_seekable {
            wrapper_warn(
                crate::wrapper::DEFAULT_WRAPPER_NAME,
                REPORT_ERRORS,
                StreamErrorCode::SeekNotSupported,
                "Cannot seek on this stream",
            );
            return false;
        }
        let Some(file) = self.file.as_mut() else {
            return false;
        };
        let target = match whence {
            SEEK_SET => match u64::try_from(offset) {
                Ok(o) => SeekFrom::Start(o),
                Err(_) => return false,
            },
            SEEK_CUR => SeekFrom::Current(offset),
            SEEK_END => SeekFrom::End(offset),
            _ => return false,
        };
        match file.seek(target) {
            Ok(p) => {
                *newpos = i64::try_from(p).unwrap_or(i64::MAX);
                true
            }
            Err(_) => false,
        }
    }

    fn stat(&mut self, _st: &mut StreamState) -> Option<StatBuf> {
        self.do_fstat(true)
    }

    fn set_option(&mut self, st: &mut StreamState, opt: &mut StreamOption<'_>) -> i64 {
        let fd = self.fd();
        match opt {
            StreamOption::Blocking(b) => {
                if fd == -1 {
                    return -1;
                }
                match sys::set_blocking(fd, *b) {
                    Ok(old) => i64::from(old),
                    Err(_) => -1,
                }
            }
            StreamOption::WriteBuffer(..) => {
                if self.is_stdio_file {
                    0
                } else {
                    -1
                }
            }
            StreamOption::LockSupported => {
                if fd == -1 {
                    -1
                } else {
                    0
                }
            }
            StreamOption::Locking(op) => {
                if fd == -1 {
                    return -1;
                }
                match sys::flock(fd, *op) {
                    Ok(()) => 0,
                    Err(e) => {
                        st.last_errno = sys::errno_of(&e);
                        -1
                    }
                }
            }
            StreamOption::Sync(data_only) => {
                let Some(file) = self.file.as_mut() else {
                    return OPT_ERR;
                };
                let r = if *data_only {
                    file.sync_data()
                } else {
                    file.sync_all()
                };
                if r.is_ok() {
                    OPT_OK
                } else {
                    OPT_ERR
                }
            }
            StreamOption::TruncateSupported | StreamOption::SyncSupported => {
                if fd == -1 {
                    OPT_ERR
                } else {
                    OPT_OK
                }
            }
            StreamOption::TruncateSetSize(size) => {
                let Ok(size) = u64::try_from(*size) else {
                    return OPT_ERR;
                };
                match self.file.as_ref().map(|f| f.set_len(size)) {
                    Some(Ok(())) => OPT_OK,
                    _ => OPT_ERR,
                }
            }
            StreamOption::MetaData(out) => {
                if fd == -1 {
                    return -1;
                }
                let blocked = sys::is_blocking(fd).unwrap_or(true);
                out.push(("timed_out".into(), crate::value::Value::Bool(false)));
                out.push(("blocked".into(), crate::value::Value::Bool(blocked)));
                out.push(("eof".into(), crate::value::Value::Bool(st.eof)));
                OPT_OK
            }
            _ => OPT_NOTIMPL,
        }
    }

    fn cast(&mut self, _st: &mut StreamState, kind: CastAs, _report_errors: bool) -> Option<RawFd> {
        match kind {
            CastAs::SocketFd => None,
            _ => (self.fd() != -1).then_some(self.fd),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Directory stream over `opendir()` (`php_plain_files_dirstream_ops`).
#[derive(Debug)]
pub struct DirOps {
    dir: Option<sys::DirHandle>,
}

impl StreamOps for DirOps {
    fn label(&self) -> &'static str {
        "dir"
    }

    fn read(&mut self, _st: &mut StreamState, _buf: &mut [u8]) -> Result<usize, OpFailed> {
        Err(OpFailed)
    }

    fn can_write(&self) -> bool {
        false
    }

    fn readdir(&mut self, _st: &mut StreamState) -> Option<Vec<u8>> {
        self.dir.as_mut()?.read()
    }

    fn close(&mut self, _st: &mut StreamState, _preserve: bool) -> i32 {
        self.dir = None;
        0
    }

    fn can_seek(&self) -> bool {
        true
    }

    fn seek(
        &mut self,
        _st: &mut StreamState,
        _offset: i64,
        _whence: i32,
        _newpos: &mut i64,
    ) -> bool {
        if let Some(d) = self.dir.as_mut() {
            d.rewind();
        }
        true
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Opens a directory stream on a plain path.
pub fn open_dir_stream(path: &[u8]) -> io::Result<Stream> {
    let dir = sys::DirHandle::open(path)?;
    let mut s = Stream::new(Box::new(DirOps { dir: Some(dir) }), "r");
    s.add_flags(flags::NO_BUFFER | flags::IS_DIR);
    Ok(s)
}

/// `_php_stream_fopen()`: opens a plain file with an fopen mode.
pub fn fopen(
    filename: &[u8],
    mode: &str,
    options: u32,
    opened_path: &mut Option<Vec<u8>>,
) -> Result<Stream, OpenError> {
    let Some(fl) = parse_fopen_mode(mode) else {
        return Err(OpenError::msg(format!(
            "`{mode}' is not a valid mode for fopen"
        )));
    };
    let realpath = if options & STREAM_ASSUME_REALPATH != 0 {
        filename.to_vec()
    } else {
        match paths::expand_filepath(filename) {
            Some(p) => p,
            None => return Err(OpenError::errno(sys::last_errno())),
        }
    };
    // virtual_file_ex() keeps the meaning of a trailing slash: "file/"
    // names a directory, so a regular file does not match (ENOENT).
    if filename.len() > 1
        && filename.ends_with(b"/")
        && std::fs::metadata(os_path(&realpath)).is_ok_and(|m| !m.is_dir())
    {
        return Err(OpenError::errno(libc::ENOENT));
    }
    let mut target = realpath.clone();
    if filename.len() > 1 && filename.ends_with(b"/") && !target.ends_with(b"/") {
        // Let the OS reject "x/" for anything but a directory.
        target.push(b'/');
    }
    let file = open_with_flags(&target, fl).map_err(|e| OpenError::errno(sys::errno_of(&e)))?;
    state::clear_stat_cache();
    let append = fl.append;
    let mut s = if options & STREAM_OPEN_FOR_INCLUDE != 0 {
        let mut ops = StdioOps::new(file);
        // include streams skip seekability detection but must be regular
        if let Some(sb) = ops.do_fstat(false) {
            if !sb.is_file() {
                return Err(OpenError::errno(0));
            }
        }
        ops.no_forced_fstat = true;
        Stream::new(Box::new(ops), mode)
    } else {
        stream_from_file(file, mode, !append)
    };
    s.set_wrapper(wrapper_info());
    *opened_path = Some(realpath);
    Ok(s)
}

/// Opens a file with parsed fopen flags and mode 0666.
pub fn open_with_flags(path: &[u8], fl: FopenFlags) -> io::Result<File> {
    let mut oo = OpenOptions::new();
    oo.read(fl.read).write(fl.write).mode(0o666);
    if fl.append {
        oo.append(true);
    }
    let mut custom = 0;
    if fl.create && !fl.exclusive {
        oo.create(true);
    }
    if fl.exclusive {
        oo.create_new(true);
    }
    if fl.truncate {
        // OpenOptions::truncate requires write access; O_TRUNC is used
        // directly so that modes like "w" on read-only handles behave like C.
        custom |= libc::O_TRUNC;
    }
    if fl.nonblock {
        custom |= libc::O_NONBLOCK;
    }
    if !fl.read && !fl.write {
        oo.read(true);
    }
    oo.custom_flags(custom);
    oo.open(os_path(path))
}

/// Creates a temporary file stream (`php_stream_fopen_temporary_file`);
/// the file is unlinked when the stream closes.
pub fn temporary_stream(dir: Option<&[u8]>, prefix: &[u8]) -> Option<Stream> {
    let (file, path) = crate::temp::open_temporary_file(dir, prefix, true)?;
    let mut s = stream_from_file(file, "r+b", true);
    s.set_wrapper(wrapper_info());
    s.orig_path = Some(path.clone());
    if let Some(o) = s.ops_as_mut::<StdioOps>() {
        o.temp_name = Some(path);
    }
    Some(s)
}

/// `tmpfile()`.
pub fn tmpfile() -> Option<Stream> {
    temporary_stream(None, b"php")
}

fn strip_file_scheme(url: &[u8]) -> &[u8] {
    if starts_with_ci(url, b"file://") {
        &url[7..]
    } else {
        url
    }
}

/// The plain files wrapper.
#[derive(Debug, Default)]
pub struct PlainFilesWrapper;

impl Wrapper for PlainFilesWrapper {
    fn label(&self) -> &'static str {
        LABEL
    }

    fn is_plain_files(&self) -> bool {
        true
    }

    fn open(
        &self,
        path: &[u8],
        mode: &str,
        options: u32,
        opened_path: &mut Option<Vec<u8>>,
        _context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        if options & STREAM_DISABLE_OPEN_BASEDIR == 0 && !paths::check_open_basedir(path) {
            return Err(OpenError::errno(libc::EPERM));
        }
        fopen(path, mode, options, opened_path)
    }

    fn url_stat(
        &self,
        url: &[u8],
        flags: u32,
        _context: Option<&ContextRef>,
    ) -> Option<Result<StatBuf, ()>> {
        let mut url = url;
        if flags & URL_STAT_IGNORE_OPEN_BASEDIR == 0 {
            url = strip_file_scheme(url);
            if !paths::check_open_basedir_ex(url, flags & URL_STAT_QUIET == 0) {
                return Some(Err(()));
            }
        }
        let r = if flags & URL_STAT_LINK != 0 {
            crate::stat::lstat_path(url)
        } else {
            crate::stat::stat_path(url)
        };
        Some(r.map_err(|_| ()))
    }

    fn has_dir_opener(&self) -> bool {
        true
    }

    fn opendir(
        &self,
        path: &[u8],
        options: u32,
        context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        if options & STREAM_USE_GLOB_DIR_OPEN != 0 {
            return crate::glob_wrapper::GlobWrapper.opendir(path, options, context);
        }
        if options & STREAM_DISABLE_OPEN_BASEDIR == 0 && !paths::check_open_basedir(path) {
            return Err(OpenError::errno(libc::EPERM));
        }
        open_dir_stream(path).map_err(|e| OpenError::errno(sys::errno_of(&e)))
    }

    fn has_unlink(&self) -> bool {
        true
    }

    fn has_rename(&self) -> bool {
        true
    }

    fn unlink(&self, url: &[u8], options: u32, _context: Option<&ContextRef>) -> Option<bool> {
        let url = strip_file_scheme(url);
        if !paths::check_open_basedir(url) {
            return Some(false);
        }
        if let Err(e) = std::fs::remove_file(os_path(url)) {
            wrapper_warn(
                LABEL,
                options,
                StreamErrorCode::UnlinkFailed,
                sys::error_text(&e),
            );
            return Some(false);
        }
        state::clear_stat_cache();
        Some(true)
    }

    fn rename(
        &self,
        from: &[u8],
        to: &[u8],
        options: u32,
        _context: Option<&ContextRef>,
    ) -> Option<bool> {
        let from = strip_file_scheme(from);
        let to = strip_file_scheme(to);
        if !paths::check_open_basedir(from) || !paths::check_open_basedir(to) {
            return Some(false);
        }
        if let Err(e) = std::fs::rename(os_path(from), os_path(to)) {
            if sys::errno_of(&e) == libc::EXDEV {
                return Some(rename_across_devices(from, to, options));
            }
            wrapper_warn(
                LABEL,
                options,
                StreamErrorCode::RenameFailed,
                sys::error_text(&e),
            );
            return Some(false);
        }
        state::clear_stat_cache();
        Some(true)
    }

    fn mkdir(
        &self,
        dir: &[u8],
        mode: i64,
        options: u32,
        _context: Option<&ContextRef>,
    ) -> Option<bool> {
        Some(plain_mkdir(strip_file_scheme(dir), mode, options))
    }

    fn rmdir(&self, url: &[u8], options: u32, _context: Option<&ContextRef>) -> Option<bool> {
        let url = strip_file_scheme(url);
        if !paths::check_open_basedir(url) {
            return Some(false);
        }
        if let Err(e) = std::fs::remove_dir(os_path(url)) {
            wrapper_warn(
                LABEL,
                options,
                StreamErrorCode::RmdirFailed,
                sys::error_text(&e),
            );
            return Some(false);
        }
        state::clear_stat_cache();
        Some(true)
    }

    fn metadata(&self, url: &[u8], op: &MetadataOp, _context: Option<&ContextRef>) -> Option<bool> {
        Some(plain_metadata(strip_file_scheme(url), op))
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn rename_across_devices(from: &[u8], to: &[u8], options: u32) -> bool {
    let oldmask = sys::umask(0o077);
    let mut success = false;
    if crate::file::copy_file_plain(from, to).is_ok() {
        match crate::stat::stat_path(from) {
            Ok(sb) => {
                success = true;
                if let Err(e) = sys::chown(to, Some(sb.uid as u32), Some(sb.gid as u32), true) {
                    if sys::errno_of(&e) != libc::EPERM {
                        success = false;
                    }
                    wrapper_error(
                        LABEL,
                        options,
                        Severity::Warning,
                        !success,
                        StreamErrorCode::ChownFailed,
                        sys::error_text(&e),
                    );
                }
                if success {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = std::fs::Permissions::from_mode(sb.mode as u32);
                    if let Err(e) = std::fs::set_permissions(os_path(to), perms) {
                        if sys::errno_of(&e) != libc::EPERM {
                            success = false;
                        }
                        wrapper_error(
                            LABEL,
                            options,
                            Severity::Warning,
                            !success,
                            StreamErrorCode::ChownFailed,
                            sys::error_text(&e),
                        );
                    }
                }
                if success {
                    let _ = std::fs::remove_file(os_path(from));
                }
            }
            Err(e) => wrapper_error(
                LABEL,
                options,
                Severity::Warning,
                false,
                StreamErrorCode::StatFailed,
                sys::error_text(&e),
            ),
        }
    } else {
        wrapper_error(
            LABEL,
            options,
            Severity::Warning,
            false,
            StreamErrorCode::CopyFailed,
            sys::strerror(sys::last_errno()),
        );
    }
    sys::umask(oldmask);
    success
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn mkdir_one(path: &[u8], mode: i64) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .mode(mode as u32)
        .create(os_path(path))
}

/// `php_plain_files_mkdir()`.
#[allow(clippy::too_many_lines)] // a direct port of the C function
pub fn plain_mkdir(dir: &[u8], mode: i64, options: u32) -> bool {
    if options & MKDIR_RECURSIVE == 0 {
        if !paths::check_open_basedir(dir) {
            return false;
        }
        if let Err(e) = mkdir_one(dir, mode) {
            wrapper_warn(
                LABEL,
                options,
                StreamErrorCode::MkdirFailed,
                sys::error_text(&e),
            );
            return false;
        }
        return true;
    }
    let Some(mut buf) = paths::expand_filepath(dir) else {
        wrapper_warn(LABEL, options, StreamErrorCode::InvalidPath, "Invalid path");
        return false;
    };
    if !paths::check_open_basedir(&buf) {
        return false;
    }
    // Port of the C loop: find the deepest existing ancestor by replacing
    // '/' with NUL from the end, then create components forwards.
    let e = buf.len();
    let dir_len = dir.len();
    let mut offset = 0;
    let first = buf[..dir_len.min(buf.len())]
        .iter()
        .position(|&c| c == b'/');
    if let Some(p) = first {
        offset = p + 1;
    }
    let mut p: Option<usize> = None;
    if !(first.is_some() && dir_len == 1) {
        loop {
            let cand = buf[offset..]
                .iter()
                .rposition(|&c| c == b'/')
                .map(|x| x + offset)
                .or_else(|| {
                    if offset == 1 {
                        None
                    } else {
                        buf.iter().rposition(|&c| c == b'/')
                    }
                });
            let Some(mut q) = cand else {
                p = None;
                break;
            };
            buf[q] = 0;
            let mut n = 0;
            while q > 0 && buf[q - 1] == b'/' {
                n += 1;
                q -= 1;
                buf[q] = 0;
            }
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            if crate::stat::stat_path(&buf[..end]).is_ok() {
                loop {
                    buf[q] = b'/';
                    if n == 0 {
                        break;
                    }
                    n -= 1;
                    q += 1;
                }
                p = Some(q);
                break;
            }
        }
    }
    let mut p = p.unwrap_or(0);
    loop {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let ret = mkdir_one(&buf[..end], mode);
        if let Err(err) = &ret {
            if err.kind() != io::ErrorKind::AlreadyExists {
                wrapper_warn(
                    LABEL,
                    options,
                    StreamErrorCode::MkdirFailed,
                    sys::error_text(err),
                );
                return false;
            }
        }
        let mut replaced = false;
        loop {
            p += 1;
            if p >= e {
                break;
            }
            if buf[p] == 0 {
                replaced = true;
                buf[p] = b'/';
                if p + 1 < e && buf[p + 1] != 0 {
                    break;
                }
            }
        }
        if p >= e || !replaced {
            if let Err(err) = ret {
                wrapper_warn(
                    LABEL,
                    options,
                    StreamErrorCode::MkdirFailed,
                    sys::error_text(&err),
                );
                return false;
            }
            return true;
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn plain_metadata(url: &[u8], op: &MetadataOp) -> bool {
    if !paths::check_open_basedir(url) {
        return false;
    }
    let r = match op {
        MetadataOp::Touch(times) => {
            if sys::access(url, sys::F_OK).is_err() {
                if let Err(e) = File::create(os_path(url)) {
                    wrapper_warn(
                        LABEL,
                        REPORT_ERRORS,
                        StreamErrorCode::PermissionDenied,
                        diag::msg(&[
                            b"Unable to create file ",
                            url,
                            b" because ",
                            sys::error_text(&e).as_bytes(),
                        ]),
                    );
                    return false;
                }
            }
            sys::utime(url, times.map(|(m, a)| (a, m)))
        }
        MetadataOp::OwnerName(name) => {
            let Some(uid) = sys::uid_by_name(name) else {
                wrapper_warn(
                    LABEL,
                    REPORT_ERRORS,
                    StreamErrorCode::MetaFailed,
                    diag::msg(&[b"Unable to find uid for ", name]),
                );
                return false;
            };
            sys::chown(url, Some(uid), None, true)
        }
        MetadataOp::Owner(uid) => sys::chown(url, Some(*uid as u32), None, true),
        MetadataOp::GroupName(name) => {
            let Some(gid) = sys::gid_by_name(name) else {
                wrapper_warn(
                    LABEL,
                    REPORT_ERRORS,
                    StreamErrorCode::MetaFailed,
                    diag::msg(&[b"Unable to find gid for ", name]),
                );
                return false;
            };
            sys::chown(url, None, Some(gid), true)
        }
        MetadataOp::Group(gid) => sys::chown(url, None, Some(*gid as u32), true),
        MetadataOp::Access(mode) => {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                os_path(url),
                std::fs::Permissions::from_mode(*mode as u32 & 0o7777),
            )
        }
    };
    if let Err(e) = r {
        wrapper_warn(
            LABEL,
            REPORT_ERRORS,
            StreamErrorCode::MetaFailed,
            format!("Operation failed: {}", sys::error_text(&e)),
        );
        return false;
    }
    state::clear_stat_cache();
    true
}
