//! `ZipArchive::*` class constants, in `php_zip.stub.php` order, gated on
//! the libzip version like the `#ifdef`s of the stub (PHP built against
//! the system libzip exposes exactly these).

use std::ffi::CStr;

use crate::ffi;

/// `ZIP_CREATE`.
pub const CREATE: i64 = 1;
/// `ZIP_EXCL`.
pub const EXCL: i64 = 2;
/// `ZIP_CHECKCONS`.
pub const CHECKCONS: i64 = 4;
/// `ZIP_TRUNCATE` (`ZipArchive::OVERWRITE`).
pub const OVERWRITE: i64 = 8;
/// `ZIP_RDONLY`.
pub const RDONLY: i64 = 16;

/// `ZIP_FL_NOCASE`.
pub const FL_NOCASE: i64 = 1;
/// `ZIP_FL_NODIR`.
pub const FL_NODIR: i64 = 2;
/// `ZIP_FL_COMPRESSED`.
pub const FL_COMPRESSED: i64 = 4;
/// `ZIP_FL_UNCHANGED`.
pub const FL_UNCHANGED: i64 = 8;
/// `ZIP_FL_RECOMPRESS` (deprecated since PHP 8.3; removed from libzip 1.10).
pub const FL_RECOMPRESS: i64 = 16;
/// `ZIP_FL_ENCRYPTED`.
pub const FL_ENCRYPTED: i64 = 32;
/// `ZIP_FL_OVERWRITE`.
pub const FL_OVERWRITE: i64 = 8192;
/// `ZIP_FL_LOCAL`.
pub const FL_LOCAL: i64 = 256;
/// `ZIP_FL_CENTRAL`.
pub const FL_CENTRAL: i64 = 512;
/// `ZIP_FL_ENC_GUESS`.
pub const FL_ENC_GUESS: i64 = 0;
/// `ZIP_FL_ENC_RAW`.
pub const FL_ENC_RAW: i64 = 64;
/// `ZIP_FL_ENC_STRICT`.
pub const FL_ENC_STRICT: i64 = 128;
/// `ZIP_FL_ENC_UTF_8`.
pub const FL_ENC_UTF_8: i64 = 2048;
/// `ZIP_FL_ENC_CP437`.
pub const FL_ENC_CP437: i64 = 4096;
/// `ZIP_FL_OPEN_FILE_NOW` (a PHP flag, not libzip's).
pub const FL_OPEN_FILE_NOW: i64 = 1 << 30;

/// `ZIP_CM_DEFAULT`.
pub const CM_DEFAULT: i64 = -1;
/// `ZIP_CM_STORE`.
pub const CM_STORE: i64 = 0;
/// `ZIP_CM_DEFLATE`.
pub const CM_DEFLATE: i64 = 8;
/// `ZIP_CM_BZIP2`.
pub const CM_BZIP2: i64 = 12;
/// `ZIP_CM_LZMA`.
pub const CM_LZMA: i64 = 14;
/// `ZIP_CM_XZ`.
pub const CM_XZ: i64 = 95;
/// `ZIP_CM_ZSTD` (libzip >= 1.8).
pub const CM_ZSTD: i64 = 93;

/// `ZIP_EM_NONE`.
pub const EM_NONE: i64 = 0;
/// `ZIP_EM_TRAD_PKWARE`.
pub const EM_TRAD_PKWARE: i64 = 1;
/// `ZIP_EM_AES_128`.
pub const EM_AES_128: i64 = 0x0101;
/// `ZIP_EM_AES_192`.
pub const EM_AES_192: i64 = 0x0102;
/// `ZIP_EM_AES_256`.
pub const EM_AES_256: i64 = 0x0103;
/// `ZIP_EM_UNKNOWN`.
pub const EM_UNKNOWN: i64 = 0xffff;

/// `ZIP_AFL_RDONLY`.
pub const AFL_RDONLY: i64 = 2;

/// `ZIP_OPSYS_UNIX`.
pub const OPSYS_UNIX: i64 = 3;
/// `ZIP_OPSYS_DEFAULT`.
pub const OPSYS_DEFAULT: i64 = OPSYS_UNIX;

/// `ZIP_LENGTH_TO_END`: `-1` since libzip 1.10.1, `0` before (`php_zip.h`).
#[cfg(libzip_1_10)]
pub const LENGTH_TO_END: i64 = -1;
/// `ZIP_LENGTH_TO_END`: `-1` since libzip 1.10.1, `0` before (`php_zip.h`).
#[cfg(not(libzip_1_10))]
pub const LENGTH_TO_END: i64 = 0;

