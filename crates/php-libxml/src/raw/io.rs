//! libxml2 I/O trampolines: the external entity loader and the default
//! input/output buffer factories, plus Rust-backed I/O contexts.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::io::{Read, Write};

use super::{c_string, errors::parser_position, guard, opt_bytes, uri};
use crate::error::{self, Diagnostic, DiagnosticLevel};
use crate::ffi;
use crate::io::{self as hooks, EntityLoaderContext, EntityLoaderResult, EntityRequest};

const NUL_URI_WARNING: &[u8] = b"URI must not contain percent-encoded NUL bytes";

// ---------------------------------------------------------------------------
// Rust-backed I/O contexts
// ---------------------------------------------------------------------------

/// Read callback over a `Box<Box<dyn Read>>` context.
unsafe extern "C" fn read_callback(ctx: *mut c_void, buffer: *mut c_char, len: c_int) -> c_int {
    if ctx.is_null() || buffer.is_null() || len <= 0 {
        return 0;
    }
    // SAFETY: ctx is the Box<Box<dyn Read>> leaked by `input_buffer_from_reader`
    // and still alive (libxml2 calls close exactly once, after all reads);
    // buffer is valid for `len` bytes.
    let (reader, buf) = unsafe {
        (
            &mut *ctx.cast::<Box<dyn Read>>(),
            std::slice::from_raw_parts_mut(buffer.cast::<u8>(), usize::try_from(len).unwrap_or(0)),
        )
    };
    guard::catch(-1, || loop {
        match reader.read(buf) {
            Ok(n) => break c_int::try_from(n).unwrap_or(-1),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break -1,
        }
    })
}

unsafe extern "C" fn read_close_callback(ctx: *mut c_void) -> c_int {
    if !ctx.is_null() {
        // SAFETY: ctx was produced by Box::into_raw in `input_buffer_from_reader`
        // and libxml2 closes it exactly once.
        let boxed = unsafe { Box::from_raw(ctx.cast::<Box<dyn Read>>()) };
        guard::catch((), || drop(boxed));
    }
    0
}

/// Wraps a reader into an `xmlParserInputBuffer` (NULL on failure; the
/// reader is dropped in that case).
pub(crate) fn input_buffer_from_reader(
    reader: Box<dyn Read>,
    enc: c_int,
) -> *mut ffi::xmlParserInputBuffer {
    let ctx = Box::into_raw(Box::new(reader)).cast::<c_void>();
    // SAFETY: the callbacks match libxml2's expectations for this context;
    // ownership of ctx passes to the buffer (released by the close callback).
    let buf = unsafe {
        ffi::xmlParserInputBufferCreateIO(Some(read_callback), Some(read_close_callback), ctx, enc)
    };
    if buf.is_null() {
        // SAFETY: the buffer was not created, so we still own ctx.
        unsafe { read_close_callback(ctx) };
    }
    buf
}

/// Write callback over a `Box<Box<dyn Write>>` context.
pub(crate) unsafe extern "C" fn write_callback(
    ctx: *mut c_void,
    buffer: *const c_char,
    len: c_int,
) -> c_int {
    if ctx.is_null() || len < 0 {
        return -1;
    }
    if len == 0 || buffer.is_null() {
        return 0;
    }
    // SAFETY: ctx is the Box<Box<dyn Write>> leaked by `output_buffer_from_writer`
    // and alive until the close callback; buffer is valid for len bytes.
    let (writer, data) = unsafe {
        (
            &mut *ctx.cast::<Box<dyn Write>>(),
            std::slice::from_raw_parts(buffer.cast::<u8>(), usize::try_from(len).unwrap_or(0)),
        )
    };
    guard::catch(-1, || match writer.write_all(data) {
        Ok(()) => len,
        Err(_) => -1,
    })
}

