// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Thin, safe wrappers over the System V IPC syscalls.
//!
//! This is the only module of the crate containing `unsafe` code.  Failures
//! are reported as the raw `errno` (`Err(errno)`); buffers handed to the
//! kernel are owned Rust allocations of the exact size passed alongside.
//! Attached shared-memory segments are wrapped in [`Segment`], which only
//! allows bounds-checked raw copies in and out of the mapping.

#![allow(unsafe_code)]

use std::os::raw::{c_int, c_long, c_void};

/// Reads the calling thread's `errno`.
pub fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `strerror()` text for an errno value (`"Unknown error N"` for unknown
/// ones), exactly as glibc spells it.
pub fn strerror(code: i32) -> String {
    let mut buf = [0 as libc::c_char; 1024];
    // SAFETY: `buf` is writable for its whole length; the XSI strerror_r
    // always NUL-terminates what it writes.
    unsafe { libc::strerror_r(code, buf.as_mut_ptr(), buf.len()) };
    #[allow(clippy::cast_sign_loss)]
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn check(ret: c_int) -> Result<c_int, i32> {
    if ret < 0 {
        Err(errno())
    } else {
        Ok(ret)
    }
}

// ---------------------------------------------------------------- messages

/// `msgget(2)`.
pub fn msgget(key: i32, flags: c_int) -> Result<c_int, i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::msgget(key, flags) })
}

/// `msgsnd(2)` of one message (`mtype` + `text`).
pub fn msgsnd(id: c_int, mtype: i64, text: &[u8], flags: c_int) -> Result<(), i32> {
    let hdr = std::mem::size_of::<c_long>();
    let mut buf = vec![0u8; hdr + text.len()];
    #[allow(clippy::cast_possible_truncation)]
    let mtype = mtype as c_long;
    buf[..hdr].copy_from_slice(&mtype.to_ne_bytes());
    buf[hdr..].copy_from_slice(text);
    // SAFETY: `buf` holds a `long` mtype followed by exactly `text.len()`
    // bytes of mtext, as msgsnd requires; the kernel only reads it.
    check(unsafe { libc::msgsnd(id, buf.as_ptr().cast::<c_void>(), text.len(), flags) }).map(|_| ())
}

/// `msgrcv(2)` with room for `max` bytes of text; returns `(mtype, text)`.
pub fn msgrcv(id: c_int, max: usize, msgtyp: i64, flags: c_int) -> Result<(i64, Vec<u8>), i32> {
    let hdr = std::mem::size_of::<c_long>();
    let mut buf = vec![0u8; hdr + max];
    // SAFETY: `buf` has room for a `long` plus `max` bytes, the size we
    // pass; the kernel writes at most that much.
    let got = unsafe { libc::msgrcv(id, buf.as_mut_ptr().cast::<c_void>(), max, msgtyp, flags) };
    let Ok(len) = usize::try_from(got) else {
        return Err(errno());
    };
    let mut mt = [0u8; std::mem::size_of::<c_long>()];
    mt.copy_from_slice(&buf[..hdr]);
    let text = buf[hdr..hdr + len.min(max)].to_vec();
    // `c_long` is `i64` on LP64 targets; the conversion matters elsewhere.
    #[allow(clippy::useless_conversion)]
    let mtype = i64::from(c_long::from_ne_bytes(mt));
    Ok((mtype, text))
}

/// The fields of `struct msqid_ds` PHP exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsgStat {
    pub uid: i64,
    pub gid: i64,
    pub mode: i64,
    pub stime: i64,
    pub rtime: i64,
    pub ctime: i64,
    pub qnum: i64,
    pub qbytes: i64,
    pub lspid: i64,
    pub lrpid: i64,
}

fn msg_stat_raw(id: c_int) -> Result<libc::msqid_ds, i32> {
    // SAFETY: msqid_ds is plain old data; all-zero is a valid value.
    let mut ds: libc::msqid_ds = unsafe { std::mem::zeroed() };
    // SAFETY: `ds` is a valid, writable msqid_ds.
    check(unsafe { libc::msgctl(id, libc::IPC_STAT, &raw mut ds) })?;
    Ok(ds)
}

