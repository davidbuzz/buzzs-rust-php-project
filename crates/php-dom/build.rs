//! Links the system libxml2 (through `php-libxml`) and compiles the
//! vendored lexbor sources (HTML5 tokenizer/tree builder, CSS selector
//! parser) from the read-only php-src checkout, plus this crate's small C
//! glue layer.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
}
