//! Locates the system libxml2 through `pkg-config` (package `libxml-2.0`)
//! and emits the link directives. Falls back to a plain `-lxml2` when
//! `pkg-config` is unavailable.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=LIBXML2_LIB_DIR");

    if let Ok(dir) = std::env::var("LIBXML2_LIB_DIR") {
        println!("cargo:rustc-link-search=native={dir}");
        println!("cargo:rustc-link-lib=xml2");
        return;
    }

    let output = Command::new("pkg-config")
        .args(["--libs", "libxml-2.0"])
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
                    linked = true;
                }
            }
        }
    }
    if !linked {
        println!("cargo:rustc-link-lib=xml2");
    }
}