/// libzip error codes (`ZIP_ER_*`).
pub mod er {
    /// No error.
    pub const OK: i32 = 0;
    /// Multi-disk zip archives not supported.
    pub const MULTIDISK: i32 = 1;
    /// Renaming temporary file failed.
    pub const RENAME: i32 = 2;
    /// Closing zip archive failed.
    pub const CLOSE: i32 = 3;
    /// Seek error.
    pub const SEEK: i32 = 4;
    /// Read error.
    pub const READ: i32 = 5;
    /// Write error.
    pub const WRITE: i32 = 6;
    /// CRC error.
    pub const CRC: i32 = 7;
    /// Containing zip archive was closed.
    pub const ZIPCLOSED: i32 = 8;
    /// No such file.
    pub const NOENT: i32 = 9;
    /// File already exists.
    pub const EXISTS: i32 = 10;
    /// Can't open file.
    pub const OPEN: i32 = 11;
    /// Failure to create temporary file.
    pub const TMPOPEN: i32 = 12;
    /// Zlib error.
    pub const ZLIB: i32 = 13;
    /// Malloc failure.
    pub const MEMORY: i32 = 14;
    /// Entry has been changed.
    pub const CHANGED: i32 = 15;
    /// Compression method not supported.
    pub const COMPNOTSUPP: i32 = 16;
    /// Premature end of file.
    pub const EOF: i32 = 17;
    /// Invalid argument.
    pub const INVAL: i32 = 18;
    /// Not a zip archive.
    pub const NOZIP: i32 = 19;
    /// Internal error.
    pub const INTERNAL: i32 = 20;
    /// Zip archive inconsistent.
    pub const INCONS: i32 = 21;
    /// Can't remove file.
    pub const REMOVE: i32 = 22;
    /// Entry has been deleted.
    pub const DELETED: i32 = 23;
    /// Encryption method not supported.
    pub const ENCRNOTSUPP: i32 = 24;
    /// Read-only archive.
    pub const RDONLY: i32 = 25;
    /// No password provided.
    pub const NOPASSWD: i32 = 26;
    /// Wrong password provided.
    pub const WRONGPASSWD: i32 = 27;
    /// Operation not supported.
    pub const OPNOTSUPP: i32 = 28;
    /// Resource still in use.
    pub const INUSE: i32 = 29;
    /// Tell error.
    pub const TELL: i32 = 30;
    /// Compressed data invalid.
    pub const COMPRESSED_DATA: i32 = 31;
    /// Operation cancelled.
    pub const CANCELLED: i32 = 32;
}

/// A constant's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstValue {
    /// An `int` constant.
    Int(i64),
    /// A `string` constant.
    Str(&'static str),
}

/// One class constant of `ZipArchive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassConstant {
    /// The constant name.
    pub name: &'static str,
    /// Its value.
    pub value: ConstValue,
    /// `#[\Deprecated(since: …)]`, if any.
    pub deprecated_since: Option<&'static str>,
}

const fn int(name: &'static str, v: i64) -> ClassConstant {
    ClassConstant {
        name,
        value: ConstValue::Int(v),
        deprecated_since: None,
    }
}

/// The runtime libzip version string (`zip_libzip_version()`), which PHP
/// exposes as `ZipArchive::LIBZIP_VERSION`.
pub fn libzip_version() -> &'static str {
    // SAFETY: zip_libzip_version() returns a pointer to a static,
    // NUL-terminated string owned by the library.
    let s = unsafe { CStr::from_ptr(ffi::zip_libzip_version()) };
    s.to_str().unwrap_or(env!("PHP_ZIP_LIBZIP_VERSION"))
}

