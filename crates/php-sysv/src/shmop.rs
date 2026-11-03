//! `ext/shmop` (ref: `ext/shmop/shmop.c`).

use crate::{c_int, key_from_arg, strerror, sys, SysvError, SysvResult};

/// A `Shmop`.  Dropping it detaches the segment (PHP's `free_obj`);
/// `shmop_close()` is a no-op.
#[derive(Debug)]
pub struct Shmop {
    shmid: i32,
    key: i32,
    readonly: bool,
    seg: sys::Segment,
    size: i64,
}

fn invalid_mode() -> SysvError {
    SysvError::ValueError("Argument #2 ($mode) must be a valid access mode".into())
}

impl Shmop {
    /// `shmop_open(int $key, string $mode, int $permissions, int $size)`.
    ///
    /// Modes: `"a"` read-only attach, `"w"` read/write attach, `"c"` create
    /// or attach, `"n"` create exclusively.  Errors: `ValueError`s for the
    /// key range, the mode and a size below 1 for `c`/`n`; warnings when
    /// `shmget`/`shmctl`/`shmat` fail.
    pub fn open(key: i64, mode: &[u8], permissions: i64, size: i64) -> SysvResult<Self> {
        let k = key_from_arg(key)?;
        if mode.len() != 1 {
            return Err(invalid_mode());
        }
        let mut flags = c_int(permissions);
        let mut readonly = false;
        let mut want = 0i64;
        match mode[0] {
            b'a' => readonly = true,
            b'c' => {
                flags |= libc::IPC_CREAT;
                want = size;
            }
            b'n' => {
                flags |= libc::IPC_CREAT | libc::IPC_EXCL;
                want = size;
            }
            b'w' => {}
            _ => return Err(invalid_mode()),
        }
        if flags & libc::IPC_CREAT != 0 && want < 1 {
            return Err(SysvError::ValueError(
                "Argument #4 ($size) must be greater than 0 for the \"c\" and \"n\" access modes"
                    .into(),
            ));
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let shmid = sys::shmget(k, want as usize, flags).map_err(|e| {
            SysvError::Warning(format!(
                "Unable to attach or create shared memory segment \"{}\"",
                strerror(e)
            ))
        })?;
        let segsz = sys::shm_size(shmid).map_err(|e| {
            SysvError::Warning(format!(
                "Unable to get shared memory segment information \"{}\"",
                strerror(e)
            ))
        })?;
        let size = i64::try_from(segsz)
            .map_err(|_| SysvError::ValueError("Argument #4 ($size) is too large".into()))?;
        let mut seg = sys::shmat(shmid, readonly).map_err(|e| {
            SysvError::Warning(format!(
                "Unable to attach to shared memory segment \"{}\"",
                strerror(e)
            ))
        })?;
        seg.set_len(segsz);
        Ok(Self {
            shmid,
            key: k,
            readonly,
            seg,
            size,
        })
    }

    /// The `key_t`.
    pub fn key(&self) -> i32 {
        self.key
    }

    /// The `shmid`.
    pub fn id(&self) -> i32 {
        self.shmid
    }

    /// `shmop_size()`.
    pub fn size(&self) -> i64 {
        self.size
    }

    /// `shmop_read(Shmop $shmop, int $offset, int $size)`; a `$size` of 0
    /// reads to the end of the segment.
    pub fn read(&self, offset: i64, size: i64) -> SysvResult<Vec<u8>> {
        if offset < 0 || offset > self.size {
            return Err(SysvError::ValueError(
                "Argument #2 ($offset) must be between 0 and the segment size".into(),
            ));
        }
        if size < 0 || offset > i64::MAX - size || offset + size > self.size {
            return Err(SysvError::ValueError(
                "Argument #3 ($size) is out of range".into(),
            ));
        }
        let bytes = if size == 0 { self.size - offset } else { size };
        let (off, n) = (to_usize(offset), to_usize(bytes));
        let mut out = vec![0u8; n];
        self.seg.read(off, &mut out);
        Ok(out)
    }

    /// `shmop_write(Shmop $shmop, string $data, int $offset)`: writes as
    /// much of `data` as fits and returns the byte count.
    pub fn write(&mut self, data: &[u8], offset: i64) -> SysvResult<i64> {
        if self.readonly {
            return Err(SysvError::Error(
                "Read-only segment cannot be written".into(),
            ));
        }
        if offset < 0 || offset > self.size {
            return Err(SysvError::ValueError(
                "Argument #3 ($offset) is out of range".into(),
            ));
        }
        let room = self.size - offset;
        let n = i64::try_from(data.len()).unwrap_or(i64::MAX).min(room);
        self.seg.write(to_usize(offset), &data[..to_usize(n)]);
        Ok(n)
    }

    /// `shmop_delete()`: marks the segment for deletion.
    pub fn delete(&self) -> SysvResult<()> {
        sys::shm_remove(self.shmid).map_err(|_| {
            SysvError::Warning("Can't mark segment for deletion (are you the owner?)".into())
        })
    }
}

/// Non-negative, range-checked `zend_long` → `usize`.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn to_usize(v: i64) -> usize {
    v as usize
}
