//! A dry run of the first steps of php-src's bundled CDF reader
//! (`libmagic/cdf.c`: `cdf_read_header`, `cdf_read_sat`, `cdf_count_chain`,
//! `cdf_read_ssat`), to learn how much it would allocate.
//!
//! The bundled libmagic allocates with `emalloc()`, so a crafted Compound
//! Document whose short-SAT chain is long (`oom_cdf_sector_chain.phpt`) makes
//! PHP fail with "Allowed memory size … exhausted" instead of allocating
//! hundreds of megabytes.  The system libmagic uses `malloc()`, which the
//! memory limit cannot see; the binding therefore reserves the same amounts
//! before handing the data to libmagic.

/// `CDF_MAGIC` (`D0 CF 11 E0 A1 B1 1A E1`).
const MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// `CDF_SECID_FREE`.
const SECID_FREE: i32 = -1;
/// `CDF_SECID_END_OF_CHAIN`.
const SECID_END_OF_CHAIN: i32 = -2;
/// `CDF_LOOP_LIMIT`.
const LOOP_LIMIT: usize = 10_000;
/// `CDF_SAT_LIMIT`.
const SAT_LIMIT: usize = 16 * 1024 * 1024;
/// Entries of `h_master_sat` in the header.
const HEADER_MASTER_SAT: usize = 109;

/// Random access to the examined data (`cdf_read()`): fills `buf` from
/// `offset`, `false` when the data is too short.
pub(crate) trait Source {
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> bool;
}

impl Source for [u8] {
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> bool {
        let Ok(start) = usize::try_from(offset) else {
            return false;
        };
        match self.get(start..start.saturating_add(buf.len())) {
            Some(src) if src.len() == buf.len() => {
                buf.copy_from_slice(src);
                true
            }
            _ => false,
        }
    }
}

impl Source for std::fs::File {
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> bool {
        std::os::unix::fs::FileExt::read_exact_at(self, buf, offset).is_ok()
    }
}

fn le32(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

/// The allocations `file_trycdf()` makes up to and including the short
/// SAT, in order (`[sat, msa, ssat]`, the ones reached); empty when the
/// data is not a CDF document or the reader gives up before allocating.
pub(crate) fn allocations(src: &(impl Source + ?Sized)) -> Vec<usize> {
    let mut out = Vec::new();
    let mut h = [0u8; 512];
    if !src.read_exact_at(0, &mut h) || h[..8] != MAGIC {
        return out;
    }
    let p2 = le16(&h, 0x1E);
    if p2 > 20 || le16(&h, 0x20) > 20 {
        return out;
    }
    let ss = 1usize << p2;
    let sector = |id: i32, buf: &mut [u8]| -> bool {
        let Ok(id) = u64::try_from(id) else {
            return false;
        };
        src.read_exact_at((id + 1) << p2, buf)
    };
    let master: Vec<i32> = (0..HEADER_MASTER_SAT)
        .map(|k| le32(&h, 0x4C + 4 * k))
        .collect();
    let num_master = usize::try_from(u32::from_le_bytes([h[0x48], h[0x49], h[0x4A], h[0x4B]]))
        .unwrap_or(usize::MAX);

    // cdf_read_sat()
    let mut i = master
        .iter()
        .position(|&s| s == SECID_FREE)
        .unwrap_or(HEADER_MASTER_SAT);
    let nsatpersec = ss / 4 - 1;
    let sec_limit = u32::MAX as usize / (64 * ss);
    if (nsatpersec > 0 && num_master > sec_limit / nsatpersec) || i > sec_limit {
        return out;
    }
    let mut sat_len = num_master * nsatpersec + i;
    if sat_len > SAT_LIMIT / ss {
        return out;
    }
    out.push(sat_len * ss);
    let mut sat = vec![0u8; sat_len * ss];
    for (k, &sid) in master.iter().enumerate() {
        if sid < 0 {
            break;
        }
        if !sector(sid, &mut sat[ss * k..ss * (k + 1)]) {
            return out;
        }
    }
    out.push(ss);
    let mut msa = vec![0u8; ss];
    let mut mid = le32(&h, 0x44);
    'chain: for j in 0..num_master {
        if mid < 0 {
            break;
        }
        if j >= LOOP_LIMIT || !sector(mid, &mut msa) {
            return out;
        }
        for k in 0..nsatpersec {
            let sec = le32(&msa, 4 * k);
            if sec < 0 {
                break 'chain;
            }
            if i >= sat_len || !sector(sec, &mut sat[ss * i..ss * (i + 1)]) {
                return out;
            }
            i += 1;
        }
        mid = le32(&msa, 4 * nsatpersec);
    }
    sat_len = i;
    sat.truncate(sat_len * ss);

    // cdf_read_ssat() → cdf_count_chain()
    if let Some(n) = count_chain(&sat, le32(&h, 0x3C), sat_len * ss / 4) {
        out.push(n * ss);
    }
    out
}

/// `cdf_count_chain()`: the length of the chain starting at `sid`, `None`
/// for a broken one.
fn count_chain(sat: &[u8], mut sid: i32, max_sector: usize) -> Option<usize> {
    if sid == SECID_END_OF_CHAIN {
        return Some(0);
    }
    let mut n = 0;
    while let Ok(idx) = usize::try_from(sid) {
        if n >= LOOP_LIMIT || idx >= max_sector {
            return None;
        }
        sid = le32(sat, 4 * idx);
        n += 1;
    }
    (n > 0).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The document of php-src's `oom_cdf_sector_chain.phpt`.
    fn oom_document() -> Vec<u8> {
        let ss = 32 * 1024;
        let mut d = Vec::new();
        d.extend_from_slice(&MAGIC);
        d.extend_from_slice(&[0; 16]);
        for v in [0x3e_u16, 4, 0xfffe, 15, 6] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&[0; 10]);
        for v in [2_i32, 2, 0, 4096, 2, 10_000, -2, 0, 0, 1] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        for _ in 0..107 {
            d.extend_from_slice(&(-1_i32).to_le_bytes());
        }
        assert_eq!(d.len(), 512);
        d.resize(ss, 0);
        let mut fat = vec![-1_i32; 16384];
        fat[0] = -3;
        fat[1] = -3;
        for (i, f) in fat.iter_mut().enumerate().take(10_001).skip(2) {
            *f = i32::try_from(i + 1).unwrap_or(-1);
        }
        fat[10_001] = -2;
        for f in fat {
            d.extend_from_slice(&f.to_le_bytes());
        }
        d.resize(4 * ss, 0);
        d
    }

    #[test]
    fn short_sat_chain_allocation() {
        let d = oom_document();
        let a = allocations(d.as_slice());
        assert_eq!(a, vec![2 * 32768, 32768, 10_000 * 32768]);
    }

    #[test]
    fn not_cdf() {
        assert!(allocations(b"hello".as_slice()).is_empty());
        let mut d = oom_document();
        d[0x1E] = 21;
        assert!(allocations(d.as_slice()).is_empty());
    }

    #[test]
    fn chains() {
        let sat: Vec<u8> = [1_i32, 2, -2, 0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(count_chain(&sat, 0, 4), Some(3));
        assert_eq!(count_chain(&sat, -2, 4), Some(0));
        assert_eq!(count_chain(&sat, 3, 3), None);
        assert_eq!(count_chain(&sat, -1, 4), None);
    }
}
