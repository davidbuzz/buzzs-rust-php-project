//! Records the build time in C's `__DATE__ " " __TIME__` format
//! (`PHP_BUILD_DATE`, the `-v` banner) and puts libc and libstdc++ at the
//! head of the binary's `DT_NEEDED` list (start-up time, see `main`).

use std::time::{SystemTime, UNIX_EPOCH};

/// Days since 1970-01-01 to (year, month, day) (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
        });
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    let month = MONTHS[usize::try_from(m - 1).unwrap_or(0)];
    println!(
        "cargo:rustc-env=PHP_CLI_BUILD_DATE={month} {d:>2} {y} {:02}:{:02}:{:02}",
        t / 3600,
        t / 60 % 60,
        t % 60
    );
    // Dynamic-linker search order, for start-up time (perf/README.md,
    // "Startup"). The ~10 000 symbol lookups of start-up (everything is
    // BIND_NOW) walk the global scope: this binary, its DT_NEEDED libraries
    // in order, then theirs. 2 700 of them resolve to libc and 1 900 to
    // libstdc++ (ICU is C++), which the usual order puts 18th and 41st. A
    // crate's own `-l` flags precede its dependencies' on the link line and
    // DT_NEEDED follows the link line, so these two lines put the two
    // libraries first: about 5 M fewer instructions per `php -r 'echo 1;'`
    // (measured by preloading them). libm and libgcc_s stay where they are:
    // moved up they cost every other lookup a probe. References into both
    // libraries are versioned, so no symbol binds differently.
    println!("cargo:rustc-link-lib=dylib=c");
    // By soname, so no `libstdc++.so` development link is needed. Nothing in
    // Rust refers to it, so `--as-needed` would drop it without the reference
    // `LIBSTDCXX_IS_NEEDED` in main.rs.
    println!("cargo:rustc-link-lib=dylib:+verbatim=libstdc++.so.6");
}
