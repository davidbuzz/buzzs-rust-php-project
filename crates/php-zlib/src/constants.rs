//! zlib constants (`zlib.stub.php`, `zlib.h`, `php_zlib.h`).

use crate::ffi;

/// `PHP_ZLIB_ENCODING_RAW` (`ZLIB_ENCODING_RAW`): raw deflate, window -15.
pub const ZLIB_ENCODING_RAW: i64 = -0xf;
/// `PHP_ZLIB_ENCODING_GZIP` (`ZLIB_ENCODING_GZIP`, `FORCE_GZIP`).
pub const ZLIB_ENCODING_GZIP: i64 = 0x1f;
/// `PHP_ZLIB_ENCODING_DEFLATE` (`ZLIB_ENCODING_DEFLATE`, `FORCE_DEFLATE`).
pub const ZLIB_ENCODING_DEFLATE: i64 = 0x0f;
/// `PHP_ZLIB_ENCODING_ANY`: gzip/zlib auto-detection, raw fallback (not a
/// userland constant; used by `zlib_decode()`).
pub const ZLIB_ENCODING_ANY: i64 = 0x2f;
/// `FORCE_GZIP`.
pub const FORCE_GZIP: i64 = ZLIB_ENCODING_GZIP;
/// `FORCE_DEFLATE`.
pub const FORCE_DEFLATE: i64 = ZLIB_ENCODING_DEFLATE;

/// `Z_NO_FLUSH`.
pub const ZLIB_NO_FLUSH: i64 = 0;
/// `Z_PARTIAL_FLUSH`.
pub const ZLIB_PARTIAL_FLUSH: i64 = 1;
/// `Z_SYNC_FLUSH`.
pub const ZLIB_SYNC_FLUSH: i64 = 2;
/// `Z_FULL_FLUSH`.
pub const ZLIB_FULL_FLUSH: i64 = 3;
/// `Z_FINISH`.
pub const ZLIB_FINISH: i64 = 4;
/// `Z_BLOCK`.
pub const ZLIB_BLOCK: i64 = 5;

/// `Z_FILTERED`.
pub const ZLIB_FILTERED: i64 = 1;
/// `Z_HUFFMAN_ONLY`.
pub const ZLIB_HUFFMAN_ONLY: i64 = 2;
/// `Z_RLE`.
pub const ZLIB_RLE: i64 = 3;
/// `Z_FIXED`.
pub const ZLIB_FIXED: i64 = 4;
/// `Z_DEFAULT_STRATEGY`.
pub const ZLIB_DEFAULT_STRATEGY: i64 = 0;

/// `Z_OK`.
pub const ZLIB_OK: i64 = 0;
/// `Z_STREAM_END`.
pub const ZLIB_STREAM_END: i64 = 1;
/// `Z_NEED_DICT`.
pub const ZLIB_NEED_DICT: i64 = 2;
/// `Z_ERRNO`.
pub const ZLIB_ERRNO: i64 = -1;
/// `Z_STREAM_ERROR`.
pub const ZLIB_STREAM_ERROR: i64 = -2;
/// `Z_DATA_ERROR`.
pub const ZLIB_DATA_ERROR: i64 = -3;
/// `Z_MEM_ERROR`.
pub const ZLIB_MEM_ERROR: i64 = -4;
/// `Z_BUF_ERROR`.
pub const ZLIB_BUF_ERROR: i64 = -5;
/// `Z_VERSION_ERROR`.
pub const ZLIB_VERSION_ERROR: i64 = -6;

/// `MAX_WBITS`.
pub const MAX_WBITS: i32 = 15;
/// `MAX_MEM_LEVEL`.
pub const MAX_MEM_LEVEL: i32 = 9;
/// `DEF_MEM_LEVEL` (used by gzFile streams).
pub const DEF_MEM_LEVEL: i32 = 8;
/// `Z_DEFAULT_COMPRESSION`.
pub const Z_DEFAULT_COMPRESSION: i32 = -1;

/// `ZLIB_VERSION`: the zlib version string. PHP reports the version of the
/// headers it was compiled against; we report the linked library, which is
/// the same system zlib.
pub fn zlib_version() -> &'static str {
    ffi::zlib_version()
}

/// `ZLIB_VERNUM`: `0xMmrs` computed from [`zlib_version`] (`"1.3"` →
/// `0x1300`, `"1.2.13"` → `0x12d0`).
pub fn zlib_vernum() -> i64 {
    vernum_of(zlib_version())
}

