// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Replays `tests/fixtures/oracle.txt`, recorded from `/usr/bin/php8.5` by
//! `tests/oracle/gen_oracle.php`: the hand-written matrix of
//! `tests/oracle/custom.php` (every filter over representative inputs,
//! flags and options, plus deterministic fuzzing of the parsers) and every
//! `filter_*` call made by `ext/filter/tests/**/*.phpt`.
//!
//! Arguments and results are `serialize()`d with objects and callables
//! replaced by marker arrays (see `wrappers.php`). The replay host answers
//! `FILTER_VALIDATE_REGEXP` with the recorded `preg_match()` results and
//! `FILTER_CALLBACK` with the recorded callback return values.
//!
//! The oracle is PHP 8.5.12, which matches php-src master (the semantics
//! this crate follows) on every recorded call: results, exceptions and
//! diagnostics are compared as recorded.

use crate::engine::{FilterArgs, FilterInputs};
use crate::host::FilterHost;
use crate::test_host::{TestObject, FAILED_PREFIX};
use php_runtime::serialize::{serialize, unserialize, UnserializeOptions};
use php_runtime::{
    ArrayKey, CastTarget, CollectingContext, Diagnostic, ErrorLevel, Obj, RtResult, RuntimeContext,
    RuntimeError, Value, ZArray, ZStr,
};
use std::collections::VecDeque;

const FIXTURE: &str = include_str!("../tests/fixtures/oracle.txt");

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn unser(bytes: &[u8]) -> Value {
    let mut ctx = CollectingContext::new();
    unserialize(bytes, &UnserializeOptions::default(), &mut ctx)
        .expect("unserialize")
        .expect("valid serialization")
}

fn marker_key(a: &ZArray, key: &str) -> Option<Value> {
    a.get(&ArrayKey::Str(ZStr::from(key))).cloned()
}

/// Rebuilds objects, closures and resources from their marker arrays.
fn decode(v: Value) -> Value {
    match v {
        Value::Array(a) => {
            if let Some(class) = marker_key(&a, "\0obj") {
                let to_string = match marker_key(&a, "\0str") {
                    Some(Value::Str(s)) => Some(s),
                    _ => None,
                };
                let class = class.as_str().cloned().unwrap_or_default();
                return Value::Object(Obj::new(TestObject { class, to_string }));
            }
            if marker_key(&a, "\0callable").is_some() {
                return Value::Array(a);
            }
            if let Some(Value::Str(t)) = marker_key(&a, "\0res") {
                return Value::Resource(php_runtime::Resource::new(t, None));
            }
            let mut out = ZArray::new();
            for (k, v) in a.iter() {
                out.make_mut().insert(k.clone(), decode(v.clone()));
            }
            Value::Array(out)
        }
        other => other,
    }
}

/// The inverse of [`decode`] for comparing results.
fn encode(v: &Value) -> Value {
    match v {
        Value::Object(o) => {
            let t = o.downcast_ref::<TestObject>().expect("test object");
            let mut a = ZArray::new();
            a.make_mut().insert(
                ArrayKey::Str(ZStr::from("\0obj")),
                Value::Str(t.class.clone()),
            );
            a.make_mut().insert(
                ArrayKey::Str(ZStr::from("\0str")),
                t.to_string.clone().map_or(Value::Null, Value::Str),
            );
            Value::Array(a)
        }
        Value::Array(a) => {
            let mut out = ZArray::new();
            for (k, v) in a.iter() {
                out.make_mut().insert(k.clone(), encode(v));
            }
            Value::Array(out)
        }
        Value::Reference(r) => encode(&r.get()),
        other => other.clone(),
    }
}

fn ser(v: &Value) -> Vec<u8> {
    let mut ctx = CollectingContext::new();
    serialize(&encode(v), &mut ctx).expect("serialize")
}

struct RegexRecord {
    pattern: Vec<u8>,
    subject: Vec<u8>,
    result: i64,
    warnings: Vec<String>,
}

#[derive(Default)]
struct ReplayHost {
    diagnostics: Vec<Diagnostic>,
    fname: &'static str,
    regexes: Vec<RegexRecord>,
    callables: Vec<Vec<u8>>,
    callbacks: VecDeque<(Value, Value)>,
    exception: String,
}

/// Marks an exception text that is reported verbatim.
const RAW: &str = "\0raw:";

