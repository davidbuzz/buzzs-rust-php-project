//! `(int)`, `(float)`, `(string)` and `(bool)` casts verified against the PHP
//! oracle (`tests/data/gen_casts.php` → `tests/data/casts_table.txt`).

use php_runtime::float::format_double;
use php_runtime::{CollectingContext, ErrorLevel, Value, ZArray};

fn operands() -> Vec<Value> {
    vec![
        Value::Null,
        Value::Bool(true),
        Value::Bool(false),
        Value::Int(0),
        Value::Int(1),
        Value::Int(-1),
        Value::Int(i64::MAX),
        Value::Int(i64::MIN),
        Value::Float(1.5),
        Value::Float(-1.5),
        Value::Float(-0.0),
        Value::Float(1e20),
        Value::Float(-1e20),
        Value::Float(1.9e19),
        Value::Float(0.1 + 0.2),
        Value::Float(1e15),
        Value::Float(1e14),
        Value::Float(123_456_789_012_345.67),
        Value::Float(1e-5),
        Value::Float(0.0001),
        Value::from(""),
        Value::from("0"),
        Value::from("0.0"),
        Value::from("abc"),
        Value::from("1abc"),
        Value::from(" 1"),
        Value::from("1 "),
        Value::from("1e3"),
        Value::from("0x1A"),
        Value::from("1e20"),
        Value::from("-1e20"),
        Value::from(" 12.5e1xyz"),
        Value::from("9223372036854775808"),
        Value::from("-9223372036854775809"),
        Value::from(".5"),
        Value::from("5."),
        Value::Array(ZArray::new()),
        Value::Array(ZArray::from_values([Value::Int(0)])),
    ]
}

fn rep(v: &Value) -> String {
    match v {
        Value::Int(i) => format!("i:{i}"),
        Value::Float(f) => format!("d:{}", format_double(*f, -1, true)),
        Value::Str(s) => {
            let hex: Vec<String> = s.as_bytes().iter().map(|c| format!("{c:02x}")).collect();
            format!("s:{}", hex.concat())
        }
        Value::Bool(b) => format!("b:{}", i32::from(*b)),
        _ => "?".to_string(),
    }
}

#[test]
fn casts_match_php() {
    let operands = operands();
    let mut failures = Vec::new();
    for line in include_str!("data/casts_table.txt").lines() {
        let mut parts = line.splitn(3, ' ');
        let cast: usize = parts.next().unwrap().parse().unwrap();
        let a: usize = parts.next().unwrap().parse().unwrap();
        let expected = parts.next().unwrap();
        let v = &operands[a];
        let mut ctx = CollectingContext::new();
        let mut got = match cast {
            0 => v.to_long(&mut ctx).map(Value::Int),
            1 => v.to_double(&mut ctx).map(Value::Float),
            2 => v.to_zstr(&mut ctx).map(Value::Str),
            _ => v.to_bool(&mut ctx).map(Value::Bool),
        }
        .map_or_else(|e| format!("E:{e}"), |v| rep(&v));
        for d in &ctx.diagnostics {
            // PHP 8.5+ warning absent from the 8.3 oracle.
            if d.message
                .ends_with("is not representable as an int, cast occurred")
            {
                continue;
            }
            let tag = if d.level == ErrorLevel::Deprecated {
                "D"
            } else {
                "W"
            };
            got = format!("{got}|{tag}:{}", d.message);
        }
        if got != expected {
            failures.push(format!("cast={cast} v={v:?}: want {expected} got {got}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
