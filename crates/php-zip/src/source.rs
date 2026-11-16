// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `php_zip_create_string_source()` (`zip_source.c`): a libzip source
//! function serving an in-memory string, which can also receive the
//! rewritten archive (used by `ZipArchive::openString()` /
//! `closeString()` and `addFromString()`).

use std::cell::RefCell;
use std::ptr;
use std::rc::Rc;

use libc::{c_void, time_t};

use crate::ffi::{self, zip_error_t, zip_int64_t, zip_source_cmd_t, zip_source_t, zip_uint64_t};

/// Where the rewritten archive is published on commit.
pub(crate) type OutStr = Rc<RefCell<Vec<u8>>>;

struct StringSource {
    in_str: Vec<u8>,
    in_offset: usize,
    mtime: time_t,
    out_str: Vec<u8>,
    out_offset: usize,
    dest: Option<OutStr>,
    error: zip_error_t,
}

fn bitmap(cmds: &[zip_source_cmd_t]) -> zip_int64_t {
    cmds.iter().fold(0, |acc, &c| acc | (1i64 << c))
}

fn now() -> time_t {
    // SAFETY: time(NULL) has no preconditions.
    unsafe { libc::time(ptr::null_mut()) }
}

fn to_i64(n: usize) -> zip_int64_t {
    zip_int64_t::try_from(n).unwrap_or(zip_int64_t::MAX)
}

impl StringSource {
    fn set_error(&mut self, ze: i32, se: i32) {
        // SAFETY: `self.error` was initialised with zip_error_init().
        unsafe { ffi::zip_error_set(&raw mut self.error, ze, se) };
    }

    #[allow(clippy::too_many_lines)]
    fn command(
        &mut self,
        data: *mut c_void,
        len: zip_uint64_t,
        cmd: zip_source_cmd_t,
    ) -> zip_int64_t {
        let len_usize = usize::try_from(len).unwrap_or(usize::MAX);
        match cmd {
            ffi::ZIP_SOURCE_SUPPORTS => {
                let mut cmds = vec![
                    ffi::ZIP_SOURCE_FREE,
                    ffi::ZIP_SOURCE_OPEN,
                    ffi::ZIP_SOURCE_READ,
                    ffi::ZIP_SOURCE_CLOSE,
                    ffi::ZIP_SOURCE_STAT,
                    ffi::ZIP_SOURCE_ERROR,
                    ffi::ZIP_SOURCE_SEEK,
                    ffi::ZIP_SOURCE_TELL,
                    ffi::ZIP_SOURCE_BEGIN_WRITE,
                    ffi::ZIP_SOURCE_WRITE,
                    ffi::ZIP_SOURCE_COMMIT_WRITE,
                    ffi::ZIP_SOURCE_ROLLBACK_WRITE,
                    ffi::ZIP_SOURCE_SEEK_WRITE,
                    ffi::ZIP_SOURCE_TELL_WRITE,
                    ffi::ZIP_SOURCE_REMOVE,
                ];
                if cfg!(libzip_1_10) {
                    cmds.push(ffi::ZIP_SOURCE_SUPPORTS_REOPEN);
                }
                bitmap(&cmds)
            }
            ffi::ZIP_SOURCE_OPEN => {
                self.in_offset = 0;
                0
            }
            ffi::ZIP_SOURCE_READ => {
                let remaining = self.in_str.len() - self.in_offset;
                let n = len_usize.min(remaining);
                if n > 0 {
                    // SAFETY: libzip passes a writable buffer of at least
                    // `len` bytes for ZIP_SOURCE_READ, and n <= len.
                    unsafe {
                        ptr::copy_nonoverlapping(
                            self.in_str.as_ptr().add(self.in_offset),
                            data.cast::<u8>(),
                            n,
                        );
                    }
                    self.in_offset += n;
                }
                to_i64(n)
            }
            ffi::ZIP_SOURCE_CLOSE | ffi::ZIP_SOURCE_FREE => 0,
            ffi::ZIP_SOURCE_STAT => {
                if len_usize < std::mem::size_of::<ffi::zip_stat_t>() {
                    self.set_error(crate::consts::er::INVAL, 0);
                    return -1;
                }
                let st = data.cast::<ffi::zip_stat_t>();
                // SAFETY: libzip passes a zip_stat_t of at least `len` bytes
                // (checked above).
                unsafe {
                    ffi::zip_stat_init(st);
                    (*st).mtime = self.mtime;
                    (*st).size = self.in_str.len() as u64;
                    (*st).comp_size = (*st).size;
                    (*st).comp_method = 0;
                    (*st).encryption_method = 0;
                    (*st).valid = ffi::ZIP_STAT_MTIME
                        | ffi::ZIP_STAT_SIZE
                        | ffi::ZIP_STAT_COMP_SIZE
                        | ffi::ZIP_STAT_COMP_METHOD
                        | ffi::ZIP_STAT_ENCRYPTION_METHOD;
                }
                to_i64(std::mem::size_of::<ffi::zip_stat_t>())
            }
            // SAFETY: `data`/`len` describe libzip's output buffer.
            ffi::ZIP_SOURCE_ERROR => unsafe {
                ffi::zip_error_to_data(&raw const self.error, data, len)
            },
            ffi::ZIP_SOURCE_SEEK => {
                // SAFETY: `data`/`len` are libzip's seek arguments; the
                // error pointer is our initialised error.
                let off = unsafe {
                    ffi::zip_source_seek_compute_offset(
                        self.in_offset as u64,
                        self.in_str.len() as u64,
                        data,
                        len,
                        &raw mut self.error,
                    )
                };
                if off < 0 {
                    return -1;
                }
                self.in_offset = usize::try_from(off).unwrap_or(usize::MAX);
                0
            }
            ffi::ZIP_SOURCE_TELL => to_i64(self.in_offset),
            ffi::ZIP_SOURCE_BEGIN_WRITE | ffi::ZIP_SOURCE_ROLLBACK_WRITE => {
                self.out_str = Vec::new();
                0
            }
            ffi::ZIP_SOURCE_WRITE => {
                if self.out_offset.checked_add(len_usize).is_none() {
                    self.set_error(crate::consts::er::MEMORY, 0);
                    return -1;
                }
                let end = self.out_offset + len_usize;
                if end > self.out_str.len() {
                    self.out_str.resize(end, 0);
                }
                if len_usize > 0 {
                    // SAFETY: libzip passes `len` readable bytes.
                    let src = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len_usize) };
                    self.out_str[self.out_offset..end].copy_from_slice(src);
                }
                self.out_offset = end;
                to_i64(len_usize)
            }
            ffi::ZIP_SOURCE_COMMIT_WRITE => {
                self.in_str = std::mem::take(&mut self.out_str);
                if let Some(dest) = &self.dest {
                    dest.borrow_mut().clone_from(&self.in_str);
                }
                0
            }
            ffi::ZIP_SOURCE_SEEK_WRITE => {
                // SAFETY: as for ZIP_SOURCE_SEEK.
                let off = unsafe {
                    ffi::zip_source_seek_compute_offset(
                        self.out_offset as u64,
                        self.out_str.len() as u64,
                        data,
                        len,
                        &raw mut self.error,
                    )
                };
                if off < 0 {
                    return -1;
                }
                self.out_offset = usize::try_from(off).unwrap_or(usize::MAX);
                0
            }
            ffi::ZIP_SOURCE_TELL_WRITE => to_i64(self.out_offset),
            ffi::ZIP_SOURCE_REMOVE => {
                self.in_str = Vec::new();
                self.in_offset = 0;
                0
            }
            _ => {
                self.set_error(crate::consts::er::OPNOTSUPP, 0);
                -1
            }
        }
    }
}