impl RuntimeContext for ReplayHost {
    fn diagnostic(&mut self, level: ErrorLevel, message: &str) -> RtResult<()> {
        self.diagnostics.push(Diagnostic {
            level,
            message: message.to_string(),
        });
        Ok(())
    }

    fn cast_object(&mut self, obj: &Obj, target: CastTarget) -> RtResult<Option<Value>> {
        let t = obj.downcast_ref::<TestObject>();
        Ok(match target {
            CastTarget::String => t.and_then(|t| t.to_string.clone()).map(Value::Str),
            CastTarget::Bool => Some(Value::Bool(true)),
            _ => None,
        })
    }
}

impl FilterHost for ReplayHost {
    fn regex_match(&mut self, pattern: &ZStr, subject: &[u8]) -> RtResult<Option<bool>> {
        let Some(r) = self
            .regexes
            .iter()
            .find(|r| r.pattern == pattern.as_bytes() && r.subject == subject)
        else {
            panic!("no recorded preg_match for {pattern:?} / {subject:?}");
        };
        let (result, warnings) = (r.result, r.warnings.clone());
        for w in warnings {
            let msg = w.replacen("preg_match()", &format!("{}()", self.fname), 1);
            self.diagnostic(ErrorLevel::Warning, &msg)?;
        }
        Ok(match result {
            -1 => None,
            r => Some(r == 1),
        })
    }

    fn is_callable(&mut self, callable: &Value) -> bool {
        let s = ser(callable);
        self.callables.contains(&s)
    }

    fn call_callback(&mut self, _callable: &Value, arg: Value) -> RtResult<Value> {
        let want = ser(&arg);
        let Some(pos) = self.callbacks.iter().position(|(a, _)| ser(a) == want) else {
            // The callback threw: rethrow what PHP reported.
            return Err(RuntimeError::Error(format!("{RAW}{}", self.exception)));
        };
        let (_, ret) = self.callbacks.remove(pos).expect("position is valid");
        Ok(ret)
    }

    fn filter_failed_exception(&mut self, message: String) -> RuntimeError {
        RuntimeError::Error(format!("{FAILED_PREFIX}{message}"))
    }
}

fn level_label(l: ErrorLevel) -> &'static str {
    l.label()
}

/// Mini zpp for `int` parameters (weak mode). `None`: a zpp `TypeError`,
/// which the VM produces (the record is skipped).
fn zpp_long(v: Option<&Value>, default: i64) -> Option<i64> {
    match v.map(Value::deref) {
        None | Some(Value::Null) => Some(default),
        Some(Value::Int(i)) => Some(i),
        Some(Value::Bool(b)) => Some(i64::from(b)),
        #[allow(clippy::cast_possible_truncation)]
        Some(Value::Float(f)) if f.fract() == 0.0 && f.is_finite() && f.abs() < 9.2e18 => {
            Some(f as i64)
        }
        Some(Value::Str(s)) => s.to_str().and_then(|t| t.trim().parse().ok()),
        _ => None,
    }
}

fn zpp_args(v: Option<&Value>, default: i64) -> Option<FilterArgs> {
    match v.map(Value::deref) {
        Some(Value::Array(a)) => Some(FilterArgs::Array(a)),
        other => zpp_long(other.as_ref(), default).map(FilterArgs::Long),
    }
}

fn zpp_str(v: Option<&Value>) -> Option<Vec<u8>> {
    match v.map(Value::deref) {
        Some(Value::Str(s)) => Some(s.as_bytes().to_vec()),
        Some(Value::Int(i)) => Some(i.to_string().into_bytes()),
        _ => None,
    }
}

fn exception_text(e: &RuntimeError) -> String {
    match e {
        RuntimeError::Error(m) => {
            if let Some(raw) = m.strip_prefix(RAW) {
                raw.to_string()
            } else if let Some(rest) = m.strip_prefix(FAILED_PREFIX) {
                format!("Filter\\FilterFailedException: {rest}")
            } else {
                format!("Error: {m}")
            }
        }
        other => format!(
            "{}: {}",
            other.class_name().unwrap_or("?"),
            other.message().unwrap_or("")
        ),
    }
}

struct Record<'a> {
    line_no: usize,
    func: &'a str,
    args: Vec<Value>,
    result: Vec<u8>,
    diags: Vec<String>,
    exception: String,
    extras: Option<ZArray>,
}