/// `msgctl(IPC_STAT)`.
#[allow(clippy::cast_possible_wrap)]
pub fn msg_stat(id: c_int) -> Result<MsgStat, i32> {
    let ds = msg_stat_raw(id)?;
    Ok(MsgStat {
        uid: i64::from(ds.msg_perm.uid),
        gid: i64::from(ds.msg_perm.gid),
        mode: i64::from(ds.msg_perm.mode),
        stime: ds.msg_stime,
        rtime: ds.msg_rtime,
        ctime: ds.msg_ctime,
        qnum: ds.msg_qnum as i64,
        qbytes: ds.msg_qbytes as i64,
        lspid: i64::from(ds.msg_lspid),
        lrpid: i64::from(ds.msg_lrpid),
    })
}

/// `msgctl(IPC_STAT)` → patch the given fields (C conversion semantics:
/// truncation to the field's type) → `msgctl(IPC_SET)`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
pub fn msg_set(
    id: c_int,
    uid: Option<i64>,
    gid: Option<i64>,
    mode: Option<i64>,
    qbytes: Option<i64>,
) -> Result<(), i32> {
    let mut ds = msg_stat_raw(id)?;
    if let Some(v) = uid {
        ds.msg_perm.uid = v as libc::uid_t;
    }
    if let Some(v) = gid {
        ds.msg_perm.gid = v as libc::gid_t;
    }
    if let Some(v) = mode {
        ds.msg_perm.mode = v as _;
    }
    if let Some(v) = qbytes {
        ds.msg_qbytes = v as _;
    }
    // SAFETY: `ds` is a valid msqid_ds obtained from IPC_STAT.
    check(unsafe { libc::msgctl(id, libc::IPC_SET, &raw mut ds) }).map(|_| ())
}

/// `msgctl(IPC_RMID)`.
pub fn msg_remove(id: c_int) -> Result<(), i32> {
    // SAFETY: IPC_RMID ignores the buffer argument.
    check(unsafe { libc::msgctl(id, libc::IPC_RMID, std::ptr::null_mut()) }).map(|_| ())
}

// -------------------------------------------------------------- semaphores

/// One `struct sembuf` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemOp {
    pub num: u16,
    pub op: i16,
    pub flags: i16,
}

/// `semget(2)`.
pub fn semget(key: i32, nsems: c_int, flags: c_int) -> Result<c_int, i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::semget(key, nsems, flags) })
}

/// `semop(2)`.
pub fn semop(id: c_int, ops: &[SemOp]) -> Result<(), i32> {
    let mut bufs: Vec<libc::sembuf> = ops
        .iter()
        .map(|o| libc::sembuf {
            sem_num: o.num,
            sem_op: o.op,
            sem_flg: o.flags,
        })
        .collect();
    // SAFETY: `bufs` is a valid array of `bufs.len()` sembufs.
    check(unsafe { libc::semop(id, bufs.as_mut_ptr(), bufs.len()) }).map(|_| ())
}

/// `semctl(GETVAL)`.
pub fn sem_getval(id: c_int, num: c_int) -> Result<c_int, i32> {
    // SAFETY: GETVAL takes no fourth argument.
    check(unsafe { libc::semctl(id, num, libc::GETVAL) })
}

/// `semctl(SETVAL)`.
pub fn sem_setval(id: c_int, num: c_int, val: c_int) -> Result<(), i32> {
    // SAFETY: SETVAL takes `union semun` whose `val` member is an int; on
    // all Linux ABIs an int vararg is passed like the union.
    check(unsafe { libc::semctl(id, num, libc::SETVAL, val) }).map(|_| ())
}

/// `semctl(IPC_STAT)` success check.
pub fn sem_exists(id: c_int) -> Result<(), i32> {
    // SAFETY: semid_ds is plain old data; all-zero is a valid value.
    let mut ds: libc::semid_ds = unsafe { std::mem::zeroed() };
    // SAFETY: IPC_STAT takes `union semun` whose `buf` member points to a
    // writable semid_ds, passed as a pointer vararg.
    check(unsafe { libc::semctl(id, 0, libc::IPC_STAT, &raw mut ds) }).map(|_| ())
}

/// `semctl(IPC_RMID)`.
pub fn sem_remove(id: c_int) -> Result<(), i32> {
    // SAFETY: IPC_RMID ignores the fourth argument.
    check(unsafe { libc::semctl(id, 0, libc::IPC_RMID) }).map(|_| ())
}

// ----------------------------------------------------------- shared memory

