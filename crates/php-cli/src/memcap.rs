//! The address-space cap shared by the `php` and `php-cgi` binaries.

/// Caps the address space (`RLIMIT_AS`) so that runaway native code fails
/// instead of exhausting the machine's memory. C PHP has no such cap:
/// `memory_limit` bounds the script's own allocations here as there, this
/// only guards what allocates outside the request heap (libgmp, libxml2,
/// ...). The cap is [`address_space_cap`]: `PHPRUST_MAX_AS_MB` when set,
/// else adapted to the machine.
pub(crate) fn limit_memory() {
    let override_mb = std::env::var("PHPRUST_MAX_AS_MB")
        .ok()
        .and_then(|v| v.parse().ok());
    let meminfo = std::fs::read("/proc/meminfo").ok();
    let limit = address_space_cap(override_mb, meminfo.as_deref());
    let rl = libc::rlimit {
        rlim_cur: limit,
        rlim_max: limit,
    };
    // SAFETY: setrlimit only reads the struct we pass; failure (a lower hard
    // limit inherited from the parent) is harmless: the inherited limits
    // stay in force.
    unsafe {
        libc::setrlimit(libc::RLIMIT_AS, &raw const rl);
    }
}

/// The floor of the address-space cap, the fixed value it used to be.
const MIN_ADDRESS_SPACE_CAP: u64 = 4096 << 20;

/// The address-space cap in bytes: `override_mb` (`PHPRUST_MAX_AS_MB`)
/// verbatim when given; else the larger of 4 GiB and 75% of the machine's
/// memory plus swap (`MemTotal` + `SwapTotal` of `meminfo`, the text of
/// `/proc/meminfo`, in kB). Scripts that legitimately need a huge address
/// space then run on a machine that has the memory (ext/gmp's 2^30-bit
/// numbers take about 10 GiB, `ext/gmp/tests/gmp_setbit_long`), while a
/// runaway allocation still fails before the machine swaps itself out. An
/// unreadable or incomplete `meminfo` gives the floor.
pub(crate) fn address_space_cap(override_mb: Option<u64>, meminfo: Option<&[u8]>) -> u64 {
    if let Some(mb) = override_mb {
        return mb.saturating_mul(1 << 20);
    }
    let total_kb = meminfo.map_or(0, |m| {
        meminfo_kb(m, b"MemTotal:").saturating_add(meminfo_kb(m, b"SwapTotal:"))
    });
    let three_quarters = total_kb.saturating_mul(1024) / 4 * 3;
    three_quarters.max(MIN_ADDRESS_SPACE_CAP)
}

/// The value of the `/proc/meminfo` line starting with `key` (such as
/// `MemTotal:`), in kB; 0 when the line is absent or malformed.
pub(crate) fn meminfo_kb(meminfo: &[u8], key: &[u8]) -> u64 {
    meminfo
        .split(|&c| c == b'\n')
        .find_map(|line| {
            let rest = line.strip_prefix(key)?;
            let field = std::str::from_utf8(rest).ok()?.split_whitespace().next()?;
            field.parse().ok()
        })
        .unwrap_or(0)
}
