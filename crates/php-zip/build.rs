//! Links the system libzip (found through pkg-config) and exports its version
//! so the crate can gate constants and methods like PHP's `config.m4` does.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let lib = match pkg_config::Config::new()
        .atleast_version("1.0")
        .probe("libzip")
    {
        Ok(lib) => lib,
        Err(e) => panic!("libzip not found through pkg-config: {e}"),
    };
    println!("cargo:rustc-env=PHP_ZIP_LIBZIP_VERSION={}", lib.version);
    let mut parts = lib
        .version
        .split('.')
        .map(|p| p.parse::<u32>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let micro = parts.next().unwrap_or(0);
    let packed = (major << 16) | (minor << 8) | micro;
    println!("cargo:rustc-env=PHP_ZIP_LIBZIP_VERSION_ID={packed}");
    println!("cargo::rustc-check-cfg=cfg(libzip_1_8)");
    println!("cargo::rustc-check-cfg=cfg(libzip_1_9_1)");
    println!("cargo::rustc-check-cfg=cfg(libzip_1_10)");
    if packed >= 0x01_08_00 {
        println!("cargo:rustc-cfg=libzip_1_8");
    }
    if packed >= 0x01_09_01 {
        println!("cargo:rustc-cfg=libzip_1_9_1");
    }
    if packed >= 0x01_0a_00 {
        println!("cargo:rustc-cfg=libzip_1_10");
    }
}
