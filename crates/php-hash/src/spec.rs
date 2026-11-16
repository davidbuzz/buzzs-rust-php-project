// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `php_hash_serialize_spec` / `php_hash_unserialize_spec` engine.
//!
//! A spec string describes the layout of a C hash context:
//!
//! * `b[COUNT]` — COUNT bytes, `s` 16-bit, `l` 32-bit, `q` 64-bit, `i` `int`
//!   integers;
//! * uppercase letters skip the field instead of (de)serializing it;
//! * a trailing `.` asserts that the fields cover the whole context.
//!
//! Each integer is serialized as a (sign-extended) 32-bit PHP int, a run of
//! two or more bytes as a string, and a 64-bit integer as two 32-bit ints,
//! least significant half first.

/// One element of a serialized hash state array.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StateValue {
    /// A PHP `int`.
    Int(i64),
    /// A PHP `string`.
    Bytes(Vec<u8>),
}

/// A borrowed view of one element of a state array being unserialized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateElem<'a> {
    /// The element is a PHP `int`.
    Int(i64),
    /// The element is a PHP `string`.
    Bytes(&'a [u8]),
    /// The element has some other type.
    Other,
}

/// `HASH_SPEC_FAILURE`.
pub(crate) const SPEC_FAILURE: i64 = -1;
/// `WRONG_CONTEXT_SIZE`.
pub(crate) const WRONG_CONTEXT_SIZE: i64 = -999;
/// `BYTE_OFFSET_POS_ERROR`.
pub(crate) const BYTE_OFFSET_POS_ERROR: i64 = -1000;
/// `CONTEXT_VALIDATION_FAILURE`.
pub(crate) const CONTEXT_VALIDATION_FAILURE: i64 = -2000;

struct Field {
    lower: bool,
    size: usize,
    count: usize,
}

fn align_to(pos: usize, alignment: usize) -> usize {
    let offset = pos & (alignment - 1);
    if offset == 0 {
        pos
    } else {
        pos + alignment - offset
    }
}

/// Parses one spec field, advancing `pos` for alignment.
fn parse_field(spec: &[u8], idx: &mut usize, pos: &mut usize, max_align: &mut usize) -> Field {
    let ch = spec[*idx];
    let (size, alignment) = match ch.to_ascii_lowercase() {
        b's' => (2, 2),
        b'l' | b'i' => (4, 4),
        b'q' => (8, 8),
        _ => (1, 1),
    };
    *pos = align_to(*pos, alignment);
    *max_align = (*max_align).max(alignment);
    *idx += 1;
    let mut count = 0usize;
    let mut has_digits = false;
    while *idx < spec.len() && spec[*idx].is_ascii_digit() {
        count = 10 * count + usize::from(spec[*idx] - b'0');
        has_digits = true;
        *idx += 1;
    }
    Field {
        lower: ch.is_ascii_lowercase(),
        size,
        count: if has_digits { count } else { 1 },
    }
}

fn read_uint(image: &[u8], pos: usize, size: usize) -> u64 {
    let mut b = [0u8; 8];
    b[..size].copy_from_slice(&image[pos..pos + size]);
    u64::from_le_bytes(b)
}

fn write_uint(image: &mut [u8], pos: usize, size: usize, v: u64) {
    image[pos..pos + size].copy_from_slice(&v.to_le_bytes()[..size]);
}

/// `php_hash_serialize_spec`: renders `image` (of `context_size` bytes)
/// according to `spec`. Returns `None` on failure.
pub(crate) fn serialize(image: &[u8], spec: &str) -> Option<Vec<StateValue>> {
    let spec = spec.as_bytes();
    let (mut pos, mut max_align, mut idx) = (0usize, 1usize, 0usize);
    let mut out = Vec::new();
    while idx < spec.len() && spec[idx] != b'.' {
        let f = parse_field(spec, &mut idx, &mut pos, &mut max_align);
        if pos + f.count * f.size > image.len() {
            return None;
        }
        if !f.lower {
            pos += f.count * f.size;
        } else if f.size == 1 && f.count > 1 {
            out.push(StateValue::Bytes(image[pos..pos + f.count].to_vec()));
            pos += f.count;
        } else {
            for _ in 0..f.count {
                let v = read_uint(image, pos, f.size);
                pos += f.size;
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                {
                    out.push(StateValue::Int(i64::from(v as u32 as i32)));
                    if f.size == 8 {
                        out.push(StateValue::Int(i64::from((v >> 32) as u32 as i32)));
                    }
                }
            }
        }
    }
    if idx < spec.len() && align_to(pos, max_align) != image.len() {
        return None;
    }
    Some(out)
}

/// `php_hash_unserialize_spec`: patches `image` from the state array.
/// `lookup(j)` returns element `j` of the array (`None` if missing).
///
/// On failure returns PHP's `hash_spec_result` code.
#[allow(clippy::cast_possible_wrap)]
pub(crate) fn unserialize<'a>(
    image: &mut [u8],
    spec: &str,
    lookup: &dyn Fn(usize) -> Option<StateElem<'a>>,
) -> Result<(), i64> {
    let spec = spec.as_bytes();
    let (mut pos, mut max_align, mut idx, mut j) = (0usize, 1usize, 0usize, 0usize);
    while idx < spec.len() && spec[idx] != b'.' {
        let f = parse_field(spec, &mut idx, &mut pos, &mut max_align);
        if pos + f.count * f.size > image.len() {
            return Err(WRONG_CONTEXT_SIZE);
        }
        let pos_err = BYTE_OFFSET_POS_ERROR - pos as i64;
        if !f.lower {
            pos += f.count * f.size;
        } else if f.size == 1 && f.count > 1 {
            match lookup(j) {
                Some(StateElem::Bytes(b)) if b.len() == f.count => {
                    image[pos..pos + f.count].copy_from_slice(b);
                }
                _ => return Err(pos_err),
            }
            j += 1;
            pos += f.count;
        } else {
            for _ in 0..f.count {
                let pos_err = BYTE_OFFSET_POS_ERROR - pos as i64;
                let Some(StateElem::Int(lo)) = lookup(j) else {
                    return Err(pos_err);
                };
                j += 1;
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let mut val = u64::from(lo as u32);
                if f.size == 8 {
                    let Some(StateElem::Int(hi)) = lookup(j) else {
                        return Err(pos_err);
                    };
                    j += 1;
                    #[allow(clippy::cast_sign_loss)]
                    {
                        val = val.wrapping_add((hi as u64) << 32);
                    }
                }
                write_uint(image, pos, f.size, val);
                pos += f.size;
            }
        }
    }
    if idx < spec.len() && align_to(pos, max_align) != image.len() {
        return Err(WRONG_CONTEXT_SIZE);
    }
    Ok(())
}
