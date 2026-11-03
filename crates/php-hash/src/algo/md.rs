//! MD2, MD4 and MD5 (ref: `ext/hash/hash_md.c`, ext/standard/md5.c).

use super::{le_words, md_update32, put_le32, HashState, ImageReader, ImageWriter, PADDING};

const MD2_S: [u8; 256] = [
    41, 46, 67, 201, 162, 216, 124, 1, 61, 54, 84, 161, 236, 240, 6, 19, 98, 167, 5, 243, 192, 199,
    115, 140, 152, 147, 43, 217, 188, 76, 130, 202, 30, 155, 87, 60, 253, 212, 224, 22, 103, 66,
    111, 24, 138, 23, 229, 18, 190, 78, 196, 214, 218, 158, 222, 73, 160, 251, 245, 142, 187, 47,
    238, 122, 169, 104, 121, 145, 21, 178, 7, 63, 148, 194, 16, 137, 11, 34, 95, 33, 128, 127, 93,
    154, 90, 144, 50, 39, 53, 62, 204, 231, 191, 247, 151, 3, 255, 25, 48, 179, 72, 165, 181, 209,
    215, 94, 146, 42, 172, 86, 170, 198, 79, 184, 56, 210, 150, 164, 125, 182, 118, 252, 107, 226,
    156, 116, 4, 241, 69, 157, 112, 89, 100, 113, 135, 32, 134, 91, 207, 101, 230, 45, 168, 2, 27,
    96, 37, 173, 174, 176, 185, 246, 28, 70, 97, 105, 52, 64, 126, 15, 85, 71, 163, 35, 221, 81,
    175, 58, 195, 92, 249, 206, 186, 197, 234, 38, 44, 83, 13, 110, 133, 40, 132, 9, 211, 223, 205,
    244, 65, 129, 77, 82, 106, 220, 55, 200, 108, 193, 171, 250, 36, 225, 123, 8, 12, 189, 177, 74,
    120, 136, 149, 139, 227, 99, 232, 109, 233, 203, 213, 254, 59, 0, 29, 57, 242, 239, 183, 14,
    102, 88, 208, 228, 166, 119, 114, 248, 235, 117, 75, 10, 49, 68, 80, 180, 143, 237, 31, 26,
    219, 153, 141, 51, 159, 17, 131, 20,
];

/// `PHP_MD2_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Md2 {
    state: [u8; 48],
    checksum: [u8; 16],
    buffer: [u8; 16],
    in_buffer: u8,
}

impl Md2 {
    pub(crate) fn new() -> Self {
        Self {
            state: [0; 48],
            checksum: [0; 16],
            buffer: [0; 16],
            in_buffer: 0,
        }
    }

    fn transform(state: &mut [u8; 48], checksum: &mut [u8; 16], block: &[u8]) {
        for i in 0..16 {
            state[16 + i] = block[i];
            state[32 + i] = state[16 + i] ^ state[i];
        }
        let mut t: u8 = 0;
        for i in 0..18u8 {
            for s in state.iter_mut() {
                *s ^= MD2_S[t as usize];
                t = *s;
            }
            t = t.wrapping_add(i);
        }
        let mut t = checksum[15];
        for i in 0..16 {
            checksum[i] ^= MD2_S[(block[i] ^ t) as usize];
            t = checksum[i];
        }
    }
}

impl HashState for Md2 {
    fn update(&mut self, data: &[u8]) {
        let mut p = data;
        let used = self.in_buffer as usize;
        if used != 0 {
            if used + p.len() < 16 {
                self.buffer[used..used + p.len()].copy_from_slice(p);
                #[allow(clippy::cast_possible_truncation)]
                {
                    self.in_buffer = self.in_buffer.wrapping_add(p.len() as u8);
                }
                return;
            }
            let fill = 16 - used;
            self.buffer[used..].copy_from_slice(&p[..fill]);
            let block = self.buffer;
            Self::transform(&mut self.state, &mut self.checksum, &block);
            p = &p[fill..];
            self.in_buffer = 0;
        }
        while p.len() >= 16 {
            Self::transform(&mut self.state, &mut self.checksum, &p[..16]);
            p = &p[16..];
        }
        if !p.is_empty() {
            self.buffer[..p.len()].copy_from_slice(p);
            #[allow(clippy::cast_possible_truncation)]
            {
                self.in_buffer = p.len() as u8;
            }
        }
    }

    fn finish(&mut self, out: &mut [u8]) {
        let used = self.in_buffer as usize;
        #[allow(clippy::cast_possible_truncation)]
        let pad = (16 - used) as u8;
        for b in &mut self.buffer[used..] {
            *b = pad;
        }
        let block = self.buffer;
        Self::transform(&mut self.state, &mut self.checksum, &block);
        let checksum = self.checksum;
        Self::transform(&mut self.state, &mut self.checksum, &checksum);
        out.copy_from_slice(&self.state[..16]);
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .bytes(&self.state)
            .bytes(&self.checksum)
            .bytes(&self.buffer)
            .u8(self.in_buffer)
            .finish(81)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        r.bytes(&mut self.state);
        r.bytes(&mut self.checksum);
        r.bytes(&mut self.buffer);
        self.in_buffer = r.u8();
    }