fn vernum_of(version: &str) -> i64 {
    let mut parts = version
        .split('.')
        .map(|p| {
            p.bytes()
                .take_while(u8::is_ascii_digit)
                .fold(0i64, |acc, b| acc * 10 + i64::from(b - b'0'))
        })
        .chain(std::iter::repeat(0));
    let mut n = 0;
    for _ in 0..4 {
        n = (n << 4) | (parts.next().unwrap_or(0) & 0xf);
    }
    n
}

/// A constant's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstValue {
    /// An `int` constant.
    Int(i64),
    /// A `string` constant.
    Str(&'static str),
}

/// Every constant ext/zlib registers, in `register_zlib_symbols()` order.
pub fn constants() -> Vec<(&'static str, ConstValue)> {
    use ConstValue::{Int, Str};
    vec![
        ("FORCE_GZIP", Int(FORCE_GZIP)),
        ("FORCE_DEFLATE", Int(FORCE_DEFLATE)),
        ("ZLIB_ENCODING_RAW", Int(ZLIB_ENCODING_RAW)),
        ("ZLIB_ENCODING_GZIP", Int(ZLIB_ENCODING_GZIP)),
        ("ZLIB_ENCODING_DEFLATE", Int(ZLIB_ENCODING_DEFLATE)),
        ("ZLIB_NO_FLUSH", Int(ZLIB_NO_FLUSH)),
        ("ZLIB_PARTIAL_FLUSH", Int(ZLIB_PARTIAL_FLUSH)),
        ("ZLIB_SYNC_FLUSH", Int(ZLIB_SYNC_FLUSH)),
        ("ZLIB_FULL_FLUSH", Int(ZLIB_FULL_FLUSH)),
        ("ZLIB_BLOCK", Int(ZLIB_BLOCK)),
        ("ZLIB_FINISH", Int(ZLIB_FINISH)),
        ("ZLIB_FILTERED", Int(ZLIB_FILTERED)),
        ("ZLIB_HUFFMAN_ONLY", Int(ZLIB_HUFFMAN_ONLY)),
        ("ZLIB_RLE", Int(ZLIB_RLE)),
        ("ZLIB_FIXED", Int(ZLIB_FIXED)),
        ("ZLIB_DEFAULT_STRATEGY", Int(ZLIB_DEFAULT_STRATEGY)),
        ("ZLIB_VERSION", Str(zlib_version())),
        ("ZLIB_VERNUM", Int(zlib_vernum())),
        ("ZLIB_OK", Int(ZLIB_OK)),
        ("ZLIB_STREAM_END", Int(ZLIB_STREAM_END)),
        ("ZLIB_NEED_DICT", Int(ZLIB_NEED_DICT)),
        ("ZLIB_ERRNO", Int(ZLIB_ERRNO)),
        ("ZLIB_STREAM_ERROR", Int(ZLIB_STREAM_ERROR)),
        ("ZLIB_DATA_ERROR", Int(ZLIB_DATA_ERROR)),
        ("ZLIB_MEM_ERROR", Int(ZLIB_MEM_ERROR)),
        ("ZLIB_BUF_ERROR", Int(ZLIB_BUF_ERROR)),
        ("ZLIB_VERSION_ERROR", Int(ZLIB_VERSION_ERROR)),
    ]
}

/// `zError()`: the message for a zlib return code (`z_errmsg[]`).
pub fn z_error(code: i32) -> &'static str {
    match code {
        2 => "need dictionary",
        1 => "stream end",
        -1 => "file error",
        -2 => "stream error",
        -3 => "data error",
        -4 => "insufficient memory",
        -5 => "buffer error",
        -6 => "incompatible version",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vernum() {
        assert_eq!(vernum_of("1.3"), 0x1300);
        assert_eq!(vernum_of("1.2.13"), 0x12d0);
        assert_eq!(vernum_of("1.3.0.1"), 0x1301);
        assert_eq!(vernum_of("1.2.11.1-motley"), 0x12b1);
        assert!(zlib_vernum() >= 0x1240);
    }

    #[test]
    fn errors() {
        assert_eq!(z_error(-3), "data error");
        assert_eq!(z_error(-4), "insufficient memory");
        assert_eq!(z_error(-5), "buffer error");
        assert_eq!(z_error(0), "");
        assert_eq!(z_error(2), "need dictionary");
    }

    #[test]
    fn table() {
        let c = constants();
        assert_eq!(c.len(), 27);
        assert_eq!(c[0], ("FORCE_GZIP", ConstValue::Int(31)));
        assert_eq!(c[2], ("ZLIB_ENCODING_RAW", ConstValue::Int(-15)));
    }
}
