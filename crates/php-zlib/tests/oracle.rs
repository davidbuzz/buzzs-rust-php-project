//! Byte-exact comparison against fixtures produced by PHP's C
//! implementation (`gen_fixtures.php`, PHP 8.3 + system zlib).

use php_streams::value::{Key, Value};
use php_zlib::context::{DeflateContext, InflateContext};
use php_zlib::{
    gzcompress, gzdecode, gzdeflate, gzencode, gzinflate, gzuncompress, zlib_decode, zlib_encode,
    Error,
};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn crc32b(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Formats a result the way the generator did.
fn fmt(r: &Result<Vec<u8>, Error>, limit: usize) -> String {
    match r {
        Err(Error::Warning(m)) => format!("W:{m}"),
        Err(e) => format!("EXC:{e}"),
        Ok(v) if v.len() > limit => format!("H:{}:{:08x}", v.len(), crc32b(v)),
        Ok(v) => v.iter().fold(String::new(), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        }),
    }
}

/// A minimal JSON reader for the option sets the generator emits.
fn json(s: &str) -> Value {
    let mut p = s.as_bytes();
    let v = parse(&mut p);
    assert!(p.is_empty(), "trailing json: {s}");
    v
}

fn parse(p: &mut &[u8]) -> Value {
    match p[0] {
        b'n' => {
            *p = &p[4..];
            Value::Null
        }
        b'"' => {
            let end = p[1..].iter().position(|&b| b == b'"').unwrap() + 1;
            let s = p[1..end].to_vec();
            *p = &p[end + 1..];
            Value::Str(s)
        }
        b'[' | b'{' => {
            let close = if p[0] == b'[' { b']' } else { b'}' };
            let obj = p[0] == b'{';
            *p = &p[1..];
            let mut items = Vec::new();
            let mut idx = 0;
            while p[0] != close {
                let key = if obj {
                    let Value::Str(k) = parse(p) else { panic!() };
                    *p = &p[1..]; // ':'
                    Key::Str(k)
                } else {
                    idx += 1;
                    Key::Int(idx - 1)
                };
                items.push((key, parse(p)));
                if p[0] == b',' {
                    *p = &p[1..];
                }
            }
            *p = &p[1..];
            Value::Array(items)
        }
        _ => {
            let end = p
                .iter()
                .position(|b| !(b.is_ascii_digit() || *b == b'-'))
                .unwrap();
            let n = std::str::from_utf8(&p[..end]).unwrap().parse().unwrap();
            *p = &p[end..];
            Value::Int(n)
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn oracle_fixtures() {
    let text = include_str!("fixtures/oracle.txt");
    let mut inputs: Vec<Vec<u8>> = Vec::new();
    let mut last_deflate: Vec<u8> = Vec::new();
    let mut counts = [0usize; 4];
    for (lineno, line) in text.lines().enumerate() {
        let f: Vec<&str> = line.split('\t').collect();
        let ctx = format!("line {}: {}", lineno + 1, &line[..line.len().min(160)]);
        match f[0] {
            "input" => inputs.push(unhex(f[2])),
            "enc" => {
                let data = &inputs[f[4].parse::<usize>().unwrap()];
                let level: i64 = f[2].parse().unwrap();
                let r = match f[1] {
                    "gzcompress" => gzcompress(data, level, 15),
                    "gzdeflate" => gzdeflate(data, level, -15),
                    "gzencode" => gzencode(data, level, 31),
                    "zlib_encode" => zlib_encode(data, f[3].parse().unwrap(), level),
                    other => panic!("{other}"),
                };
                assert_eq!(fmt(&r, 1500), f[5], "{ctx}");
                counts[0] += 1;
            }
            "dec" => {
                let data = unhex(f[2]);
                let got: Vec<String> = [0i64, 1, 5, 100]
                    .iter()
                    .map(|&max| {
                        let r = match f[1] {
                            "gzinflate" => gzinflate(&data, max),
                            "gzuncompress" => gzuncompress(&data, max),
                            "gzdecode" => gzdecode(&data, max),
                            "zlib_decode" => zlib_decode(&data, max),
                            other => panic!("{other}"),
                        };
                        fmt(&r, 32)
                    })
                    .collect();
                assert_eq!(got.join(","), f[3], "{ctx}");
                counts[1] += 1;
            }
            "defl" => {
                let encoding: i64 = f[1].parse().unwrap();
                let opts = json(f[2]);
                let mut c = DeflateContext::init(encoding, opts.as_array().unwrap()).unwrap();
                let chunks: Vec<usize> = f[3].split(',').map(|x| x.parse().unwrap()).collect();
                let flushes: Vec<i64> = f[4].split(',').map(|x| x.parse().unwrap()).collect();
                last_deflate.clear();
                let mut results = Vec::new();
                for (chunk, flush) in chunks.iter().zip(&flushes) {
                    let r = c.add(&inputs[*chunk], *flush);
                    if let Ok(v) = &r {
                        last_deflate.extend_from_slice(v);
                    }
                    results.push(fmt(&r, 1500));
                }
                assert_eq!(results.join(","), f[5], "{ctx}");
                counts[2] += 1;
            }
            "infl" => {
                let encoding = text
                    .lines()
                    .nth(lineno - 1)
                    .unwrap()
                    .split('\t')
                    .nth(1)
                    .unwrap();
                let encoding: i64 = encoding.parse().unwrap();
                let mut opts = vec![(Key::str("window"), Value::Int(f[1].parse().unwrap()))];
                let dict = json(f[2]);
                if dict != Value::Null {
                    opts.push((Key::str("dictionary"), dict));
                }
                let mut c = InflateContext::init(encoding, &opts).unwrap();
                let fl: i64 = f[3].parse().unwrap();
                let pieces: Vec<&[u8]> = if last_deflate.is_empty() {
                    vec![&[]]
                } else {
                    last_deflate.chunks(61).collect()
                };
                let mut results = Vec::new();
                for (i, p) in pieces.iter().enumerate() {
                    let last = i == pieces.len() - 1;
                    let flush = if last {
                        if fl == 4 {
                            2
                        } else {
                            4
                        }
                    } else {
                        fl
                    };
                    results.push(fmt(&c.add(p, flush), 32));
                }
                assert_eq!(results.join(","), f[4], "{ctx}");
                assert_eq!(c.status().to_string(), f[5], "{ctx}");
                assert_eq!(c.read_len().to_string(), f[6], "{ctx}");
                counts[3] += 1;
            }
            other => panic!("unknown record {other}"),
        }
    }
    assert!(counts.iter().all(|&c| c > 100), "{counts:?}");
}
