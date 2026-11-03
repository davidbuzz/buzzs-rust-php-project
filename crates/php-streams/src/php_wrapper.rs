//! The `php://` wrapper (`ext/standard/php_fopen_wrapper.c`).

use crate::context::ContextRef;
use crate::diag::StreamErrorCode;
use crate::memory::{memory_create, memory_open, temp_create, MAX_MEM};
use crate::mode::TempMode;
use crate::stream::{OpFailed, Stream, StreamOps, StreamState, WrapperInfo, SEEK_SET};
use crate::util::{starts_with_ci, strtol};
use crate::value::Value;
use crate::wrapper::{wrapper_warn, OpenError, Wrapper, STREAM_OPEN_FOR_INCLUDE};
use crate::{state, sys};

/// The wrapper label.
pub const LABEL: &str = "PHP";

/// The [`WrapperInfo`] for `php://` streams.
pub fn wrapper_info() -> WrapperInfo {
    WrapperInfo {
        label: LABEL,
        is_url: false,
        is_plain_files: false,
    }
}

/// `php://output` (`php_stream_output_ops`).
#[derive(Debug, Default)]
pub struct OutputOps;

impl StreamOps for OutputOps {
    fn label(&self) -> &'static str {
        "Output"
    }

    fn read(&mut self, st: &mut StreamState, _buf: &mut [u8]) -> Result<usize, OpFailed> {
        st.eof = true;
        Err(OpFailed)
    }

    fn write(&mut self, _st: &mut StreamState, buf: &[u8]) -> Result<usize, OpFailed> {
        state::php_write(buf);
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// `php://input` (`php_stream_input_ops`).
#[derive(Debug)]
pub struct InputOps {
    body: Box<Stream>,
    position: i64,
}

impl StreamOps for InputOps {
    fn label(&self) -> &'static str {
        "Input"
    }

    fn read(&mut self, st: &mut StreamState, buf: &mut [u8]) -> Result<usize, OpFailed> {
        if self.body.read_filters.is_empty() {
            self.body.seek(self.position, SEEK_SET);
        }
        let r = self.body.read(buf);
        match r {
            Ok(0) | Err(_) => st.eof = true,
            Ok(n) => self.position += i64::try_from(n).unwrap_or(0),
        }
        r
    }

    fn can_seek(&self) -> bool {
        true
    }

    fn seek(&mut self, _st: &mut StreamState, offset: i64, whence: i32, newpos: &mut i64) -> bool {
        let r = self.body.seek(offset, whence);
        self.position = self.body.tell();
        *newpos = self.position;
        r == 0
    }

    fn flush(&mut self, _st: &mut StreamState) -> i32 {
        -1
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

const MAX_FILTER_COUNT_DEFAULT: i64 = 16;

/// `php_stream_apply_filter_list()`: returns false when the filter count
/// limit is reached.
fn apply_filter_list(
    stream: &mut Stream,
    list: &[u8],
    read: bool,
    write: bool,
    context: Option<&ContextRef>,
) -> bool {
    let max = context
        .and_then(|c| {
            c.borrow()
                .get("filter", "max_filter_count")
                .map(Value::to_long)
        })
        .unwrap_or(MAX_FILTER_COUNT_DEFAULT);
    for name in list.split(|&c| c == b'|').filter(|p| !p.is_empty()) {
        let rc = if read { stream.read_filters.len() } else { 0 };
        let wc = if write { stream.write_filters.len() } else { 0 };
        if i64::try_from(rc).unwrap_or(i64::MAX) == max
            || i64::try_from(wc).unwrap_or(i64::MAX) == max
        {
            return false;
        }
        let decoded = crate::memory::url_decode(name);
        let name = String::from_utf8_lossy(&decoded).into_owned();
        for (enabled, kind) in [
            (read, crate::filter::ChainKind::Read),
            (write, crate::filter::ChainKind::Write),
        ] {
            if !enabled {
                continue;
            }
            match crate::wrapper::create_filter(&name, None, false) {
                Some(f) => {
                    stream.append_filter(kind, f);
                }
                None => wrapper_warn(
                    crate::wrapper::DEFAULT_WRAPPER_NAME,
                    crate::wrapper::REPORT_ERRORS,
                    StreamErrorCode::CreateFailed,
                    format!("Unable to create filter ({name})"),
                ),
            }
        }
    }
    true
}

/// Errors that PHP raises as exceptions while opening `php://` URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhpUrlException {
    /// `ValueError`: "must be greater than or equal to 0" (argument 2).
    NegativeMaxMemory,
    /// `Error`: "No URL resource specified".
    NoResource,
}

thread_local! {
    static PENDING_EXCEPTION: std::cell::RefCell<Option<PhpUrlException>> = const { std::cell::RefCell::new(None) };
}

/// Takes the exception raised by the last `php://` open, if any. The VM
/// must check this after a failed open and throw accordingly.
pub fn take_exception() -> Option<PhpUrlException> {
    PENDING_EXCEPTION.with(|e| e.borrow_mut().take())
}

fn raise(e: PhpUrlException) -> OpenError {
    PENDING_EXCEPTION.with(|p| *p.borrow_mut() = Some(e));
    OpenError {
        exception: true,
        ..OpenError::default()
    }
}

/// The `php://` wrapper.
#[derive(Debug, Default)]
pub struct PhpWrapper;

impl PhpWrapper {
    fn std_stream(which: u8, mode: &str) -> Result<Stream, OpenError> {
        let settings = state::settings();
        let fd: i32 = i32::from(which);
        let file = if settings.sapi_is_cli && !state::cli_std_opened(which) {
            Some(sys::file_from_fd(fd))
        } else {
            None
        };
        if let Some(f) = file {
            return Ok(crate::plain::stream_from_stdio_file(f, mode));
        }
        let dup = sys::dup(fd).map_err(|e| OpenError::errno(sys::errno_of(&e)))?;
        Ok(crate::plain::stream_from_file(
            sys::file_from_fd(dup),
            mode,
            false,
        ))
    }
}

impl Wrapper for PhpWrapper {
    fn label(&self) -> &'static str {
        LABEL
    }

    #[allow(clippy::too_many_lines)]
    fn open(
        &self,
        path: &[u8],
        mode: &str,
        options: u32,
        opened_path: &mut Option<Vec<u8>>,
        context: Option<&ContextRef>,
    ) -> Result<Stream, OpenError> {
        let mut p = path;
        if starts_with_ci(p, b"php://") {
            p = &p[6..];
        }
        let settings = state::settings();
        let include_denied = options & STREAM_OPEN_FOR_INCLUDE != 0 && !settings.allow_url_include;
        if starts_with_ci(p, b"temp") {
            p = &p[4..];
            let mut max = i64::try_from(MAX_MEM).unwrap_or(i64::MAX);
            if starts_with_ci(p, b"/maxmemory:") {
                max = strtol(&p[11..]).0;
                if max < 0 {
                    return Err(raise(PhpUrlException::NegativeMaxMemory));
                }
            }
            return Ok(temp_create(
                TempMode::from_mode(mode),
                usize::try_from(max).unwrap_or(0),
                None,
            ));
        }
        if p.eq_ignore_ascii_case(b"memory") {
            return Ok(memory_create(TempMode::from_mode(mode)));
        }
        if p.eq_ignore_ascii_case(b"output") {
            return Ok(Stream::new(Box::new(OutputOps), "wb"));
        }
        let disabled = |opts| {
            wrapper_warn(
                LABEL,
                opts,
                StreamErrorCode::Disabled,
                "URL file-access is disabled in the server configuration",
            );
            Err(OpenError::silent())
        };
        if p.eq_ignore_ascii_case(b"input") {
            if include_denied {
                return disabled(options);
            }
            let body = memory_open(TempMode::ReadOnly, state::request_body());
            return Ok(Stream::new(
                Box::new(InputOps {
                    body: Box::new(body),
                    position: 0,
                }),
                "rb",
            ));
        }
        if p.eq_ignore_ascii_case(b"stdin") {
            if include_denied {
                return disabled(options);
            }
            return Self::std_stream(0, mode);
        }
        if p.eq_ignore_ascii_case(b"stdout") {
            return Self::std_stream(1, mode);
        }
        if p.eq_ignore_ascii_case(b"stderr") {
            return Self::std_stream(2, mode);
        }
        if starts_with_ci(p, b"fd/") {
            if !settings.sapi_is_cli {
                wrapper_warn(
                    LABEL,
                    options,
                    StreamErrorCode::Disabled,
                    "Direct access to file descriptors is only available from command-line PHP",
                );
                return Err(OpenError::silent());
            }
            if include_denied {
                return disabled(options);
            }
            let start = &p[3..];
            let (fd, used) = strtol(start);
            if used == 0 || used != start.len() {
                return Err(OpenError::msg(
                    "php://fd/ stream must be specified in the form php://fd/<orig fd>",
                ));
            }
            let dtable = sys::dtablesize();
            if fd < 0 || fd >= dtable {
                return Err(OpenError::msg(format!(
                    "The file descriptors must be non-negative numbers smaller than {dtable}"
                )));
            }
            let fd = i32::try_from(fd).unwrap_or(-1);
            return match sys::dup(fd) {
                Ok(nfd) => Ok(crate::plain::stream_from_file(
                    sys::file_from_fd(nfd),
                    mode,
                    false,
                )),
                Err(e) => {
                    let errno = sys::errno_of(&e);
                    Err(OpenError::msg(format!(
                        "Error duping file descriptor {fd}; possibly it doesn't exist: [{errno}]: {}",
                        sys::strerror(errno)
                    )))
                }
            };
        }
        if starts_with_ci(p, b"filter/") {
            let mut read = mode.contains('r') || mode.contains('+');
            let mut write = mode.contains('w') || mode.contains('+') || mode.contains('a');
            if !read && !write {
                read = false;
                write = false;
            }
            let pathdup = &p[6..];
            let Some(res) = crate::util::find_bytes(pathdup, b"/resource=") else {
                return Err(raise(PhpUrlException::NoResource));
            };
            let target = &pathdup[res + 10..];
            let mut stream = crate::open::open_wrapper(target, mode, options, opened_path, context)
                .ok_or_else(OpenError::silent)?;
            let spec = &pathdup[1..res];
            let mut ok = true;
            for tok in spec.split(|&c| c == b'/').filter(|t| !t.is_empty()) {
                ok = if starts_with_ci(tok, b"read=") {
                    apply_filter_list(&mut stream, &tok[5..], true, false, context)
                } else if starts_with_ci(tok, b"write=") {
                    apply_filter_list(&mut stream, &tok[6..], false, true, context)
                } else {
                    apply_filter_list(&mut stream, tok, read, write, context)
                };
                if !ok {
                    break;
                }
            }
            if !ok {
                stream.close();
                return Err(OpenError::msg("too many filters"));
            }
            return Ok(stream);
        }
        wrapper_warn(
            LABEL,
            options,
            StreamErrorCode::InvalidUrl,
            "Invalid php:// URL specified",
        );
        Err(OpenError::silent())
    }
}