unsafe extern "C" fn string_cb(
    userdata: *mut c_void,
    data: *mut c_void,
    len: zip_uint64_t,
    cmd: zip_source_cmd_t,
) -> zip_int64_t {
    let ctx = userdata.cast::<StringSource>();
    if cmd == ffi::ZIP_SOURCE_FREE {
        // SAFETY: `userdata` is the Box leaked in create(); libzip sends
        // ZIP_SOURCE_FREE exactly once, after which it never uses it again.
        let mut b = unsafe { Box::from_raw(ctx) };
        // SAFETY: the error was initialised in create().
        unsafe { ffi::zip_error_fini(&raw mut b.error) };
        return 0;
    }
    // SAFETY: `userdata` is our live StringSource until ZIP_SOURCE_FREE;
    // libzip never invokes a source re-entrantly.
    let ctx = unsafe { &mut *ctx };
    // A panic must not unwind into C.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx.command(data, len, cmd)))
        .unwrap_or(-1)
}

/// Creates the source. With `dest`, `dest` receives the input string now
/// and the rewritten archive on every commit. On failure the libzip error
/// is returned as `(zip_err, sys_err)`.
pub(crate) fn create(input: &[u8], dest: Option<OutStr>) -> Result<*mut zip_source_t, (i32, i32)> {
    if let Some(d) = &dest {
        d.borrow_mut().clear();
        d.borrow_mut().extend_from_slice(input);
    }
    let mut error = zip_error_t {
        zip_err: 0,
        sys_err: 0,
        str_: ptr::null_mut(),
    };
    // SAFETY: initialises a stack error struct.
    unsafe { ffi::zip_error_init(&raw mut error) };
    let ctx = Box::new(StringSource {
        in_str: input.to_vec(),
        in_offset: 0,
        mtime: now(),
        out_str: Vec::new(),
        out_offset: 0,
        dest,
        error: zip_error_t {
            zip_err: 0,
            sys_err: 0,
            str_: ptr::null_mut(),
        },
    });
    let raw = Box::into_raw(ctx);
    // SAFETY: `raw` is a valid, exclusively owned StringSource.
    unsafe { ffi::zip_error_init(&raw mut (*raw).error) };
    // SAFETY: the callback/userdata pair stays valid until libzip sends
    // ZIP_SOURCE_FREE; `error` is a valid stack error struct.
    let src = unsafe { ffi::zip_source_function_create(string_cb, raw.cast(), &raw mut error) };
    if src.is_null() {
        // SAFETY: libzip did not take ownership, so reclaim the box.
        drop(unsafe { Box::from_raw(raw) });
        // SAFETY: reading the codes of the initialised stack error.
        let codes = unsafe {
            (
                ffi::zip_error_code_zip(&raw const error),
                ffi::zip_error_code_system(&raw const error),
            )
        };
        // SAFETY: finalises the stack error.
        unsafe { ffi::zip_error_fini(&raw mut error) };
        return Err(codes);
    }
    // SAFETY: finalises the stack error.
    unsafe { ffi::zip_error_fini(&raw mut error) };
    Ok(src)
}
