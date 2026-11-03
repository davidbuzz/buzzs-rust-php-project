//! Shared helpers for the .phpt ports: a tiny PHP value model with
//! `print_r` / `var_dump` renderers.

#![allow(dead_code, clippy::cast_possible_wrap, clippy::manual_assert)]

use php_libxml::xml::{StructBuilder, StructEntry, StructValue};

/// Array key.
#[derive(Debug, Clone)]
pub enum Key {
    Int(i64),
    Str(Vec<u8>),
}

/// A PHP value.
#[derive(Debug, Clone)]
pub enum PVal {
    Str(Vec<u8>),
    Int(i64),
    Bool(bool),
    Null,
    Arr(Vec<(Key, PVal)>),
}

pub fn s(b: &[u8]) -> PVal {
    PVal::Str(b.to_vec())
}

pub fn opt(b: Option<&[u8]>) -> PVal {
    b.map_or(PVal::Bool(false), s)
}

pub fn list(items: Vec<PVal>) -> PVal {
    PVal::Arr(
        items
            .into_iter()
            .enumerate()
            .map(|(i, v)| (Key::Int(i as i64), v))
            .collect(),
    )
}

pub fn assoc(items: Vec<(&[u8], PVal)>) -> PVal {
    PVal::Arr(
        items
            .into_iter()
            .map(|(k, v)| (Key::Str(k.to_vec()), v))
            .collect(),
    )
}

fn key_from_bytes(k: &[u8]) -> Key {
    // PHP converts canonical decimal strings to integer keys.
    if let Ok(text) = std::str::from_utf8(k) {
        if let Ok(n) = text.parse::<i64>() {
            if n.to_string() == text {
                return Key::Int(n);
            }
        }
    }
    Key::Str(k.to_vec())
}

pub fn pairs(items: &[(Vec<u8>, Vec<u8>)]) -> PVal {
    PVal::Arr(
        items
            .iter()
            .map(|(k, v)| (key_from_bytes(k), s(v)))
            .collect(),
    )
}

pub fn entry(e: &StructEntry) -> PVal {
    PVal::Arr(
        e.fields
            .iter()
            .map(|(k, v)| {
                let v = match v {
                    StructValue::Str(x) => s(x),
                    StructValue::Int(n) => PVal::Int(*n),
                    StructValue::Attributes(a) => pairs(a),
                };
                (Key::Str(k.as_bytes().to_vec()), v)
            })
            .collect(),
    )
}

pub fn values(b: &StructBuilder) -> PVal {
    list(b.values.iter().map(entry).collect())
}

pub fn index(b: &StructBuilder) -> PVal {
    PVal::Arr(
        b.index
            .as_ref()
            .map(|idx| {
                idx.iter()
                    .map(|(k, v)| {
                        (
                            key_from_bytes(k),
                            list(v.iter().map(|n| PVal::Int(*n)).collect()),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn key_text(k: &Key) -> Vec<u8> {
    match k {
        Key::Int(n) => n.to_string().into_bytes(),
        Key::Str(s) => s.clone(),
    }
}

fn print_r_into(out: &mut Vec<u8>, v: &PVal, level: usize) {
    match v {
        PVal::Str(x) => out.extend_from_slice(x),
        PVal::Int(n) => out.extend_from_slice(n.to_string().as_bytes()),
        PVal::Bool(true) => out.push(b'1'),
        PVal::Bool(false) | PVal::Null => {}
        PVal::Arr(items) => {
            let pad = " ".repeat(level * 8);
            out.extend_from_slice(b"Array\n");
            out.extend_from_slice(pad.as_bytes());
            out.extend_from_slice(b"(\n");
            for (k, item) in items {
                out.extend_from_slice(pad.as_bytes());
                out.extend_from_slice(b"    [");
                out.extend_from_slice(&key_text(k));
                out.extend_from_slice(b"] => ");
                print_r_into(out, item, level + 1);
                out.push(b'\n');
            }
            out.extend_from_slice(pad.as_bytes());
            out.extend_from_slice(b")\n");
        }
    }
}

pub fn print_r(v: &PVal) -> Vec<u8> {
    let mut out = Vec::new();
    print_r_into(&mut out, v, 0);
    out
}

fn var_dump_into(out: &mut Vec<u8>, v: &PVal, indent: usize) {
    let pad = " ".repeat(indent);
    out.extend_from_slice(pad.as_bytes());
    match v {
        PVal::Str(x) => {
            out.extend_from_slice(format!("string({}) \"", x.len()).as_bytes());
            out.extend_from_slice(x);
            out.extend_from_slice(b"\"\n");
        }
        PVal::Int(n) => out.extend_from_slice(format!("int({n})\n").as_bytes()),
        PVal::Bool(b) => out.extend_from_slice(format!("bool({b})\n").as_bytes()),
        PVal::Null => out.extend_from_slice(b"NULL\n"),
        PVal::Arr(items) => {
            out.extend_from_slice(format!("array({}) {{\n", items.len()).as_bytes());
            for (k, item) in items {
                out.extend_from_slice(pad.as_bytes());
                match k {
                    Key::Int(n) => out.extend_from_slice(format!("  [{n}]=>\n").as_bytes()),
                    Key::Str(x) => {
                        out.extend_from_slice(b"  [\"");
                        out.extend_from_slice(x);
                        out.extend_from_slice(b"\"]=>\n");
                    }
                }
                var_dump_into(out, item, indent + 2);
            }
            out.extend_from_slice(pad.as_bytes());
            out.extend_from_slice(b"}\n");
        }
    }
}

pub fn var_dump(v: &PVal) -> Vec<u8> {
    let mut out = Vec::new();
    var_dump_into(&mut out, v, 0);
    out
}

/// Compares byte output, showing both sides lossily on mismatch.
#[track_caller]
pub fn assert_out(actual: &[u8], expected: &[u8]) {
    if actual != expected {
        panic!(
            "output mismatch\n--- actual ---\n{}\n--- expected ---\n{}\n",
            String::from_utf8_lossy(actual),
            String::from_utf8_lossy(expected)
        );
    }
}
