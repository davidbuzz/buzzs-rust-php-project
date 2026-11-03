//! SHA-224/256 and SHA-384/512, SHA-512/224, SHA-512/256 (ref: `ext/hash/hash_sha.c`).

use super::{
    be_words, md_update32, md_update64, put_be32, HashState, ImageReader, ImageWriter, PADDING,
};

const K256: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

const K512: [u64; 80] = [
    0x428a_2f98_d728_ae22,
    0x7137_4491_23ef_65cd,
    0xb5c0_fbcf_ec4d_3b2f,
    0xe9b5_dba5_8189_dbbc,
    0x3956_c25b_f348_b538,
    0x59f1_11f1_b605_d019,
    0x923f_82a4_af19_4f9b,
    0xab1c_5ed5_da6d_8118,
    0xd807_aa98_a303_0242,
    0x1283_5b01_4570_6fbe,
    0x2431_85be_4ee4_b28c,
    0x550c_7dc3_d5ff_b4e2,
    0x72be_5d74_f27b_896f,
    0x80de_b1fe_3b16_96b1,
    0x9bdc_06a7_25c7_1235,
    0xc19b_f174_cf69_2694,
    0xe49b_69c1_9ef1_4ad2,
    0xefbe_4786_384f_25e3,
    0x0fc1_9dc6_8b8c_d5b5,
    0x240c_a1cc_77ac_9c65,
    0x2de9_2c6f_592b_0275,
    0x4a74_84aa_6ea6_e483,
    0x5cb0_a9dc_bd41_fbd4,
    0x76f9_88da_8311_53b5,
    0x983e_5152_ee66_dfab,
    0xa831_c66d_2db4_3210,
    0xb003_27c8_98fb_213f,
    0xbf59_7fc7_beef_0ee4,
    0xc6e0_0bf3_3da8_8fc2,
    0xd5a7_9147_930a_a725,
    0x06ca_6351_e003_826f,
    0x1429_2967_0a0e_6e70,
    0x27b7_0a85_46d2_2ffc,
    0x2e1b_2138_5c26_c926,
    0x4d2c_6dfc_5ac4_2aed,
    0x5338_0d13_9d95_b3df,
    0x650a_7354_8baf_63de,
    0x766a_0abb_3c77_b2a8,
    0x81c2_c92e_47ed_aee6,
    0x9272_2c85_1482_353b,
    0xa2bf_e8a1_4cf1_0364,
    0xa81a_664b_bc42_3001,
    0xc24b_8b70_d0f8_9791,
    0xc76c_51a3_0654_be30,
    0xd192_e819_d6ef_5218,
    0xd699_0624_5565_a910,
    0xf40e_3585_5771_202a,
    0x106a_a070_32bb_d1b8,
    0x19a4_c116_b8d2_d0c8,
    0x1e37_6c08_5141_ab53,
    0x2748_774c_df8e_eb99,
    0x34b0_bcb5_e19b_48a8,
    0x391c_0cb3_c5c9_5a63,
    0x4ed8_aa4a_e341_8acb,
    0x5b9c_ca4f_7763_e373,
    0x682e_6ff3_d6b2_b8a3,
    0x748f_82ee_5def_b2fc,
    0x78a5_636f_4317_2f60,
    0x84c8_7814_a1f0_ab72,
    0x8cc7_0208_1a64_39ec,
    0x90be_fffa_2363_1e28,
    0xa450_6ceb_de82_bde9,
    0xbef9_a3f7_b2c6_7915,
    0xc671_78f2_e372_532b,
    0xca27_3ece_ea26_619c,
    0xd186_b8c7_21c0_c207,
    0xeada_7dd6_cde0_eb1e,
    0xf57d_4f7f_ee6e_d178,
    0x06f0_67aa_7217_6fba,
    0x0a63_7dc5_a2c8_98a6,
    0x113f_9804_bef9_0dae,
    0x1b71_0b35_131c_471b,
    0x28db_77f5_2304_7d84,
    0x32ca_ab7b_40c7_2493,
    0x3c9e_be0a_15c9_bebc,
    0x431d_67c4_9c10_0d4c,
    0x4cc5_d4be_cb3e_42b6,
    0x597f_299c_fc65_7e2a,
    0x5fcb_6fab_3ad6_faec,
    0x6c44_198c_4a47_5817,
];

/// Which member of the SHA-256 family a [`Sha256`] context computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sha256Variant {
    Sha224,
    Sha256,
}

/// `PHP_SHA224_CTX` / `PHP_SHA256_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Sha256 {
    state: [u32; 8],
    count: [u32; 2],
    buffer: [u8; 64],
}

impl Sha256 {
    pub(crate) fn new(variant: Sha256Variant) -> Self {
        let state = match variant {
            Sha256Variant::Sha224 => [
                0xc105_9ed8,
                0x367c_d507,
                0x3070_dd17,
                0xf70e_5939,
                0xffc0_0b31,
                0x6858_1511,
                0x64f9_8fa7,
                0xbefa_4fa4,
            ],
            Sha256Variant::Sha256 => [
                0x6a09_e667,
                0xbb67_ae85,
                0x3c6e_f372,
                0xa54f_f53a,
                0x510e_527f,
                0x9b05_688c,
                0x1f83_d9ab,
                0x5be0_cd19,
            ],
        };
        Self {
            state,
            count: [0; 2],
            buffer: [0; 64],
        }
    }