/// All `ZipArchive` constants in declaration order.
#[allow(clippy::too_many_lines)]
pub fn class_constants() -> Vec<ClassConstant> {
    let mut v = vec![
        int("CREATE", CREATE),
        int("EXCL", EXCL),
        int("CHECKCONS", CHECKCONS),
        int("OVERWRITE", OVERWRITE),
        int("RDONLY", RDONLY),
        int("FL_NOCASE", FL_NOCASE),
        int("FL_NODIR", FL_NODIR),
        int("FL_COMPRESSED", FL_COMPRESSED),
        int("FL_UNCHANGED", FL_UNCHANGED),
    ];
    #[cfg(not(libzip_1_10))]
    v.push(ClassConstant {
        name: "FL_RECOMPRESS",
        value: ConstValue::Int(FL_RECOMPRESS),
        deprecated_since: Some("8.3"),
    });
    v.extend([
        int("FL_ENCRYPTED", FL_ENCRYPTED),
        int("FL_OVERWRITE", FL_OVERWRITE),
        int("FL_LOCAL", FL_LOCAL),
        int("FL_CENTRAL", FL_CENTRAL),
        int("FL_ENC_GUESS", FL_ENC_GUESS),
        int("FL_ENC_RAW", FL_ENC_RAW),
        int("FL_ENC_STRICT", FL_ENC_STRICT),
        int("FL_ENC_UTF_8", FL_ENC_UTF_8),
        int("FL_ENC_CP437", FL_ENC_CP437),
        int("FL_OPEN_FILE_NOW", FL_OPEN_FILE_NOW),
        int("CM_DEFAULT", CM_DEFAULT),
        int("CM_STORE", CM_STORE),
        int("CM_SHRINK", 1),
        int("CM_REDUCE_1", 2),
        int("CM_REDUCE_2", 3),
        int("CM_REDUCE_3", 4),
        int("CM_REDUCE_4", 5),
        int("CM_IMPLODE", 6),
        int("CM_DEFLATE", CM_DEFLATE),
        int("CM_DEFLATE64", 9),
        int("CM_PKWARE_IMPLODE", 10),
        int("CM_BZIP2", CM_BZIP2),
        int("CM_LZMA", CM_LZMA),
        int("CM_LZMA2", 33),
    ]);
    #[cfg(libzip_1_8)]
    v.push(int("CM_ZSTD", CM_ZSTD));
    v.extend([
        int("CM_XZ", CM_XZ),
        int("CM_TERSE", 18),
        int("CM_LZ77", 19),
        int("CM_WAVPACK", 97),
        int("CM_PPMD", 98),
        int("ER_OK", 0),
        int("ER_MULTIDISK", 1),
        int("ER_RENAME", 2),
        int("ER_CLOSE", 3),
        int("ER_SEEK", 4),
        int("ER_READ", 5),
        int("ER_WRITE", 6),
        int("ER_CRC", 7),
        int("ER_ZIPCLOSED", 8),
        int("ER_NOENT", 9),
        int("ER_EXISTS", 10),
        int("ER_OPEN", 11),
        int("ER_TMPOPEN", 12),
        int("ER_ZLIB", 13),
        int("ER_MEMORY", 14),
        int("ER_CHANGED", 15),
        int("ER_COMPNOTSUPP", 16),
        int("ER_EOF", 17),
        int("ER_INVAL", 18),
        int("ER_NOZIP", 19),
        int("ER_INTERNAL", 20),
        int("ER_INCONS", 21),
        int("ER_REMOVE", 22),
        int("ER_DELETED", 23),
        int("ER_ENCRNOTSUPP", 24),
        int("ER_RDONLY", 25),
        int("ER_NOPASSWD", 26),
        int("ER_WRONGPASSWD", 27),
        int("ER_OPNOTSUPP", 28),
        int("ER_INUSE", 29),
        int("ER_TELL", 30),
        int("ER_COMPRESSED_DATA", 31),
        int("ER_CANCELLED", 32),
    ]);
    #[cfg(libzip_1_10)]
    v.extend([
        int("ER_DATA_LENGTH", 33),
        int("ER_NOT_ALLOWED", 34),
        int("ER_TRUNCATED_ZIP", 35),
    ]);
    v.push(int("AFL_RDONLY", AFL_RDONLY));
    #[cfg(libzip_1_10)]
    v.extend([
        int("AFL_IS_TORRENTZIP", 4),
        int("AFL_WANT_TORRENTZIP", 8),
        int("AFL_CREATE_OR_KEEP_FILE_FOR_EMPTY_ARCHIVE", 16),
    ]);
    v.extend([
        int("OPSYS_DOS", 0),
        int("OPSYS_AMIGA", 1),
        int("OPSYS_OPENVMS", 2),
        int("OPSYS_UNIX", 3),
        int("OPSYS_VM_CMS", 4),
        int("OPSYS_ATARI_ST", 5),
        int("OPSYS_OS_2", 6),
        int("OPSYS_MACINTOSH", 7),
        int("OPSYS_Z_SYSTEM", 8),
        int("OPSYS_CPM", 9),
        int("OPSYS_WINDOWS_NTFS", 10),
        int("OPSYS_MVS", 11),
        int("OPSYS_VSE", 12),
        int("OPSYS_ACORN_RISC", 13),
        int("OPSYS_VFAT", 14),
        int("OPSYS_ALTERNATE_MVS", 15),
        int("OPSYS_BEOS", 16),
        int("OPSYS_TANDEM", 17),
        int("OPSYS_OS_400", 18),
        int("OPSYS_OS_X", 19),
        int("OPSYS_DEFAULT", OPSYS_DEFAULT),
        int("EM_NONE", EM_NONE),
        int("EM_TRAD_PKWARE", EM_TRAD_PKWARE),
        int("EM_AES_128", EM_AES_128),
        int("EM_AES_192", EM_AES_192),
        int("EM_AES_256", EM_AES_256),
        int("EM_UNKNOWN", EM_UNKNOWN),
        ClassConstant {
            name: "LIBZIP_VERSION",
            value: ConstValue::Str(libzip_version()),
            deprecated_since: None,
        },
        int("LENGTH_TO_END", LENGTH_TO_END),
    ]);
    #[cfg(libzip_1_10)]
    v.push(int("LENGTH_UNCHECKED", -2));
    v
}

