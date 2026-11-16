// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! HAVAL with 3, 4 or 5 passes and 128..256-bit output
//! (ref: `ext/hash/hash_haval.c`).

use super::{le_words, md_update32, put_le32, HashState, ImageReader, ImageWriter};

const D0: [u32; 8] = [
    0x243F_6A88,
    0x85A3_08D3,
    0x1319_8A2E,
    0x0370_7344,
    0xA409_3822,
    0x299F_31D0,
    0x082E_FA98,
    0xEC4E_6C89,
];

const K2: [u32; 32] = [
    0x4528_21E6,
    0x38D0_1377,
    0xBE54_66CF,
    0x34E9_0C6C,
    0xC0AC_29B7,
    0xC97C_50DD,
    0x3F84_D5B5,
    0xB547_0917,
    0x9216_D5D9,
    0x8979_FB1B,
    0xD131_0BA6,
    0x98DF_B5AC,
    0x2FFD_72DB,
    0xD01A_DFB7,
    0xB8E1_AFED,
    0x6A26_7E96,
    0xBA7C_9045,
    0xF12C_7F99,
    0x24A1_9947,
    0xB391_6CF7,
    0x0801_F2E2,
    0x858E_FC16,
    0x6369_20D8,
    0x7157_4E69,
    0xA458_FEA3,
    0xF493_3D7E,
    0x0D95_748F,
    0x728E_B658,
    0x718B_CD58,
    0x8215_4AEE,
    0x7B54_A41D,
    0xC25A_59B5,
];
const K3: [u32; 32] = [
    0x9C30_D539,
    0x2AF2_6013,
    0xC5D1_B023,
    0x2860_85F0,
    0xCA41_7918,
    0xB8DB_38EF,
    0x8E79_DCB0,
    0x603A_180E,
    0x6C9E_0E8B,
    0xB01E_8A3E,
    0xD715_77C1,
    0xBD31_4B27,
    0x78AF_2FDA,
    0x5560_5C60,
    0xE655_25F3,
    0xAA55_AB94,
    0x5748_9862,
    0x63E8_1440,
    0x55CA_396A,
    0x2AAB_10B6,
    0xB4CC_5C34,
    0x1141_E8CE,
    0xA154_86AF,
    0x7C72_E993,
    0xB3EE_1411,
    0x636F_BC2A,
    0x2BA9_C55D,
    0x7418_31F6,
    0xCE5C_3E16,
    0x9B87_931E,
    0xAFD6_BA33,
    0x6C24_CF5C,
];
const K4: [u32; 32] = [
    0x7A32_5381,
    0x2895_8677,
    0x3B8F_4898,
    0x6B4B_B9AF,
    0xC4BF_E81B,
    0x6628_2193,
    0x61D8_09CC,
    0xFB21_A991,
    0x487C_AC60,
    0x5DEC_8032,
    0xEF84_5D5D,
    0xE985_75B1,
    0xDC26_2302,
    0xEB65_1B88,
    0x2389_3E81,
    0xD396_ACC5,
    0x0F6D_6FF3,
    0x83F4_4239,
    0x2E0B_4482,
    0xA484_2004,
    0x69C8_F04A,
    0x9E1F_9B5E,
    0x21C6_6842,
    0xF6E9_6C9A,
    0x670C_9C61,
    0xABD3_88F0,
    0x6A51_A0D2,
    0xD854_2F68,
    0x960F_A728,
    0xAB51_33A3,
    0x6EEF_0B6C,
    0x137A_3BE4,
];
const K5: [u32; 32] = [
    0xBA3B_F050,
    0x7EFB_2A98,
    0xA1F1_651D,
    0x39AF_0176,
    0x66CA_593E,
    0x8243_0E88,
    0x8CEE_8619,
    0x456F_9FB4,
    0x7D84_A5C3,
    0x3B8B_5EBE,
    0xE06F_75D8,
    0x85C1_2073,
    0x401A_449F,
    0x56C1_6AA6,
    0x4ED3_AA62,
    0x363F_7706,
    0x1BFE_DF72,
    0x429B_023D,
    0x37D0_D724,
    0xD00A_1248,
    0xDB0F_EAD3,
    0x49F1_C09B,
    0x0753_72C9,
    0x8099_1B7B,
    0x25D4_79D8,
    0xF6E8_DEF7,
    0xE3FE_501A,
    0xB679_4C3B,
    0x976C_E0BD,
    0x04C0_06BA,
    0xC1A9_4FB6,
    0x409F_60C4,
];
const I2: [usize; 32] = [
    5, 14, 26, 18, 11, 28, 7, 16, 0, 23, 20, 22, 1, 10, 4, 8, 30, 3, 21, 9, 17, 24, 29, 6, 19, 12,
    15, 13, 2, 25, 31, 27,
];
const I3: [usize; 32] = [
    19, 9, 4, 20, 28, 17, 8, 22, 29, 14, 25, 12, 24, 30, 16, 26, 31, 15, 7, 3, 1, 0, 18, 27, 13, 6,
    21, 10, 23, 11, 5, 2,
];
const I4: [usize; 32] = [
    24, 4, 0, 14, 2, 7, 28, 23, 26, 6, 30, 20, 18, 25, 19, 3, 22, 11, 31, 21, 8, 27, 12, 9, 1, 29,
    5, 15, 17, 10, 16, 13,
];
const I5: [usize; 32] = [
    27, 3, 21, 26, 17, 11, 20, 29, 19, 0, 12, 7, 13, 8, 31, 10, 5, 9, 14, 30, 18, 6, 28, 24, 2, 23,
    16, 22, 4, 1, 25, 15,
];

