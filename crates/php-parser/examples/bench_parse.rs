// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Measures the time to parse a small script with each stack policy:
//! `cargo run --release --example bench_parse`.

use php_parser::{ParseOptions, StackPolicy};
use std::time::Instant;

const SCRIPT: &[u8] = b"<?php\nfunction f($a, $b = 1) {\n    foreach ($a as $k => $v) {\n        if ($v > $b) { echo \"$k: $v\\n\"; }\n    }\n    return array_sum($a) * 2;\n}\nclass C { public function __construct(private int $x = 0) {} public function get(): int { return $this->x; } }\necho f([1, 2, 3]), (new C(4))->get(), PHP_EOL;\n";

fn main() {
    let n = 20_000;
    for (name, stack) in [
        ("Auto", StackPolicy::Auto),
        ("CurrentThread", StackPolicy::CurrentThread),
        ("Spawn(512 MB)", StackPolicy::Spawn(512 * 1024 * 1024)),
    ] {
        let opts = ParseOptions {
            stack,
            ..ParseOptions::default()
        };
        let start = Instant::now();
        for _ in 0..n {
            php_parser::parse(SCRIPT, &opts).expect("parses");
        }
        let per = start.elapsed() / n;
        println!("{name:14} {per:?} per parse");
    }
}