/// Looks up a constant by name.
pub fn class_constant(name: &str) -> Option<ClassConstant> {
    class_constants().into_iter().find(|c| c.name == name)
}

/// `ZipArchive::isCompressionMethodSupported()` minus the range check
/// (`zip_compression_method_supported()`).
pub fn compression_method_supported(method: i32, compress: bool) -> bool {
    // SAFETY: a pure query function without pointer arguments.
    unsafe { ffi::zip_compression_method_supported(method, i32::from(compress)) != 0 }
}

/// `zip_encryption_method_supported()`.
pub fn encryption_method_supported(method: u16, encode: bool) -> bool {
    // SAFETY: a pure query function without pointer arguments.
    unsafe { ffi::zip_encryption_method_supported(method, i32::from(encode)) != 0 }
}

/// The `phpinfo()` rows of the extension (`PHP_MINFO_FUNCTION(zip)`).
pub fn info_rows() -> Vec<(&'static str, String)> {
    let yes_no = |b: bool| if b { "Yes" } else { "No" }.to_string();
    let version = libzip_version();
    let mut rows = vec![("Zip", "enabled".to_string())];
    if version == env!("PHP_ZIP_LIBZIP_VERSION") {
        rows.push(("Libzip version", version.to_string()));
    } else {
        rows.push((
            "Libzip headers version",
            env!("PHP_ZIP_LIBZIP_VERSION").to_string(),
        ));
        rows.push(("Libzip library version", version.to_string()));
    }
    let cm = |m: i64| compression_method_supported(i32::try_from(m).unwrap_or(-2), true);
    let em = |m: i64| encryption_method_supported(u16::try_from(m).unwrap_or(0), true);
    rows.push(("BZIP2 compression", yes_no(cm(CM_BZIP2))));
    rows.push(("XZ compression", yes_no(cm(CM_XZ))));
    #[cfg(libzip_1_8)]
    rows.push(("ZSTD compression", yes_no(cm(CM_ZSTD))));
    #[cfg(not(libzip_1_8))]
    rows.push(("ZSTD compression", "No".to_string()));
    rows.push(("AES-128 encryption", yes_no(em(EM_AES_128))));
    rows.push(("AES-192 encryption", yes_no(em(EM_AES_192))));
    rows.push(("AES-256 encryption", yes_no(em(EM_AES_256))));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_shape() {
        let all = class_constants();
        assert_eq!(all[0].name, "CREATE");
        assert_eq!(
            class_constant("OVERWRITE").map(|c| c.value),
            Some(ConstValue::Int(8))
        );
        assert_eq!(
            class_constant("FL_OPEN_FILE_NOW").map(|c| c.value),
            Some(ConstValue::Int(1_073_741_824))
        );
        assert_eq!(
            class_constant("LIBZIP_VERSION").map(|c| c.value),
            Some(ConstValue::Str(libzip_version()))
        );
        let mut names: Vec<_> = all.iter().map(|c| c.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all.len());
    }

    #[test]
    fn support_queries() {
        assert!(compression_method_supported(0, true));
        assert!(compression_method_supported(8, false));
        assert!(!compression_method_supported(1, true));
        assert!(encryption_method_supported(0, true));
        assert_eq!(info_rows()[0], ("Zip", "enabled".to_string()));
    }
}