/// `shmget(2)`.
pub fn shmget(key: i32, size: usize, flags: c_int) -> Result<c_int, i32> {
    // SAFETY: no memory-safety preconditions.
    check(unsafe { libc::shmget(key, size, flags) })
}

/// `shmctl(IPC_STAT)` → `shm_segsz`.
pub fn shm_size(id: c_int) -> Result<usize, i32> {
    // SAFETY: shmid_ds is plain old data; all-zero is a valid value.
    let mut ds: libc::shmid_ds = unsafe { std::mem::zeroed() };
    // SAFETY: `ds` is a valid, writable shmid_ds.
    check(unsafe { libc::shmctl(id, libc::IPC_STAT, &raw mut ds) })?;
    Ok(ds.shm_segsz)
}

/// `shmctl(IPC_RMID)`.
pub fn shm_remove(id: c_int) -> Result<(), i32> {
    // SAFETY: IPC_RMID ignores the buffer argument.
    check(unsafe { libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut()) }).map(|_| ())
}

/// An attached shared-memory segment (`shmat`); detached on drop.
///
/// The usable length is unknown at attach time (PHP stats the segment
/// afterwards), so it starts at 0 and is set with [`Segment::set_len`],
/// which never lets it exceed the kernel's size of the segment: the
/// bounds the raw copies check cannot be widened past the mapping.
#[derive(Debug)]
pub struct Segment {
    addr: *mut u8,
    len: usize,
    /// The segment's id (`shmid`), for its size.
    id: c_int,
}

// SAFETY: the mapping is process-wide (not thread-bound); all access goes
// through `&self`/`&mut self` methods doing bounds-checked raw copies.
unsafe impl Send for Segment {}

/// `shmat(2)`; `readonly` adds `SHM_RDONLY`.
pub fn shmat(id: c_int, readonly: bool) -> Result<Segment, i32> {
    let flags = if readonly { libc::SHM_RDONLY } else { 0 };
    // SAFETY: a null address lets the kernel choose the mapping location.
    let p = unsafe { libc::shmat(id, std::ptr::null(), flags) };
    if p as isize == -1 {
        Err(errno())
    } else {
        Ok(Segment {
            addr: p.cast::<u8>(),
            len: 0,
            id,
        })
    }
}

impl Segment {
    /// Sets the accessible length, at most the segment's size as
    /// `shmctl(IPC_STAT)` reports it (a segment's size never changes), so a
    /// wrong `len` can shorten what is reachable but never lengthen it.
    pub fn set_len(&mut self, len: usize) {
        self.len = len.min(shm_size(self.id).unwrap_or(0));
    }

    /// The accessible length.
    pub fn len(&self) -> usize {
        self.len
    }

    fn in_bounds(&self, off: usize, n: usize) -> bool {
        off.checked_add(n).is_some_and(|end| end <= self.len)
    }

    /// Copies `out.len()` bytes from `off`; `false` (nothing copied) when out
    /// of bounds.
    pub fn read(&self, off: usize, out: &mut [u8]) -> bool {
        if !self.in_bounds(off, out.len()) {
            return false;
        }
        // SAFETY: `off..off+out.len()` lies within the mapping (checked
        // above); `out` is a distinct Rust buffer.
        unsafe { std::ptr::copy_nonoverlapping(self.addr.add(off), out.as_mut_ptr(), out.len()) };
        true
    }

    /// Copies `data` to `off`; `false` (nothing copied) when out of bounds.
    /// Writing to a read-only attachment faults, so callers must check.
    pub fn write(&mut self, off: usize, data: &[u8]) -> bool {
        if !self.in_bounds(off, data.len()) {
            return false;
        }
        // SAFETY: the destination range lies within the mapping (checked
        // above); `data` is a distinct Rust buffer.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.addr.add(off), data.len()) };
        true
    }

    /// `memmove(addr + dst, addr + src, n)`; `false` when out of bounds.
    pub fn copy_within(&mut self, src: usize, dst: usize, n: usize) -> bool {
        if !self.in_bounds(src, n) || !self.in_bounds(dst, n) {
            return false;
        }
        // SAFETY: both ranges lie within the mapping; `copy` permits overlap.
        unsafe { std::ptr::copy(self.addr.add(src), self.addr.add(dst), n) };
        true
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        // SAFETY: `addr` came from a successful shmat and is detached once.
        unsafe { libc::shmdt(self.addr.cast::<c_void>()) };
    }
}
