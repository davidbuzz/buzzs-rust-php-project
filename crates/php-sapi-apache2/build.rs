//! Compiles the httpd module shim (`csrc/mod_phprust.c`) against the
//! Apache/APR headers located with `apxs` / `apr-1-config`, and links the
//! object into the cdylib only (the rlib and test binaries never reference
//! httpd symbols: Rust reaches httpd through a function table the shim
//! registers at load time).

use std::path::PathBuf;
use std::process::Command;

fn query(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=csrc/mod_phprust.c");
    println!("cargo:rerun-if-changed=csrc/phprs_api.h");
    println!("cargo:rerun-if-env-changed=APXS");

    let apxs = std::env::var("APXS").unwrap_or_else(|_| "apxs".to_string());
    let mut includes: Vec<PathBuf> = Vec::new();
    let mut defines: Vec<String> = Vec::new();
    if let Some(dir) = query(&apxs, &["-q", "INCLUDEDIR"]) {
        includes.push(dir.into());
    }
    if let Some(dir) = query(&apxs, &["-q", "APR_INCLUDEDIR"]) {
        includes.push(dir.into());
    }
    if let Some(flags) = query("apr-1-config", &["--includes", "--cppflags"]) {
        for f in flags.split_whitespace() {
            if let Some(dir) = f.strip_prefix("-I") {
                includes.push(dir.into());
            } else if let Some(def) = f.strip_prefix("-D") {
                defines.push(def.to_string());
            }
        }
    }
    for fallback in ["/usr/include/apache2", "/usr/include/apr-1.0", "/usr/include/apr-1"] {
        let p = PathBuf::from(fallback);
        if p.is_dir() {
            includes.push(p);
        }
    }
    if !includes.iter().any(|d| d.join("httpd.h").is_file()) {
        println!(
            "cargo:warning=httpd headers not found (install apache2-dev / set APXS): \
             libphprust_apache2.so will not contain the Apache module"
        );
        return;
    }

    let mut build = cc::Build::new();
    build.file("csrc/mod_phprust.c").include("csrc").warnings(true);
    for dir in &includes {
        build.include(dir);
    }
    for def in &defines {
        match def.split_once('=') {
            Some((k, v)) => build.define(k, Some(v)),
            None => build.define(def, None),
        };
    }
    let objects = build.compile_intermediates();
    for obj in objects {
        println!("cargo:rustc-cdylib-link-arg={}", obj.display());
    }
}
