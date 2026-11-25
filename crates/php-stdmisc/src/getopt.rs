// Derived from php-src main/getopt.c. Similarity metric to original: 29%
//   Copyright © The PHP Group and Contributors. Author: Marcus Boerger <helly@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! `getopt()` (`ext/standard/basic_functions.c` over `main/getopt.c`'s
//! `php_getopt`), including its quirks: options parsed from the leading
//! alphanumeric run of `$short_options`, `::` optional values (only in the
//! `-xVALUE`/`-x=VALUE` forms), `--name=value`, clustered short options,
//! repeated options collected into arrays and `$rest_index`.

use php_runtime::numeric::{is_numeric_string, Number};
use php_runtime::{ArrayKey, RtResult, RuntimeContext, Value, ZArray, ZStr};

/// `opt_struct` (field names kept from the C code).
#[derive(Clone, Debug)]
#[allow(clippy::struct_field_names)]
struct Opt {
    opt_char: u8,
    need_param: u8,
    opt_name: Option<Vec<u8>>,
}

/// `PHP_GETOPT_INVALID_ARG`.
const INVALID_ARG: i32 = -2;
const EOF: i32 = -1;

/// `parse_opts()`.
fn parse_opts(opts: &[u8]) -> Vec<Opt> {
    let opts = opts.split(|&c| c == 0).next().unwrap_or_default();
    let count = opts.iter().filter(|c| c.is_ascii_alphanumeric()).count();
    let mut out = vec![
        Opt {
            opt_char: 0,
            need_param: 0,
            opt_name: None,
        };
        count
    ];
    let mut i = 0;
    let mut slot = 0;
    let at = |i: usize| opts.get(i).copied().unwrap_or(0);
    while at(i).is_ascii_alphanumeric() {
        let p = &mut out[slot];
        p.opt_char = at(i);
        i += 1;
        p.need_param = u8::from(at(i) == b':');
        if p.need_param == 1 {
            i += 1;
            if at(i) == b':' {
                p.need_param += 1;
                i += 1;
            }
        }
        slot += 1;
    }
    out
}

/// The scanner state of `php_getopt()`.
struct Getopt<'a> {
    argv: &'a [Vec<u8>],
    opts: &'a [Opt],
    optind: usize,
    optchr: usize,
    dash: bool,
    optidx: isize,
}

impl Getopt<'_> {
    fn ch(&self, i: usize, j: usize) -> u8 {
        self.argv[i].get(j).copied().unwrap_or(0)
    }

    /// The option at `idx`, or the terminator (`opt_char == '-'`).
    fn opt(&self, idx: isize) -> Option<&Opt> {
        usize::try_from(idx).ok().and_then(|i| self.opts.get(i))
    }

    /// One `php_getopt()` call: the option char (0 = long option), EOF or
    /// `INVALID_ARG`, and the option argument.
    #[allow(clippy::too_many_lines)]
    fn next(&mut self) -> (i32, Option<Vec<u8>>) {
        let argc = self.argv.len();
        self.optidx = -1;
        if self.optind >= argc {
            return (EOF, None);
        }
        let oi = self.optind;
        if !self.dash {
            if self.ch(oi, 0) != b'-' {
                return (EOF, None);
            }
            if self.ch(oi, 1) == 0 {
                return (EOF, None);
            }
        }
        let mut arg_start: usize;
        if self.ch(oi, 0) == b'-' && self.ch(oi, 1) == b'-' {
            let arg = &self.argv[oi];
            let mut arg_end = arg.len() - 1;
            if self.ch(oi, 2) == 0 {
                self.optind += 1;
                return (EOF, None);
            }
            arg_start = 2;
            let search_len = arg_end.saturating_sub(arg_start);
            if let Some(pos) = arg[arg_start..arg_start + search_len]
                .iter()
                .position(|&c| c == b'=')
            {
                arg_end = pos;
                arg_start += 1;
            } else {
                arg_end -= 1;
            }
            loop {
                self.optidx += 1;
                match self.opt(self.optidx) {
                    None => {
                        self.optind += 1;
                        return (INVALID_ARG, None);
                    }
                    Some(o) => {
                        let a = &arg[2..];
                        if o.opt_name.as_ref().is_some_and(|name| {
                            arg_end == name.len() && a.get(..arg_end) == Some(name.as_slice())
                        }) {
                            break;
                        }
                    }
                }
            }
            self.optchr = 0;
            self.dash = false;
            arg_start += self
                .opt(self.optidx)
                .and_then(|o| o.opt_name.as_ref())
                .map_or(0, Vec::len);
        } else {
            if !self.dash {
                self.dash = true;
                self.optchr = 1;
            }
            if self.ch(oi, self.optchr) == b':' {
                self.dash = false;
                self.optind += 1;
                return (INVALID_ARG, None);
            }
            arg_start = 1 + self.optchr;
        }
        if self.optidx < 0 {
            loop {
                self.optidx += 1;
                match self.opt(self.optidx) {
                    None => {
                        if self.ch(oi, self.optchr + 1) == 0 {
                            self.dash = false;
                            self.optind += 1;
                        } else {
                            self.optchr += 1;
                        }
                        return (INVALID_ARG, None);
                    }
                    Some(o) if self.ch(oi, self.optchr) == o.opt_char => break,
                    Some(_) => {}
                }
            }
        }
        let (opt_char, need_param) = match self.opt(self.optidx) {
            Some(o) => (o.opt_char, o.need_param),
            None => return (INVALID_ARG, None),
        };
        if need_param > 0 {
            self.dash = false;
            let mut optarg = None;
            if self.ch(oi, arg_start) == 0 {
                self.optind += 1;
                if self.optind == argc {
                    if need_param == 1 {
                        return (INVALID_ARG, None);
                    }
                } else if need_param == 1 {
                    optarg = Some(self.argv[self.optind].clone());
                    self.optind += 1;
                }
            } else if self.ch(oi, arg_start) == b'=' {
                optarg = Some(self.argv[oi][arg_start + 1..].to_vec());
                self.optind += 1;
            } else {
                optarg = Some(self.argv[oi][arg_start..].to_vec());
                self.optind += 1;
            }
            return (i32::from(opt_char), optarg);
        }
        if arg_start >= 2 && !(self.ch(oi, 0) == b'-' && self.ch(oi, 1) == b'-') {
            if self.ch(oi, self.optchr + 1) == 0 {
                self.dash = false;
                self.optind += 1;
            } else {
                self.optchr += 1;
            }
        } else {
            self.optind += 1;
        }
        (i32::from(opt_char), None)
    }
}

