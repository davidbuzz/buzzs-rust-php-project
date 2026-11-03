//! `sodium_crypto_core_ristretto255_*` and
//! `sodium_crypto_scalarmult_ristretto255*`.

#![allow(unsafe_code)]

use crate::error::{arg, check_len, exc, internal, Result};
use crate::{ffi, init};

const BYTES: usize = 32;
const HASHBYTES: usize = 64;
const SCALARBYTES: usize = 32;
const NONREDUCEDSCALARBYTES: usize = 64;

const P: &str = "sodium_crypto_core_ristretto255";
const POINT: &str = "SODIUM_CRYPTO_CORE_RISTRETTO255_BYTES";
const SCALAR: &str = "SODIUM_CRYPTO_CORE_RISTRETTO255_SCALARBYTES";

fn binary(
    name: &str,
    (n1, n2): (&str, &str),
    x: &[u8],
    y: &[u8],
    (len, konst): (usize, &str),
    op: impl FnOnce(*mut u8, *const u8, *const u8) -> i32,
) -> Result<Vec<u8>> {
    init();
    let f = format!("{P}_{name}");
    check_len(&f, 1, n1, x, len, konst)?;
    check_len(&f, 2, n2, y, len, konst)?;
    let mut r = vec![0u8; len];
    if op(r.as_mut_ptr(), x.as_ptr(), y.as_ptr()) != 0 {
        return Err(internal());
    }
    Ok(r)
}

fn unary(
    name: &str,
    s: &[u8],
    (len, konst): (usize, &str),
    op: impl FnOnce(*mut u8, *const u8) -> i32,
) -> Result<Vec<u8>> {
    init();
    check_len(&format!("{P}_{name}"), 1, "s", s, len, konst)?;
    let mut r = vec![0u8; 32];
    if op(r.as_mut_ptr(), s.as_ptr()) != 0 {
        return Err(internal());
    }
    Ok(r)
}

/// `sodium_crypto_core_ristretto255_add($p, $q)`.
pub fn add(p: &[u8], q: &[u8]) -> Result<Vec<u8>> {
    // SAFETY (closure): 32-byte buffers, lengths checked by `binary`.
    binary("add", ("p", "q"), p, q, (BYTES, POINT), |r, a, b| unsafe {
        ffi::crypto_core_ristretto255_add(r, a, b)
    })
}

/// `sodium_crypto_core_ristretto255_sub($p, $q)`.
pub fn sub(p: &[u8], q: &[u8]) -> Result<Vec<u8>> {
    // SAFETY (closure): as for `add`.
    binary("sub", ("p", "q"), p, q, (BYTES, POINT), |r, a, b| unsafe {
        ffi::crypto_core_ristretto255_sub(r, a, b)
    })
}

/// `sodium_crypto_core_ristretto255_from_hash($s)`.
pub fn from_hash(s: &[u8]) -> Result<Vec<u8>> {
    // SAFETY (closure): 64-byte input, 32-byte output.
    unary(
        "from_hash",
        s,
        (HASHBYTES, "SODIUM_CRYPTO_CORE_RISTRETTO255_HASHBYTES"),
        |r, a| unsafe { ffi::crypto_core_ristretto255_from_hash(r, a) },
    )
}

/// `sodium_crypto_core_ristretto255_is_valid_point($s)`.
pub fn is_valid_point(s: &[u8]) -> Result<bool> {
    init();
    check_len(&format!("{P}_is_valid_point"), 1, "s", s, BYTES, POINT)?;
    // SAFETY: 32-byte input.
    Ok(unsafe { ffi::crypto_core_ristretto255_is_valid_point(s.as_ptr()) } == 1)
}

/// `sodium_crypto_core_ristretto255_random()`.
pub fn random() -> Vec<u8> {
    init();
    let mut r = vec![0u8; BYTES];
    // SAFETY: 32-byte output.
    unsafe { ffi::crypto_core_ristretto255_random(r.as_mut_ptr()) };
    r
}

/// `sodium_crypto_core_ristretto255_scalar_random()`.
pub fn scalar_random() -> Vec<u8> {
    init();
    let mut r = vec![0u8; SCALARBYTES];
    // SAFETY: 32-byte output.
    unsafe { ffi::crypto_core_ristretto255_scalar_random(r.as_mut_ptr()) };
    r
}

/// `sodium_crypto_core_ristretto255_scalar_add($x, $y)`.
pub fn scalar_add(x: &[u8], y: &[u8]) -> Result<Vec<u8>> {
    // SAFETY (closure): 32-byte scalars.
    binary(
        "scalar_add",
        ("x", "y"),
        x,
        y,
        (SCALARBYTES, SCALAR),
        |r, a, b| {
            unsafe { ffi::crypto_core_ristretto255_scalar_add(r, a, b) };
            0
        },
    )
}

