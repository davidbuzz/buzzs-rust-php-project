//! Binds the system libgd (`libgd-dev`, found through pkg-config) lazily:
//! `build/lazy_dl.rs` (shared from php-libxml) generates stubs for the
//! libgd functions this crate calls, which `dlopen` libgd on first use, so
//! libgd and its image-format libraries (libheif, libtiff, libwebp,
//! fontconfig, ...) cost nothing at start-up. Falls back to an ordinary
//! link where the stubs are unsupported or `PHPRUST_EAGER_LINK` is set.

#[path = "../php-libxml/build/lazy_dl.rs"]
mod lazy_dl;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../php-libxml/build/lazy_dl.rs");
    let probe = pkg_config::Config::new()
        .atleast_version("2.3")
        .cargo_metadata(false)
        .probe("gdlib");
    if let Err(e) = &probe {
        println!("cargo:warning=libgd not found through pkg-config ({e})");
    }
    let lazy = lazy_dl::generate(&lazy_dl::LazyLib {
        tag: "gd",
        name: "gd",
        pkg: Some("gdlib"),
        suffix: None,
    });
    if lazy {
        return;
    }
    match probe {
        Ok(lib) => {
            for dir in &lib.link_paths {
                println!("cargo:rustc-link-search=native={}", dir.display());
            }
            for name in &lib.libs {
                println!("cargo:rustc-link-lib={name}");
            }
        }
        Err(_) => println!("cargo:rustc-link-lib=gd"),
    }
}
