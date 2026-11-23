// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Crate-level tests.

use std::fmt::Write as _;

use super::*;

#[test]
fn version_is_sane() {
    let (major, minor) = version_numbers();
    assert_eq!(major, 10);
    assert!(minor >= 30, "{}", pcre_version());
    assert!(!unicode_version().is_empty());
}

#[test]
fn jit_compiles_when_supported() {
    let mut pcre = Pcre::new();
    let re = pcre.compile(b"/a+b/").unwrap();
    assert!(pcre.take_warnings().is_empty());
    assert_eq!(re.is_jit(), jit_support());
    pcre.set_jit(false);
    let re = pcre.compile(b"/a+c/").unwrap();
    assert!(!re.is_jit());
}

fn bytes(v: Option<&Value>) -> &[u8] {
    v.and_then(Value::as_bytes).unwrap()
}

/// `bug70345.phpt` / `gh21134.phpt`: master no longer allows `\K` in lookarounds
/// (PHP 8.3 set `PCRE2_EXTRA_ALLOW_LOOKAROUND_BSK`) and forbids `\C` under `/u`.
#[test]
fn master_compile_errors() {
    let mut pcre = Pcre::new();
    let e = pcre.compile(b"/(?=xyz\\K)/").unwrap_err();
    assert_eq!(
        e.to_string(),
        "Compilation failed: \\K is not allowed in lookarounds (but see PCRE2_EXTRA_ALLOW_LOOKAROUND_BSK) at offset 9"
    );
    let e = pcre.compile(b"/(a(?=xyz\\K))/").unwrap_err();
    assert!(e.to_string().ends_with("at offset 12"), "{e}");
    let e = pcre.compile(b"/(.*)\\C/u").unwrap_err();
    assert_eq!(
        e.to_string(),
        "Compilation failed: using \\C is incompatible with the 'u' modifier at offset 6"
    );
    assert_eq!(pcre.last_error(), PregError::Internal);
}

/// `bug74183.phpt`
#[test]
fn too_large_pattern() {
    let mut pcre = Pcre::new();
    let e = pcre
        .compile(
            "/([A-Z]|[a-z]|[0-9]| |Ñ|ñ|!|&quot;|%|&amp;|'|´|-|:|;|>|=|&lt;|@|_|,|\\{|\\}|`|~|á|é|í|ó|ú|Á|É|Í|Ó|Ú|ü|Ü){1,300}/"
                .as_bytes(),
        )
        .unwrap_err();
    assert!(
        e.to_string()
            .starts_with("Compilation failed: regular expression is too large at offset"),
        "{e}"
    );
}

/// `errors05.phpt` / `preg_match_error3.phpt`: the 192K JIT stack overflows.
#[test]
fn jit_stack_limit() {
    if !jit_support() {
        return;
    }
    let mut pcre = Pcre::new();
    let re = pcre.compile(b"/^(foo)+$/").unwrap();
    assert!(re.is_jit());
    let subject = b"foo".repeat(1024 * 8192);
    let o = match_one(&re, &pcre.config(), &subject, false, 0, 0);
    assert_eq!(o.result, Err(MatchFailure::Failed));
    assert_eq!(o.last_error, Some(PregError::JitStackLimit));
}

/// `bug76909.phpt` / `gh11374.phpt`: JIT and interpreter agree.
#[test]
fn jit_and_interpreter_agree() {
    let mut pcre = Pcre::new();
    let a = pcre
        .compile("/^[\\x{0100}-\\x{017f}]{1,63}$/iu".as_bytes())
        .unwrap();
    let b = pcre
        .compile("/(*NO_JIT)^[\\x{0100}-\\x{017f}]{1,63}$/iu".as_bytes())
        .unwrap();
    let cfg = pcre.config();
    for re in [&a, &b] {
        let o = match_one(re, &cfg, b" domain.com", true, 0, 0);
        assert_eq!(o.result, Ok(0));
        assert!(o.matches.unwrap().is_empty());
    }
    // GH-11374 was an interpreter bug fixed in PCRE2 10.43 (the test is skipped below that).
    if version_numbers() < (10, 43) {
        return;
    }
    let regex = "
    (?<types>
        (?:
            (?:\\{ (?&types) \\})
            | (a)
        )
        (\\*?)
    )
";
    pcre.set_jit(false);
    let p1 = format!("{{^{regex}$}}x");
    let re1 = pcre.compile(p1.as_bytes()).unwrap();
    let m1 = match_one(&re1, &pcre.config(), b"{a}", true, PREG_OFFSET_CAPTURE, 0);
    pcre.set_jit(true);
    let p2 = format!("{{^{regex}$}}xx");
    let re2 = pcre.compile(p2.as_bytes()).unwrap();
    let m2 = match_one(&re2, &pcre.config(), b"{a}", true, PREG_OFFSET_CAPTURE, 0);
    assert_eq!(m1.matches, m2.matches);
    let m = m1.matches.unwrap();
    let keys: Vec<_> = m.keys().cloned().collect();
    assert_eq!(
        keys,
        vec![
            Key::Index(0),
            Key::name(b"types"),
            Key::Index(1),
            Key::Index(2),
            Key::Index(3)
        ]
    );
    let g2 = m.get_index(2).unwrap().as_array().unwrap();
    assert_eq!(g2.get_index(1), Some(&Value::Int(-1)));
    let g3 = m.get_index(3).unwrap().as_array().unwrap();
    assert_eq!(g3.get_index(1), Some(&Value::Int(3)));
}

