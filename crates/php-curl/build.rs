// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Binds the system libcurl lazily: `build/lazy_dl.rs` (shared from
//! php-libxml) generates stubs for the libcurl functions this crate calls,
//! which `dlopen` libcurl on first use, so its large dependency tree
//! (gnutls, libssh, libldap, kerberos, nghttp2, ...) costs nothing at
//! start-up. Falls back to an ordinary `-lcurl` link where the stubs are
//! unsupported or `PHPRUST_EAGER_LINK` is set.

#[path = "../php-libxml/build/lazy_dl.rs"]
mod lazy_dl;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../php-libxml/build/lazy_dl.rs");
    let lazy = lazy_dl::generate(&lazy_dl::LazyLib {
        tag: "curl",
        name: "curl",
        pkg: Some("libcurl"),
        suffix: None,
    });
    if !lazy {
        println!("cargo:rustc-link-lib=curl");
    }
}
