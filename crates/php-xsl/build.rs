//! Locates the system libxslt/libexslt through `pkg-config` and compiles
//! the crate's small C glue (`csrc/glue.c`): the variadic error handlers
//! libxslt/libxml2 require (C-variadic functions cannot be defined in
//! stable Rust) and accessors for libxslt structure fields, so that no
//! libxslt structure layout has to be replicated in Rust.

use std::process::Command;

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

    let mut linked = false;
    if let Some(libs) = pkg_config(&["--libs", "libxslt", "libexslt"]) {
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
        for lib in ["exslt", "xslt", "xml2"] {
            println!("cargo:rustc-link-lib={lib}");
        }
    }
}
