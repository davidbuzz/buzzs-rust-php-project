// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Constants from `phar_internal.h` and the `Phar::*` class constants.

/// `PHP_PHAR_API_VERSION`.
pub const API_VERSION_STR: &str = "1.1.1";
/// `PHAR_API_VERSION` (x.y.z maps to 0xyz0).
pub const API_VERSION: u16 = 0x1110;
/// `PHAR_API_VERSION_NODIR`: written when the archive has no directories.
pub const API_VERSION_NODIR: u16 = 0x1100;
/// `PHAR_API_MIN_DIR`: first version with directory entries.
pub const API_MIN_DIR: u16 = 0x1110;
/// `PHAR_API_MIN_READ`.
pub const API_MIN_READ: u16 = 0x1000;
/// `PHAR_API_VER_MASK`.
pub const API_VER_MASK: u16 = 0xFFF0;

/// `PHAR_HDR_SIGNATURE`: the archive carries a signature.
pub const HDR_SIGNATURE: u32 = 0x0001_0000;

/// `PHAR_FILE_COMPRESSION_MASK` (whole-archive compression).
pub const FILE_COMPRESSION_MASK: u32 = 0x00F0_0000;
/// `PHAR_FILE_COMPRESSED_NONE`.
pub const FILE_COMPRESSED_NONE: u32 = 0;
/// `PHAR_FILE_COMPRESSED_GZ`.
pub const FILE_COMPRESSED_GZ: u32 = 0x0010_0000;
/// `PHAR_FILE_COMPRESSED_BZ2`.
pub const FILE_COMPRESSED_BZ2: u32 = 0x0020_0000;

/// `PHAR_SIG_MD5`.
pub const SIG_MD5: u32 = 0x0001;
/// `PHAR_SIG_SHA1`.
pub const SIG_SHA1: u32 = 0x0002;
/// `PHAR_SIG_SHA256`.
pub const SIG_SHA256: u32 = 0x0003;
/// `PHAR_SIG_SHA512`.
pub const SIG_SHA512: u32 = 0x0004;
/// `PHAR_SIG_OPENSSL`.
pub const SIG_OPENSSL: u32 = 0x0010;
/// `PHAR_SIG_OPENSSL_SHA256`.
pub const SIG_OPENSSL_SHA256: u32 = 0x0011;
/// `PHAR_SIG_OPENSSL_SHA512`.
pub const SIG_OPENSSL_SHA512: u32 = 0x0012;

/// `PHAR_ENT_COMPRESSION_MASK` (per-entry compression).
pub const ENT_COMPRESSION_MASK: u32 = 0x0000_F000;
/// `PHAR_ENT_COMPRESSED_NONE`.
pub const ENT_COMPRESSED_NONE: u32 = 0;
/// `PHAR_ENT_COMPRESSED_GZ`.
pub const ENT_COMPRESSED_GZ: u32 = 0x0000_1000;
/// `PHAR_ENT_COMPRESSED_BZ2`.
pub const ENT_COMPRESSED_BZ2: u32 = 0x0000_2000;
/// `PHAR_ENT_PERM_MASK`.
pub const ENT_PERM_MASK: u32 = 0x0000_01FF;
/// `PHAR_ENT_PERM_DEF_FILE` (0666).
pub const ENT_PERM_DEF_FILE: u32 = 0x0000_01B6;
/// `PHAR_ENT_PERM_DEF_DIR` (0777).
pub const ENT_PERM_DEF_DIR: u32 = 0x0000_01FF;

/// `PHAR_FORMAT_SAME`.
pub const FORMAT_SAME: i64 = 0;
/// `PHAR_FORMAT_PHAR`.
pub const FORMAT_PHAR: i64 = 1;
/// `PHAR_FORMAT_TAR`.
pub const FORMAT_TAR: i64 = 2;
/// `PHAR_FORMAT_ZIP`.
pub const FORMAT_ZIP: i64 = 3;

