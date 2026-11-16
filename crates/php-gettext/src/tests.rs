// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Unit tests.  Tests touching libintl's process-global state (locale,
//! default domain, bindings) serialise on [`GLOBAL`]; the catalog tests use
//! the `.mo` fixtures copied from `ext/gettext/tests` into
//! `tests/fixtures`.

use super::*;
use std::ffi::CString;
use std::sync::Mutex;

static GLOBAL: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    GLOBAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn fixtures() -> String {
    format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"))
}

fn ve(msg: &str) -> GettextError {
    GettextError::ValueError(msg.to_owned())
}

/// Switches to `en_US.UTF-8`, or returns `false` (test skipped) when the
/// system lacks that locale.
fn en_us() -> bool {
    std::env::set_var("LC_ALL", "en_US.UTF-8");
    std::env::remove_var("LANGUAGE");
    let loc = CString::new("en_US.UTF-8").unwrap();
    sys::setlocale(libc::LC_ALL, &loc)
}

fn c_locale() {
    std::env::remove_var("LC_ALL");
    let loc = CString::new("C").unwrap();
    sys::setlocale(libc::LC_ALL, &loc);
}

#[test]
fn overlong_arguments_44938() {
    // ext/gettext/tests/44938.phpt
    let long = vec![b'C'; 8_476_509];
    let too_long = |a: u32, n: &str| ve(&format!("Argument #{a} (${n}) is too long"));
    assert_eq!(
        bindtextdomain(&long, Some(b"path")),
        Err(too_long(1, "domain"))
    );
    assert_eq!(
        dngettext(&long, b"msgid", b"msgid", 1),
        Err(too_long(1, "domain"))
    );
    assert_eq!(
        dngettext(b"domain", &long, b"msgid", 1),
        Err(too_long(2, "singular"))
    );
    assert_eq!(
        dngettext(b"domain", b"msgid", &long, 1),
        Err(too_long(3, "plural"))
    );
    assert_eq!(gettext(&long), Err(too_long(1, "message")));
    assert_eq!(ngettext(&long, b"msgid", -1), Err(too_long(1, "singular")));
    assert_eq!(ngettext(b"msgid", &long, -1), Err(too_long(2, "plural")));
    assert_eq!(dcgettext(&long, b"msgid", -1), Err(too_long(1, "domain")));
    assert_eq!(dcgettext(b"domain", &long, -1), Err(too_long(2, "message")));
    assert_eq!(
        dcngettext(&long, b"m", b"m", -1, -1),
        Err(too_long(1, "domain"))
    );
    assert_eq!(
        dcngettext(b"d", &long, b"m", -1, -1),
        Err(too_long(2, "singular"))
    );
    assert_eq!(
        dcngettext(b"d", b"m", &long, -1, -1),
        Err(too_long(3, "plural"))
    );
    assert_eq!(dgettext(&long, b"msgid"), Err(too_long(1, "domain")));
    assert_eq!(dgettext(b"domain", &long), Err(too_long(2, "message")));
    assert_eq!(textdomain(Some(&long)), Err(too_long(1, "domain")));
    // Exactly at the limits is fine.
    let _g = lock();
    assert!(gettext(&vec![b'x'; MAX_MSGID_LENGTH]).is_ok());
    assert!(dgettext(&vec![b'x'; MAX_DOMAIN_LENGTH], b"m").is_ok());
}

#[test]
fn lc_all_category_rejected() {
    // dcgettext_lcall.phpt
    assert_eq!(
        dcgettext(b"dngettextTest", b"item", LC_ALL),
        Err(ve("Argument #3 ($category) cannot be LC_ALL"))
    );
    assert_eq!(
        dcngettext(b"dngettextTest", b"item", b"item2", 1, LC_ALL),
        Err(ve("Argument #5 ($category) cannot be LC_ALL"))
    );
    assert_eq!(LC_ALL, 6);
}

