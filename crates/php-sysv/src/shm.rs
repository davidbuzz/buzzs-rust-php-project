// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/sysvshm` (ref: `ext/sysvshm/sysvshm.c`, `php_sysvshm.h`).
//!
//! A PHP shared-memory segment starts with a `sysvshm_chunk_head`
//! (`magic[8] = "PHP_SM\0"`, then `size_t start, end, free, total`)
//! followed by packed `sysvshm_chunk`s (`zend_long key; size_t length;
//! size_t next;` then the serialized bytes), each padded to a multiple of
//! `sizeof(zend_long)`.  The layout is reproduced byte for byte so PHP and
//! phprust processes can share segments.

use crate::{c_int, key_from_arg, strerror, sys, xkey, xlong, SysvError, SysvResult};

/// Default segment size when `$size` is null and `sysvshm.init_mem` is not
/// configured.
pub const DEFAULT_INIT_MEM: i64 = 10000;
/// Default `$permissions` of `shm_attach()`.
pub const DEFAULT_PERMISSIONS: i64 = 0o666;

/// `sizeof(sysvshm_chunk_head)`.
pub const CHUNK_HEAD_SIZE: usize = 40;
/// `sizeof(sysvshm_chunk)` (24 header bytes + 1 `char mem`, padded).
pub const CHUNK_SIZE: usize = 32;
/// Offset of `mem` inside a chunk.
const CHUNK_DATA: usize = 24;
const MAGIC: &[u8; 7] = b"PHP_SM\0";

/// Error thrown by every `shm_*` function on a detached handle.
pub const ALREADY_DESTROYED: &str = "Shared memory block has already been destroyed";
/// Error thrown by `shm_put_var()` when serialization detached the handle.
pub const DESTROYED_BY_SERIALIZATION: &str =
    "Shared memory block has been destroyed by the serialization function";
/// Warning `shm_get_var()` emits (returning `false`) when unserializing the
/// stored bytes fails.
pub const DATA_CORRUPTED: &str = "Variable data in shared memory is corrupted";

/// Byte-addressed storage holding a PHP segment (the attached mapping, or
/// a plain buffer in tests).
trait Memory {
    fn size(&self) -> usize;
    fn read(&self, off: usize, out: &mut [u8]) -> bool;
    fn write(&mut self, off: usize, data: &[u8]) -> bool;
    fn copy_within(&mut self, src: usize, dst: usize, n: usize) -> bool;

    fn word(&self, off: usize) -> Option<u64> {
        let mut b = [0u8; 8];
        self.read(off, &mut b).then(|| u64::from_ne_bytes(b))
    }

    fn set_word(&mut self, off: usize, v: u64) -> bool {
        self.write(off, &v.to_ne_bytes())
    }
}

impl Memory for sys::Segment {
    fn size(&self) -> usize {
        self.len()
    }
    fn read(&self, off: usize, out: &mut [u8]) -> bool {
        sys::Segment::read(self, off, out)
    }
    fn write(&mut self, off: usize, data: &[u8]) -> bool {
        sys::Segment::write(self, off, data)
    }
    fn copy_within(&mut self, src: usize, dst: usize, n: usize) -> bool {
        sys::Segment::copy_within(self, src, dst, n)
    }
}

impl Memory for Vec<u8> {
    fn size(&self) -> usize {
        self.len()
    }
    fn read(&self, off: usize, out: &mut [u8]) -> bool {
        match off.checked_add(out.len()).and_then(|e| self.get(off..e)) {
            Some(src) => {
                out.copy_from_slice(src);
                true
            }
            None => false,
        }
    }
    fn write(&mut self, off: usize, data: &[u8]) -> bool {
        match off
            .checked_add(data.len())
            .and_then(|e| self.get_mut(off..e))
        {
            Some(dst) => {
                dst.copy_from_slice(data);
                true
            }
            None => false,
        }
    }
    fn copy_within(&mut self, src: usize, dst: usize, n: usize) -> bool {
        let ok = src.checked_add(n).is_some_and(|e| e <= self.len())
            && dst.checked_add(n).is_some_and(|e| e <= self.len());
        if ok {
            <[u8]>::copy_within(self, src..src + n, dst);
        }
        ok
    }
}

/// Field offsets in `sysvshm_chunk_head`.
const H_START: usize = 8;
const H_END: usize = 16;
const H_FREE: usize = 24;
const H_TOTAL: usize = 32;

