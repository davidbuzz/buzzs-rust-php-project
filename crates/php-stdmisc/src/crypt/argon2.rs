// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Argon2 (RFC 9106; Argon2i / Argon2id / Argon2d, versions 0x10 and 0x13)
//! with `BLAKE2b`, plus the PHC string encoding of the reference `libargon2`
//! (`encoding.c`), which PHP's `password_hash()` uses when built with
//! `--with-password-argon2`.

// Cryptographic code ported from C: single-letter state variables, fixed-width
// truncating casts and indexed loops mirror the reference implementation.
#![allow(
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::needless_range_loop,
    clippy::explicit_counter_loop,
    clippy::trivially_copy_pass_by_ref
)]

/// `ARGON2_VERSION_10`.
pub const VERSION_10: u32 = 0x10;
/// `ARGON2_VERSION_13` (`ARGON2_VERSION_NUMBER`).
pub const VERSION_13: u32 = 0x13;

const SYNC_POINTS: u32 = 4;
const BLOCK_WORDS: usize = 128;
/// `ARGON2_MIN_MEMORY`.
pub const MIN_MEMORY: u64 = 2 * SYNC_POINTS as u64;
/// `ARGON2_MAX_MEMORY` on 64-bit platforms.
pub const MAX_MEMORY: u64 = 0xFFFF_FFFF;
/// `ARGON2_MIN_TIME`.
pub const MIN_TIME: u64 = 1;
/// `ARGON2_MAX_TIME`.
pub const MAX_TIME: u64 = 0xFFFF_FFFF;
/// `ARGON2_MAX_LANES`.
pub const MAX_LANES: u64 = 0x00FF_FFFF;

/// The Argon2 variant (`argon2_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// `Argon2_d` (data-dependent addressing).
    D = 0,
    /// `Argon2_i` (data-independent addressing).
    I = 1,
    /// `Argon2_id` (hybrid).
    Id = 2,
}

impl Variant {
    /// `argon2_type2string(type, 0)`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Variant::D => "argon2d",
            Variant::I => "argon2i",
            Variant::Id => "argon2id",
        }
    }
}

/// Argon2 error codes (subset of `argon2_error_codes`) with
/// `argon2_error_message()` texts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Argon2Error {
    /// `ARGON2_OUTPUT_TOO_SHORT`.
    OutputTooShort,
    /// `ARGON2_PWD_TOO_LONG`.
    PwdTooLong,
    /// `ARGON2_SALT_TOO_SHORT`.
    SaltTooShort,
    /// `ARGON2_SALT_TOO_LONG`.
    SaltTooLong,
    /// `ARGON2_TIME_TOO_SMALL`.
    TimeTooSmall,
    /// `ARGON2_TIME_TOO_LARGE`.
    TimeTooLarge,
    /// `ARGON2_MEMORY_TOO_LITTLE`.
    MemoryTooLittle,
    /// `ARGON2_MEMORY_TOO_MUCH`.
    MemoryTooMuch,
    /// `ARGON2_LANES_TOO_FEW`.
    LanesTooFew,
    /// `ARGON2_LANES_TOO_MANY`.
    LanesTooMany,
    /// `ARGON2_MEMORY_ALLOCATION_ERROR`.
    MemoryAllocation,
    /// `ARGON2_DECODING_FAIL`.
    DecodingFail,
    /// `ARGON2_VERIFY_MISMATCH`.
    VerifyMismatch,
}

impl Argon2Error {
    /// `argon2_error_message()`.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Argon2Error::OutputTooShort => "Output is too short",
            Argon2Error::PwdTooLong => "Password is too long",
            Argon2Error::SaltTooShort => "Salt is too short",
            Argon2Error::SaltTooLong => "Salt is too long",
            Argon2Error::TimeTooSmall => "Time cost is too small",
            Argon2Error::TimeTooLarge => "Time cost is too large",
            Argon2Error::MemoryTooLittle => "Memory cost is too small",
            Argon2Error::MemoryTooMuch => "Memory cost is too large",
            Argon2Error::LanesTooFew => "Too few lanes",
            Argon2Error::LanesTooMany => "Too many lanes",
            Argon2Error::MemoryAllocation => "Memory allocation error",
            Argon2Error::DecodingFail => "Decoding failed",
            Argon2Error::VerifyMismatch => "The password does not match the supplied hash",
        }
    }
}