fn f1(x6: u32, x5: u32, x4: u32, x3: u32, x2: u32, x1: u32, x0: u32) -> u32 {
    (x1 & x4) ^ (x2 & x5) ^ (x3 & x6) ^ (x0 & x1) ^ x0
}
fn f2(x6: u32, x5: u32, x4: u32, x3: u32, x2: u32, x1: u32, x0: u32) -> u32 {
    (x1 & x2 & x3)
        ^ (x2 & x4 & x5)
        ^ (x1 & x2)
        ^ (x1 & x4)
        ^ (x2 & x6)
        ^ (x3 & x5)
        ^ (x4 & x5)
        ^ (x0 & x2)
        ^ x0
}
fn f3(x6: u32, x5: u32, x4: u32, x3: u32, x2: u32, x1: u32, x0: u32) -> u32 {
    (x1 & x2 & x3) ^ (x1 & x4) ^ (x2 & x5) ^ (x3 & x6) ^ (x0 & x3) ^ x0
}
fn f4(x6: u32, x5: u32, x4: u32, x3: u32, x2: u32, x1: u32, x0: u32) -> u32 {
    (x1 & x2 & x3)
        ^ (x2 & x4 & x5)
        ^ (x3 & x4 & x6)
        ^ (x1 & x4)
        ^ (x2 & x6)
        ^ (x3 & x4)
        ^ (x3 & x5)
        ^ (x3 & x6)
        ^ (x4 & x5)
        ^ (x4 & x6)
        ^ (x0 & x4)
        ^ x0
}
fn f5(x6: u32, x5: u32, x4: u32, x3: u32, x2: u32, x1: u32, x0: u32) -> u32 {
    (x1 & x4) ^ (x2 & x5) ^ (x3 & x6) ^ (x0 & x1 & x2 & x3) ^ (x0 & x5) ^ x0
}

type BoolFn = fn(u32, u32, u32, u32, u32, u32, u32) -> u32;