pub(crate) unsafe extern "C" fn write_close_callback(ctx: *mut c_void) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    // SAFETY: ctx was produced by Box::into_raw in `output_buffer_from_writer`
    // and libxml2 closes it exactly once.
    let mut boxed = unsafe { Box::from_raw(ctx.cast::<Box<dyn Write>>()) };
    guard::catch(-1, move || {
        let ok = boxed.flush().is_ok();
        drop(boxed);
        if ok {
            0
        } else {
            -1
        }
    })
}

/// Wraps a writer into an `xmlOutputBuffer` using `encoder` (consumed).
/// Returns NULL on failure (the writer is dropped).
pub(crate) fn output_buffer_from_writer(
    writer: Box<dyn Write>,
    encoder: *mut ffi::xmlCharEncodingHandler,
) -> *mut ffi::xmlOutputBuffer {
    let ctx = Box::into_raw(Box::new(writer)).cast::<c_void>();
    // SAFETY: callbacks match the context type; ownership of ctx passes to
    // the buffer, which calls the close callback when closed.
    let buf = unsafe {
        ffi::xmlOutputBufferCreateIO(
            Some(write_callback),
            Some(write_close_callback),
            ctx,
            encoder,
        )
    };
    if buf.is_null() {
        // SAFETY: not adopted by libxml2, release it ourselves.
        unsafe { write_close_callback(ctx) };
    }
    buf
}

// ---------------------------------------------------------------------------
// Default buffer factories (php_libxml_input/output_buffer_create_filename)
// ---------------------------------------------------------------------------

fn contains_percent_nul(s: &[u8]) -> bool {
    s.windows(3).any(|w| w == b"%00")
}

/// `php_libxml_input_buffer_create_filename`.
pub(crate) unsafe extern "C" fn input_buffer_trampoline(
    uri_ptr: *const c_char,
    enc: c_int,
) -> *mut ffi::xmlParserInputBuffer {
    if uri_ptr.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: libxml2 passes a valid C string.
    let uri_bytes = unsafe { CStr::from_ptr(uri_ptr) }.to_bytes().to_vec();
    guard::catch(std::ptr::null_mut(), || {
        open_input(&uri_bytes, uri_ptr, enc)
    })
}

fn open_input(
    uri_bytes: &[u8],
    uri_ptr: *const c_char,
    enc: c_int,
) -> *mut ffi::xmlParserInputBuffer {
    if hooks::entity_loader_disabled() {
        return std::ptr::null_mut();
    }
    let Some(stream_hooks) = hooks::stream_hooks() else {
        // SAFETY: libxml2's own factory with the original arguments.
        return unsafe { ffi::__xmlParserInputBufferCreateFilename(uri_ptr, enc) };
    };
    let Some(open) = stream_hooks.open_read.as_ref() else {
        // SAFETY: as above.
        return unsafe { ffi::__xmlParserInputBufferCreateFilename(uri_ptr, enc) };
    };
    if contains_percent_nul(uri_bytes) {
        error::emit(Diagnostic {
            level: DiagnosticLevel::Warning,
            message: NUL_URI_WARNING.to_vec(),
        });
        return std::ptr::null_mut();
    }
    let resolved = match uri::parse(uri_bytes) {
        Some(parsed)
            if parsed
                .scheme
                .as_deref()
                .is_none_or(|s| s.len() >= 4 && &s[..4] == b"file") =>
        {
            match uri::unescape(uri_bytes) {
                Some(r) => r,
                None => return std::ptr::null_mut(),
            }
        }
        _ => uri_bytes.to_vec(),
    };
    let Some(stream) = open(&resolved) else {
        return std::ptr::null_mut();
    };
    let mut enc = enc;
    if enc == ffi::XML_CHAR_ENCODING_NONE {
        if let Some(charset) = &stream.charset {
            enc = super::parse_char_encoding(charset);
        }
    }
    input_buffer_from_reader(stream.reader, enc)
}

