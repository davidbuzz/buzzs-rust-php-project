//! Parses PHP files and prints the result: `cargo run --example parse -- [-e] FILE...`
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
                    Ok(p) => {
                        if debug {
                            println!("{p:#?}");
                        } else if export {
                            let _ = &p; println!("(export not yet available)");
                        } else {
                            println!("{path}: OK");
                        }
                    }
                    Err(e) => println!("{path}: {e}"),
                }
            }
        }
    }
}