fn parse_record(line_no: usize, line: &str) -> Record<'_> {
    let f: Vec<&str> = line.split('\t').collect();
    let args = match decode(unser(&unhex(f[1]))) {
        Value::Array(a) => a.values().cloned().collect(),
        _ => panic!("args"),
    };
    let diags: Vec<String> = {
        let json = String::from_utf8(unhex(f[3])).expect("utf8 diag");
        parse_json_string_list(&json)
    };
    let extras = if f[5].is_empty() {
        None
    } else {
        match unser(&unhex(f[5])) {
            Value::Array(a) => Some(a),
            _ => None,
        }
    };
    Record {
        line_no,
        func: f[0],
        args,
        result: unhex(f[2]),
        diags,
        exception: String::from_utf8_lossy(&unhex(f[4])).into_owned(),
        extras,
    }
}

/// Parses a JSON array of strings (as written by `json_encode`).
fn parse_json_string_list(json: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = json.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            let mut s = Vec::new();
            i += 1;
            while b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                    match b[i] {
                        b'n' => s.push(b'\n'),
                        b't' => s.push(b'\t'),
                        b'r' => s.push(b'\r'),
                        b'/' => s.push(b'/'),
                        b'u' => {
                            let cp = u32::from_str_radix(&json[i + 1..i + 5], 16).expect("hex");
                            i += 4;
                            let mut buf = [0u8; 4];
                            let c = char::from_u32(cp).unwrap_or('\u{fffd}');
                            s.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                        }
                        c => s.push(c),
                    }
                } else {
                    s.push(b[i]);
                }
                i += 1;
            }
            out.push(String::from_utf8_lossy(&s).into_owned());
        }
        i += 1;
    }
    out
}

fn host_for(rec: &Record<'_>) -> ReplayHost {
    let mut host = ReplayHost {
        exception: rec.exception.clone(),
        ..ReplayHost::default()
    };
    let Some(extras) = &rec.extras else {
        return host;
    };
    if let Some(Value::Array(cb)) = marker_key(extras, "cb") {
        for pair in cb.values() {
            if let Value::Array(p) = pair {
                let arg = decode(p.get_int(0).cloned().unwrap_or(Value::Null));
                let ret = decode(p.get_int(1).cloned().unwrap_or(Value::Null));
                host.callbacks.push_back((arg, ret));
            }
        }
    }
    if let Some(Value::Array(c)) = marker_key(extras, "callables") {
        host.callables = c.values().map(|v| ser(&decode(v.clone()))).collect();
    }
    if let Some(Value::Array(re)) = marker_key(extras, "re") {
        for r in re.values() {
            let Value::Array(r) = r else { continue };
            let s = |i| match r.get_int(i) {
                Some(Value::Str(s)) => unhex(&s.to_string_lossy()),
                _ => Vec::new(),
            };
            let warnings = match r.get_int(3) {
                Some(Value::Array(w)) => w
                    .values()
                    .map(|v| {
                        String::from_utf8_lossy(&unhex(
                            &v.as_str().map(ZStr::to_string_lossy).unwrap_or_default(),
                        ))
                        .into_owned()
                    })
                    .collect(),
                _ => Vec::new(),
            };
            host.regexes.push(RegexRecord {
                pattern: s(0),
                subject: s(1),
                result: r.get_int(2).and_then(Value::as_int).unwrap_or(0),
                warnings,
            });
        }
    }
    host
}

fn inputs_for(rec: &Record<'_>) -> FilterInputs {
    let mut inputs = FilterInputs::default();
    let Some(Value::Array(inp)) = rec.extras.as_ref().and_then(|e| marker_key(e, "in")) else {
        return inputs;
    };
    for (k, v) in inp.iter() {
        let Value::Str(hex) = v else { continue };
        let Value::Array(a) = unser(&unhex(&hex.to_string_lossy())) else {
            continue;
        };
        match k.to_bytes().as_slice() {
            b"post" => inputs.post = Some(a),
            b"get" => inputs.get = Some(a),
            b"cookie" => inputs.cookie = Some(a),
            b"env" => inputs.env = Some(a),
            b"server" => inputs.server = Some(a),
            _ => {}
        }
    }
    inputs
}