#[test]
fn dcngettext_basic() {
    // dcngettext.phpt
    let _g = lock();
    assert_eq!(dcngettext(b"1", b"1", b"1", 1, 1).unwrap(), b"1");
    assert_eq!(
        dcngettext(b"test", b"test", b"test", 1, 1).unwrap(),
        b"test"
    );
    assert_eq!(
        dcngettext(b"test", b"test", b"test", 0, 1).unwrap(),
        b"test"
    );
    assert_eq!(
        dcngettext(b"test", b"test", b"test", -1, -1).unwrap(),
        b"test"
    );
    let empty = Err(ve("Argument #1 ($domain) must not be empty"));
    assert_eq!(dcngettext(b"", b"", b"", 1, 1), empty);
    assert_eq!(dcngettext(b"", b"", b"", 0, 1), empty);
    // Untranslated plural selection: n == 1 → singular, else plural.
    assert_eq!(dcngettext(b"x", b"one", b"many", 1, 5).unwrap(), b"one");
    assert_eq!(dcngettext(b"x", b"one", b"many", 2, 5).unwrap(), b"many");
    assert_eq!(ngettext(b"one", b"many", 0).unwrap(), b"many");
    assert_eq!(ngettext(b"one", b"many", -1).unwrap(), b"many");
}

#[test]
fn empty_domain_errors() {
    let empty = ve("Argument #1 ($domain) must not be empty");
    assert_eq!(bindtextdomain(b"", Some(b"foobar")), Err(empty.clone()));
    assert_eq!(
        bind_textdomain_codeset(b"", Some(b"UTF-8")),
        Err(empty.clone())
    );
    assert_eq!(dgettext(b"", b"x"), Err(empty.clone()));
    assert_eq!(textdomain(Some(b"")), Err(empty));
    assert_eq!(
        textdomain(Some(b"0")),
        Err(ve("Argument #1 ($domain) cannot be zero"))
    );
}

#[test]
fn null_bytes_gh17400() {
    let first = b"\x00\xd8\x00\xdc";
    let bad = b"\x00\xdc\x00\xdc";
    assert_eq!(
        bindtextdomain(bad, Some(first)),
        Err(ve("Argument #1 ($domain) must not contain any null bytes"))
    );
    assert_eq!(
        bindtextdomain(b"", Some(b"a\0b")),
        Err(ve(
            "Argument #2 ($directory) must not contain any null bytes"
        ))
    );
    // Non-path strings are silently truncated at the NUL, but an
    // untranslated message comes back whole.
    let _g = lock();
    assert_eq!(gettext(b"ab\0c").unwrap(), b"ab\0c");
    assert_eq!(ngettext(b"a\0b", b"c", 1).unwrap(), b"a");
}

#[test]
fn textdomain_retval_and_bug73730() {
    let _g = lock();
    assert_eq!(textdomain(None).unwrap(), b"messages");
    assert_eq!(textdomain(Some(b"test")).unwrap(), b"test");
    assert_eq!(textdomain(None).unwrap(), b"test");
    assert_eq!(textdomain(Some(b"foo")).unwrap(), b"foo");
    assert_eq!(textdomain(Some(b"a\0b")).unwrap(), b"a");
    assert_eq!(textdomain(Some(b"messages")).unwrap(), b"messages");
}

#[test]
fn bindtextdomain_paths() {
    let _g = lock();
    // gettext_bindtextdomain-path.phpt: missing directory → false.
    let missing = format!("{}/foobar-does-not-exist", fixtures());
    assert_eq!(
        bindtextdomain(b"example.org", Some(missing.as_bytes())),
        Ok(None)
    );
    // "" and "0" bind to the cwd.
    let cwd = std::env::current_dir().unwrap();
    let cwd = cwd.to_str().unwrap().as_bytes().to_vec();
    assert_eq!(bindtextdomain(b"cwdtest", Some(b"")).unwrap().unwrap(), cwd);
    assert_eq!(
        bindtextdomain(b"cwdtest", Some(b"0")).unwrap().unwrap(),
        cwd
    );
    // Relative paths are canonicalised.
    let rel = bindtextdomain(b"cwdtest", Some(b"./src/.."))
        .unwrap()
        .unwrap();
    assert_eq!(rel, cwd);
    assert_eq!(bindtextdomain(b"cwdtest", None).unwrap().unwrap(), cwd);
}

#[test]
fn bug53251_query_forms() {
    let _g = lock();
    assert!(bindtextdomain(b"foo53251", None).unwrap().is_some());
    let dir = bindtextdomain(b"foo53251", Some(b".")).unwrap();
    assert_eq!(bindtextdomain(b"foo53251", None).unwrap(), dir);
    assert_eq!(bind_textdomain_codeset(b"foo53251", None), Ok(None));
    assert_eq!(
        bind_textdomain_codeset(b"foo53251", Some(b"UTF-8")),
        Ok(Some(b"UTF-8".to_vec()))
    );
    assert_eq!(
        bind_textdomain_codeset(b"foo53251", None),
        Ok(Some(b"UTF-8".to_vec()))
    );
}