/// `gh16189.phpt`
#[test]
fn offset_int_min() {
    let re = Regex::compile(b"/a/").unwrap();
    let cfg = MatchConfig::default();
    for f in [match_one, match_all] {
        let o = f(&re, &cfg, b"a", true, 0, i64::MIN);
        assert_eq!(o.result, Err(MatchFailure::InvalidOffset));
        assert_eq!(o.matches, None);
        assert_eq!(o.last_error, None);
    }
}

/// `pcre_reentrancy.phpt`: callbacks can use the same `Pcre` again.
#[test]
fn reentrant_callbacks() {
    let mut pcre = Pcre::new();
    let outer = pcre.compile(b"/./").unwrap();
    let cfg = pcre.config();
    let out = replace_callback(&outer, &cfg, b"abc", usize::MAX, 0, |m| {
        let inner = pcre.compile(b"/n/").unwrap();
        let r = replace(
            &inner,
            &pcre.config(),
            b"inner",
            &Replacement::parse(b"N"),
            usize::MAX,
        );
        assert_eq!(r.result.unwrap(), b"iNNer");
        let s = split(&inner, &pcre.config(), b"inner", -1, 0).unwrap();
        assert_eq!(s.len(), 3);
        Some(bytes(m.get_index(0)).to_ascii_uppercase())
    });
    assert_eq!(out.result.unwrap(), b"ABC");

    let a = pcre.compile(b"/a/").unwrap();
    let out = replace_callback(&a, &cfg, b"a", usize::MAX, 0, |_| {
        let b = pcre.compile(b"/b/").unwrap();
        let cfg = pcre.config();
        replace_callback(&b, &cfg, b"b", usize::MAX, 0, |_| {
            let c = pcre.compile(b"/c/").unwrap();
            replace_callback(&c, &pcre.config(), b"c", usize::MAX, 0, |_| {
                Some(b"SUCCESS".to_vec())
            })
            .result
            .ok()
        })
        .result
        .ok()
    });
    assert_eq!(out.result.unwrap(), b"SUCCESS");
}

/// `mark_named_collision.phpt`
#[test]
fn mark_overwrites_named_group_in_place() {
    let re = Regex::compile(b"/(?P<MARK>[abc])(*MARK:value)/").unwrap();
    let o = match_one(&re, &MatchConfig::default(), b"abc", true, 0, 0);
    let m = o.matches.unwrap();
    assert_eq!(m.len(), 3);
    assert_eq!(bytes(m.get_name(b"MARK")), b"value");
    let keys: Vec<_> = m.keys().cloned().collect();
    assert_eq!(keys, vec![Key::Index(0), Key::name(b"MARK"), Key::Index(1)]);
}

/// `bug72688.phpt`: more than 255 named groups.
#[test]
fn many_named_groups() {
    let pattern: Vec<String> = (0..300).map(|i| format!("(?'group{i}'{i}$)")).collect();
    let full = format!("/{}/uix", pattern.join("|"));
    let re = Regex::compile(full.as_bytes()).unwrap();
    let o = match_one(&re, &MatchConfig::default(), b"290", true, 0, 0);
    assert_eq!(bytes(o.matches.unwrap().get_name(b"group290")), b"290");
}