/// Runs one record; `None` when it needs VM-only behaviour (zpp errors).
fn replay(rec: &Record<'_>, host: &mut ReplayHost) -> Option<RtResult<Value>> {
    let a = &rec.args;
    Some(match rec.func {
        "filter_var" => {
            host.fname = "filter_var";
            let filter = zpp_long(a.get(1), crate::constants::FILTER_DEFAULT)?;
            let opts = zpp_args(a.get(2), 0)?;
            let value = a.first()?.clone();
            crate::filter_var(host, &value, filter, &opts)
        }
        "filter_var_array" => {
            host.fname = "filter_var_array";
            let Value::Array(arr) = a.first()?.deref() else {
                return None;
            };
            let opts = zpp_args(a.get(1), crate::constants::FILTER_DEFAULT)?;
            let add_empty = a.get(2).is_none_or(|v| !matches!(v, Value::Bool(false)));
            crate::filter_var_array(host, &arr, &opts, add_empty)
        }
        "filter_input" => {
            host.fname = "filter_input";
            let inputs = inputs_for(rec);
            let t = zpp_long(a.first(), -1)?;
            let name = zpp_str(a.get(1))?;
            let filter = zpp_long(a.get(2), crate::constants::FILTER_DEFAULT)?;
            let opts = zpp_args(a.get(3), 0)?;
            crate::filter_input(host, &inputs, t, &name, filter, &opts)
        }
        "filter_input_array" => {
            host.fname = "filter_input_array";
            let inputs = inputs_for(rec);
            let t = zpp_long(a.first(), -1)?;
            let opts = zpp_args(a.get(1), crate::constants::FILTER_DEFAULT)?;
            let add_empty = a.get(2).is_none_or(|v| !matches!(v, Value::Bool(false)));
            crate::filter_input_array(host, &inputs, t, &opts, add_empty)
        }
        "filter_has_var" => {
            let inputs = inputs_for(rec);
            let t = zpp_long(a.first(), -1)?;
            let name = zpp_str(a.get(1))?;
            crate::filter_has_var(&inputs, t, &name).map(Value::Bool)
        }
        "filter_id" => {
            let name = zpp_str(a.first())?;
            Ok(crate::filter_id(&name).map_or(Value::Bool(false), Value::Int))
        }
        "filter_list" => Ok(Value::Array(ZArray::from_values(
            crate::filter_list().into_iter().map(Value::from),
        ))),
        _ => return None,
    })
}

#[test]
fn oracle() {
    let mut checked = 0;
    let mut skipped = 0;
    let mut failures = Vec::new();
    for (i, line) in FIXTURE.lines().enumerate() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let rec = parse_record(i + 1, line);
        // zpp TypeErrors and argument-count errors are the VM's.
        if rec.exception.contains("must be of type")
            || rec.exception.starts_with("ArgumentCountError")
        {
            skipped += 1;
            continue;
        }
        let mut host = host_for(&rec);
        let Some(out) = replay(&rec, &mut host) else {
            skipped += 1;
            continue;
        };
        let (result, exception) = match &out {
            Ok(v) => (ser(v), String::new()),
            Err(e) => (ser(&Value::Null), exception_text(e)),
        };
        let diags: Vec<String> = host
            .diagnostics
            .iter()
            .map(|d| format!("{}: {}", level_label(d.level), d.message))
            .collect();
        let expected_diags: Vec<String> = rec
            .diags
            .iter()
            .filter(|d| !d.contains("Passing null to parameter"))
            .cloned()
            .collect();
        let ok = result == rec.result && exception == rec.exception && diags == expected_diags;
        if ok {
            checked += 1;
            continue;
        }
        let ours = format!("{} {exception} {diags:?}", String::from_utf8_lossy(&result));
        let theirs = format!(
            "{} {} {:?}",
            String::from_utf8_lossy(&rec.result),
            rec.exception,
            rec.diags
        );
        failures.push(format!(
            "line {}: {}({:?})\n  ours:   {ours}\n  php8.5: {theirs}",
            rec.line_no, rec.func, rec.args
        ));
    }
    let n = failures.len();
    failures.truncate(40);
    assert!(
        n == 0,
        "{n} mismatches ({checked} ok, {skipped} skipped):\n{}",
        failures.join("\n")
    );
    assert!(checked > 20_000, "too few records checked: {checked}");
    eprintln!("filter oracle: {checked} ok, {skipped} skipped");
}

/// Regenerates the fixture with `/usr/bin/php8.5`:
/// `cargo test -p php-filter -- --ignored regenerate_oracle`.
#[test]
#[ignore = "needs /usr/bin/php8.5 and modules/php-src"]
fn regenerate_oracle() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/oracle/gen_oracle.php");
    let status = std::process::Command::new("/usr/bin/php8.5")
        .args(["-d", "memory_limit=1G"])
        .arg(script)
        .status()
        .expect("run php");
    assert!(status.success());
}
