//! Harness self-test.
mod common;
use common::{check, pure, regenerate, Case};

fn cases() -> Vec<Case> {
    vec![
        pure("1.5e-7", 1.5e-7),
        pure(
            "[1, 'a' => \"x\\0y\"]",
            common::assoc([
                (common::K::Int(0), common::V::Int(1)),
                ("a".into(), common::V::from(&b"x\0y"[..])),
            ]),
        ),
    ]
}

#[test]
fn oracle() {
    check("smoke", &cases());
}

#[test]
#[ignore = "runs /usr/bin/php to rebuild the fixture"]
fn regenerate_fixture() {
    regenerate("smoke", &cases());
}