/// Argon2 parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    /// Iterations (`t_cost`).
    pub t_cost: u32,
    /// Memory in KiB (`m_cost`).
    pub m_cost: u32,
    /// Lanes (= threads).
    pub lanes: u32,
    /// Version (0x10 or 0x13).
    pub version: u32,
}

// ------------------------------------------------------------------ BLAKE2b

const BLAKE2B_IV: [u64; 8] = [
    0x6a09_e667_f3bc_c908,
    0xbb67_ae85_84ca_a73b,
    0x3c6e_f372_fe94_f82b,
    0xa54f_f53a_5f1d_36f1,
    0x510e_527f_ade6_82d1,
    0x9b05_688c_2b3e_6c1f,
    0x1f83_d9ab_fb41_bd6b,
    0x5be0_cd19_137e_2179,
];

const SIGMA: [[usize; 16]; 12] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
];

/// An unkeyed `BLAKE2b` state with output length `outlen` (1..=64).
struct Blake2b {
    h: [u64; 8],
    t: u128,
    buf: [u8; 128],
    buflen: usize,
    outlen: usize,
}

impl Blake2b {
    fn new(outlen: usize) -> Self {
        let mut h = BLAKE2B_IV;
        h[0] ^= 0x0101_0000 ^ outlen as u64;
        Blake2b {
            h,
            t: 0,
            buf: [0; 128],
            buflen: 0,
            outlen,
        }
    }