/// Adds an option value to the result (repeated options become arrays).
fn add(result: &mut ZArray, name: &[u8], val: Value) {
    let numeric = if name.len() > 1 && name[0] == b'0' {
        None
    } else {
        match is_numeric_string(name, false) {
            Some(Number::Int(i)) => Some(i),
            _ => None,
        }
    };
    let key = match numeric {
        Some(i) => ArrayKey::Int(i),
        None => ArrayKey::Str(ZStr::new(name)),
    };
    let t = result.make_mut();
    if let Some(existing) = t.get_mut(&key) {
        if !matches!(existing, Value::Array(_)) {
            let old = std::mem::replace(existing, Value::Null);
            *existing = Value::Array(ZArray::from_values([old]));
        }
        if let Value::Array(a) = existing {
            let _ = a.make_mut().push(val);
        }
    } else {
        t.insert(key, val);
    }
}

/// `getopt($short_options, $long_options, &$rest_index)`.
///
/// `argv` is `$_SERVER['argv']` (or the global `$argv`) as found by the
/// VM; `None` or a non-array yields `false`. Returns the result value and
/// the new `$rest_index` (1 when `argv` is unavailable, as PHP assigns it
/// before looking).
pub fn getopt(
    short_options: &[u8],
    long_options: Option<&ZArray>,
    argv: Option<&Value>,
    ctx: &mut (impl RuntimeContext + ?Sized),
) -> RtResult<(Value, i64)> {
    let Some(Value::Array(arg_list)) = argv.map(Value::deref) else {
        return Ok((Value::Bool(false), 1));
    };
    let mut argv_c: Vec<Vec<u8>> = Vec::with_capacity(arg_list.len());
    for v in arg_list.values() {
        let s = v.to_zstr(ctx)?;
        let s = s.as_bytes();
        argv_c.push(s.split(|&c| c == 0).next().unwrap_or_default().to_vec());
    }
    let mut opts = parse_opts(short_options);
    if let Some(longs) = long_options {
        for v in longs.values() {
            let s = v.to_zstr(ctx)?;
            let mut name = s
                .as_bytes()
                .split(|&c| c == 0)
                .next()
                .unwrap_or_default()
                .to_vec();
            let mut need_param = 0;
            let len = name.len();
            if len > 0 && name[len - 1] == b':' {
                need_param += 1;
                name.truncate(len - 1);
                if len > 1 && name[len - 2] == b':' {
                    need_param += 1;
                    name.truncate(len - 2);
                }
            }
            opts.push(Opt {
                opt_char: 0,
                need_param,
                opt_name: Some(name),
            });
        }
    }
    let mut g = Getopt {
        argv: &argv_c,
        opts: &opts,
        optind: 1,
        optchr: 0,
        dash: false,
        optidx: -1,
    };
    let mut result = ZArray::new();
    loop {
        let (o, optarg) = g.next();
        if o == EOF {
            break;
        }
        if o == INVALID_ARG {
            continue;
        }
        let name: Vec<u8> = if o == 0 {
            g.opt(g.optidx)
                .and_then(|x| x.opt_name.clone())
                .unwrap_or_default()
        } else if o == 1 {
            b"-".to_vec()
        } else {
            vec![u8::try_from(o).unwrap_or(b'?')]
        };
        // The name is a C string.
        let name = name.split(|&c| c == 0).next().unwrap_or_default().to_vec();
        let val = match optarg {
            Some(a) => Value::Str(ZStr::new(a)),
            None => Value::Bool(false),
        };
        add(&mut result, &name, val);
    }
    Ok((
        Value::Array(result),
        i64::try_from(g.optind).unwrap_or(i64::MAX),
    ))
}
