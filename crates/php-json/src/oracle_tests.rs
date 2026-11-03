//! Replays `tests/fixtures/oracle.txt`, recorded from `/usr/bin/php` by
//! `tests/oracle/gen_oracle.php` (hand-written cases plus every
//! `json_encode`/`json_decode`/`json_validate` call made by
//! `ext/json/tests/*.phpt` whose arguments can be rebuilt from `serialize()`).
//!
//! The oracle is PHP 8.3, which does not append error locations to messages;
//! php-src master (which this crate follows) does, so a message may carry an
//! extra `" near location L:C"` suffix.

use crate::functions::{json_decode, json_encode, json_validate, JsonState};
use crate::test_host::Kind;
use crate::test_host::TestHost;
use php_runtime::serialize::serialize;
use php_runtime::{ArrayKey, HashTable, RuntimeError, Value, ZArray, ZStr};

const FIXTURE: &str = include_str!("../tests/fixtures/oracle.txt");

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn text(s: &str) -> String {
    String::from_utf8_lossy(&unhex(s)).into_owned()
}

fn num(s: &str) -> i64 {
    text(s).parse().expect("number")
}

fn msg_matches(ours: &str, php83: &str) -> bool {
    ours == php83 || ours.starts_with(&format!("{php83} near location "))
}

/// GH-22527 (php-src master): an unterminated string is a syntax error, where
/// PHP 8.3 reported a control-character error.
fn gh22527(input: &[u8], ours: i64, php83: i64) -> bool {
    php83 == 3 && ours == 4 && !input.iter().any(|&b| b < 0x20)
}

fn exception_text(e: &RuntimeError) -> (String, Option<i64>) {
    match e {
        RuntimeError::ValueError(m) => (format!("ValueError:{m}"), Some(0)),
        RuntimeError::Error(m) => {
            // TestHost::json_exception: "JsonException(code): msg".
            if let Some(rest) = m.strip_prefix("JsonException(") {
                let (code, msg) = rest.split_once("): ").expect("exception format");
                (format!("JsonException:{msg}"), code.parse().ok())
            } else {
                (format!("Error:{m}"), Some(0))
            }
        }
        other => (format!("{other}"), None),
    }
}

fn check_exception(e: &RuntimeError, expected: &str, code: i64) -> Result<(), String> {
    let (ours, our_code) = exception_text(e);
    let ok = msg_matches(&ours, expected) && our_code == Some(code);
    if ok {
        Ok(())
    } else {
        Err(format!(
            "exception {ours:?} code {our_code:?}, expected {expected:?} code {code}"
        ))
    }
}

#[allow(clippy::many_single_char_names)]
/// A minimal `unserialize()` for the fixture values (`N`, `b`, `i`, `d`,
/// `s`, `a`, `O`; no references). Objects keep their property keys verbatim.
fn mini_unserialize(d: &[u8], pos: &mut usize, host: &mut TestHost) -> Option<Value> {
    fn until(d: &[u8], pos: &mut usize, end: u8) -> Option<String> {
        let start = *pos;
        while *d.get(*pos)? != end {
            *pos += 1;
        }
        let s = String::from_utf8(d[start..*pos].to_vec()).ok()?;
        *pos += 1;
        Some(s)
    }
    fn string(d: &[u8], pos: &mut usize) -> Option<Vec<u8>> {
        let len: usize = until(d, pos, b':')?.parse().ok()?;
        let s = d.get(*pos + 1..*pos + 1 + len)?.to_vec();
        *pos += len + 2;
        Some(s)
    }
    let tag = *d.get(*pos)?;
    *pos += 2;
    Some(match tag {
        b'N' => Value::Null,
        b'b' => Value::Bool(until(d, pos, b';')? == "1"),
        b'i' => Value::Int(until(d, pos, b';')?.parse().ok()?),
        b'd' => Value::Float(match until(d, pos, b';')?.as_str() {
            "NAN" => f64::NAN,
            "INF" => f64::INFINITY,
            "-INF" => f64::NEG_INFINITY,
            other => other.parse().ok()?,
        }),
        b's' => {
            let s = string(d, pos)?;
            *pos += 1;
            Value::Str(ZStr::from(s))
        }
        b'a' | b'O' => {
            let class = if tag == b'O' {
                let c = string(d, pos)?;
                *pos += 1;
                Some(String::from_utf8(c).ok()?)
            } else {
                None
            };
            let n: usize = until(d, pos, b':')?.parse().ok()?;
            *pos += 1;
            let mut t = HashTable::new();
            for _ in 0..n {
                let k = mini_unserialize(d, pos, host)?;
                let v = mini_unserialize(d, pos, host)?;
                match (k, class.is_some()) {
                    (Value::Int(i), false) => {
                        t.insert(ArrayKey::Int(i), v);
                    }
                    (Value::Int(i), true) => {
                        t.insert_str_raw(ZStr::from(i.to_string()), v);
                    }
                    (Value::Str(s), false) => {
                        t.insert(ArrayKey::Str(s), v);
                    }
                    (Value::Str(s), true) => {
                        t.insert_str_raw(s, v);
                    }
                    _ => return None,
                }
            }
            *pos += 1;
            match class {
                Some(c) => host.object_with_table(&c, Kind::Plain, t),
                None => Value::Array(ZArray::from_table(t)),
            }
        }
        _ => return None,
    })
}

