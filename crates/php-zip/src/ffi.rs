//! Raw declarations for the subset of the libzip C API this crate uses
//! (`<zip.h>` of libzip 1.7.3; newer entry points are gated on the version
//! found by `build.rs`).
//!
//! Everything here is `unsafe` to call; the safe wrappers live in the other
//! modules, each call site carrying a `// SAFETY:` justification.

#![allow(non_camel_case_types, dead_code)]

use libc::{c_char, c_double, c_int, c_void, time_t, FILE};

/// `zip_int8_t`.
pub type zip_int8_t = i8;
/// `zip_uint8_t`.
pub type zip_uint8_t = u8;
/// `zip_int16_t`.
pub type zip_int16_t = i16;
/// `zip_uint16_t`.
pub type zip_uint16_t = u16;
/// `zip_int32_t`.
pub type zip_int32_t = i32;
/// `zip_uint32_t`.
pub type zip_uint32_t = u32;
/// `zip_int64_t`.
pub type zip_int64_t = i64;
/// `zip_uint64_t`.
pub type zip_uint64_t = u64;
/// `zip_flags_t`.
pub type zip_flags_t = u32;

/// `zip_t` (opaque).
#[repr(C)]
pub struct zip_t {
    _private: [u8; 0],
}

/// `zip_file_t` (opaque).
#[repr(C)]
pub struct zip_file_t {
    _private: [u8; 0],
}

/// `zip_source_t` (opaque).
#[repr(C)]
pub struct zip_source_t {
    _private: [u8; 0],
}

/// `struct zip_error`.
#[repr(C)]
#[derive(Debug)]
pub struct zip_error_t {
    /// libzip error code (`ZIP_ER_*`).
    pub zip_err: c_int,
    /// errno or zlib error code.
    pub sys_err: c_int,
    /// Cached string representation (owned by libzip).
    pub str_: *mut c_char,
}

/// `struct zip_stat`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct zip_stat_t {
    pub valid: zip_uint64_t,
    pub name: *const c_char,
    pub index: zip_uint64_t,
    pub size: zip_uint64_t,
    pub comp_size: zip_uint64_t,
    pub mtime: time_t,
    pub crc: zip_uint32_t,
    pub comp_method: zip_uint16_t,
    pub encryption_method: zip_uint16_t,
    pub flags: zip_uint32_t,
}

/// `zip_source_cmd_t`.
pub type zip_source_cmd_t = c_int;

pub const ZIP_SOURCE_OPEN: zip_source_cmd_t = 0;
pub const ZIP_SOURCE_READ: zip_source_cmd_t = 1;
pub const ZIP_SOURCE_CLOSE: zip_source_cmd_t = 2;
pub const ZIP_SOURCE_STAT: zip_source_cmd_t = 3;
pub const ZIP_SOURCE_ERROR: zip_source_cmd_t = 4;
pub const ZIP_SOURCE_FREE: zip_source_cmd_t = 5;
pub const ZIP_SOURCE_SEEK: zip_source_cmd_t = 6;
pub const ZIP_SOURCE_TELL: zip_source_cmd_t = 7;
pub const ZIP_SOURCE_BEGIN_WRITE: zip_source_cmd_t = 8;
pub const ZIP_SOURCE_COMMIT_WRITE: zip_source_cmd_t = 9;
pub const ZIP_SOURCE_ROLLBACK_WRITE: zip_source_cmd_t = 10;
pub const ZIP_SOURCE_WRITE: zip_source_cmd_t = 11;
pub const ZIP_SOURCE_SEEK_WRITE: zip_source_cmd_t = 12;
pub const ZIP_SOURCE_TELL_WRITE: zip_source_cmd_t = 13;
pub const ZIP_SOURCE_SUPPORTS: zip_source_cmd_t = 14;
pub const ZIP_SOURCE_REMOVE: zip_source_cmd_t = 15;
pub const ZIP_SOURCE_SUPPORTS_REOPEN: zip_source_cmd_t = 21;

/// `ZIP_RDONLY` for `zip_open()`.
pub const ZIP_RDONLY: c_int = 16;
/// `ZIP_FL_NOCASE`.
pub const ZIP_FL_NOCASE: zip_flags_t = 1;

pub const ZIP_STAT_SIZE: zip_uint64_t = 0x0004;
pub const ZIP_STAT_COMP_SIZE: zip_uint64_t = 0x0008;
pub const ZIP_STAT_MTIME: zip_uint64_t = 0x0010;
pub const ZIP_STAT_COMP_METHOD: zip_uint64_t = 0x0040;
pub const ZIP_STAT_ENCRYPTION_METHOD: zip_uint64_t = 0x0080;

