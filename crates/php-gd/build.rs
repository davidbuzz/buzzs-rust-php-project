//! Links the system libgd (`libgd-dev`), found through pkg-config.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    match pkg_config::Config::new().atleast_version("2.3").probe("gdlib") {
        Ok(_) => {}
        Err(e) => {
            println!("cargo:warning=libgd not found through pkg-config ({e}); linking -lgd directly");
            println!("cargo:rustc-link-lib=gd");
        }
    }
}
