//! Compiles the vendored lexbor sources (HTML5 tokenizer/tree builder,
//! encodings, CSS selector parser) from the read-only php-src checkout,
//! php-src's libxml2-adapted lexbor selector engine
//! (`ext/dom/lexbor/selectors-adapted/selectors.c`, built against a small
//! Zend shim), and this crate's C glue layer (`csrc/glue.c`).
//!
//! libxml2 itself is linked through `php-libxml`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Source files of ext/lexbor (from its `config.m4`, minus `php_lexbor.c`).
fn lexbor_sources(lexbor_ext: &Path) -> Vec<PathBuf> {
    let config =
        fs::read_to_string(lexbor_ext.join("config.m4")).expect("ext/lexbor/config.m4 is readable");
    config
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("$LEXBOR_DIR/"))
        .filter(|l| {
            Path::new(l)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("c"))
        })
        .map(|l| lexbor_ext.join("lexbor").join(l))
        .collect()
}

fn libxml2_includes() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(out) = Command::new("pkg-config")
        .args(["--cflags-only-I", "libxml-2.0"])
        .output()
    {
        if out.status.success() {
            for flag in String::from_utf8_lossy(&out.stdout).split_whitespace() {
                if let Some(d) = flag.strip_prefix("-I") {
                    dirs.push(PathBuf::from(d));
                }
            }
        }
    }
    if dirs.is_empty() {
        dirs.push(PathBuf::from("/usr/include/libxml2"));
    }
    dirs
}

fn main() {
    let manifest =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let php_src = manifest.join("../../modules/php-src");
    let lexbor_ext = php_src.join("ext/lexbor");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    // 2. The adapted selector engine, copied next to shim headers so its
    //    relative `../../php_dom.h` includes resolve to the shims.
    let sel_root = out.join("selectors");
    let sel_dir = sel_root.join("dom/lexbor/selectors-adapted");
    fs::create_dir_all(&sel_dir).expect("create selectors dir");
    fs::create_dir_all(sel_root.join("inc/Zend")).expect("create shim dir");
    let adapted = php_src.join("ext/dom/lexbor/selectors-adapted");
    for f in ["selectors.c", "selectors.h"] {
        fs::copy(adapted.join(f), sel_dir.join(f)).expect("copy selectors-adapted source");
    }
    let shim = "#include \"phpdom_shim.h\"\n";
    for h in [
        "dom/php_dom.h",
        "dom/namespace_compat.h",
        "dom/domexception.h",
        "inc/php.h",
        "inc/Zend/zend.h",
        "inc/Zend/zend_operators.h",
        "inc/Zend/zend_API.h",
    ] {
        fs::write(sel_root.join(h), shim).expect("write shim header");
    }
    // 3. Glue + selector engine in one archive (they reference each other),
    //    linked before lexbor which both depend on.
    let mut glue = cc::Build::new();
    glue.include(sel_root.join("inc"))
        .include(manifest.join("csrc"))
        .include(&lexbor_ext)
        .include(&sel_dir)
        .define("LEXBOR_STATIC", None)
        .opt_level(2)
        .warnings(false)
        .file(manifest.join("csrc/glue.c"))
        .file(sel_dir.join("selectors.c"));
    for d in libxml2_includes() {
        glue.include(d);
    }
    glue.compile("phpdom_glue");

    // 4. lexbor.
    let mut lexbor = cc::Build::new();
    lexbor
        .include(&lexbor_ext)
        .define("LEXBOR_STATIC", None)
        .warnings(false)
        .flag_if_supported("-Wno-unterminated-string-initialization")
        .opt_level(2);
    for src in lexbor_sources(&lexbor_ext) {
        lexbor.file(src);
    }
    lexbor.compile("phpdom_lexbor");
}
