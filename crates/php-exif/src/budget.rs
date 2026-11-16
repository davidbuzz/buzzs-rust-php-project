// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Emulation of the Zend memory limit for the allocations whose size is
//! controlled by the input file (the `ZEND_HAS_OOM()` checks of exif.c and
//! the "fallible" allocations of image.c).
//!
//! The VM passes the `memory_limit` setting and its current usage; when the
//! bytes this crate would retain exceed what is left, parsing stops and an
//! `E_ERROR` "Allowed memory size of %d bytes exhausted (tried to allocate
//! %d bytes)" diagnostic is returned, which the VM must turn into a fatal
//! error.  Sizes are approximations of what the C code allocates.

use crate::diag::{Diagnostic, Level};

/// The `memory_limit` state handed in by the VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryLimit {
    /// `memory_limit` in bytes.
    pub limit: u64,
    /// Memory the script already uses.
    pub used: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Budget {
    limit: Option<MemoryLimit>,
    used: u64,
    pub(crate) exhausted: Option<Diagnostic>,
}

impl Budget {
    pub(crate) fn new(limit: Option<MemoryLimit>) -> Self {
        Self {
            limit,
            used: 0,
            exhausted: None,
        }
    }

    /// Accounts for an allocation of `n` bytes; `false` once the limit is hit.
    pub(crate) fn alloc(&mut self, n: u64) -> bool {
        if self.exhausted.is_some() {
            return false;
        }
        let Some(l) = self.limit else {
            return true;
        };
        let total = l.used.saturating_add(self.used).saturating_add(n);
        if total > l.limit {
            self.exhausted = Some(Diagnostic {
                level: Level::Error,
                message: format!(
                    "Allowed memory size of {} bytes exhausted (tried to allocate {n} bytes)",
                    l.limit
                )
                .into_bytes(),
            });
            return false;
        }
        self.used += n;
        true
    }

    /// Releases a transient allocation.
    pub(crate) fn free(&mut self, n: u64) {
        self.used = self.used.saturating_sub(n);
    }

    pub(crate) fn is_exhausted(&self) -> bool {
        self.exhausted.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_limits() {
        let mut b = Budget::new(Some(MemoryLimit {
            limit: 100,
            used: 50,
        }));
        assert!(b.alloc(40));
        b.free(40);
        assert!(b.alloc(50));
        assert!(!b.alloc(1));
        assert!(b.is_exhausted());
        assert_eq!(
            b.exhausted
                .as_ref()
                .map(Diagnostic::message_lossy)
                .as_deref(),
            Some("Allowed memory size of 100 bytes exhausted (tried to allocate 1 bytes)")
        );
        let mut unlimited = Budget::new(None);
        assert!(unlimited.alloc(u64::MAX));
    }
}
