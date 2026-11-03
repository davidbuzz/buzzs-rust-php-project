//! Parsing of stub-style signatures for builtin functions and methods:
//! `"strlen(string $string): int"`.

use crate::opcode::ParamInfo;
use crate::types::{mask, TypeDecl};
use php_runtime::{Value, ZArray, ZStr};

/// A parsed signature.
pub(crate) struct Sig {
    pub name: String,
    pub params: Vec<ParamInfo>,
    pub defaults: Vec<Option<Value>>,
    pub required: u32,
    pub ret: Option<TypeDecl>,
    pub variadic: bool,
    pub by_ref_return: bool,
}

/// Parses a type expression (`?int`, `array|string`, `Foo`).
pub(crate) fn parse_type(s: &str) -> TypeDecl {
    let s = s.trim();
    let mut t = TypeDecl::default();
    let (s, nullable) = match s.strip_prefix('?') {
        Some(r) => (r, true),
        None => (s, false),
    };
    for part in s.split('|') {
        let part = part.trim().trim_start_matches('(').trim_end_matches(')');
        if part.contains('&') {
            t.intersections
                .push(part.split('&').map(|p| ZStr::from(p.trim())).collect());
            continue;
        }
        let m = match part.to_ascii_lowercase().as_str() {
            "int" => mask::LONG,
            "float" => mask::DOUBLE,
            "string" => mask::STRING,
            "bool" => mask::BOOL,
            "false" => mask::FALSE,
            "true" => mask::TRUE,
            "null" => mask::NULL,
            "void" => mask::VOID,
            "never" => mask::NEVER,
            "mixed" => mask::ANY,
            "object" => mask::OBJECT,
            "iterable" => mask::ITERABLE,
            "array" => mask::ARRAY,
            "callable" => mask::CALLABLE,
            "static" => mask::STATIC,
            "resource" => mask::RESOURCE,
            _ => {
                t.classes.push(ZStr::from(part.trim_start_matches('\\')));
                0
            }
        };
        t.mask |= m;
    }
    if nullable {
        t.mask |= mask::NULL;
    }
    t
}

fn parse_default(s: &str) -> Option<Value> {
    let s = s.trim();
    match s.to_ascii_lowercase().as_str() {
        "null" => return Some(Value::Null),
        "true" => return Some(Value::Bool(true)),
        "false" => return Some(Value::Bool(false)),
        "[]" => return Some(Value::Array(ZArray::new())),
        "php_int_max" => return Some(Value::Int(i64::MAX)),
        "php_int_min" => return Some(Value::Int(i64::MIN)),
        "e_all" => return Some(Value::Int(30719)),
        "e_error" => return Some(Value::Int(1)),
        "e_user_notice" => return Some(Value::Int(1024)),
        "php_round_half_up" => return Some(Value::Int(1)),
        "sort_regular" => return Some(Value::Int(0)),
        "count_normal" => return Some(Value::Int(0)),
        "ent_quotes | ent_substitute | ent_html401" => return Some(Value::Int(3 | 8)),
        _ => {}
    }
    if let Ok(i) = s.parse::<i64>() {
        return Some(Value::Int(i));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Some(Value::Float(f));
    }
    if (s.starts_with('"') && s.ends_with('"') || s.starts_with('\'') && s.ends_with('\'')) && s.len() >= 2 {
        let inner = &s[1..s.len() - 1];
        let unescaped = inner
            .replace("\\n", "\n")
            .replace("\\t", "\t")
            .replace("\\r", "\r")
            .replace("\\0", "\0")
            .replace("\\x0B", "\x0B")
            .replace("\\\\", "\\");
        return Some(Value::Str(ZStr::from(unescaped)));
    }
    None
}

/// Splits on commas at depth 0 (ignoring commas in brackets/quotes).
fn split_params(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                cur.push(c);
            }
            '(' | '[' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Parses `name(params): ret`.
///
/// # Panics
/// Panics on malformed signatures (they are compile-time constants).
pub(crate) fn parse_sig(sig: &str) -> Sig {
    let open = sig.find('(').expect("signature has (");
    let close = sig.rfind(')').expect("signature has )");
    let mut name = sig[..open].trim().to_string();
    let by_ref_return = name.starts_with('&');
    if by_ref_return {
        name = name[1..].to_string();
    }
    let params_s = &sig[open + 1..close];
    let ret_s = sig[close + 1..].trim();
    let ret = ret_s.strip_prefix(':').map(parse_type);
    let mut params = Vec::new();
    let mut defaults = Vec::new();
    let mut required = 0u32;
    let mut variadic = false;
    for (i, p) in split_params(params_s).iter().enumerate() {
        let p = p.trim();
        let (decl, default) = match p.find('=') {
            Some(eq) => (p[..eq].trim(), Some(p[eq + 1..].trim())),
            None => (p, None),
        };
        let dollar = decl.find('$').expect("param has $");
        let mut before = decl[..dollar].trim();
        let pname = decl[dollar + 1..].trim();
        let mut by_ref = false;
        let mut is_var = false;
        if let Some(b) = before.strip_suffix("...") {
            is_var = true;
            before = b.trim();
        }
        if let Some(b) = before.strip_suffix('&') {
            by_ref = true;
            before = b.trim();
        }
        if let Some(b) = before.strip_suffix("...") {
            is_var = true;
            before = b.trim();
        }
        let ty = if before.is_empty() {
            None
        } else {
            Some(parse_type(before))
        };
        if is_var {
            variadic = true;
        }
        if default.is_none() && !is_var {
            required = u32::try_from(i + 1).unwrap_or(0);
        }
        params.push(ParamInfo {
            name: ZStr::from(pname),
            by_ref,
            variadic: is_var,
            ty,
            has_default: default.is_some(),
            default_text: default.map(str::to_string),
            promote: 0,
            sensitive: false,
        });
        defaults.push(default.and_then(parse_default));
    }
    Sig {
        name,
        params,
        defaults,
        required,
        ret,
        variadic,
        by_ref_return,
    }
}
