//! Locates the system ICU through pkg-config and tells the crate which
//! versioned symbol suffix (`_74` for ICU 74.x) the C API is exported under.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PHP_INTL_ICU_SUFFIX");

    let mut version = String::new();
    for lib in ["icu-uc", "icu-i18n", "icu-io"] {
        match pkg_config::Config::new().probe(lib) {
            Ok(found) => {
                if version.is_empty() {
                    version = found.version;
                }
            }
            Err(e) => assert!(
                lib == "icu-io",
                "php-intl needs the system ICU ({lib}): {e}"
            ),
        }
    }
    let major = version.split('.').next().unwrap_or_default().to_owned();
    assert!(
        !major.is_empty() && major.bytes().all(|b| b.is_ascii_digit()),
        "cannot determine the ICU major version from pkg-config version {version:?}"
    );
    let suffix = std::env::var("PHP_INTL_ICU_SUFFIX").unwrap_or_else(|_| format!("_{major}"));
    println!("cargo:rustc-env=PHP_INTL_ICU_SUFFIX={suffix}");
    println!("cargo:rustc-env=PHP_INTL_ICU_VERSION={version}");
    println!("cargo:rustc-env=PHP_INTL_ICU_MAJOR={major}");
}
