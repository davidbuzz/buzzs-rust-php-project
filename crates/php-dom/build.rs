//! Compiles the vendored lexbor sources in `vendor/lexbor` (HTML5
//! tokenizer/tree builder, encodings, CSS selector parser; override with
//! `PHPRUST_LEXBOR_SRC`), php-src's libxml2-adapted lexbor selector engine
//! (`vendor/selectors-adapted/selectors.c`, built against a small Zend
//! shim), and this crate's C glue layer (`csrc/glue.c`). See
//! `vendor/NOTICE` for provenance and licenses.
//!
//! libxml2 itself is linked through `php-libxml`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Modules of the lexbor tree the DOM does not use (WHATWG URL support).
const UNUSED_MODULES: &[&str] = &["url", "unicode", "punycode"];

/// Every `.c` file of the lexbor tree under `root/lexbor`, minus the
/// unused modules, in a stable order.
fn lexbor_sources(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("c")) {
                out.push(p);
            }
        }
    }
    let base = root.join("lexbor");
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(&base) else {
        panic!("lexbor sources not found in {}", base.display())
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        if p.is_dir() && !UNUSED_MODULES.iter().any(|m| name == *m) {
            walk(&p, &mut out);
        }
    }
    out.sort();
    out
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
    // A directory containing `lexbor/` (the vendored copy by default).
    let lexbor_ext = std::env::var_os("PHPRUST_LEXBOR_SRC")
        .map_or_else(|| manifest.join("vendor/lexbor"), PathBuf::from);
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-changed=vendor");
    println!("cargo:rerun-if-env-changed=PHPRUST_LEXBOR_SRC");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    // 2. The adapted selector engine, copied next to shim headers so its
    //    relative `../../php_dom.h` includes resolve to the shims.
    let sel_root = out.join("selectors");
    let sel_dir = sel_root.join("dom/lexbor/selectors-adapted");
    fs::create_dir_all(&sel_dir).expect("create selectors dir");
    fs::create_dir_all(sel_root.join("inc/Zend")).expect("create shim dir");
    let adapted = manifest.join("vendor/selectors-adapted");
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

    // 4. lexbor. php-src builds it with small initial pools
    // (`-DLXB_HTML_TKZ_TOKEN_POOL_SIZE=16 -DLXB_HTML_TKZ_ATTR_POOL_SIZE=256
    // -DLEXBOR_MRAW_CACHE_SIZE=64`), but its blocks come from ZendMM, whose
    // `memory_limit` counts whole 2 MiB chunks; the counting allocator here
    // counts bytes. lexbor's own larger defaults keep the "Allowed memory
    // size" fatal of a document where php-src raises it (gh25118, gh25305).
    let mut lexbor = cc::Build::new();
    lexbor
        .include(&lexbor_ext)
        .define("LEXBOR_STATIC", None)
        .define("LXB_HTML_TKZ_TOKEN_POOL_SIZE", "4096")
        .define("LXB_HTML_TKZ_ATTR_POOL_SIZE", "4096")
        .define("LEXBOR_MRAW_CACHE_SIZE", "512")
        .warnings(false)
        .flag_if_supported("-Wno-unterminated-string-initialization")
        .opt_level(2);
    for src in lexbor_sources(&lexbor_ext) {
        lexbor.file(src);
    }
    lexbor.compile("phpdom_lexbor");
}