/// `bug47662.phpt`: 129 named groups.
#[test]
fn named_groups_beyond_prealloc() {
    let mut regex = String::from("@");
    for i in 0..129 {
        let _ = write!(regex, "((?P<x{i}>))");
    }
    regex.push_str("fo+bar@");
    let re = Regex::compile(regex.as_bytes()).unwrap();
    let o = match_one(&re, &MatchConfig::default(), b"foobar", false, 0, 0);
    assert_eq!(o.result, Ok(1));
}

/// `bug72685.phpt`: anchored scanning with an offset stays linear once the
/// subject is known to be valid UTF-8 (PHP flags the string after the first
/// successful `/u` match at offset 0; the caller passes the hint back).
#[test]
fn offset_scan_is_fast() {
    let re = Regex::compile(b"/\\G\\w/u").unwrap();
    let mut cfg = MatchConfig::default();
    let s = vec![b'a'; 64 * 1024];
    let start = std::time::Instant::now();
    let mut pos = 0;
    loop {
        let o = match_one(&re, &cfg, &s, true, 0, pos);
        if o.subject_valid_utf8 {
            cfg = cfg.with_known_valid_utf8(true);
        }
        if o.result != Ok(1) {
            break;
        }
        pos += 1;
    }
    assert_eq!(pos, 64 * 1024);
    assert!(cfg.known_valid_utf8);
    assert!(start.elapsed().as_secs() < 5, "{:?}", start.elapsed());
}

/// `007.phpt`: subject validity is checked on every call.
#[test]
fn utf8_checked_per_call() {
    let re = Regex::compile(b"#.#u").unwrap();
    let cfg = MatchConfig::default();
    let o = replace_callback(&re, &cfg, b"ola123", usize::MAX, 0, |m| {
        Some(bytes(m.get_index(0)).to_vec())
    });
    assert_eq!(o.result.unwrap(), b"ola123");
    let o = replace_callback(&re, &cfg, b"ola\xff23", usize::MAX, 0, |_| Some(Vec::new()));
    assert_eq!(o.result, Err(PregError::BadUtf8));
    assert_eq!(o.count, 0);
}

#[test]
fn constants() {
    assert_eq!(PREG_PATTERN_ORDER, 1);
    assert_eq!(PREG_SET_ORDER, 2);
    assert_eq!(PREG_OFFSET_CAPTURE, 256);
    assert_eq!(PREG_UNMATCHED_AS_NULL, 512);
    assert_eq!(PREG_SPLIT_NO_EMPTY, 1);
    assert_eq!(PREG_SPLIT_DELIM_CAPTURE, 2);
    assert_eq!(PREG_SPLIT_OFFSET_CAPTURE, 4);
    assert_eq!(PREG_GREP_INVERT, 1);
    assert_eq!(PREG_JIT_STACKLIMIT_ERROR, PregError::JitStackLimit.code());
    assert_eq!(PREG_BAD_UTF8_OFFSET_ERROR, PregError::BadUtf8Offset.code());
}

/// `locales.phpt`: locale-aware patterns are cached per `LC_CTYPE` name and
/// use character tables built from the process locale (the C locale here).
#[test]
fn locale_aware_cache_keys() {
    let mut pcre = Pcre::new();
    let plain = pcre.compile(b"/^\\w$/").unwrap();
    let localized = pcre
        .compile_with_locale(b"/^\\w$/", Some(b"de_DE"))
        .unwrap();
    assert!(!Regex::ptr_eq(&plain, &localized));
    let again = pcre
        .compile_with_locale(b"/^\\w$/", Some(b"de_DE"))
        .unwrap();
    assert!(Regex::ptr_eq(&localized, &again));
    assert_eq!(pcre.cache().len(), 2);
    let cfg = pcre.config();
    // In the C locale \xe4 is not a word character.
    assert_eq!(
        match_one(&localized, &cfg, b"\xe4", false, 0, 0).result,
        Ok(0)
    );
    assert_eq!(match_one(&localized, &cfg, b"a", false, 0, 0).result, Ok(1));
    // Delimiter errors are reported before any table work.
    assert_eq!(
        pcre.compile_with_locale(b"abc", Some(b"de_DE"))
            .unwrap_err(),
        CompileError::BadDelimiter
    );
}

/// `VmSize` of this process in kB (Linux only).
#[cfg(target_os = "linux")]
fn vm_size_kb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmSize:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap()
}

