// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `sodium_memzero/increment/add/memcmp/compare`, hex/base64 codecs and
//! ISO/IEC 7816-4 padding.

#![allow(unsafe_code)]

use std::ffi::c_char;

use crate::error::{arg, exc, overflow, Error, Result};
use crate::{c_string, ffi, init, ZSTR_MAX_LEN};

/// `sodium_memzero(&$string)`: wipes the buffer.  PHP then sets the
/// variable to `null` (and skips the wipe for shared/interned strings).
pub fn memzero(buf: &mut Vec<u8>) {
    if !buf.is_empty() {
        // SAFETY: `buf` is valid for `len` writable bytes.
        unsafe { ffi::sodium_memzero(buf.as_mut_ptr(), buf.len()) };
    }
}

/// `sodium_increment(&$string)`: little-endian, constant-time `+1`.
pub fn increment(buf: &mut [u8]) {
    init();
    // SAFETY: `buf` is valid for `len` writable bytes.
    unsafe { ffi::sodium_increment(buf.as_mut_ptr(), buf.len()) };
}

fn same_length(func: &str) -> Error {
    arg(
        func,
        1,
        "string1",
        "and argument #2 ($string2) must have the same length",
    )
}

/// `sodium_add(&$string1, $string2)`: little-endian addition modulo
/// 2^(8·len).
pub fn add(a: &mut [u8], b: &[u8]) -> Result<()> {
    init();
    if a.len() != b.len() {
        return Err(same_length("sodium_add"));
    }
    // SAFETY: both buffers hold `a.len()` bytes.
    unsafe { ffi::sodium_add(a.as_mut_ptr(), b.as_ptr(), a.len()) };
    Ok(())
}

/// `sodium_memcmp()`: 0 when equal, -1 otherwise (constant time).
pub fn memcmp(a: &[u8], b: &[u8]) -> Result<i64> {
    init();
    if a.len() != b.len() {
        return Err(same_length("sodium_memcmp"));
    }
    // SAFETY: both buffers hold `a.len()` bytes.
    Ok(i64::from(unsafe {
        ffi::sodium_memcmp(a.as_ptr(), b.as_ptr(), a.len())
    }))
}

/// `sodium_compare()`: -1/0/1 comparing little-endian numbers.
pub fn compare(a: &[u8], b: &[u8]) -> Result<i64> {
    init();
    if a.len() != b.len() {
        return Err(same_length("sodium_compare"));
    }
    // SAFETY: both buffers hold `a.len()` bytes.
    Ok(i64::from(unsafe {
        ffi::sodium_compare(a.as_ptr(), b.as_ptr(), a.len())
    }))
}

/// `sodium_bin2hex()` (lowercase, constant time).
pub fn bin2hex(bin: &[u8]) -> Result<Vec<u8>> {
    init();
    if bin.len() >= ZSTR_MAX_LEN / 2 {
        return Err(overflow());
    }
    let mut hex = vec![0u8; bin.len() * 2 + 1];
    // SAFETY: `hex` has room for 2·len digits plus the NUL.
    unsafe {
        ffi::sodium_bin2hex(
            hex.as_mut_ptr().cast::<c_char>(),
            hex.len(),
            bin.as_ptr(),
            bin.len(),
        );
    }
    hex.pop();
    Ok(hex)
}

/// `sodium_hex2bin($string, $ignore)`: `$ignore` lists characters skipped
/// between digit pairs (as a C string: cut at the first NUL).
pub fn hex2bin(hex: &[u8], ignore: &[u8]) -> Result<Vec<u8>> {
    init();
    let mut bin = vec![0u8; hex.len() / 2];
    let ignore = c_string(ignore);
    let mut real_len = 0usize;
    let mut end: *const c_char = std::ptr::null();
    // SAFETY: `bin` has `hex.len() / 2` writable bytes, `hex` is readable
    // for its length, `ignore` is NUL-terminated, the out-pointers are live.
    let rc = unsafe {
        ffi::sodium_hex2bin(
            bin.as_mut_ptr(),
            bin.len(),
            hex.as_ptr().cast::<c_char>(),
            hex.len(),
            ignore.as_ptr().cast::<c_char>(),
            &raw mut real_len,
            &raw mut end,
        )
    };
    let consumed = end as usize - hex.as_ptr() as usize;
    if rc != 0 || consumed != hex.len() {
        return Err(arg(
            "sodium_hex2bin",
            1,
            "string",
            "must be a valid hexadecimal string",
        ));
    }
    if real_len > bin.len() {
        return Err(overflow());
    }
    bin.truncate(real_len);
    Ok(bin)
}

/// Validates a `SODIUM_BASE64_VARIANT_*` identifier (argument #2).
fn check_variant(func: &str, variant: i64) -> Result<i32> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let v = variant as u32;
    if v & !0x6 != 0x1 {
        return Err(arg(
            func,
            2,
            "id",
            "must be a valid base64 variant identifier",
        ));
    }
    #[allow(clippy::cast_possible_wrap)]
    Ok(v as i32)
}