/// `zip_source_callback`.
pub type zip_source_callback =
    unsafe extern "C" fn(*mut c_void, *mut c_void, zip_uint64_t, zip_source_cmd_t) -> zip_int64_t;
/// `zip_progress_callback`.
pub type zip_progress_callback = unsafe extern "C" fn(*mut zip_t, c_double, *mut c_void);
/// `zip_cancel_callback`.
pub type zip_cancel_callback = unsafe extern "C" fn(*mut zip_t, *mut c_void) -> c_int;
/// State destructor of the `*_with_state` callback registrations.
pub type zip_state_free = unsafe extern "C" fn(*mut c_void);

// libzip is bound lazily (see `build.rs`): the generated stubs define the
// functions below and load the library on the first call, keeping it out of every
// start-up.
include!(concat!(env!("OUT_DIR"), "/lazy_zip.rs"));

extern "C" {
    pub fn zip_libzip_version() -> *const c_char;

    pub fn zip_open(path: *const c_char, flags: c_int, errorp: *mut c_int) -> *mut zip_t;
    pub fn zip_open_from_source(
        src: *mut zip_source_t,
        flags: c_int,
        error: *mut zip_error_t,
    ) -> *mut zip_t;
    pub fn zip_close(za: *mut zip_t) -> c_int;
    pub fn zip_discard(za: *mut zip_t);

    pub fn zip_get_error(za: *mut zip_t) -> *mut zip_error_t;
    pub fn zip_error_clear(za: *mut zip_t);
    pub fn zip_error_code_zip(err: *const zip_error_t) -> c_int;
    pub fn zip_error_code_system(err: *const zip_error_t) -> c_int;
    pub fn zip_error_fini(err: *mut zip_error_t);
    pub fn zip_error_init(err: *mut zip_error_t);
    pub fn zip_error_set(err: *mut zip_error_t, ze: c_int, se: c_int);
    pub fn zip_error_strerror(err: *mut zip_error_t) -> *const c_char;
    pub fn zip_error_to_data(
        err: *const zip_error_t,
        data: *mut c_void,
        len: zip_uint64_t,
    ) -> zip_int64_t;
    pub fn zip_strerror(za: *mut zip_t) -> *const c_char;

    pub fn zip_get_num_entries(za: *mut zip_t, flags: zip_flags_t) -> zip_int64_t;
    pub fn zip_get_name(za: *mut zip_t, index: zip_uint64_t, flags: zip_flags_t) -> *const c_char;
    pub fn zip_name_locate(za: *mut zip_t, name: *const c_char, flags: zip_flags_t) -> zip_int64_t;
    pub fn zip_stat(
        za: *mut zip_t,
        name: *const c_char,
        flags: zip_flags_t,
        sb: *mut zip_stat_t,
    ) -> c_int;
    pub fn zip_stat_index(
        za: *mut zip_t,
        index: zip_uint64_t,
        flags: zip_flags_t,
        sb: *mut zip_stat_t,
    ) -> c_int;
    pub fn zip_stat_init(sb: *mut zip_stat_t);

    pub fn zip_fopen(za: *mut zip_t, name: *const c_char, flags: zip_flags_t) -> *mut zip_file_t;
    pub fn zip_fopen_index(
        za: *mut zip_t,
        index: zip_uint64_t,
        flags: zip_flags_t,
    ) -> *mut zip_file_t;
    pub fn zip_fread(zf: *mut zip_file_t, buf: *mut c_void, nbytes: zip_uint64_t) -> zip_int64_t;
    pub fn zip_fclose(zf: *mut zip_file_t) -> c_int;
    pub fn zip_file_get_error(zf: *mut zip_file_t) -> *mut zip_error_t;
    pub fn zip_fseek(zf: *mut zip_file_t, offset: zip_int64_t, whence: c_int) -> zip_int8_t;
    pub fn zip_ftell(zf: *mut zip_file_t) -> zip_int64_t;
    #[cfg(libzip_1_9_1)]
    pub fn zip_file_is_seekable(zf: *mut zip_file_t) -> c_int;

    pub fn zip_set_default_password(za: *mut zip_t, password: *const c_char) -> c_int;

    pub fn zip_dir_add(za: *mut zip_t, name: *const c_char, flags: zip_flags_t) -> zip_int64_t;
    pub fn zip_file_add(
        za: *mut zip_t,
        name: *const c_char,
        source: *mut zip_source_t,
        flags: zip_flags_t,
    ) -> zip_int64_t;
    pub fn zip_file_replace(
        za: *mut zip_t,
        index: zip_uint64_t,
        source: *mut zip_source_t,
        flags: zip_flags_t,
    ) -> c_int;
    pub fn zip_delete(za: *mut zip_t, index: zip_uint64_t) -> c_int;
    pub fn zip_file_rename(
        za: *mut zip_t,
        index: zip_uint64_t,
        name: *const c_char,
        flags: zip_flags_t,
    ) -> c_int;
    pub fn zip_unchange(za: *mut zip_t, index: zip_uint64_t) -> c_int;
    pub fn zip_unchange_all(za: *mut zip_t) -> c_int;
    pub fn zip_unchange_archive(za: *mut zip_t) -> c_int;

    pub fn zip_get_archive_comment(
        za: *mut zip_t,
        lenp: *mut c_int,
        flags: zip_flags_t,
    ) -> *const c_char;
    pub fn zip_set_archive_comment(
        za: *mut zip_t,
        comment: *const c_char,
        len: zip_uint16_t,
    ) -> c_int;
    pub fn zip_get_archive_flag(za: *mut zip_t, flag: zip_flags_t, flags: zip_flags_t) -> c_int;
    pub fn zip_set_archive_flag(za: *mut zip_t, flag: zip_flags_t, value: c_int) -> c_int;
    pub fn zip_file_get_comment(
        za: *mut zip_t,
        index: zip_uint64_t,
        lenp: *mut zip_uint32_t,
        flags: zip_flags_t,
    ) -> *const c_char;
    pub fn zip_file_set_comment(
        za: *mut zip_t,
        index: zip_uint64_t,
        comment: *const c_char,
        len: zip_uint16_t,
        flags: zip_flags_t,
    ) -> c_int;
    pub fn zip_file_set_mtime(
        za: *mut zip_t,
        index: zip_uint64_t,
        mtime: time_t,
        flags: zip_flags_t,
    ) -> c_int;
    pub fn zip_file_get_external_attributes(
        za: *mut zip_t,
        index: zip_uint64_t,
        flags: zip_flags_t,
        opsys: *mut zip_uint8_t,
        attributes: *mut zip_uint32_t,
    ) -> c_int;
    pub fn zip_file_set_external_attributes(
        za: *mut zip_t,
        index: zip_uint64_t,
        flags: zip_flags_t,
        opsys: zip_uint8_t,
        attributes: zip_uint32_t,
    ) -> c_int;
    pub fn zip_set_file_compression(
        za: *mut zip_t,
        index: zip_uint64_t,
        comp: zip_int32_t,
        comp_flags: zip_uint32_t,
    ) -> c_int;
    pub fn zip_file_set_encryption(
        za: *mut zip_t,
        index: zip_uint64_t,
        method: zip_uint16_t,
        password: *const c_char,
    ) -> c_int;

    pub fn zip_register_progress_callback_with_state(
        za: *mut zip_t,
        precision: c_double,
        callback: Option<zip_progress_callback>,
        ud_free: Option<zip_state_free>,
        ud: *mut c_void,
    ) -> c_int;
    pub fn zip_register_cancel_callback_with_state(
        za: *mut zip_t,
        callback: Option<zip_cancel_callback>,
        ud_free: Option<zip_state_free>,
        ud: *mut c_void,
    ) -> c_int;

    pub fn zip_compression_method_supported(method: zip_int32_t, compress: c_int) -> c_int;
    pub fn zip_encryption_method_supported(method: zip_uint16_t, encode: c_int) -> c_int;

    pub fn zip_source_file(
        za: *mut zip_t,
        fname: *const c_char,
        start: zip_uint64_t,
        len: zip_int64_t,
    ) -> *mut zip_source_t;
    pub fn zip_source_filep(
        za: *mut zip_t,
        file: *mut FILE,
        start: zip_uint64_t,
        len: zip_int64_t,
    ) -> *mut zip_source_t;
    pub fn zip_source_function_create(
        callback: zip_source_callback,
        userdata: *mut c_void,
        error: *mut zip_error_t,
    ) -> *mut zip_source_t;
    pub fn zip_source_free(src: *mut zip_source_t);
    pub fn zip_source_seek_compute_offset(
        offset: zip_uint64_t,
        length: zip_uint64_t,
        data: *mut c_void,
        data_length: zip_uint64_t,
        error: *mut zip_error_t,
    ) -> zip_int64_t;
}
