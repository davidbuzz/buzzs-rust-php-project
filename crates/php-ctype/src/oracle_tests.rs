// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Replays `tests/fixtures/oracle.txt`, recorded from `/usr/bin/php8.5` by
//! `tests/oracle/gen_oracle.php`: every `ctype_*` function over all bytes,
//! integers and other types, plus every call made by `ext/ctype/tests/*.phpt`.

use crate::{ctype_call, CtypeClass};
use php_runtime::{Obj, PhpObject, Resource, Value, ZArray, ZStr};
use std::any::Any;

const FIXTURE: &str = include_str!("../tests/fixtures/oracle.txt");

struct TestObject(ZStr);

impl PhpObject for TestObject {
    fn handle(&self) -> u32 {
        1
    }
    fn class_name(&self) -> ZStr {
        self.0.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn arg(spec: &str) -> Value {
    let (tag, rest) = spec.split_once(':').unwrap_or((spec, ""));
    match tag {
        "s" => Value::from(unhex(rest).as_slice()),
        "i" => Value::Int(rest.parse().expect("int")),
        "d" => Value::Float(match rest {
            "NAN" => f64::NAN,
            "INF" => f64::INFINITY,
            "-INF" => f64::NEG_INFINITY,
            r => r.parse().expect("float"),
        }),
        "b" => Value::Bool(rest == "1"),
        "n" => Value::Null,
        "a" => Value::Array(ZArray::new()),
        "o" => Value::Object(Obj::new(TestObject(ZStr::from(rest)))),
        "r" => Value::Resource(Resource::new("stream", None)),
        _ => panic!("bad arg spec {spec}"),
    }
}

#[test]
fn oracle() {
    let mut n = 0;
    let mut failures = Vec::new();
    for line in FIXTURE.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        let class = CtypeClass::from_function_name(f[0]).expect("function");
        let out = ctype_call(class, &arg(f[1]));
        let dep = out
            .prefixed_deprecation(class)
            .unwrap_or_else(|| "-".to_string());
        if out.result != (f[2] == "1") || dep != f[3] {
            failures.push(format!("{line}\n  got {} {dep}", u8::from(out.result)));
        }
        n += 1;
    }
    assert!(n > 10_000, "fixture too small: {n}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Regenerates the fixture with `/usr/bin/php8.5`:
/// `cargo test -p php-ctype -- --ignored regenerate_oracle`.
#[test]
#[ignore = "needs /usr/bin/php8.5 and modules/php-src"]
fn regenerate_oracle() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/oracle/gen_oracle.php");
    let status = std::process::Command::new("php")
        .arg(dir)
        .status()
        .expect("run php");
    assert!(status.success());
}