#[test]
fn catalog_lookups() {
    let _g = lock();
    if !en_us() {
        eprintln!("skip: en_US.UTF-8 locale not supported");
        return;
    }
    let locale = format!("{}/locale", fixtures());
    let lb = locale.as_bytes();

    // gettext_basic-enus.phpt
    bindtextdomain(b"messages", Some(lb)).unwrap();
    textdomain(Some(b"messages")).unwrap();
    assert_eq!(gettext(b"Basic test").unwrap(), b"A basic test");

    // gettext_dgettext.phpt
    bindtextdomain(b"dgettextTest", Some(lb)).unwrap();
    bindtextdomain(b"dgettextTest_switch", Some(lb)).unwrap();
    textdomain(Some(b"dgettextTest")).unwrap();
    assert_eq!(gettext(b"item").unwrap(), b"Produkt");
    assert_eq!(
        dgettext(b"dgettextTest_switch", b"item").unwrap(),
        b"Produkt_switched"
    );
    assert_eq!(gettext(b"item").unwrap(), b"Produkt");

    // gettext_dngettext-plural.phpt / gettext_ngettext.phpt
    bindtextdomain(b"dngettextTest", Some(lb)).unwrap();
    assert_eq!(
        dngettext(b"dngettextTest", b"item", b"items", 1).unwrap(),
        b"Produkt"
    );
    assert_eq!(
        dngettext(b"dngettextTest", b"item", b"items", 2).unwrap(),
        b"Produkte"
    );
    textdomain(Some(b"dngettextTest")).unwrap();
    assert_eq!(ngettext(b"item", b"items", 1).unwrap(), b"Produkt");
    assert_eq!(ngettext(b"item", b"items", 2).unwrap(), b"Produkte");

    // gettext_dcgettext.phpt
    let lc_ctype = i64::from(libc::LC_CTYPE);
    let lc_messages = i64::from(libc::LC_MESSAGES);
    assert_eq!(
        dcgettext(b"dngettextTest", b"item", lc_ctype).unwrap(),
        b"cProdukt"
    );
    assert_eq!(
        dcgettext(b"dngettextTest", b"item", lc_messages).unwrap(),
        b"Produkt"
    );
    assert_eq!(
        dcngettext(b"dngettextTest", b"item", b"items", 2, lc_messages).unwrap(),
        b"Produkte"
    );

    // gettext_bind_textdomain_codeset-retval.phpt
    assert_eq!(
        bind_textdomain_codeset(b"messages", Some(b"UTF-8")),
        Ok(Some(b"UTF-8".to_vec()))
    );

    // gettext_bindtextdomain-cwd.phpt (with a fresh domain).
    let old = std::env::current_dir().unwrap();
    std::env::set_current_dir(fixtures()).unwrap();
    bindtextdomain(b"messages", None).unwrap();
    textdomain(Some(b"messages")).unwrap();
    bindtextdomain(b"messages", Some(b"./locale")).unwrap();
    assert_eq!(gettext(b"Basic test").unwrap(), b"A basic test");
    std::env::set_current_dir(old).unwrap();

    textdomain(Some(b"messages")).unwrap();
    c_locale();
}

#[test]
fn locale_switching_bug66267() {
    let _g = lock();
    let path = format!("{}/66265", fixtures());
    let mut ran = false;
    for (l, expected) in [("de_DE", "hallo"), ("fr_FR", "salut"), ("en_US", "hello")] {
        std::env::set_var("LC_ALL", l);
        let loc = CString::new(l).unwrap();
        if !sys::setlocale(libc::LC_ALL, &loc) {
            continue;
        }
        ran = true;
        bindtextdomain(b"domain", Some(path.as_bytes())).unwrap();
        bind_textdomain_codeset(b"domain", Some(b"UTF-8")).unwrap();
        textdomain(Some(b"domain")).unwrap();
        assert_eq!(gettext(b"hello").unwrap(), expected.as_bytes());
    }
    if !ran {
        eprintln!("skip: de_DE/fr_FR/en_US locales not supported");
    }
    textdomain(Some(b"messages")).unwrap();
    c_locale();
}

#[test]
fn error_display() {
    assert_eq!(
        GettextError::Error("Could not set text domain".into()).to_string(),
        "Could not set text domain"
    );
    assert_eq!(ve("x").to_string(), "x");
}