/// `sodium_bin2base64($string, $id)`.
pub fn bin2base64(bin: &[u8], variant: i64) -> Result<Vec<u8>> {
    init();
    let v = check_variant("sodium_bin2base64", variant)?;
    if bin.len() >= ZSTR_MAX_LEN / 4 * 3 - 3 - 1 {
        return Err(overflow());
    }
    // SAFETY: pure computation.
    let encoded_len = unsafe { ffi::sodium_base64_encoded_len(bin.len(), v) };
    let mut out = vec![0u8; encoded_len];
    // SAFETY: `out` has the size libsodium asked for (including the NUL).
    unsafe {
        ffi::sodium_bin2base64(
            out.as_mut_ptr().cast::<c_char>(),
            out.len(),
            bin.as_ptr(),
            bin.len(),
            v,
        );
    }
    out.pop();
    Ok(out)
}

/// `sodium_base642bin($string, $id, $ignore)`.
pub fn base642bin(b64: &[u8], variant: i64, ignore: &[u8]) -> Result<Vec<u8>> {
    init();
    let v = check_variant("sodium_base642bin", variant)?;
    let mut bin = vec![0u8; b64.len() / 4 * 3 + 2];
    let ignore = c_string(ignore);
    let mut real_len = 0usize;
    let mut end: *const c_char = std::ptr::null();
    // SAFETY: as for `hex2bin`.
    let rc = unsafe {
        ffi::sodium_base642bin(
            bin.as_mut_ptr(),
            bin.len(),
            b64.as_ptr().cast::<c_char>(),
            b64.len(),
            ignore.as_ptr().cast::<c_char>(),
            &raw mut real_len,
            &raw mut end,
            v,
        )
    };
    let consumed = end as usize - b64.as_ptr() as usize;
    if rc != 0 || consumed != b64.len() {
        return Err(arg(
            "sodium_base642bin",
            1,
            "string",
            "must be a valid base64 string",
        ));
    }
    if real_len > bin.len() {
        return Err(overflow());
    }
    bin.truncate(real_len);
    Ok(bin)
}

fn check_block_size(func: &str, block_size: i64) -> Result<usize> {
    if block_size <= 0 {
        return Err(arg(func, 2, "block_size", "must be greater than 0"));
    }
    usize::try_from(block_size).map_err(|_| arg(func, 2, "block_size", "is too large"))
}

/// `sodium_pad($string, $block_size)`: ISO/IEC 7816-4 padding to a
/// multiple of `block_size` (always adding at least one byte).
pub fn pad(data: &[u8], block_size: i64) -> Result<Vec<u8>> {
    init();
    let block = check_block_size("sodium_pad", block_size)?;
    let mut xpadlen = block - 1;
    if block.is_power_of_two() {
        xpadlen -= data.len() & (block - 1);
    } else {
        xpadlen -= data.len() % block;
    }
    if usize::MAX - data.len() <= xpadlen {
        return Err(exc("input is too large"));
    }
    let xpadded_len = data.len() + xpadlen;
    let mut padded = vec![0u8; xpadded_len + 1];
    padded[..data.len()].copy_from_slice(data);
    let mut out_len = 0usize;
    // SAFETY: `padded` holds `xpadded_len + 1` bytes, the maximum passed.
    let rc = unsafe {
        ffi::sodium_pad(
            &raw mut out_len,
            padded.as_mut_ptr(),
            data.len(),
            block,
            padded.len(),
        )
    };
    if rc != 0 {
        return Err(crate::error::internal());
    }
    Ok(padded)
}