fn host_with_precision(prec: i64) -> TestHost {
    let mut host = TestHost::new();
    host.serialize_precision = Some(prec);
    host
}

fn run_encode(f: &[&str]) -> Result<(), String> {
    let prec = num(f[0]);
    let flags = num(f[1]);
    let depth = num(f[2]);
    let mut host = host_with_precision(prec);
    let data = unhex(f[3]);
    let mut pos = 0;
    let value = mini_unserialize(&data, &mut pos, &mut host).ok_or("unserialize failed")?;
    if i32::try_from(depth).is_err() {
        // php-src master rejects depths outside the C int range with a
        // ValueError; PHP 8.3 silently truncated them.
        return Ok(());
    }
    let mut state = JsonState::new();
    let kind = text(f[4]);
    let expected_out = unhex(f[5]);
    let code = num(f[6]);
    let result = json_encode(&mut state, &mut host, &value, flags, depth);
    match (kind.as_str(), &result) {
        ("X", Err(e)) => return check_exception(e, &text(f[5]), code),
        ("S", Ok(Value::Str(s))) if s.as_bytes() == expected_out.as_slice() => {}
        ("F", Ok(Value::Bool(false))) => {}
        _ => {
            return Err(format!(
                "result {:?}, expected {kind} {:?}",
                result.map(|v| match v {
                    Value::Str(s) => s.to_string_lossy(),
                    other => format!("{other:?}"),
                }),
                String::from_utf8_lossy(&expected_out)
            ))
        }
    }
    let msg = text(f[7]);
    if state.last_error.code() == 5
        && code == 7
        && flags & crate::flags::PARTIAL_OUTPUT_ON_ERROR == 0
    {
        // php-src master aborts at an invalid UTF-8 key
        // (json_encode_invalid_key_aborts.phpt); PHP 8.3 went on and
        // reported a later NAN.
        return Ok(());
    }
    if state.last_error.code() != code || state.last_error.msg() != msg {
        return Err(format!(
            "last error {} {:?}, expected {code} {msg:?}",
            state.last_error.code(),
            state.last_error.msg()
        ));
    }
    Ok(())
}

fn run_decode(f: &[&str]) -> Result<(), String> {
    let prec = num(f[0]);
    let flags = num(f[1]);
    let depth = num(f[2]);
    let assoc = match text(f[3]).as_str() {
        "t" => Some(true),
        "f" => Some(false),
        _ => None,
    };
    let input = unhex(f[4]);
    let kind = text(f[5]);
    let code = num(f[7]);
    let mut host = host_with_precision(prec);
    let mut state = JsonState::new();
    let result = json_decode(&mut state, &mut host, &input, assoc, depth, flags);
    match (kind.as_str(), result) {
        ("X", Err(e)) => {
            if gh22527(&input, exception_text(&e).1.unwrap_or(0), code) {
                return Ok(());
            }
            check_exception(&e, &text(f[6]), code)
        }
        ("V", Ok(v)) => {
            let ser = serialize(&v, &mut host).map_err(|e| format!("serialize: {e}"))?;
            let expected = unhex(f[6]);
            if ser != expected {
                return Err(format!(
                    "value {:?}, expected {:?}",
                    String::from_utf8_lossy(&ser),
                    String::from_utf8_lossy(&expected)
                ));
            }
            let msg = text(f[8]);
            if gh22527(&input, state.last_error.code(), code) {
                return Ok(());
            }
            if flags & crate::flags::THROW_ON_ERROR != 0 {
                // json_decode() with JSON_THROW_ON_ERROR leaves the last
                // error of the previous call (not replayed here) untouched.
                return Ok(());
            }
            if state.last_error.code() != code || !msg_matches(&state.last_error.msg(), &msg) {
                return Err(format!(
                    "last error {} {:?}, expected {code} {msg:?}",
                    state.last_error.code(),
                    state.last_error.msg()
                ));
            }
            Ok(())
        }
        (_, r) => Err(format!("result {r:?}, expected kind {kind}")),
    }
}

