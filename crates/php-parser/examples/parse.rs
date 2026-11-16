// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Parses PHP files and prints the result:
//! `cargo run --example parse -- [-d|-e] FILE...`
//! (`-e` prints the AST exported as PHP code, `-d` the debug AST).

fn main() {
    let mut debug = false;
    let mut export = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-d" => debug = true,
            "-e" => export = true,
            path => {
                let src = std::fs::read(path).expect("read file");
                match php_parser::parse(&src, &php_parser::ParseOptions::default()) {
                    Ok(p) if debug => println!("{p:#?}"),
                    Ok(p) if export => print!(
                        "{}",
                        String::from_utf8_lossy(&php_parser::export::export_program(&p))
                    ),
                    Ok(_) => println!("{path}: OK"),
                    Err(e) => println!("{path}: {e}"),
                }
            }
        }
    }
}