    fn compress(&mut self, last: bool) {
        let mut m = [0u64; 16];
        for (i, w) in m.iter_mut().enumerate() {
            let mut b = [0u8; 8];
            b.copy_from_slice(&self.buf[8 * i..8 * i + 8]);
            *w = u64::from_le_bytes(b);
        }
        let mut v = [0u64; 16];
        v[..8].copy_from_slice(&self.h);
        v[8..].copy_from_slice(&BLAKE2B_IV);
        #[allow(clippy::cast_possible_truncation)]
        {
            v[12] ^= self.t as u64;
            v[13] ^= (self.t >> 64) as u64;
        }
        if last {
            v[14] = !v[14];
        }
        for s in &SIGMA {
            let mut g = |a: usize, b: usize, c: usize, d: usize, x: u64, y: u64| {
                v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
                v[d] = (v[d] ^ v[a]).rotate_right(32);
                v[c] = v[c].wrapping_add(v[d]);
                v[b] = (v[b] ^ v[c]).rotate_right(24);
                v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
                v[d] = (v[d] ^ v[a]).rotate_right(16);
                v[c] = v[c].wrapping_add(v[d]);
                v[b] = (v[b] ^ v[c]).rotate_right(63);
            };
            g(0, 4, 8, 12, m[s[0]], m[s[1]]);
            g(1, 5, 9, 13, m[s[2]], m[s[3]]);
            g(2, 6, 10, 14, m[s[4]], m[s[5]]);
            g(3, 7, 11, 15, m[s[6]], m[s[7]]);
            g(0, 5, 10, 15, m[s[8]], m[s[9]]);
            g(1, 6, 11, 12, m[s[10]], m[s[11]]);
            g(2, 7, 8, 13, m[s[12]], m[s[13]]);
            g(3, 4, 9, 14, m[s[14]], m[s[15]]);
        }
        for i in 0..8 {
            self.h[i] ^= v[i] ^ v[i + 8];
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            if self.buflen == 128 {
                self.t += 128;
                self.compress(false);
                self.buflen = 0;
            }
            let n = (128 - self.buflen).min(data.len());
            self.buf[self.buflen..self.buflen + n].copy_from_slice(&data[..n]);
            self.buflen += n;
            data = &data[n..];
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.t += self.buflen as u128;
        self.buf[self.buflen..].fill(0);
        self.compress(true);
        let mut out = Vec::with_capacity(64);
        for w in &self.h {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out.truncate(self.outlen);
        out
    }
}

/// `BLAKE2b` with an `outlen`-byte digest (1..=64).
#[must_use]
pub fn blake2b(outlen: usize, data: &[u8]) -> Vec<u8> {
    let mut b = Blake2b::new(outlen);
    b.update(data);
    b.finish()
}

fn le32(x: usize) -> [u8; 4] {
    u32::try_from(x).unwrap_or(u32::MAX).to_le_bytes()
}

/// `blake2b_long()` (H'): the variable-length hash.
fn blake2b_long(outlen: usize, input: &[&[u8]]) -> Vec<u8> {
    if outlen <= 64 {
        let mut b = Blake2b::new(outlen);
        b.update(&le32(outlen));
        for p in input {
            b.update(p);
        }
        return b.finish();
    }
    let mut out = Vec::with_capacity(outlen);
    let mut b = Blake2b::new(64);
    b.update(&le32(outlen));
    for p in input {
        b.update(p);
    }
    let mut v = b.finish();
    out.extend_from_slice(&v[..32]);
    let mut remaining = outlen - 32;
    while remaining > 64 {
        v = blake2b(64, &v);
        out.extend_from_slice(&v[..32]);
        remaining -= 32;
    }
    v = blake2b(remaining, &v);
    out.extend_from_slice(&v);
    out
}

// ------------------------------------------------------------------ core

type Block = [u64; BLOCK_WORDS];

#[inline]
fn fblamka(x: u64, y: u64) -> u64 {
    let m = 0xFFFF_FFFFu64;
    let xy = (x & m) * (y & m);
    x.wrapping_add(y).wrapping_add(xy.wrapping_mul(2))
}

#[inline]
fn round_nomsg(v: &mut [u64; 128], idx: [usize; 16]) {
    let mut g = |a: usize, b: usize, c: usize, d: usize| {
        let (a, b, c, d) = (idx[a], idx[b], idx[c], idx[d]);
        v[a] = fblamka(v[a], v[b]);
        v[d] = (v[d] ^ v[a]).rotate_right(32);
        v[c] = fblamka(v[c], v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(24);
        v[a] = fblamka(v[a], v[b]);
        v[d] = (v[d] ^ v[a]).rotate_right(16);
        v[c] = fblamka(v[c], v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(63);
    };
    g(0, 4, 8, 12);
    g(1, 5, 9, 13);
    g(2, 6, 10, 14);
    g(3, 7, 11, 15);
    g(0, 5, 10, 15);
    g(1, 6, 11, 12);
    g(2, 7, 8, 13);
    g(3, 4, 9, 14);
}

/// `fill_block()`: returns the new block, xored into `next` if `with_xor`.
fn fill_block(prev: &Block, reference: &Block, next: &Block, with_xor: bool) -> Block {
    let mut r = [0u64; 128];
    for i in 0..128 {
        r[i] = prev[i] ^ reference[i];
    }
    let mut tmp = r;
    if with_xor {
        for i in 0..128 {
            tmp[i] ^= next[i];
        }
    }
    for i in 0..8 {
        let mut idx = [0usize; 16];
        for (j, slot) in idx.iter_mut().enumerate() {
            *slot = 16 * i + j;
        }
        round_nomsg(&mut r, idx);
    }
    for i in 0..8 {
        let mut idx = [0usize; 16];
        for (j, slot) in idx.iter_mut().enumerate() {
            *slot = 2 * i + (j % 2) + 16 * (j / 2);
        }
        round_nomsg(&mut r, idx);
    }
    for i in 0..128 {
        tmp[i] ^= r[i];
    }
    tmp
}

fn bytes_to_block(b: &[u8]) -> Block {
    let mut out = [0u64; 128];
    for (i, w) in out.iter_mut().enumerate() {
        let mut x = [0u8; 8];
        x.copy_from_slice(&b[8 * i..8 * i + 8]);
        *w = u64::from_le_bytes(x);
    }
    out
}

/// Argon2's block array, allocated outside the global (memory-limit
/// counting) allocator like libargon2's `malloc()`.
struct Memory(crate::sys::UntrackedWords);

impl Memory {
    fn new(blocks: usize) -> Option<Self> {
        crate::sys::UntrackedWords::zeroed(blocks.checked_mul(BLOCK_WORDS)?).map(Memory)
    }
}

impl std::ops::Index<usize> for Memory {
    type Output = Block;
    fn index(&self, i: usize) -> &Block {
        let w = &self.0[i * BLOCK_WORDS..(i + 1) * BLOCK_WORDS];
        w.try_into()
            .unwrap_or_else(|_| unreachable!("block slice has BLOCK_WORDS words"))
    }
}

impl std::ops::IndexMut<usize> for Memory {
    fn index_mut(&mut self, i: usize) -> &mut Block {
        let w = &mut self.0[i * BLOCK_WORDS..(i + 1) * BLOCK_WORDS];
        w.try_into()
            .unwrap_or_else(|_| unreachable!("block slice has BLOCK_WORDS words"))
    }
}

struct Instance {
    memory: Memory,
    passes: u32,
    lanes: u32,
    lane_length: u32,
    segment_length: u32,
    memory_blocks: u32,
    variant: Variant,
    version: u32,
}

impl Instance {
    fn index_alpha(
        &self,
        pass: u32,
        slice: u32,
        index: u32,
        pseudo_rand: u32,
        same_lane: bool,
    ) -> u32 {
        let reference_area_size: u32 = if pass == 0 {
            if slice == 0 {
                index - 1
            } else if same_lane {
                slice * self.segment_length + index - 1
            } else {
                (slice * self.segment_length).wrapping_add(if index == 0 { u32::MAX } else { 0 })
            }
        } else if same_lane {
            self.lane_length - self.segment_length + index - 1
        } else {
            (self.lane_length - self.segment_length).wrapping_add(if index == 0 {
                u32::MAX
            } else {
                0
            })
        };
        let mut rel = u64::from(pseudo_rand);
        rel = (rel * rel) >> 32;
        let area = u64::from(reference_area_size);
        let rel = area - 1 - ((area * rel) >> 32);
        let start = if pass != 0 && slice != SYNC_POINTS - 1 {
            u64::from((slice + 1) * self.segment_length)
        } else {
            0
        };
        #[allow(clippy::cast_possible_truncation)]
        let pos = ((start + rel) % u64::from(self.lane_length)) as u32;
        pos
    }

    fn fill_segment(&mut self, pass: u32, lane: u32, slice: u32) {
        let independent = self.variant == Variant::I
            || (self.variant == Variant::Id && pass == 0 && slice < SYNC_POINTS / 2);
        let zero = [0u64; 128];
        let mut input = [0u64; 128];
        let mut address = [0u64; 128];
        if independent {
            input[0] = u64::from(pass);
            input[1] = u64::from(lane);
            input[2] = u64::from(slice);
            input[3] = u64::from(self.memory_blocks);
            input[4] = u64::from(self.passes);
            input[5] = self.variant as u64;
        }
        let next_addresses = |input: &mut Block, address: &mut Block| {
            input[6] += 1;
            let a = fill_block(&zero, input, &zero, false);
            *address = fill_block(&zero, &a, &zero, false);
        };
        let mut starting_index = 0;
        if pass == 0 && slice == 0 {
            starting_index = 2;
            if independent {
                next_addresses(&mut input, &mut address);
            }
        }
        let mut curr = lane * self.lane_length + slice * self.segment_length + starting_index;
        let mut prev = if curr % self.lane_length == 0 {
            curr + self.lane_length - 1
        } else {
            curr - 1
        };
        for i in starting_index..self.segment_length {
            if curr % self.lane_length == 1 {
                prev = curr - 1;
            }
            let pseudo_rand = if independent {
                if i as usize % BLOCK_WORDS == 0 {
                    next_addresses(&mut input, &mut address);
                }
                address[i as usize % BLOCK_WORDS]
            } else {
                self.memory[prev as usize][0]
            };
            #[allow(clippy::cast_possible_truncation)]
            let mut ref_lane = ((pseudo_rand >> 32) % u64::from(self.lanes)) as u32;
            if pass == 0 && slice == 0 {
                ref_lane = lane;
            }
            #[allow(clippy::cast_possible_truncation)]
            let ref_index = self.index_alpha(pass, slice, i, pseudo_rand as u32, ref_lane == lane);
            let ref_pos = (self.lane_length * ref_lane + ref_index) as usize;
            let with_xor = self.version != VERSION_10 && pass != 0;
            let block = fill_block(
                &self.memory[prev as usize],
                &self.memory[ref_pos],
                &self.memory[curr as usize],
                with_xor,
            );
            self.memory[curr as usize] = block;
            curr += 1;
            prev += 1;
        }
    }
}

/// Validates the inputs like `validate_inputs()` (in its order).
fn validate(
    params: &Params,
    pwd_len: usize,
    salt_len: usize,
    out_len: usize,
) -> Result<(), Argon2Error> {
    if out_len < 4 {
        return Err(Argon2Error::OutputTooShort);
    }
    if pwd_len > 0xFFFF_FFFF {
        return Err(Argon2Error::PwdTooLong);
    }
    if salt_len < 8 {
        return Err(Argon2Error::SaltTooShort);
    }
    if salt_len > 0xFFFF_FFFF {
        return Err(Argon2Error::SaltTooLong);
    }
    if u64::from(params.m_cost) < MIN_MEMORY {
        return Err(Argon2Error::MemoryTooLittle);
    }
    if u64::from(params.m_cost) < 8 * u64::from(params.lanes) {
        return Err(Argon2Error::MemoryTooLittle);
    }
    if u64::from(params.t_cost) < MIN_TIME {
        return Err(Argon2Error::TimeTooSmall);
    }
    if params.lanes < 1 {
        return Err(Argon2Error::LanesTooFew);
    }
    if u64::from(params.lanes) > MAX_LANES {
        return Err(Argon2Error::LanesTooMany);
    }
    Ok(())
}

/// The raw Argon2 hash (`argon2_ctx()` without secret/associated data).
pub fn hash_raw(
    variant: Variant,
    params: &Params,
    pwd: &[u8],
    salt: &[u8],
    out_len: usize,
) -> Result<Vec<u8>, Argon2Error> {
    validate(params, pwd.len(), salt.len(), out_len)?;
    let lanes = params.lanes;
    let mut memory_blocks = params.m_cost;
    if memory_blocks < 2 * SYNC_POINTS * lanes {
        memory_blocks = 2 * SYNC_POINTS * lanes;
    }
    let segment_length = memory_blocks / (lanes * SYNC_POINTS);
    let memory_blocks = segment_length * lanes * SYNC_POINTS;
    let lane_length = segment_length * SYNC_POINTS;

    let mut memory = Memory::new(memory_blocks as usize).ok_or(Argon2Error::MemoryAllocation)?;

    // H0
    let mut h = Blake2b::new(64);
    h.update(&lanes.to_le_bytes());
    h.update(&le32(out_len));
    h.update(&params.m_cost.to_le_bytes());
    h.update(&params.t_cost.to_le_bytes());
    h.update(&params.version.to_le_bytes());
    h.update(&(variant as u32).to_le_bytes());
    h.update(&le32(pwd.len()));
    h.update(pwd);
    h.update(&le32(salt.len()));
    h.update(salt);
    h.update(&0u32.to_le_bytes());
    h.update(&0u32.to_le_bytes());
    let h0 = h.finish();

    for l in 0..lanes {
        for i in 0..2u32 {
            let b = blake2b_long(1024, &[&h0, &i.to_le_bytes(), &l.to_le_bytes()]);
            memory[(l * lane_length + i) as usize] = bytes_to_block(&b);
        }
    }
    let mut inst = Instance {
        memory,
        passes: params.t_cost,
        lanes,
        lane_length,
        segment_length,
        memory_blocks,
        variant,
        version: params.version,
    };
    for pass in 0..params.t_cost {
        for slice in 0..SYNC_POINTS {
            for lane in 0..lanes {
                inst.fill_segment(pass, lane, slice);
            }
        }
    }
    let mut last = inst.memory[(lane_length - 1) as usize];
    for l in 1..lanes {
        let b = &inst.memory[(l * lane_length + lane_length - 1) as usize];
        for i in 0..128 {
            last[i] ^= b[i];
        }
    }
    let mut bytes = Vec::with_capacity(1024);
    for w in &last {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    Ok(blake2b_long(out_len, &[&bytes]))
}

// ------------------------------------------------------------------ encoding

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Unpadded standard base64 (`to_base64()`).
fn to_base64(src: &[u8]) -> String {
    let mut out = String::new();
    let mut acc: u32 = 0;
    let mut acc_len = 0;
    for &b in src {
        acc = (acc << 8) | u32::from(b);
        acc_len += 8;
        while acc_len >= 6 {
            acc_len -= 6;
            out.push(char::from(B64[((acc >> acc_len) & 0x3f) as usize]));
        }
    }
    if acc_len > 0 {
        out.push(char::from(B64[((acc << (6 - acc_len)) & 0x3f) as usize]));
    }
    out
}

fn b64_char_to_byte(c: u8) -> Option<u32> {
    B64.iter()
        .position(|&x| x == c)
        .and_then(|p| u32::try_from(p).ok())
}

/// `from_base64()`: decodes until the first non-alphabet character.
/// Returns the bytes and the number of characters consumed.
fn from_base64(src: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut acc_len = 0;
    let mut i = 0;
    while let Some(d) = src.get(i).and_then(|&c| b64_char_to_byte(c)) {
        i += 1;
        acc = (acc << 6) | d;
        acc_len += 6;
        if acc_len >= 8 {
            acc_len -= 8;
            out.push(((acc >> acc_len) & 0xff) as u8);
        }
        acc &= (1 << acc_len) - 1;
    }
    if acc_len > 4 || (acc & ((1 << acc_len) - 1)) != 0 {
        return None;
    }
    Some((out, i))
}

/// `encode_string()`: the PHC string of a hash.
#[must_use]
pub fn encode(variant: Variant, params: &Params, salt: &[u8], hash: &[u8]) -> String {
    format!(
        "${}$v={}$m={},t={},p={}${}${}",
        variant.name(),
        params.version,
        params.m_cost,
        params.t_cost,
        params.lanes,
        to_base64(salt),
        to_base64(hash)
    )
}

/// `decode_decimal()`.
fn decode_decimal(s: &[u8]) -> Option<(u32, usize)> {
    let mut acc: u64 = 0;
    let mut i = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        acc = acc.checked_mul(10)?.checked_add(u64::from(s[i] - b'0'))?;
        i += 1;
    }
    if i == 0 {
        return None;
    }
    Some((u32::try_from(acc).ok()?, i))
}

/// A decoded PHC string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    /// The parameters.
    pub params: Params,
    /// The raw salt.
    pub salt: Vec<u8>,
    /// The raw hash.
    pub hash: Vec<u8>,
}

/// `decode_string()` for the given variant.
pub fn decode(variant: Variant, s: &[u8]) -> Result<Decoded, Argon2Error> {
    let fail = Argon2Error::DecodingFail;
    let mut p = s;
    let cc = |p: &mut &[u8], prefix: &[u8]| -> Result<(), Argon2Error> {
        *p = p.strip_prefix(prefix).ok_or(fail)?;
        Ok(())
    };
    let dec = |p: &mut &[u8]| -> Result<u32, Argon2Error> {
        let (v, n) = decode_decimal(p).ok_or(fail)?;
        *p = &p[n..];
        Ok(v)
    };
    cc(&mut p, b"$")?;
    cc(&mut p, variant.name().as_bytes())?;
    let mut version = VERSION_10;
    if let Some(rest) = p.strip_prefix(b"$v=") {
        p = rest;
        version = dec(&mut p)?;
    }
    cc(&mut p, b"$m=")?;
    let m_cost = dec(&mut p)?;
    cc(&mut p, b",t=")?;
    let t_cost = dec(&mut p)?;
    cc(&mut p, b",p=")?;
    let lanes = dec(&mut p)?;
    cc(&mut p, b"$")?;
    let (salt, n) = from_base64(p).ok_or(fail)?;
    p = &p[n..];
    cc(&mut p, b"$")?;
    let (hash, n) = from_base64(p).ok_or(fail)?;
    p = &p[n..];
    let params = Params {
        t_cost,
        m_cost,
        lanes,
        version,
    };
    validate(&params, 0, salt.len(), hash.len())?;
    if !p.is_empty() {
        return Err(fail);
    }
    Ok(Decoded { params, salt, hash })
}

/// `argon2_hash()` producing the encoded string.
pub fn hash_encoded(
    variant: Variant,
    params: &Params,
    pwd: &[u8],
    salt: &[u8],
    out_len: usize,
) -> Result<String, Argon2Error> {
    let hash = hash_raw(variant, params, pwd, salt, out_len)?;
    Ok(encode(variant, params, salt, &hash))
}

/// `argon2_verify()`.
pub fn verify(variant: Variant, encoded: &[u8], pwd: &[u8]) -> Result<(), Argon2Error> {
    // The C code stops at the first NUL of the encoded string.
    let encoded = encoded.split(|&c| c == 0).next().unwrap_or_default();
    let d = decode(variant, encoded)?;
    let computed = hash_raw(variant, &d.params, pwd, &d.salt, d.hash.len())?;
    let diff = computed
        .iter()
        .zip(&d.hash)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    if diff == 0 && computed.len() == d.hash.len() {
        Ok(())
    } else {
        Err(Argon2Error::VerifyMismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake2b_abc() {
        let h = blake2b(64, b"abc");
        assert_eq!(
            php_hash::bin2hex(&h),
            "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d17d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923"
        );
    }

    /// RFC 9106 test vectors need secret/AD; use the reference CLI vectors.
    #[test]
    fn reference_vectors() {
        // echo -n password | argon2 somesalt -t 2 -m 16 -p 1 -l 32 (v=19)
        let p = Params {
            t_cost: 2,
            m_cost: 65536,
            lanes: 1,
            version: VERSION_13,
        };
        let h = hash_raw(Variant::I, &p, b"password", b"somesalt", 32).unwrap();
        assert_eq!(
            php_hash::bin2hex(&h),
            "c1628832147d9720c5bd1cfd61367078729f6dfb6f8fea9ff98158e0d7816ed0"
        );
        let p = Params {
            t_cost: 2,
            m_cost: 65536,
            lanes: 1,
            version: VERSION_10,
        };
        let h = hash_raw(Variant::I, &p, b"password", b"somesalt", 32).unwrap();
        assert_eq!(
            php_hash::bin2hex(&h),
            "f6c4db4a54e2a370627aff3db6176b94a2a209a62c8e36152711802f7b30c694"
        );
        let p = Params {
            t_cost: 2,
            m_cost: 65536,
            lanes: 1,
            version: VERSION_13,
        };
        let h = hash_raw(Variant::Id, &p, b"password", b"somesalt", 32).unwrap();
        assert_eq!(
            php_hash::bin2hex(&h),
            "09316115d5cf24ed5a15a31a3ba326e5cf32edc24702987c02b6566f61913cf7"
        );
    }

    #[test]
    fn encoding_roundtrip() {
        let p = Params {
            t_cost: 1,
            m_cost: 8,
            lanes: 1,
            version: VERSION_13,
        };
        let e = hash_encoded(Variant::Id, &p, b"pw", b"saltsalt", 32).unwrap();
        assert!(e.starts_with("$argon2id$v=19$m=8,t=1,p=1$c2FsdHNhbHQ$"));
        assert_eq!(verify(Variant::Id, e.as_bytes(), b"pw"), Ok(()));
        assert_eq!(
            verify(Variant::Id, e.as_bytes(), b"px"),
            Err(Argon2Error::VerifyMismatch)
        );
        assert_eq!(
            verify(Variant::I, e.as_bytes(), b"pw"),
            Err(Argon2Error::DecodingFail)
        );
    }
}
