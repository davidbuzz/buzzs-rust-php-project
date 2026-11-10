//! Locates the system libxslt/libexslt through `pkg-config` and compiles
//! the crate's small C glue (`csrc/glue.c`): the variadic error handlers
//! libxslt/libxml2 require (C-variadic functions cannot be defined in
//! stable Rust) and accessors for libxslt structure fields, so that no
//! libxslt structure layout has to be replicated in Rust.
//!
//! libxslt and libexslt are bound lazily: `build/lazy_dl.rs` (shared from
//! php-libxml) generates stubs for the functions this crate and its glue
//! call, which `dlopen` the libraries on first use (libexslt also pulls in
//! libgcrypt), so they cost nothing at start-up. Each falls back to an
//! ordinary link where the stubs are unsupported or `PHPRUST_EAGER_LINK`
//! is set. libxml2 is always linked (through php-libxml as well).

use std::process::Command;

#[path = "../php-libxml/build/lazy_dl.rs"]
mod lazy_dl;

fn pkg_config(args: &[&str]) -> Option<String> {
    let out = Command::new("pkg-config").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=csrc/glue.c");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    let mut build = cc::Build::new();
    build.file("csrc/glue.c").warnings(true);

    match pkg_config(&["--cflags", "libxslt", "libexslt"]) {
        Some(cflags) => {
            for flag in cflags.split_whitespace() {
                if let Some(dir) = flag.strip_prefix("-I") {
                    build.include(dir);
                } else if let Some(def) = flag.strip_prefix("-D") {
                    let (k, v) = def
                        .split_once('=')
                        .map_or((def, None), |(k, v)| (k, Some(v)));
                    build.define(k, v);
                }
            }
        }
        None => {
            build.include("/usr/include/libxml2");
        }
    }
    build.compile("php_xsl_glue");

    println!("cargo:rerun-if-changed=../php-libxml/build/lazy_dl.rs");
    let mut eager = Vec::new();
    for (tag, pkg) in [("xslt", "libxslt"), ("exslt", "libexslt")] {
        let lazy = lazy_dl::generate(&lazy_dl::LazyLib {
            tag,
            name: tag,
            pkg: Some(pkg),
            suffix: None,
        });
        if !lazy {
            eager.push(pkg);
        }
    }
    if eager.is_empty() {
        println!("cargo:rustc-link-lib=xml2");
        return;
    }
    let mut args = vec!["--libs"];
    args.extend(&eager);
    let mut linked = false;
    if let Some(libs) = pkg_config(&args) {
        for flag in libs.split_whitespace() {
            if let Some(dir) = flag.strip_prefix("-L") {
                println!("cargo:rustc-link-search=native={dir}");
            } else if let Some(lib) = flag.strip_prefix("-l") {
                println!("cargo:rustc-link-lib={lib}");
                linked = true;
            }
        }
    }
    if !linked {
        for pkg in eager {
            println!("cargo:rustc-link-lib={}", &pkg[3..]);
        }
        println!("cargo:rustc-link-lib=xml2");
    }
}