/// `sodium_crypto_core_ristretto255_scalar_sub($x, $y)`.
pub fn scalar_sub(x: &[u8], y: &[u8]) -> Result<Vec<u8>> {
    // SAFETY (closure): 32-byte scalars.
    binary(
        "scalar_sub",
        ("x", "y"),
        x,
        y,
        (SCALARBYTES, SCALAR),
        |r, a, b| {
            unsafe { ffi::crypto_core_ristretto255_scalar_sub(r, a, b) };
            0
        },
    )
}

/// `sodium_crypto_core_ristretto255_scalar_mul($x, $y)`.
pub fn scalar_mul(x: &[u8], y: &[u8]) -> Result<Vec<u8>> {
    // SAFETY (closure): 32-byte scalars.
    binary(
        "scalar_mul",
        ("x", "y"),
        x,
        y,
        (SCALARBYTES, SCALAR),
        |r, a, b| {
            unsafe { ffi::crypto_core_ristretto255_scalar_mul(r, a, b) };
            0
        },
    )
}

/// `sodium_crypto_core_ristretto255_scalar_invert($s)`.
pub fn scalar_invert(s: &[u8]) -> Result<Vec<u8>> {
    // SAFETY (closure): 32-byte scalars.
    unary("scalar_invert", s, (SCALARBYTES, SCALAR), |r, a| unsafe {
        ffi::crypto_core_ristretto255_scalar_invert(r, a)
    })
}

/// `sodium_crypto_core_ristretto255_scalar_negate($s)`.
pub fn scalar_negate(s: &[u8]) -> Result<Vec<u8>> {
    // SAFETY (closure): 32-byte scalars.
    unary("scalar_negate", s, (SCALARBYTES, SCALAR), |r, a| {
        unsafe { ffi::crypto_core_ristretto255_scalar_negate(r, a) };
        0
    })
}

/// `sodium_crypto_core_ristretto255_scalar_complement($s)`.
pub fn scalar_complement(s: &[u8]) -> Result<Vec<u8>> {
    // SAFETY (closure): 32-byte scalars.
    unary("scalar_complement", s, (SCALARBYTES, SCALAR), |r, a| {
        unsafe { ffi::crypto_core_ristretto255_scalar_complement(r, a) };
        0
    })
}

/// `sodium_crypto_core_ristretto255_scalar_reduce($s)`.
pub fn scalar_reduce(s: &[u8]) -> Result<Vec<u8>> {
    // SAFETY (closure): 64-byte input, 32-byte output.
    unary(
        "scalar_reduce",
        s,
        (
            NONREDUCEDSCALARBYTES,
            "SODIUM_CRYPTO_CORE_RISTRETTO255_NONREDUCEDSCALARBYTES",
        ),
        |r, a| {
            unsafe { ffi::crypto_core_ristretto255_scalar_reduce(r, a) };
            0
        },
    )
}

/// `sodium_crypto_scalarmult_ristretto255($n, $p)`.
pub fn scalarmult(n: &[u8], p: &[u8]) -> Result<Vec<u8>> {
    init();
    let f = "sodium_crypto_scalarmult_ristretto255";
    check_len(
        f,
        1,
        "n",
        n,
        SCALARBYTES,
        "SODIUM_CRYPTO_SCALARMULT_RISTRETTO255_SCALARBYTES",
    )?;
    check_len(
        f,
        2,
        "p",
        p,
        BYTES,
        "SODIUM_CRYPTO_SCALARMULT_RISTRETTO255_BYTES",
    )?;
    let mut q = vec![0u8; BYTES];
    // SAFETY: 32-byte buffers.
    if unsafe { ffi::crypto_scalarmult_ristretto255(q.as_mut_ptr(), n.as_ptr(), p.as_ptr()) } != 0 {
        return Err(exc("Result is identity element"));
    }
    Ok(q)
}

/// `sodium_crypto_scalarmult_ristretto255_base($n)`.
pub fn scalarmult_base(n: &[u8]) -> Result<Vec<u8>> {
    init();
    let f = "sodium_crypto_scalarmult_ristretto255_base";
    check_len(
        f,
        1,
        "n",
        n,
        SCALARBYTES,
        "SODIUM_CRYPTO_SCALARMULT_RISTRETTO255_SCALARBYTES",
    )?;
    let mut q = vec![0u8; BYTES];
    // SAFETY: 32-byte buffers.
    if unsafe { ffi::crypto_scalarmult_ristretto255_base(q.as_mut_ptr(), n.as_ptr()) } != 0 {
        return Err(arg(f, 1, "n", "must not be zero"));
    }
    Ok(q)
}
