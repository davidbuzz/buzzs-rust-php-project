//! Links the system Oniguruma (`libonig`) behind the `mb_ereg*` functions
//! (see `src/onig.rs`).
//!
//! Lookup order:
//! 1. `ONIG_LIB_DIR` (a directory holding `libonig.so` or `libonig.so.5`);
//! 2. `pkg-config --libs oniguruma` (installed by `libonig-dev`);
//! 3. the usual library directories: a `libonig.so` development link is
//!    linked as `-lonig`; when only the runtime `libonig.so.5` exists (the
//!    `libonig5` package without `libonig-dev`), that file is linked by its
//!    exact name (`-l:libonig.so.5`, rustc's `+verbatim` modifier), which
//!    needs no development symlink;
//! 4. otherwise plain `-lonig`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Runtime sonames tried when no `libonig.so` development link exists.
const SONAMES: &[&str] = &["libonig.so.5"];

/// Library directories searched when pkg-config knows no `oniguruma`.
fn lib_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if !arch.is_empty() && os == "linux" {
        let abi = if env.is_empty() {
            "gnu".to_string()
        } else {
            env
        };
        dirs.push(PathBuf::from(format!("/usr/lib/{arch}-linux-{abi}")));
        dirs.push(PathBuf::from(format!("/lib/{arch}-linux-{abi}")));
    }
    for d in ["/usr/lib64", "/usr/lib", "/usr/local/lib", "/lib64", "/lib"] {
        dirs.push(PathBuf::from(d));
    }
    dirs
}

/// Links through pkg-config; `true` when it named `-lonig`.
fn link_pkg_config() -> bool {
    let Ok(out) = Command::new("pkg-config")
        .args(["--libs", "oniguruma"])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let flags = String::from_utf8_lossy(&out.stdout);
    let mut linked = false;
    for flag in flags.split_whitespace() {
        if let Some(dir) = flag.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={dir}");
        } else if let Some(lib) = flag.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={lib}");
            linked |= lib == "onig";
        }
    }
    linked
}

/// Links from a directory holding `libonig.so` or a runtime soname.
fn link_from_dir(dir: &Path) -> bool {
    if dir.join("libonig.so").exists() {
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=dylib=onig");
        return true;
    }
    for so in SONAMES {
        if dir.join(so).exists() {
            println!("cargo:rustc-link-search=native={}", dir.display());
            println!("cargo:rustc-link-lib=dylib:+verbatim={so}");
            return true;
        }
    }
    false
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=ONIG_LIB_DIR");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    if let Ok(d) = std::env::var("ONIG_LIB_DIR") {
        if link_from_dir(Path::new(&d)) {
            return;
        }
    }
    if link_pkg_config() {
        return;
    }
    for dir in lib_dirs() {
        if link_from_dir(&dir) {
            return;
        }
    }
    println!("cargo:rustc-link-lib=dylib=onig");
}