/// `php_libxml_output_buffer_create_filename`.
pub(crate) unsafe extern "C" fn output_buffer_trampoline(
    uri_ptr: *const c_char,
    encoder: *mut ffi::xmlCharEncodingHandler,
    compression: c_int,
) -> *mut ffi::xmlOutputBuffer {
    let uri_bytes = if uri_ptr.is_null() {
        None
    } else {
        // SAFETY: libxml2 passes a valid C string.
        Some(unsafe { CStr::from_ptr(uri_ptr) }.to_bytes().to_vec())
    };
    let result = guard::catch(Err(()), || {
        open_output(uri_bytes.as_deref(), uri_ptr, encoder, compression)
    });
    if let Ok(buf) = result {
        buf
    } else {
        if !encoder.is_null() {
            // SAFETY: libxml2 < 2.14.4 leaves the encoder to the factory on
            // failure; we close it exactly once.
            unsafe { ffi::xmlCharEncCloseFunc(encoder) };
        }
        std::ptr::null_mut()
    }
}

fn open_output(
    uri_bytes: Option<&[u8]>,
    uri_ptr: *const c_char,
    encoder: *mut ffi::xmlCharEncodingHandler,
    compression: c_int,
) -> Result<*mut ffi::xmlOutputBuffer, ()> {
    let open = hooks::stream_hooks().filter(|h| h.open_write.is_some());
    let Some(stream_hooks) = open else {
        if let Some(path) = uri_bytes.and_then(local_path) {
            // PHP writes local files through the unbuffered plain-files
            // stream wrapper; libxml2's own FILE* output would only become
            // visible on close.
            let file = std::fs::File::create(bytes_to_path(&path)).map_err(|_| ())?;
            let buf = output_buffer_from_writer(Box::new(file), encoder);
            return if buf.is_null() { Err(()) } else { Ok(buf) };
        }
        // SAFETY: libxml2's own factory with the original arguments; it
        // takes care of the encoder.
        return Ok(unsafe { ffi::__xmlOutputBufferCreateFilename(uri_ptr, encoder, compression) });
    };
    let Some(open_write) = stream_hooks.open_write.as_ref() else {
        return Err(());
    };
    let Some(uri_bytes) = uri_bytes else {
        return Err(());
    };
    if contains_percent_nul(uri_bytes) {
        error::emit(Diagnostic {
            level: DiagnosticLevel::Warning,
            message: NUL_URI_WARNING.to_vec(),
        });
        return Err(());
    }
    let unescaped = match uri::parse(uri_bytes) {
        Some(parsed) if parsed.scheme.is_some() => uri::unescape(uri_bytes),
        _ => None,
    };
    let writer = unescaped
        .and_then(|u| open_write(&u))
        .or_else(|| open_write(uri_bytes));
    let Some(writer) = writer else { return Err(()) };
    let buf = output_buffer_from_writer(writer, encoder);
    if buf.is_null() {
        Err(())
    } else {
        Ok(buf)
    }
}

/// The local file path of a scheme-less or `file:` URI (what PHP's
/// plain-files wrapper would open), `None` for other schemes.
fn local_path(uri_bytes: &[u8]) -> Option<Vec<u8>> {
    let parsed = uri::parse(uri_bytes);
    match parsed.as_ref().and_then(|p| p.scheme.as_deref()) {
        None => Some(uri_bytes.to_vec()),
        Some(s) if s.eq_ignore_ascii_case(b"file") => {
            let u = uri::unescape(uri_bytes)?;
            let lower = u.to_ascii_lowercase();
            let rest = if lower.starts_with(b"file://localhost/") {
                &u[16..]
            } else if lower.starts_with(b"file:///") {
                &u[7..]
            } else if lower.starts_with(b"file:/") {
                &u[5..]
            } else {
                return None;
            };
            Some(rest.to_vec())
        }
        Some(_) => None,
    }
}

fn bytes_to_path(b: &[u8]) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b))
    }
    #[cfg(not(unix))]
    {
        std::path::PathBuf::from(String::from_utf8_lossy(b).into_owned())
    }
}