    fn is_valid(&self) -> bool {
        (self.in_buffer as usize) < self.buffer.len()
    }
}

/// `PHP_MD4_CTX`.
#[derive(Clone, Debug)]
pub(crate) struct Md4 {
    state: [u32; 4],
    count: [u32; 2],
    buffer: [u8; 64],
}

impl Md4 {
    pub(crate) fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            count: [0; 2],
            buffer: [0; 64],
        }
    }

    fn transform(state: &mut [u32; 4], block: &[u8]) {
        let x: [u32; 16] = le_words(block);
        let [mut a, mut b, mut c, mut d] = *state;
        let f = |x: u32, y: u32, z: u32| z ^ (x & (y ^ z));
        let g = |x: u32, y: u32, z: u32| (x & (y | z)) | (y & z);
        let h = |x: u32, y: u32, z: u32| x ^ y ^ z;
        for &k in &[0usize, 4, 8, 12] {
            a = a.wrapping_add(f(b, c, d)).wrapping_add(x[k]).rotate_left(3);
            d = d
                .wrapping_add(f(a, b, c))
                .wrapping_add(x[k + 1])
                .rotate_left(7);
            c = c
                .wrapping_add(f(d, a, b))
                .wrapping_add(x[k + 2])
                .rotate_left(11);
            b = b
                .wrapping_add(f(c, d, a))
                .wrapping_add(x[k + 3])
                .rotate_left(19);
        }
        for k in 0..4 {
            let r2 = |v: u32, w: u32| v.wrapping_add(w).wrapping_add(0x5A82_7999);
            a = r2(a.wrapping_add(g(b, c, d)), x[k]).rotate_left(3);
            d = r2(d.wrapping_add(g(a, b, c)), x[k + 4]).rotate_left(5);
            c = r2(c.wrapping_add(g(d, a, b)), x[k + 8]).rotate_left(9);
            b = r2(b.wrapping_add(g(c, d, a)), x[k + 12]).rotate_left(13);
        }
        for &k in &[0usize, 2, 1, 3] {
            let r3 = |v: u32, w: u32| v.wrapping_add(w).wrapping_add(0x6ED9_EBA1);
            a = r3(a.wrapping_add(h(b, c, d)), x[k]).rotate_left(3);
            d = r3(d.wrapping_add(h(a, b, c)), x[k + 8]).rotate_left(9);
            c = r3(c.wrapping_add(h(d, a, b)), x[k + 4]).rotate_left(11);
            b = r3(b.wrapping_add(h(c, d, a)), x[k + 12]).rotate_left(15);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }
}

impl HashState for Md4 {
    fn update(&mut self, data: &[u8]) {
        let state = &mut self.state;
        md_update32(&mut self.count, &mut self.buffer, data, |b| {
            Self::transform(state, b);
        });
    }

    fn finish(&mut self, out: &mut [u8]) {
        let mut bits = [0u8; 8];
        put_le32(&mut bits, &self.count);
        let index = ((self.count[0] >> 3) & 0x3f) as usize;
        let pad_len = if index < 56 { 56 - index } else { 120 - index };
        self.update(&PADDING[..pad_len]);
        self.update(&bits);
        put_le32(out, &self.state);
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u32s(&self.state)
            .u32s(&self.count)
            .bytes(&self.buffer)
            .finish(88)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        r.u32s(&mut self.state);
        r.u32s(&mut self.count);
        r.bytes(&mut self.buffer);
    }
}

/// `PHP_MD5_CTX` (Solar Designer's implementation as used by ext/standard).
#[derive(Clone, Debug)]
pub(crate) struct Md5 {
    lo: u32,
    hi: u32,
    abcd: [u32; 4],
    buffer: [u8; 64],
    /// Scratch block; never written on x86 builds of PHP but part of the
    /// serialized image.
    block: [u32; 16],
}

impl Md5 {
    pub(crate) fn new() -> Self {
        Self {
            lo: 0,
            hi: 0,
            abcd: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            buffer: [0; 64],
            block: [0; 16],
        }
    }