#[allow(clippy::cast_possible_truncation)]
fn to_usize(v: u64) -> usize {
    v as usize
}

/// Initialises the header unless it already carries the magic.
fn init_head(m: &mut impl Memory, size: usize) {
    let mut magic = [0u8; 8];
    m.read(0, &mut magic);
    let len = magic.iter().position(|&b| b == 0).unwrap_or(magic.len());
    if &magic[..len] == b"PHP_SM" && len < 8 {
        return;
    }
    m.write(0, MAGIC);
    m.set_word(H_START, CHUNK_HEAD_SIZE as u64);
    m.set_word(H_END, CHUNK_HEAD_SIZE as u64);
    m.set_word(H_TOTAL, size as u64);
    m.set_word(H_FREE, size.wrapping_sub(CHUNK_HEAD_SIZE) as u64);
}

/// `php_check_shm_data`: offset of the chunk holding `key`.
fn find(m: &impl Memory, key: i64) -> Option<usize> {
    let start = to_usize(m.word(H_START)?);
    let end = to_usize(m.word(H_END)?);
    let mut pos = start;
    loop {
        if pos >= end {
            return None;
        }
        #[allow(clippy::cast_possible_wrap)]
        if m.word(pos)? as i64 == key {
            return Some(pos);
        }
        let next = to_usize(m.word(pos + 16)?);
        pos = pos.wrapping_add(next);
        if next == 0 || pos < start {
            return None;
        }
    }
}

/// `php_remove_shm_data`.
fn remove_at(m: &mut impl Memory, pos: usize) {
    let (Some(next), Some(end), Some(free)) = (m.word(pos + 16), m.word(H_END), m.word(H_FREE))
    else {
        return;
    };
    let (next, end) = (to_usize(next), to_usize(end));
    let len = end.wrapping_sub(pos).wrapping_sub(next);
    m.set_word(H_FREE, free.wrapping_add(next as u64));
    m.set_word(H_END, end.wrapping_sub(next) as u64);
    if len > 0 {
        m.copy_within(pos.wrapping_add(next), pos, len);
    }
}

/// `php_put_shm_data`; `false` when there is not enough space.
fn put(m: &mut impl Memory, key: i64, data: &[u8]) -> bool {
    let word = std::mem::size_of::<i64>();
    let total = (data.len() + CHUNK_SIZE - 1) / word * word + word;
    if let Some(pos) = find(m, key) {
        if pos > 0 {
            remove_at(m, pos);
        }
    }
    let (Some(free), Some(end)) = (m.word(H_FREE), m.word(H_END)) else {
        return false;
    };
    let (free, end) = (to_usize(free), to_usize(end));
    if free < total {
        return false;
    }
    #[allow(clippy::cast_sign_loss)]
    let ok = m.set_word(end, key as u64)
        && m.set_word(end + 8, data.len() as u64)
        && m.set_word(end + 16, total as u64)
        && m.write(end + CHUNK_DATA, data);
    if !ok {
        return false;
    }
    m.set_word(H_END, (end + total) as u64);
    m.set_word(H_FREE, (free - total) as u64);
    true
}

/// The bytes stored in the chunk at `pos`.
fn data_at(m: &impl Memory, pos: usize) -> Option<Vec<u8>> {
    let len = to_usize(m.word(pos + 8)?);
    if pos.checked_add(CHUNK_DATA)?.checked_add(len)? > m.size() {
        return None;
    }
    let mut out = vec![0u8; len];
    m.read(pos + CHUNK_DATA, &mut out).then_some(out)
}

fn missing(key: i64) -> SysvError {
    SysvError::Warning(format!("Variable key {key} doesn't exist"))
}

/// A `SysvSharedMemory`.  Dropping it detaches the segment (PHP's
/// `free_obj`).
#[derive(Debug)]
pub struct SharedMemory {
    key: i32,
    id: i32,
    seg: Option<sys::Segment>,
}