// ---------------------------------------------------------------------------
// External entity loader (php_libxml_external_entity_loader)
// ---------------------------------------------------------------------------

pub(crate) unsafe extern "C" fn entity_loader_trampoline(
    url: *const c_char,
    id: *const c_char,
    ctxt: *mut ffi::xmlParserCtxt,
) -> *mut ffi::xmlParserInput {
    let Some(loader) = hooks::external_entity_loader() else {
        return match super::default_entity_loader() {
            // SAFETY: forwarding libxml2's own arguments to its default loader.
            Some(default) => unsafe { default(url, id, ctxt) },
            None => std::ptr::null_mut(),
        };
    };
    // SAFETY: url/id are NULL or valid C strings; ctxt is NULL or the live
    // parser context that requested the entity.
    let request = unsafe {
        EntityRequest {
            public_id: opt_bytes(id),
            system_id: opt_bytes(url),
            context: if ctxt.is_null() {
                EntityLoaderContext::default()
            } else {
                EntityLoaderContext {
                    directory: opt_bytes((*ctxt).directory),
                    int_sub_name: opt_bytes((*ctxt).intSubName.cast()),
                    ext_sub_uri: opt_bytes((*ctxt).extSubURI.cast()),
                    ext_sub_system: opt_bytes((*ctxt).extSubSystem.cast()),
                }
            },
        }
    };
    guard::catch(std::ptr::null_mut(), || {
        let result = loader(&request);
        // SAFETY: ctxt is NULL or live (see above).
        let position = unsafe { parser_position(ctxt) };
        let mut resource: Option<Vec<u8>> = None;
        let mut ret: *mut ffi::xmlParserInput = std::ptr::null_mut();
        match result {
            EntityLoaderResult::Failed { function_name } => {
                let mut msg = b"Call to user entity loader callback '".to_vec();
                msg.extend_from_slice(&function_name);
                msg.extend_from_slice(b"' has failed");
                error::ctx_error(position.as_ref(), &msg);
            }
            EntityLoaderResult::Null => {}
            EntityLoaderResult::Path(p) => resource = Some(p),
            EntityLoaderResult::Stream(reader) => {
                let pib = input_buffer_from_reader(reader, ffi::XML_CHAR_ENCODING_NONE);
                if pib.is_null() {
                    error::ctx_error(position.as_ref(), b"Could not allocate parser input buffer");
                } else {
                    // SAFETY: ctxt is live (or NULL, which libxml2 accepts);
                    // pib ownership passes to the input on success.
                    ret =
                        unsafe { ffi::xmlNewIOInputStream(ctxt, pib, ffi::XML_CHAR_ENCODING_NONE) };
                    if ret.is_null() {
                        // SAFETY: not adopted by the input stream.
                        unsafe { ffi::xmlFreeParserInputBuffer(pib) };
                    }
                }
            }
        }
        if ret.is_null() {
            match resource {
                None => match &request.public_id {
                    None => error::ctx_error(
                        position.as_ref(),
                        b"Failed to load external entity because the resolver function returned null\n",
                    ),
                    Some(id) => {
                        let mut msg = b"Failed to load external entity \"".to_vec();
                        msg.extend_from_slice(id);
                        msg.extend_from_slice(b"\"\n");
                        error::ctx_error(position.as_ref(), &msg);
                    }
                },
                Some(path) => {
                    let c = c_string(&path);
                    // SAFETY: ctxt is live or NULL; c is a valid C string.
                    ret = unsafe { ffi::xmlNewInputFromFile(ctxt, c.as_ptr()) };
                }
            }
        }
        ret
    })
}

#[cfg(test)]
mod tests {
    use super::contains_percent_nul;

    #[test]
    fn percent_nul_detection() {
        assert!(contains_percent_nul(b"file:///a%00b"));
        assert!(!contains_percent_nul(b"file:///a%0b"));
    }
}