    fn body(abcd: &mut [u32; 4], data: &[u8]) {
        const S: [u32; 64] = [
            7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20,
            5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
            6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
        ];
        const K: [u32; 64] = [
            0xd76a_a478,
            0xe8c7_b756,
            0x2420_70db,
            0xc1bd_ceee,
            0xf57c_0faf,
            0x4787_c62a,
            0xa830_4613,
            0xfd46_9501,
            0x6980_98d8,
            0x8b44_f7af,
            0xffff_5bb1,
            0x895c_d7be,
            0x6b90_1122,
            0xfd98_7193,
            0xa679_438e,
            0x49b4_0821,
            0xf61e_2562,
            0xc040_b340,
            0x265e_5a51,
            0xe9b6_c7aa,
            0xd62f_105d,
            0x0244_1453,
            0xd8a1_e681,
            0xe7d3_fbc8,
            0x21e1_cde6,
            0xc337_07d6,
            0xf4d5_0d87,
            0x455a_14ed,
            0xa9e3_e905,
            0xfcef_a3f8,
            0x676f_02d9,
            0x8d2a_4c8a,
            0xfffa_3942,
            0x8771_f681,
            0x6d9d_6122,
            0xfde5_380c,
            0xa4be_ea44,
            0x4bde_cfa9,
            0xf6bb_4b60,
            0xbebf_bc70,
            0x289b_7ec6,
            0xeaa1_27fa,
            0xd4ef_3085,
            0x0488_1d05,
            0xd9d4_d039,
            0xe6db_99e5,
            0x1fa2_7cf8,
            0xc4ac_5665,
            0xf429_2244,
            0x432a_ff97,
            0xab94_23a7,
            0xfc93_a039,
            0x655b_59c3,
            0x8f0c_cc92,
            0xffef_f47d,
            0x8584_5dd1,
            0x6fa8_7e4f,
            0xfe2c_e6e0,
            0xa301_4314,
            0x4e08_11a1,
            0xf753_7e82,
            0xbd3a_f235,
            0x2ad7_d2bb,
            0xeb86_d391,
        ];
        for chunk in data.chunks_exact(64) {
            let x: [u32; 16] = le_words(chunk);
            let [mut a, mut b, mut c, mut d] = *abcd;
            for i in 0..64 {
                let (f, g) = match i / 16 {
                    0 => (d ^ (b & (c ^ d)), i),
                    1 => (c ^ (d & (b ^ c)), (5 * i + 1) % 16),
                    2 => (b ^ c ^ d, (3 * i + 5) % 16),
                    _ => (c ^ (b | !d), (7 * i) % 16),
                };
                let t = a.wrapping_add(f).wrapping_add(x[g]).wrapping_add(K[i]);
                a = d;
                d = c;
                c = b;
                b = b.wrapping_add(t.rotate_left(S[i]));
            }
            abcd[0] = abcd[0].wrapping_add(a);
            abcd[1] = abcd[1].wrapping_add(b);
            abcd[2] = abcd[2].wrapping_add(c);
            abcd[3] = abcd[3].wrapping_add(d);
        }
    }
}

impl HashState for Md5 {
    fn update(&mut self, data: &[u8]) {
        let saved_lo = self.lo;
        #[allow(clippy::cast_possible_truncation)]
        {
            self.lo = saved_lo.wrapping_add(data.len() as u32) & 0x1fff_ffff;
            if self.lo < saved_lo {
                self.hi = self.hi.wrapping_add(1);
            }
            self.hi = self.hi.wrapping_add((data.len() >> 29) as u32);
        }
        let used = (saved_lo & 0x3f) as usize;
        let mut data = data;
        if used != 0 {
            let free = 64 - used;
            if data.len() < free {
                self.buffer[used..used + data.len()].copy_from_slice(data);
                return;
            }
            self.buffer[used..].copy_from_slice(&data[..free]);
            data = &data[free..];
            let buf = self.buffer;
            Self::body(&mut self.abcd, &buf);
        }
        if data.len() >= 64 {
            let whole = data.len() & !0x3f;
            Self::body(&mut self.abcd, &data[..whole]);
            data = &data[whole..];
        }
        self.buffer[..data.len()].copy_from_slice(data);
    }

    fn finish(&mut self, out: &mut [u8]) {
        let mut used = (self.lo & 0x3f) as usize;
        self.buffer[used] = 0x80;
        used += 1;
        let mut free = 64 - used;
        if free < 8 {
            self.buffer[used..].fill(0);
            let buf = self.buffer;
            Self::body(&mut self.abcd, &buf);
            used = 0;
            free = 64;
        }
        self.buffer[used..used + free - 8].fill(0);
        self.lo <<= 3;
        self.buffer[56..60].copy_from_slice(&self.lo.to_le_bytes());
        self.buffer[60..64].copy_from_slice(&self.hi.to_le_bytes());
        let buf = self.buffer;
        Self::body(&mut self.abcd, &buf);
        put_le32(out, &self.abcd);
    }

    fn image(&self) -> Vec<u8> {
        ImageWriter::new()
            .u32(self.lo)
            .u32(self.hi)
            .u32s(&self.abcd)
            .bytes(&self.buffer)
            .u32s(&self.block)
            .finish(152)
    }

    fn load_image(&mut self, image: &[u8]) {
        let mut r = ImageReader::new(image);
        self.lo = r.u32();
        self.hi = r.u32();
        r.u32s(&mut self.abcd);
        r.bytes(&mut self.buffer);
        r.u32s(&mut self.block);
    }
}