fn run_validate(f: &[&str]) -> Result<(), String> {
    let flags = num(f[1]);
    let depth = num(f[2]);
    let input = unhex(f[3]);
    let kind = text(f[4]);
    let code = num(f[6]);
    let mut state = JsonState::new();
    let result = json_validate(&mut state, &input, depth, flags);
    match (kind.as_str(), result) {
        ("X", Err(e)) => check_exception(&e, &text(f[5]), code),
        ("1" | "0", Ok(b)) if b == (kind == "1") => {
            let msg = text(f[7]);
            if gh22527(&input, state.last_error.code(), code) {
                return Ok(());
            }
            if state.last_error.code() != code || !msg_matches(&state.last_error.msg(), &msg) {
                return Err(format!(
                    "last error {} {:?}, expected {code} {msg:?}",
                    state.last_error.code(),
                    state.last_error.msg()
                ));
            }
            Ok(())
        }
        (_, r) => Err(format!("result {r:?}, expected {kind}")),
    }
}

#[test]
fn oracle_fixture() {
    // Deeply nested fixture values need more than the default test stack in
    // debug builds (for unserialize/serialize/drop as much as for encoding).
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(run_fixture)
        .expect("spawn")
        .join()
        .expect("oracle thread");
}

fn run_fixture() {
    let mut failures = Vec::new();
    let mut counts = [0usize; 3];
    for (lineno, line) in FIXTURE.lines().enumerate() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let (kind, rest) = (fields[0], &fields[1..]);
        let r = match kind {
            "E" => {
                counts[0] += 1;
                run_encode(rest)
            }
            "D" => {
                counts[1] += 1;
                run_decode(rest)
            }
            "V" => {
                counts[2] += 1;
                run_validate(rest)
            }
            _ => continue,
        };
        if let Err(e) = r {
            let input = text(if kind == "D" { rest[4] } else { rest[3] });
            let short: String = input.chars().take(120).collect();
            failures.push(format!(
                "line {}: {kind} flags={} depth={} input={short:?}: {e}",
                lineno + 1,
                text(rest[1]),
                text(rest[2])
            ));
        }
    }
    assert!(
        counts.iter().all(|&c| c > 100),
        "fixture too small: {counts:?}"
    );
    if !failures.is_empty() {
        for f in failures.iter().take(40) {
            eprintln!("{f}");
        }
        panic!(
            "{} of {} oracle records failed",
            failures.len(),
            counts.iter().sum::<usize>()
        );
    }
}

const LOCATIONS: &str = include_str!("../tests/fixtures/locations.txt");

/// Replays php-src master's error-location tests (see
/// `tests/oracle/gen_locations.php`): each file's `json_validate()` calls run
/// in order on one state, then `json_last_error()`/`json_last_error_msg()`
/// must match the EXPECT section exactly.
#[test]
fn master_error_locations() {
    let mut failures = Vec::new();
    let mut state = JsonState::new();
    let mut current = "";
    let mut n = 0;
    for line in LOCATIONS.lines().filter(|l| l.starts_with("L\t")) {
        let f: Vec<&str> = line.split('\t').collect();
        if f[1] != current {
            current = f[1];
            state = JsonState::new();
        }
        let input = unhex(f[2]);
        let depth: i64 = f[3].parse().expect("depth");
        let flags: i64 = f[4].parse().expect("flags");
        let code: i64 = f[5].parse().expect("code");
        let msg = text(f[6]);
        let _ = json_validate(&mut state, &input, depth, flags);
        n += 1;
        if state.last_error.code() != code || state.last_error.msg() != msg {
            failures.push(format!(
                "{current}: {:?}: got {} {:?}, expected {code} {msg:?}",
                String::from_utf8_lossy(&input),
                state.last_error.code(),
                state.last_error.msg()
            ));
        }
    }
    assert!(n > 100, "fixture too small");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
