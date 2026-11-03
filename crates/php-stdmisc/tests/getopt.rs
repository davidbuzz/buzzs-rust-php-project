//! Oracle tests for `getopt()` (fixtures produced by running PHP child
//! processes with real argv) plus the hand-ported phpt expectations.

mod common;

use common::{fixture_lines, show, unhex};
use php_runtime::serialize::{serialize, unserialize, UnserializeOptions};
use php_runtime::{CollectingContext, Value, ZArray};
use php_stdmisc::getopt::getopt;

#[test]
fn getopt_matches_oracle() {
    let mut ctx = CollectingContext::new();
    let mut n = 0;
    for line in fixture_lines("getopt.txt") {
        let (spec, expect) = line.split_once(' ').unwrap();
        let spec = unserialize(&unhex(spec), &UnserializeOptions::default(), &mut ctx)
            .unwrap()
            .unwrap();
        let spec = spec.as_array().unwrap().clone();
        let short = spec.get_int(0).unwrap().as_str().unwrap().clone();
        let long = spec.get_int(1).unwrap().as_array().cloned();
        let argv = spec.get_int(2).unwrap().clone();
        let (res, rest) = getopt(short.as_bytes(), long.as_ref(), Some(&argv), &mut ctx).unwrap();
        let got = Value::Array(ZArray::from_values([res, Value::Int(rest)]));
        let got = serialize(&got, &mut ctx).unwrap();
        assert_eq!(
            show(&got),
            show(&unhex(expect)),
            "getopt spec {}",
            show(&serialize(&Value::Array(spec), &mut ctx).unwrap())
        );
        n += 1;
    }
    assert!(n > 800);
}

#[test]
fn no_argv() {
    let mut ctx = CollectingContext::new();
    let (v, rest) = getopt(b"a", None, None, &mut ctx).unwrap();
    assert!(matches!(v, Value::Bool(false)));
    assert_eq!(rest, 1);
    let (v, _) = getopt(b"a", None, Some(&Value::from("x")), &mut ctx).unwrap();
    assert!(matches!(v, Value::Bool(false)));
}