/// One 32-step pass: `func` applied to the state words picked by `perm`
/// (indices into the `M0..M6` rotation tables), message order `order` and
/// constants `k`.
fn haval_pass(
    e: &mut [u32; 8],
    x: &[u32; 32],
    func: BoolFn,
    perm: [usize; 7],
    order: Option<&[usize; 32]>,
    k: Option<&[u32; 32]>,
) {
    for i in 0..32 {
        // Mn[i] == (n - i) mod 8
        let m = |n: usize| (n + 8 - (i % 8)) % 8;
        let fv = func(
            e[m(perm[0])],
            e[m(perm[1])],
            e[m(perm[2])],
            e[m(perm[3])],
            e[m(perm[4])],
            e[m(perm[5])],
            e[m(perm[6])],
        );
        let xi = order.map_or(x[i], |o| x[o[i]]);
        let ki = k.map_or(0, |k| k[i]);
        e[7 - (i % 8)] = fv
            .rotate_right(7)
            .wrapping_add(e[m(7)].rotate_right(11))
            .wrapping_add(xi)
            .wrapping_add(ki);
    }
}

fn transform(passes: u8, state: &mut [u32; 8], block: &[u8]) {
    let x: [u32; 32] = le_words(block);
    let mut e = *state;
    match passes {
        3 => {
            haval_pass(&mut e, &x, f1, [1, 0, 3, 5, 6, 2, 4], None, None);
            haval_pass(&mut e, &x, f2, [4, 2, 1, 0, 5, 3, 6], Some(&I2), Some(&K2));
            haval_pass(&mut e, &x, f3, [6, 1, 2, 3, 4, 5, 0], Some(&I3), Some(&K3));
        }
        4 => {
            haval_pass(&mut e, &x, f1, [2, 6, 1, 4, 5, 3, 0], None, None);
            haval_pass(&mut e, &x, f2, [3, 5, 2, 0, 1, 6, 4], Some(&I2), Some(&K2));
            haval_pass(&mut e, &x, f3, [1, 4, 3, 6, 0, 2, 5], Some(&I3), Some(&K3));
            haval_pass(&mut e, &x, f4, [6, 4, 0, 5, 2, 1, 3], Some(&I4), Some(&K4));
        }
        _ => {
            haval_pass(&mut e, &x, f1, [3, 4, 1, 0, 5, 2, 6], None, None);
            haval_pass(&mut e, &x, f2, [6, 2, 1, 0, 3, 4, 5], Some(&I2), Some(&K2));
            haval_pass(&mut e, &x, f3, [2, 6, 0, 4, 3, 1, 5], Some(&I3), Some(&K3));
            haval_pass(&mut e, &x, f4, [1, 5, 3, 2, 0, 4, 6], Some(&I4), Some(&K4));
            haval_pass(&mut e, &x, f5, [2, 5, 0, 6, 4, 3, 1], Some(&I5), Some(&K5));
        }
    }
    for (s, v) in state.iter_mut().zip(e) {
        *s = s.wrapping_add(v);
    }
}

/// `PHP_HAVAL_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Haval {
    state: [u32; 8],
    count: [u32; 2],
    buffer: [u8; 128],
    passes: u8,
    output: u16,
}

