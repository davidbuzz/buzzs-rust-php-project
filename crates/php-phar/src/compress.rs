// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Compression codecs exactly as phar applies them: the `zlib.deflate` /
//! `zlib.inflate` stream filters (raw deflate per entry, gzip for whole
//! archives) and `bzip2.compress` / `bzip2.decompress`.

use crate::bz2;

/// Raw deflate as written by `zlib.deflate` without parameters (level -1,
/// window -15, `MAX_MEM_LEVEL`, default strategy, `Z_FINISH`).
pub fn deflate_raw(data: &[u8]) -> Option<Vec<u8>> {
    php_zlib::codec::encode_raw(data, -15, -1).ok()
}

/// Raw inflate (`zlib.inflate` without parameters).
pub fn inflate_raw(data: &[u8]) -> Option<Vec<u8>> {
    php_zlib::codec::decode_raw(data, -15, 0).ok()
}

/// Gzip as written for whole-archive `.gz` compression (`window` 31).
pub fn gzip(data: &[u8]) -> Option<Vec<u8>> {
    php_zlib::codec::encode_raw(data, 31, -1).ok()
}

/// Inflate with gzip/zlib auto-detection (`window` 47).
pub fn gunzip(data: &[u8]) -> Option<Vec<u8>> {
    php_zlib::codec::decode_raw(data, 47, 0).ok()
}

/// bzip2 with the filter defaults.
pub fn bzip2(data: &[u8]) -> Option<Vec<u8>> {
    bz2::compress(data)
}

/// bunzip2; `None` when the data is corrupt.
pub fn bunzip2(data: &[u8]) -> Option<Vec<u8>> {
    let (out, ok) = bz2::decompress(data);
    ok.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips() {
        let data = b"phar phar phar phar".repeat(20);
        assert_eq!(inflate_raw(&deflate_raw(&data).unwrap()).unwrap(), data);
        let gz = gzip(&data).unwrap();
        assert_eq!(&gz[..3], b"\x1f\x8b\x08");
        assert_eq!(gunzip(&gz).unwrap(), data);
        assert_eq!(bunzip2(&bzip2(&data).unwrap()).unwrap(), data);
    }
}