/// `sodium_unpad($string, $block_size)`.
pub fn unpad(padded: &[u8], block_size: i64) -> Result<Vec<u8>> {
    init();
    let block = check_block_size("sodium_unpad", block_size)?;
    if padded.len() < block {
        return Err(arg(
            "sodium_unpad",
            1,
            "string",
            "must be at least as long as the block size",
        ));
    }
    let mut unpadded_len = 0usize;
    // SAFETY: `padded` is readable for its length.
    let rc =
        unsafe { ffi::sodium_unpad(&raw mut unpadded_len, padded.as_ptr(), padded.len(), block) };
    if rc != 0 || i64::try_from(unpadded_len).is_err() {
        return Err(exc("invalid padding"));
    }
    Ok(padded[..unpadded_len].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utils_phpt() {
        let mut v = b"test".to_vec();
        memzero(&mut v);
        assert_eq!(v, [0, 0, 0, 0]);
        assert_eq!(memcmp(b"string", b"string"), Ok(0));
        assert_eq!(memcmp(b"string", b"String"), Ok(-1));
        let mut v = b"\xFF\xFF\x80\x01\x02\x03\x04\x05\x06\x07\x08".to_vec();
        increment(&mut v);
        assert_eq!(bin2hex(&v).unwrap(), b"0000810102030405060708");
        add(&mut v, b"\x01\x02\x03\x04\x05\x06\x07\x08\xFA\xFB\xFC").unwrap();
        assert_eq!(bin2hex(&v).unwrap(), b"0102840507090b0d000305");
        let mut v1: Vec<u8> = (1..=31).collect();
        let mut v2 = v1.clone();
        v2[0] = 2;
        assert_eq!(compare(&v1, &v2), Ok(-1));
        increment(&mut v1);
        assert_eq!(compare(&v1, &v2), Ok(0));
        increment(&mut v1);
        assert_eq!(compare(&v1, &v2), Ok(1));
        let padded = pad(b"xyz", 16).unwrap();
        assert_eq!(
            bin2hex(&padded).unwrap(),
            b"78797a80000000000000000000000000"
        );
        assert_eq!(unpad(&padded, 16).unwrap(), b"xyz");
    }

    #[test]
    fn length_mismatch() {
        let msg = |r: Result<i64>| r.unwrap_err().message().to_owned();
        assert_eq!(
            msg(memcmp(b"a", b"ab")),
            "sodium_memcmp(): Argument #1 ($string1) and argument #2 ($string2) must have the same length"
        );
        assert_eq!(
            msg(compare(b"a", b"ab")),
            "sodium_compare(): Argument #1 ($string1) and argument #2 ($string2) must have the same length"
        );
        let mut a = b"abcd".to_vec();
        assert_eq!(
            add(&mut a, b"x").unwrap_err().message(),
            "sodium_add(): Argument #1 ($string1) and argument #2 ($string2) must have the same length"
        );
        // inc_add.phpt
        let mut s = b"abc".to_vec();
        increment(&mut s);
        assert_eq!(s, b"bbc");
        add(&mut s, b"\x02\0\0").unwrap();
        assert_eq!(s, b"dbc");
    }

    #[test]
    fn hex() {
        assert_eq!(bin2hex(b"\x00\xffA").unwrap(), b"00ff41");
        assert_eq!(hex2bin(b"00FFa1", b"").unwrap(), b"\x00\xff\xa1");
        assert_eq!(hex2bin(b"[00ff]", b"[]").unwrap(), b"\x00\xff");
        assert_eq!(hex2bin(b"", b"").unwrap(), b"");
        for bad in [&b"0"[..], b"zz", b"00 11", b"0g"] {
            assert_eq!(
                hex2bin(bad, b"").unwrap_err(),
                Error::Sodium(
                    "sodium_hex2bin(): Argument #1 ($string) must be a valid hexadecimal string"
                        .into()
                )
            );
        }
        assert_eq!(hex2bin(b"00 11", b" \0x").unwrap(), b"\x00\x11");
    }

    #[test]
    fn base64() {
        assert_eq!(bin2base64(b"", 1).unwrap(), b"");
        assert_eq!(bin2base64(b"abcd", 1).unwrap(), b"YWJjZA==");
        assert_eq!(bin2base64(b"abcd", 3).unwrap(), b"YWJjZA");
        assert_eq!(bin2base64(b"\xfb\xff", 5).unwrap(), b"-_8=");
        assert_eq!(bin2base64(b"\xfb\xff", 7).unwrap(), b"-_8");
        assert_eq!(base642bin(b"YWJjZA", 3, b"").unwrap(), b"abcd");
        assert_eq!(base642bin(b" YWJjZA==\n", 1, b" \n").unwrap(), b"abcd");
        for bad in [&b"O1R"[..], b"O1", b"O"] {
            assert!(base642bin(bad, 3, b"").is_err(), "{bad:?}");
        }
        assert_eq!(
            base642bin(b"YWJjZA==", 3, b"").unwrap_err().message(),
            "sodium_base642bin(): Argument #1 ($string) must be a valid base64 string"
        );
        for bad in [0, 2, 8, -1, 0x1_0000_0001] {
            let err = if bad == 0x1_0000_0001 {
                // Truncated to `unsigned int`, this is variant 1.
                assert!(bin2base64(b"a", bad).is_ok());
                continue;
            } else {
                bin2base64(b"a", bad).unwrap_err()
            };
            assert_eq!(
                err.message(),
                "sodium_bin2base64(): Argument #2 ($id) must be a valid base64 variant identifier"
            );
        }
        assert_eq!(
            base642bin(b"", 0, b"").unwrap_err().message(),
            "sodium_base642bin(): Argument #2 ($id) must be a valid base64 variant identifier"
        );
    }

    #[test]
    fn padding_errors() {
        assert_eq!(
            pad(b"x", 0).unwrap_err().message(),
            "sodium_pad(): Argument #2 ($block_size) must be greater than 0"
        );
        assert_eq!(
            unpad(b"x", -1).unwrap_err().message(),
            "sodium_unpad(): Argument #2 ($block_size) must be greater than 0"
        );
        assert_eq!(
            unpad(b"x", 2).unwrap_err().message(),
            "sodium_unpad(): Argument #1 ($string) must be at least as long as the block size"
        );
        assert_eq!(
            unpad(b"\0\0\0\0", 4).unwrap_err(),
            Error::Sodium("invalid padding".into())
        );
        assert_eq!(pad(b"", 1).unwrap(), b"\x80");
        assert_eq!(pad(b"abc", 3).unwrap(), b"abc\x80\0\0");
        assert_eq!(pad(b"abcd", 5).unwrap(), b"abcd\x80");
        assert_eq!(unpad(b"abcd\x80", 5).unwrap(), b"abcd");
    }
}