impl SharedMemory {
    /// `shm_attach(int $key, ?int $size = null, int $permissions = 0666)`;
    /// `init_mem` is the `sysvshm.init_mem` setting ([`DEFAULT_INIT_MEM`]
    /// when unset), used when `$size` is null.
    ///
    /// An existing segment is reused whatever its size; otherwise one of
    /// `$size` bytes is created.  Errors: `ValueError` for an out-of-range
    /// key or a size below 1; `Warning("Failed for key 0x%lx: ...")`
    /// otherwise.
    pub fn attach(
        key: i64,
        size: Option<i64>,
        permissions: i64,
        init_mem: i64,
    ) -> SysvResult<Self> {
        let k = key_from_arg(key)?;
        let size = size.unwrap_or(init_mem);
        if size < 1 {
            return Err(SysvError::ValueError(
                "Argument #2 ($size) must be greater than 0".into(),
            ));
        }
        let fail =
            |what: String| SysvError::Warning(format!("Failed for key 0x{}: {what}", xlong(key)));
        let mut created = false;
        let id = if let Ok(id) = sys::shmget(k, 0, 0) {
            id
        } else {
            // `size < sizeof(sysvshm_chunk_head)`
            if usize::try_from(size).is_ok_and(|s| s < CHUNK_HEAD_SIZE) {
                return Err(fail("memorysize too small".into()));
            }
            let bytes = usize::try_from(size).unwrap_or(usize::MAX);
            let flags = c_int(permissions) | libc::IPC_CREAT | libc::IPC_EXCL;
            let id = sys::shmget(k, bytes, flags).map_err(|e| fail(strerror(e)))?;
            created = true;
            id
        };
        let cleanup = |created: bool| {
            if created {
                let _ = sys::shm_remove(id);
            }
        };
        let mut seg = match sys::shmat(id, false) {
            Ok(s) => s,
            Err(e) => {
                cleanup(created);
                return Err(fail(strerror(e)));
            }
        };
        let segsz = match sys::shm_size(id) {
            Ok(s) => s,
            Err(e) => {
                drop(seg);
                cleanup(created);
                return Err(fail(strerror(e)));
            }
        };
        if segsz < CHUNK_HEAD_SIZE {
            drop(seg);
            cleanup(created);
            return Err(fail("segment too small".into()));
        }
        seg.set_len(segsz);
        init_head(&mut seg, segsz);
        Ok(Self {
            key: k,
            id,
            seg: Some(seg),
        })
    }

    /// The `key_t`.
    pub fn key(&self) -> i32 {
        self.key
    }

    /// The `shmid`.
    pub fn id(&self) -> i32 {
        self.id
    }

    /// Whether the handle is still attached.
    pub fn is_attached(&self) -> bool {
        self.seg.is_some()
    }

    /// The `Error` every `shm_*` function throws on a detached handle.
    /// `shm_put_var()` must call this *before* serializing its value.
    pub fn ensure_attached(&self) -> SysvResult<()> {
        if self.seg.is_some() {
            Ok(())
        } else {
            Err(SysvError::Error(ALREADY_DESTROYED.into()))
        }
    }

    fn seg(&self) -> SysvResult<&sys::Segment> {
        self.seg
            .as_ref()
            .ok_or_else(|| SysvError::Error(ALREADY_DESTROYED.into()))
    }

    fn seg_mut(&mut self) -> SysvResult<&mut sys::Segment> {
        self.seg
            .as_mut()
            .ok_or_else(|| SysvError::Error(ALREADY_DESTROYED.into()))
    }

    /// `shm_detach()`.
    pub fn detach(&mut self) -> SysvResult<()> {
        self.ensure_attached()?;
        self.seg = None;
        Ok(())
    }

    /// `shm_remove()`: marks the segment for deletion.
    pub fn remove(&self) -> SysvResult<()> {
        self.ensure_attached()?;
        sys::shm_remove(self.id).map_err(|e| {
            SysvError::Warning(format!(
                "Failed for key 0x{}, id {}: {}",
                xkey(self.key),
                self.id,
                strerror(e)
            ))
        })
    }

    /// `shm_put_var()` with the already-serialized value.
    ///
    /// The VM calls [`Self::ensure_attached`], serializes (propagating
    /// exceptions), then calls this; if serialization detached the handle
    /// this throws [`DESTROYED_BY_SERIALIZATION`].  `Warning("Not enough
    /// shared memory left")` when the variable does not fit.
    pub fn put_var(&mut self, key: i64, serialized: &[u8]) -> SysvResult<()> {
        let seg = self
            .seg
            .as_mut()
            .ok_or_else(|| SysvError::Error(DESTROYED_BY_SERIALIZATION.into()))?;
        if put(seg, key, serialized) {
            Ok(())
        } else {
            Err(SysvError::Warning("Not enough shared memory left".into()))
        }
    }

