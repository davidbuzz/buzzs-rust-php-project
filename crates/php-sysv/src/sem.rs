// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/sysvsem` (ref: `ext/sysvsem/sysvsem.c`).
//!
//! PHP allocates three semaphores per key:
//!
//! * [`SYSVSEM_SEM`] — the semaphore proper, initialised to `max_acquire`;
//! * [`SYSVSEM_USAGE`] — the number of processes attached (each `sem_get()`
//!   increments it with `SEM_UNDO`, so a dying process decrements it);
//! * [`SYSVSEM_SETVAL`] — a lock guarding the "first user initialises
//!   `SYSVSEM_SEM`" step.

use crate::{c_int, key_from_arg, strerror, sys, xkey, xlong, SysvError, SysvResult};
use sys::SemOp;

/// Index of the semaphore proper.
pub const SYSVSEM_SEM: u16 = 0;
/// Index of the usage counter.
pub const SYSVSEM_USAGE: u16 = 1;
/// Index of the initialisation lock.
pub const SYSVSEM_SETVAL: u16 = 2;

/// Default `$max_acquire` of `sem_get()`.
pub const DEFAULT_MAX_ACQUIRE: i64 = 1;
/// Default `$permissions` of `sem_get()`.
pub const DEFAULT_PERMISSIONS: i64 = 0o666;

#[allow(clippy::cast_possible_truncation)]
const SEM_UNDO: i16 = libc::SEM_UNDO as i16;
#[allow(clippy::cast_possible_truncation)]
const IPC_NOWAIT: i16 = libc::IPC_NOWAIT as i16;

/// `semop()` retried on `EINTR`, as PHP's loops do.
fn semop_retry(id: i32, ops: &[SemOp]) -> Result<(), i32> {
    loop {
        match sys::semop(id, ops) {
            Err(libc::EINTR) => {}
            other => return other,
        }
    }
}

/// A `SysvSemaphore`.  Dropping it does what PHP's `free_obj` does:
/// unless removed or created with `auto_release = false`, it decrements
/// the usage count and releases every acquisition still held.
#[derive(Debug)]
pub struct Semaphore {
    key: i32,
    semid: i32,
    /// Acquisitions held by this handle; `-1` once removed.
    count: i64,
    auto_release: bool,
}

impl Semaphore {
    /// `sem_get(int $key, int $max_acquire = 1, int $permissions = 0666,
    /// bool $auto_release = true)`.
    ///
    /// Non-fatal diagnostics during the initialisation protocol are passed
    /// to `warn` (PHP emits them as `E_WARNING` and still returns the
    /// object).  Errors: `ValueError` for an out-of-range key,
    /// `Warning("Failed for key 0x%lx: %s")` when `semget()` fails.
    pub fn get(
        key: i64,
        max_acquire: i64,
        permissions: i64,
        auto_release: bool,
        warn: &mut dyn FnMut(String),
    ) -> SysvResult<Self> {
        let k = key_from_arg(key)?;
        let fail = |what: &str, e: i32| format!("{what} for key 0x{}: {}", xlong(key), strerror(e));
        let semid = sys::semget(k, 3, c_int(permissions) | libc::IPC_CREAT)
            .map_err(|e| SysvError::Warning(fail("Failed", e)))?;

        // Wait for SETVAL to be zero, take it, and bump the usage count.
        let take = [
            SemOp {
                num: SYSVSEM_SETVAL,
                op: 0,
                flags: 0,
            },
            SemOp {
                num: SYSVSEM_SETVAL,
                op: 1,
                flags: SEM_UNDO,
            },
            SemOp {
                num: SYSVSEM_USAGE,
                op: 1,
                flags: SEM_UNDO,
            },
        ];
        if let Err(e) = semop_retry(semid, &take) {
            warn(fail("Failed acquiring SYSVSEM_SETVAL", e));
        }

        let count = sys::sem_getval(semid, c_int(SYSVSEM_USAGE.into()));
        if let Err(e) = count {
            warn(fail("Failed", e));
        }
        // If we are the only user, set the maximum.
        if count == Ok(1) {
            if let Err(e) = sys::sem_setval(semid, c_int(SYSVSEM_SEM.into()), c_int(max_acquire)) {
                warn(fail("Failed", e));
            }
        }

        let release = [SemOp {
            num: SYSVSEM_SETVAL,
            op: -1,
            flags: SEM_UNDO,
        }];
        if let Err(e) = semop_retry(semid, &release) {
            warn(fail("Failed releasing SYSVSEM_SETVAL", e));
        }

        Ok(Self {
            key: k,
            semid,
            count: 0,
            auto_release,
        })
    }

    /// The `key_t`.
    pub fn key(&self) -> i32 {
        self.key
    }

    /// The `semid`.
    pub fn id(&self) -> i32 {
        self.semid
    }

    /// Acquisitions currently held through this handle (`-1` once removed).
    pub fn count(&self) -> i64 {
        self.count
    }

    fn semop(&mut self, acquire: bool, nowait: bool) -> SysvResult<()> {
        if !acquire && self.count == 0 {
            return Err(SysvError::Warning(format!(
                "SysV semaphore for key 0x{} is not currently acquired",
                xkey(self.key)
            )));
        }
        let op = SemOp {
            num: SYSVSEM_SEM,
            op: if acquire { -1 } else { 1 },
            flags: SEM_UNDO | if nowait { IPC_NOWAIT } else { 0 },
        };
        match semop_retry(self.semid, &[op]) {
            Ok(()) => {
                self.count += if acquire { 1 } else { -1 };
                Ok(())
            }
            Err(libc::EAGAIN) => Err(SysvError::Failed(libc::EAGAIN)),
            Err(e) => Err(SysvError::Warning(format!(
                "Failed to {} key 0x{}: {}",
                if acquire { "acquire" } else { "release" },
                xkey(self.key),
                strerror(e)
            ))),
        }
    }

    /// `sem_acquire(SysvSemaphore $semaphore, bool $non_blocking = false)`.
    /// `Failed(EAGAIN)` (silent `false`) when non-blocking and unavailable.
    pub fn acquire(&mut self, non_blocking: bool) -> SysvResult<()> {
        self.semop(true, non_blocking)
    }

    /// `sem_release()`.
    pub fn release(&mut self) -> SysvResult<()> {
        self.semop(false, false)
    }

    /// `sem_remove()`.
    pub fn remove(&mut self) -> SysvResult<()> {
        if sys::sem_exists(self.semid).is_err() {
            return Err(SysvError::Warning(format!(
                "SysV semaphore for key 0x{} does not (any longer) exist",
                xkey(self.key)
            )));
        }
        sys::sem_remove(self.semid).map_err(|e| {
            SysvError::Warning(format!(
                "Failed for SysV semaphore for key 0x{}: {}",
                xkey(self.key),
                strerror(e)
            ))
        })?;
        self.count = -1;
        Ok(())
    }
}

impl Drop for Semaphore {
    fn drop(&mut self) {
        if self.count == -1 || !self.auto_release {
            return;
        }
        let mut ops = vec![SemOp {
            num: SYSVSEM_USAGE,
            op: -1,
            flags: SEM_UNDO,
        }];
        if self.count != 0 {
            #[allow(clippy::cast_possible_truncation)]
            ops.push(SemOp {
                num: SYSVSEM_SEM,
                op: self.count as i16,
                flags: SEM_UNDO,
            });
        }
        // Errors are ignored, as in PHP.
        let _ = sys::semop(self.semid, &ops);
    }
}