impl Haval {
    pub(crate) fn new(passes: u8, output: u16) -> Self {
        Self {
            state: D0,
            count: [0; 2],
            buffer: [0; 128],
            passes,
            output,
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn fold(&mut self) {
        let s = &mut self.state;
        match self.output {
            128 => {
                s[3] = s[3].wrapping_add(
                    (s[7] & 0xFF00_0000)
                        | (s[6] & 0x00FF_0000)
                        | (s[5] & 0x0000_FF00)
                        | (s[4] & 0x0000_00FF),
                );
                s[2] = s[2].wrapping_add(
                    (((s[7] & 0x00FF_0000) | (s[6] & 0x0000_FF00) | (s[5] & 0x0000_00FF)) << 8)
                        | ((s[4] & 0xFF00_0000) >> 24),
                );
                s[1] = s[1].wrapping_add(
                    (((s[7] & 0x0000_FF00) | (s[6] & 0x0000_00FF)) << 16)
                        | (((s[5] & 0xFF00_0000) | (s[4] & 0x00FF_0000)) >> 16),
                );
                s[0] = s[0].wrapping_add(
                    ((s[7] & 0x0000_00FF) << 24)
                        | (((s[6] & 0xFF00_0000) | (s[5] & 0x00FF_0000) | (s[4] & 0x0000_FF00))
                            >> 8),
                );
            }
            160 => {
                s[4] = s[4].wrapping_add(
                    ((s[7] & 0xFE00_0000) | (s[6] & 0x01F8_0000) | (s[5] & 0x0007_F000)) >> 12,
                );
                s[3] = s[3].wrapping_add(
                    ((s[7] & 0x01F8_0000) | (s[6] & 0x0007_F000) | (s[5] & 0x0000_0FC0)) >> 6,
                );
                s[2] = s[2].wrapping_add(
                    (s[7] & 0x0007_F000) | (s[6] & 0x0000_0FC0) | (s[5] & 0x0000_003F),
                );
                s[1] = s[1].wrapping_add(
                    ((s[7] & 0x0000_0FC0) | (s[6] & 0x0000_003F) | (s[5] & 0xFE00_0000))
                        .rotate_right(25),
                );
                s[0] = s[0].wrapping_add(
                    ((s[7] & 0x0000_003F) | (s[6] & 0xFE00_0000) | (s[5] & 0x01F8_0000))
                        .rotate_right(19),
                );
            }
            192 => {
                s[5] = s[5].wrapping_add(((s[7] & 0xFC00_0000) | (s[6] & 0x03E0_0000)) >> 21);
                s[4] = s[4].wrapping_add(((s[7] & 0x03E0_0000) | (s[6] & 0x001F_0000)) >> 16);
                s[3] = s[3].wrapping_add(((s[7] & 0x001F_0000) | (s[6] & 0x0000_FC00)) >> 10);
                s[2] = s[2].wrapping_add(((s[7] & 0x0000_FC00) | (s[6] & 0x0000_03E0)) >> 5);
                s[1] = s[1].wrapping_add((s[7] & 0x0000_03E0) | (s[6] & 0x0000_001F));
                s[0] = s[0]
                    .wrapping_add(((s[7] & 0x0000_001F) | (s[6] & 0xFC00_0000)).rotate_right(26));
            }
            224 => {
                s[6] = s[6].wrapping_add(s[7] & 0x0000_000F);
                s[5] = s[5].wrapping_add((s[7] >> 4) & 0x0000_001F);
                s[4] = s[4].wrapping_add((s[7] >> 9) & 0x0000_000F);
                s[3] = s[3].wrapping_add((s[7] >> 13) & 0x0000_001F);
                s[2] = s[2].wrapping_add((s[7] >> 18) & 0x0000_000F);
                s[1] = s[1].wrapping_add((s[7] >> 22) & 0x0000_001F);
                s[0] = s[0].wrapping_add((s[7] >> 27) & 0x0000_001F);
            }
            _ => {}
        }
    }
}

impl HashState for Haval {
    fn update(&mut self, data: &[u8]) {
        let passes = self.passes;
        let state = &mut self.state;
        md_update32(&mut self.count, &mut self.buffer, data, |b| {
            transform(passes, state, b);
        });
    }

    #[allow(clippy::cast_possible_truncation)]
    fn finish(&mut self, out: &mut [u8]) {
        const VERSION: u8 = 0x01;
        let mut bits = [0u8; 10];
        bits[0] =
            (VERSION & 0x07) | ((self.passes & 0x07) << 3) | (((self.output & 0x03) as u8) << 6);
        bits[1] = (self.output >> 2) as u8;
        put_le32(&mut bits[2..], &self.count);
        let index = ((self.count[0] >> 3) & 0x7f) as usize;
        let pad_len = if index < 118 {
            118 - index
        } else {
            246 - index
        };
        let mut padding = [0u8; 246];
        padding[0] = 1;
        self.update(&padding[..pad_len]);
        self.update(&bits);
        self.fold();
        put_le32(out, &self.state);
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u32s(&self.state)
            .u32s(&self.count)
            .bytes(&self.buffer)
            .u8(self.passes)
            .u16(self.output)
            .finish(184)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        r.u32s(&mut self.state);
        r.u32s(&mut self.count);
        r.bytes(&mut self.buffer);
    }
}