/// `TAR_FILE`.
pub const TAR_FILE: u8 = b'0';
/// `TAR_LINK`.
pub const TAR_LINK: u8 = b'1';
/// `TAR_SYMLINK`.
pub const TAR_SYMLINK: u8 = b'2';
/// `TAR_CHAR`.
pub const TAR_CHAR: u8 = b'3';
/// `TAR_BLOCK`.
pub const TAR_BLOCK: u8 = b'4';
/// `TAR_DIR`.
pub const TAR_DIR: u8 = b'5';
/// `TAR_FIFO`.
pub const TAR_FIFO: u8 = b'6';
/// `TAR_LONGLINK` (GNU `K`).
pub const TAR_LONGLINK: u8 = b'K';
/// `TAR_LONGNAME` (GNU `L`, `././@LongLink`).
pub const TAR_LONGNAME: u8 = b'L';
/// `TAR_GLOBAL_HDR` (pax `g`).
pub const TAR_GLOBAL_HDR: u8 = b'g';
/// `TAR_FILE_HDR` (pax `x`).
pub const TAR_FILE_HDR: u8 = b'x';

/// `PHAR_MUNG_PHP_SELF`.
pub const MUNG_PHP_SELF: u32 = 1 << 0;
/// `PHAR_MUNG_REQUEST_URI`.
pub const MUNG_REQUEST_URI: u32 = 1 << 1;
/// `PHAR_MUNG_SCRIPT_NAME`.
pub const MUNG_SCRIPT_NAME: u32 = 1 << 2;
/// `PHAR_MUNG_SCRIPT_FILENAME`.
pub const MUNG_SCRIPT_FILENAME: u32 = 1 << 3;

/// `PHAR_MIME_PHP` (`Phar::PHP`).
pub const MIME_PHP: i64 = 0;
/// `PHAR_MIME_PHPS` (`Phar::PHPS`).
pub const MIME_PHPS: i64 = 1;
/// `PHAR_MIME_OTHER`.
pub const MIME_OTHER: i64 = 2;

/// `SPL_FILE_DIR_SKIPDOTS | SPL_FILE_DIR_UNIXPATHS`, the default `$flags` of
/// `Phar::__construct()`.
pub const DEFAULT_CONSTRUCT_FLAGS: i64 = 0x0000_1000 | 0x0000_2000;

/// The `Phar` / `PharData` class constants (`phar_object.stub.php`), in
/// declaration order.
pub const CLASS_CONSTANTS: &[(&str, i64)] = &[
    ("BZ2", ENT_COMPRESSED_BZ2 as i64),
    ("GZ", ENT_COMPRESSED_GZ as i64),
    ("NONE", ENT_COMPRESSED_NONE as i64),
    ("PHAR", FORMAT_PHAR),
    ("TAR", FORMAT_TAR),
    ("ZIP", FORMAT_ZIP),
    ("COMPRESSED", ENT_COMPRESSION_MASK as i64),
    ("PHP", MIME_PHP),
    ("PHPS", MIME_PHPS),
    ("MD5", SIG_MD5 as i64),
    ("OPENSSL", SIG_OPENSSL as i64),
    ("OPENSSL_SHA256", SIG_OPENSSL_SHA256 as i64),
    ("OPENSSL_SHA512", SIG_OPENSSL_SHA512 as i64),
    ("SHA1", SIG_SHA1 as i64),
    ("SHA256", SIG_SHA256 as i64),
    ("SHA512", SIG_SHA512 as i64),
];

/// The magic stub written into new tar-based phars.
pub const TAR_NEWSTUB: &[u8] = b"<?php // tar-based phar archive stub file\n__HALT_COMPILER();";
/// The magic stub written into new zip-based phars.
pub const ZIP_NEWSTUB: &[u8] = b"<?php // zip-based phar archive stub file\n__HALT_COMPILER();";
/// The token terminating a stub.
pub const HALT_TOKEN: &[u8] = b"__HALT_COMPILER();";
/// What `setStub()`/flush append after the halt token.
pub const STUB_END_SEQUENCE: &[u8] = b" ?>\r\n";