    fn transform(state: &mut [u32; 8], block: &[u8]) {
        let mut w = [0u32; 64];
        w[..16].copy_from_slice(&be_words::<16>(block));
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = *state;
        for i in 0..64 {
            let [a, b, c, d, e, f, g, h] = v;
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K256[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), a, b, c, d.wrapping_add(t1), e, f, g];
        }
        for (s, x) in state.iter_mut().zip(v) {
            *s = s.wrapping_add(x);
        }
    }
}

impl HashState for Sha256 {
    fn update(&mut self, data: &[u8]) {
        let state = &mut self.state;
        md_update32(&mut self.count, &mut self.buffer, data, |b| {
            Self::transform(state, b);
        });
    }

    fn finish(&mut self, out: &mut [u8]) {
        let mut bits = [0u8; 8];
        put_be32(&mut bits, &[self.count[1], self.count[0]]);
        let index = ((self.count[0] >> 3) & 0x3f) as usize;
        let pad_len = if index < 56 { 56 - index } else { 120 - index };
        self.update(&PADDING[..pad_len]);
        self.update(&bits);
        put_be32(out, &self.state);
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u32s(&self.state)
            .u32s(&self.count)
            .bytes(&self.buffer)
            .finish(104)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        r.u32s(&mut self.state);
        r.u32s(&mut self.count);
        r.bytes(&mut self.buffer);
    }
}

/// Which member of the SHA-512 family a [`Sha512`] context computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sha512Variant {
    Sha384,
    Sha512_224,
    Sha512_256,
    Sha512,
}

/// `PHP_SHA384_CTX` / `PHP_SHA512_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Sha512 {
    state: [u64; 8],
    count: [u64; 2],
    buffer: [u8; 128],
}

impl Sha512 {
    pub(crate) fn new(variant: Sha512Variant) -> Self {
        let state = match variant {
            Sha512Variant::Sha384 => [
                0xcbbb_9d5d_c105_9ed8,
                0x629a_292a_367c_d507,
                0x9159_015a_3070_dd17,
                0x152f_ecd8_f70e_5939,
                0x6733_2667_ffc0_0b31,
                0x8eb4_4a87_6858_1511,
                0xdb0c_2e0d_64f9_8fa7,
                0x47b5_481d_befa_4fa4,
            ],
            Sha512Variant::Sha512_224 => [
                0x8C3D_37C8_1954_4DA2,
                0x73E1_9966_89DC_D4D6,
                0x1DFA_B7AE_32FF_9C82,
                0x679D_D514_582F_9FCF,
                0x0F6D_2B69_7BD4_4DA8,
                0x77E3_6F73_04C4_8942,
                0x3F9D_85A8_6A1D_36C8,
                0x1112_E6AD_91D6_92A1,
            ],
            Sha512Variant::Sha512_256 => [
                0x2231_2194_FC2B_F72C,
                0x9F55_5FA3_C84C_64C2,
                0x2393_B86B_6F53_B151,
                0x9638_7719_5940_EABD,
                0x9628_3EE2_A88E_FFE3,
                0xBE5E_1E25_5386_3992,
                0x2B01_99FC_2C85_B8AA,
                0x0EB7_2DDC_81C5_2CA2,
            ],
            Sha512Variant::Sha512 => [
                0x6a09_e667_f3bc_c908,
                0xbb67_ae85_84ca_a73b,
                0x3c6e_f372_fe94_f82b,
                0xa54f_f53a_5f1d_36f1,
                0x510e_527f_ade6_82d1,
                0x9b05_688c_2b3e_6c1f,
                0x1f83_d9ab_fb41_bd6b,
                0x5be0_cd19_137e_2179,
            ],
        };
        Self {
            state,
            count: [0; 2],
            buffer: [0; 128],
        }
    }

    fn transform(state: &mut [u64; 8], block: &[u8]) {
        let mut w = [0u64; 80];
        for (wi, c) in w.iter_mut().zip(block.chunks_exact(8)) {
            *wi = u64::from_be_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = *state;
        for i in 0..80 {
            let [a, b, c, d, e, f, g, h] = v;
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K512[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), a, b, c, d.wrapping_add(t1), e, f, g];
        }
        for (s, x) in state.iter_mut().zip(v) {
            *s = s.wrapping_add(x);
        }
    }
}

impl HashState for Sha512 {
    fn update(&mut self, data: &[u8]) {
        let state = &mut self.state;
        md_update64(&mut self.count, &mut self.buffer, data, |b| {
            Self::transform(state, b);
        });
    }

    fn finish(&mut self, out: &mut [u8]) {
        let mut bits = [0u8; 16];
        bits[..8].copy_from_slice(&self.count[1].to_be_bytes());
        bits[8..].copy_from_slice(&self.count[0].to_be_bytes());
        let index = ((self.count[0] >> 3) & 0x7f) as usize;
        let pad_len = if index < 112 {
            112 - index
        } else {
            240 - index
        };
        let mut padding = [0u8; 240];
        padding[0] = 0x80;
        self.update(&padding[..pad_len]);
        self.update(&bits);
        let mut full = [0u8; 64];
        for (c, s) in full.chunks_exact_mut(8).zip(self.state) {
            c.copy_from_slice(&s.to_be_bytes());
        }
        out.copy_from_slice(&full[..out.len()]);
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u64s(&self.state)
            .u64s(&self.count)
            .bytes(&self.buffer)
            .finish(208)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        r.u64s(&mut self.state);
        r.u64s(&mut self.count);
        r.bytes(&mut self.buffer);
    }
}
