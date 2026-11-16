// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Locates the system ICU through pkg-config and tells the crate which
//! versioned symbol suffix (`_74` for ICU 74.x) the C API is exported under.
//!
//! libicuuc is linked normally (libxml2 needs it anyway). libicui18n is
//! bound lazily: `build/lazy_dl.rs` (shared from php-libxml) generates stubs
//! for its functions the crate declares, which `dlopen` it on first use, so
//! its ~700 relocations cost nothing at start-up. It falls back to an
//! ordinary link where the stubs are unsupported or `PHPRUST_EAGER_LINK` is
//! set.

#[path = "../php-libxml/build/lazy_dl.rs"]
mod lazy_dl;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../php-libxml/build/lazy_dl.rs");
    println!("cargo:rerun-if-env-changed=PHP_INTL_ICU_SUFFIX");

    let mut version = String::new();
    let mut found = Vec::new();
    for lib in ["icu-uc", "icu-i18n", "icu-io"] {
        match pkg_config::Config::new().cargo_metadata(false).probe(lib) {
            Ok(probe) => {
                if version.is_empty() {
                    version.clone_from(&probe.version);
                }
                found.push(probe);
            }
            Err(e) => assert!(
                lib == "icu-io",
                "php-intl needs the system ICU ({lib}): {e}"
            ),
        }
    }
    let major = version.split('.').next().unwrap_or_default().to_owned();
    assert!(
        !major.is_empty() && major.bytes().all(|b| b.is_ascii_digit()),
        "cannot determine the ICU major version from pkg-config version {version:?}"
    );
    let suffix = std::env::var("PHP_INTL_ICU_SUFFIX").unwrap_or_else(|_| format!("_{major}"));
    println!("cargo:rustc-env=PHP_INTL_ICU_SUFFIX={suffix}");
    println!("cargo:rustc-env=PHP_INTL_ICU_VERSION={version}");
    println!("cargo:rustc-env=PHP_INTL_ICU_MAJOR={major}");

    let lazy = lazy_dl::generate(&lazy_dl::LazyLib {
        tag: "icui18n",
        name: "icui18n",
        pkg: Some("icu-i18n"),
        suffix: Some(&suffix),
    });
    let mut dirs = std::collections::HashSet::new();
    let mut seen = std::collections::HashSet::new();
    for lib in &found {
        for dir in &lib.link_paths {
            if dirs.insert(dir) {
                println!("cargo:rustc-link-search=native={}", dir.display());
            }
        }
        for name in &lib.libs {
            if !(lazy && name == "icui18n") && seen.insert(name) {
                println!("cargo:rustc-link-lib={name}");
            }
        }
    }
}
