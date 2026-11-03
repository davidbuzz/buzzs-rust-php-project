//! Links the system libsodium, located through `pkg-config` when available.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    let output = Command::new("pkg-config")
        .args(["--libs", "libsodium"])
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
                    linked |= lib == "sodium";
                }
            }
        }
    }
    if !linked {
        println!("cargo:rustc-link-lib=magic");
    }
}