    /// `shm_get_var()`: the stored serialized bytes, which the VM
    /// unserializes (warning [`DATA_CORRUPTED`] and returning `false` on
    /// failure).  `Warning("Variable key %ld doesn't exist")` when missing.
    pub fn get_var(&self, key: i64) -> SysvResult<Vec<u8>> {
        let seg = self.seg()?;
        let pos = find(seg, key).ok_or_else(|| missing(key))?;
        data_at(seg, pos).ok_or_else(|| SysvError::Warning(DATA_CORRUPTED.into()))
    }

    /// `shm_has_var()`.
    pub fn has_var(&self, key: i64) -> SysvResult<bool> {
        Ok(find(self.seg()?, key).is_some())
    }

    /// `shm_remove_var()`.
    pub fn remove_var(&mut self, key: i64) -> SysvResult<()> {
        let seg = self.seg_mut()?;
        let pos = find(seg, key).ok_or_else(|| missing(key))?;
        remove_at(seg, pos);
        Ok(())
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    fn fresh(size: usize) -> Vec<u8> {
        let mut m = vec![0u8; size];
        init_head(&mut m, size);
        m
    }

    #[test]
    fn header_layout() {
        let m = fresh(1024);
        assert_eq!(&m[..7], b"PHP_SM\0");
        assert_eq!(m.word(H_START), Some(40));
        assert_eq!(m.word(H_END), Some(40));
        assert_eq!(m.word(H_FREE), Some(984));
        assert_eq!(m.word(H_TOTAL), Some(1024));
        // An initialised header is left alone.
        let mut m2 = m.clone();
        m2.set_word(H_FREE, 5);
        init_head(&mut m2, 1024);
        assert_eq!(m2.word(H_FREE), Some(5));
    }

    #[test]
    fn chunk_sizes_are_word_aligned() {
        let mut m = fresh(1024);
        assert!(put(&mut m, 1, b"s:6:\"qwerty\";"));
        // 13 bytes → (13 + 31) / 8 * 8 + 8 = 48
        assert_eq!(m.word(H_END), Some(40 + 48));
        assert_eq!(m.word(40), Some(1));
        assert_eq!(m.word(48), Some(13));
        assert_eq!(m.word(56), Some(48));
        assert_eq!(&m[64..77], b"s:6:\"qwerty\";");
        assert!(put(&mut m, -1, b""));
        assert_eq!(m.word(H_END), Some(40 + 48 + 32));
    }

    #[test]
    fn replace_find_remove() {
        let mut m = fresh(1024);
        assert!(put(&mut m, 1, b"one"));
        assert!(put(&mut m, 2, b"two"));
        assert!(put(&mut m, 3, b"three"));
        assert!(put(&mut m, 2, b"TWO!"));
        assert_eq!(data_at(&m, find(&m, 2).unwrap()).unwrap(), b"TWO!");
        assert_eq!(data_at(&m, find(&m, 3).unwrap()).unwrap(), b"three");
        let pos = find(&m, 1).unwrap();
        remove_at(&mut m, pos);
        assert_eq!(find(&m, 1), None);
        assert_eq!(data_at(&m, find(&m, 2).unwrap()).unwrap(), b"TWO!");
        let used = to_usize(m.word(H_END).unwrap()) - 40;
        assert_eq!(to_usize(m.word(H_FREE).unwrap()), 984 - used);
    }

    #[test]
    fn out_of_space() {
        let mut m = fresh(128);
        assert!(!put(&mut m, 11, &[b'x'; 2048]));
        assert!(put(&mut m, 1, &[b'x'; 50]));
        assert!(!put(&mut m, 2, &[b'x'; 50]));
    }

    #[test]
    fn corrupt_header_is_contained() {
        let mut m = fresh(128);
        m.set_word(H_END, 1 << 40);
        assert_eq!(find(&m, 5), None);
        assert!(!put(&mut m, 5, b"x"));
        let mut m = fresh(128);
        assert!(put(&mut m, 1, b"abc"));
        m.set_word(48, 1 << 40); // absurd length
        assert_eq!(data_at(&m, find(&m, 1).unwrap()), None);
    }
}