/// The JIT stack is created once per thread and assigned through the match
/// context; match data and contexts are freed on every path (including
/// errors and callbacks that abort), so address space stays flat over many
/// calls and across threads.
#[cfg(target_os = "linux")]
#[test]
fn no_address_space_growth() {
    let re = Regex::compile(b"/(\\w+)(\\d)/").unwrap();
    let bad = Regex::compile(b"/x/u").unwrap();
    let cfg = MatchConfig::default();
    let s = b"abc123 ".repeat(20);
    let round = || {
        let _ = match_one(&re, &cfg, &s, true, 0, 0);
        let _ = match_all(&re, &cfg, &s, true, PREG_SET_ORDER, 0);
        let _ = match_one(&bad, &cfg, b"\xff", false, 0, 0);
        let _ = replace_callback(&re, &cfg, &s, usize::MAX, 0, |_| None);
        let _ = split(&re, &cfg, &s, -1, 0);
    };
    for _ in 0..1000 {
        round();
    }
    let before = vm_size_kb();
    for _ in 0..20_000 {
        round();
    }
    // Fresh threads each get (and on exit free) their own JIT stack.
    for _ in 0..200 {
        let (re, s) = (re.clone(), s.clone());
        std::thread::spawn(move || {
            let _ = match_one(&re, &MatchConfig::default(), &s, true, 0, 0);
        })
        .join()
        .unwrap();
    }
    let after = vm_size_kb();
    // A per-match or per-thread JIT stack leak would add many megabytes;
    // allow some slack for other tests running concurrently.
    assert!(
        after < before + 32 * 1024,
        "VmSize grew from {before} kB to {after} kB"
    );
}

/// An empty subject or pattern reaches PCRE2 at a real address: its
/// vectorised scans (aarch64 NEON in libpcre2 10.46) read the aligned
/// 16-byte block around the subject pointer first, which faulted at
/// address 0 for an empty slice's dangling `0x1` (php-src passes
/// `ZSTR_VAL` of the empty string, a real address).
#[test]
fn empty_subjects_and_patterns_are_matched_at_a_real_address() {
    for jit in [true, false] {
        let mut pcre = Pcre::new();
        pcre.set_jit(jit);
        for (pattern, want) in [
            (&b"/\\0/i"[..], 0),
            (b"/a/", 0),
            (b"/x?/", 1),
            (b"/^$/", 1),
            (b"//", 1),
        ] {
            let re = pcre.compile(pattern).expect("compiles");
            let o = match_one(&re, &pcre.config(), b"", false, 0, 0);
            assert_eq!(
                o.result.ok(),
                Some(want),
                "{} (jit: {jit})",
                String::from_utf8_lossy(pattern)
            );
        }
    }
}

/// A subject whose last byte is the last byte of a page, with nothing
/// readable after it: PCRE2's vectorised scans (aarch64 NEON) read the
/// aligned 16-byte block at the subject's end, which is on the next page
/// (an intermittent crash of the Laravel app's feature tests). The match
/// runs from a copy with a byte after it, and reports the same results.
#[test]
fn subjects_ending_before_an_unreadable_page_are_matched() {
    // SAFETY (test only): two fresh anonymous pages, the second made
    // inaccessible; the subject is a slice of the first that ends where it
    // ends. Both are unmapped at the end, after the last use of the slice.
    unsafe {
        let page = usize::try_from(libc::sysconf(libc::_SC_PAGESIZE)).expect("page size");
        let base = libc::mmap(
            std::ptr::null_mut(),
            2 * page,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        assert_ne!(base, libc::MAP_FAILED);
        let base = base.cast::<u8>();
        assert_eq!(
            libc::mprotect(base.add(page).cast(), page, libc::PROT_NONE),
            0
        );
        let len = 100;
        let start = base.add(page - len);
        std::ptr::write_bytes(start, b'a', len);
        let subject = std::slice::from_raw_parts(start, len);
        assert!(super::ffi::ends_on_page_boundary(subject));
        for jit in [true, false] {
            let mut pcre = Pcre::new();
            pcre.set_jit(jit);
            for (pattern, offset, want) in [
                (&b"/z/"[..], 0, 0),
                (b"/\\0/i", 0, 0),
                (b"/a$/", 0, 1),
                (b"/b|c/", 90, 0),
                (b"/a/", 100, 0),
            ] {
                let re = pcre.compile(pattern).expect("compiles");
                let o = match_one(&re, &pcre.config(), subject, false, 0, offset);
                assert_eq!(
                    o.result.ok(),
                    Some(want),
                    "{} at {offset} (jit: {jit})",
                    String::from_utf8_lossy(pattern)
                );
            }
        }
        assert_eq!(libc::munmap(base.cast(), 2 * page), 0);
    }
}
