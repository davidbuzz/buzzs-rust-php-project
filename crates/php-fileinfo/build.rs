// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Links the system libmagic, located through `pkg-config` when available.
//! It is bound lazily (`build/lazy_dl.rs`, shared from php-libxml: stubs
//! that `dlopen` it on first use), falling back to an ordinary link.

use std::process::Command;

#[path = "../php-libxml/build/lazy_dl.rs"]
mod lazy_dl;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-changed=../php-libxml/build/lazy_dl.rs");
    if lazy_dl::generate(&lazy_dl::LazyLib {
        tag: "magic",
        name: "magic",
        pkg: Some("libmagic"),
        suffix: None,
    }) {
        return;
    }
    let output = Command::new("pkg-config")
        .args(["--libs", "libmagic"])
        .output();
    let mut linked = false;
    if let Ok(out) = output {
        if out.status.success() {
            let flags = String::from_utf8_lossy(&out.stdout);
            for flag in flags.split_whitespace() {
                if let Some(dir) = flag.strip_prefix("-L") {
                    println!("cargo:rustc-link-search=native={dir}");
                } else if let Some(lib) = flag.strip_prefix("-l") {
                    println!("cargo:rustc-link-lib={lib}");
                    linked |= lib == "magic";
                }
            }
        }
    }
    if !linked {
        println!("cargo:rustc-link-lib=magic");
    }
}
