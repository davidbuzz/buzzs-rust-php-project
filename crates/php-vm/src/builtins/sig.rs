//! Parsing of stub-style signatures for builtin functions and methods:
//! `"strlen(string $string): int"`.
//!
//! php-src compiles its stubs into static `zend_internal_arg_info` tables;
//! here every builtin's signature (some 4 400 functions and methods) is
//! parsed at start-up, before the request's memory baseline, so the parser
//! works on byte slices of the signature and allocates only what it
//! returns.

use crate::opcode::{ParamInfo, PREFER_REF, UNCHECKED_TYPE};
use crate::types::{mask, TypeDecl, TypeMember};
use php_runtime::{Value, ZArray, ZStr};

/// A parsed signature; [`parse_sig_borrowed`] leaves the name in the
/// signature text (`N = &str`).
pub(crate) struct Sig<N = String> {
    pub name: N,
    pub params: Vec<ParamInfo>,
    pub defaults: Vec<Option<Value>>,
    pub required: u32,
    pub ret: Option<TypeDecl>,
    pub variadic: bool,
    pub by_ref_return: bool,
}

/// `char::is_whitespace` for an ASCII byte.
#[inline]
fn is_ascii_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\x0B' | b'\x0C' | b'\r')
}

/// `str::trim`, with an inlined fast path for ASCII ends.
#[inline]
fn trim(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && is_ascii_ws(bytes[start]) {
        start += 1;
    }
    while end > start && is_ascii_ws(bytes[end - 1]) {
        end -= 1;
    }
    // Only ASCII bytes were skipped, so both ends are char boundaries.
    let trimmed = &s[start..end];
    match (trimmed.as_bytes().first(), trimmed.as_bytes().last()) {
        (Some(&first), Some(&last)) if first >= 0x80 || last >= 0x80 => trimmed.trim(),
        _ => trimmed,
    }
}

/// The byte offset of the first `c` in `s`.
#[inline]
fn find_byte(s: &str, c: u8) -> Option<usize> {
    s.as_bytes().iter().position(|&b| b == c)
}

/// The builtin type names of [`parse_type`] (matched ignoring case).
const BUILTIN_TYPES: [(&str, u32); 16] = [
    ("int", mask::LONG),
    ("float", mask::DOUBLE),
    ("string", mask::STRING),
    ("bool", mask::BOOL),
    ("false", mask::FALSE),
    ("true", mask::TRUE),
    ("null", mask::NULL),
    ("void", mask::VOID),
    ("never", mask::NEVER),
    ("mixed", mask::ANY),
    ("object", mask::OBJECT),
    ("iterable", mask::ITERABLE),
    ("array", mask::ARRAY),
    ("callable", mask::CALLABLE),
    ("static", mask::STATIC),
    ("resource", mask::RESOURCE),
];

/// Parses a type expression (`?int`, `array|string`, `Foo`).
pub(crate) fn parse_type(s: &str) -> TypeDecl {
    let s = trim(s);
    let mut t = TypeDecl::default();
    let (s, nullable) = match s.strip_prefix('?') {
        Some(r) => (r, true),
        None => (s, false),
    };
    for part in s.split('|') {
        let part = trim(part);
        let part = part.trim_start_matches('(').trim_end_matches(')');
        if find_byte(part, b'&').is_some() {
            t.members
                .push(TypeMember::Intersection(t.intersections.len()));
            t.intersections
                .push(part.split('&').map(|p| ZStr::from(trim(p))).collect());
            continue;
        }
        let builtin = BUILTIN_TYPES
            .iter()
            .find(|(name, _)| part.eq_ignore_ascii_case(name));
        let m = if let Some(&(_, m)) = builtin {
            m
        } else {
            t.members.push(TypeMember::Class(t.classes.len()));
            t.classes.push(ZStr::from(part.trim_start_matches('\\')));
            0
        };
        if m == mask::ITERABLE {
            t.members.push(TypeMember::Iterable);
        }
        t.mask |= m;
    }
    t.pure_intersection =
        t.intersections.len() == 1 && t.classes.is_empty() && t.mask == 0 && !nullable;
    if nullable {
        t.mask |= mask::NULL;
    }
    t
}

/// The value of an integer literal already stripped of `-` and `_` and
/// lower-cased.
fn int_literal_body(lower: &str) -> Option<i64> {
    if let Some(h) = lower.strip_prefix("0x") {
        i64::from_str_radix(h, 16).ok()
    } else if let Some(b) = lower.strip_prefix("0b") {
        i64::from_str_radix(b, 2).ok()
    } else if let Some(o) = lower.strip_prefix("0o") {
        i64::from_str_radix(o, 8).ok()
    } else if lower.len() > 1 && lower.starts_with('0') && lower.bytes().all(|c| c.is_ascii_digit())
    {
        i64::from_str_radix(&lower[1..], 8).ok()
    } else if !lower.is_empty() && lower.bytes().all(|c| c.is_ascii_digit()) {
        lower.parse::<i64>().ok()
    } else {
        None
    }
}

/// A PHP integer literal: decimal, `0x`, `0b`, `0o` or leading-`0` octal,
/// optionally negative.
fn parse_int_literal(s: &str) -> Option<i64> {
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s),
    };
    let v = if body.bytes().any(|c| c == b'_' || c.is_ascii_uppercase()) {
        int_literal_body(&body.replace('_', "").to_ascii_lowercase())?
    } else {
        int_literal_body(body)?
    };
    Some(if neg { v.wrapping_neg() } else { v })
}

/// Defaults spelled as names (matched ignoring case).
fn named_default(s: &str) -> Option<Value> {
    const INTS: [(&str, i64); 9] = [
        ("php_int_max", i64::MAX),
        ("php_int_min", i64::MIN),
        ("e_all", 30719),
        ("e_error", 1),
        ("e_user_notice", 1024),
        ("php_round_half_up", 1),
        ("sort_regular", 0),
        ("count_normal", 0),
        ("ent_quotes | ent_substitute | ent_html401", 3 | 8),
    ];
    if s.eq_ignore_ascii_case("null") {
        return Some(Value::Null);
    }
    if s.eq_ignore_ascii_case("true") {
        return Some(Value::Bool(true));
    }
    if s.eq_ignore_ascii_case("false") {
        return Some(Value::Bool(false));
    }
    if s == "[]" {
        return Some(Value::Array(ZArray::new()));
    }
    INTS.iter()
        .find(|(name, _)| s.eq_ignore_ascii_case(name))
        .map(|&(_, v)| Value::Int(v))
}

fn parse_default(s: &str) -> Option<Value> {
    let s = trim(s);
    if let Some(v) = named_default(s) {
        return Some(v);
    }
    if let Some(i) = parse_int_literal(s) {
        return Some(Value::Int(i));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Some(Value::Float(f));
    }
    if (s.starts_with('"') && s.ends_with('"') || s.starts_with('\'') && s.ends_with('\''))
        && s.len() >= 2
    {
        let inner = &s[1..s.len() - 1];
        // Every escape starts with a backslash.
        if find_byte(inner, b'\\').is_none() {
            return Some(Value::Str(ZStr::from(inner)));
        }
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

/// The parameter declarations of a parameter list: split on commas at
/// depth 0 (ignoring commas in brackets and quotes). A blank last piece is
/// dropped.
struct ParamSplit<'a> {
    s: &'a str,
    pos: usize,
    done: bool,
}

impl<'a> ParamSplit<'a> {
    fn new(s: &'a str) -> Self {
        ParamSplit {
            s,
            pos: 0,
            done: false,
        }
    }
}

impl<'a> Iterator for ParamSplit<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.done {
            return None;
        }
        // A piece starts after a comma at depth 0 outside quotes, so the
        // depth and quote state start afresh.
        let s: &'a str = self.s;
        let start = self.pos;
        let mut depth = 0i32;
        let mut quote: Option<u8> = None;
        for (i, &c) in s.as_bytes().iter().enumerate().skip(start) {
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
                continue;
            }
            match c {
                b'"' | b'\'' => quote = Some(c),
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b',' if depth == 0 => {
                    self.pos = i + 1;
                    return Some(&s[start..i]);
                }
                _ => {}
            }
        }
        self.done = true;
        let last = &s[start..];
        (!trim(last).is_empty()).then_some(last)
    }
}

/// The pieces of one parameter declaration
/// (`#[\SensitiveParameter] ?string &...$name = null`).
struct ParamParts<'a> {
    /// The type (empty when untyped).
    ty: &'a str,
    name: &'a str,
    default: Option<&'a str>,
    by_ref: bool,
    variadic: bool,
    sensitive: bool,
    /// The [`NATIVE_PARAM_BITS`](crate::opcode::NATIVE_PARAM_BITS) of the
    /// `@prefer-ref` and `@unchecked` markers.
    native: u32,
}

/// Splits a parameter declaration.
///
/// # Panics
/// Panics without `$` (signatures are compile-time constants).
fn split_param(p: &str) -> ParamParts<'_> {
    let p = trim(p);
    let (decl, default) = match find_byte(p, b'=') {
        Some(eq) => (trim(&p[..eq]), Some(trim(&p[eq + 1..]))),
        None => (p, None),
    };
    // Native markers: `#[\SensitiveParameter]`, `@prefer-ref` and
    // `@unchecked` (an arginfo-only type).
    let mut decl = trim(decl);
    let mut sensitive = false;
    let mut native = 0;
    loop {
        if let Some(r) = decl
            .strip_prefix("#[\\SensitiveParameter]")
            .or_else(|| decl.strip_prefix("#[SensitiveParameter]"))
        {
            sensitive = true;
            decl = r.trim_start();
        } else if let Some(r) = decl.strip_prefix("@prefer-ref") {
            native |= PREFER_REF;
            decl = r.trim_start();
        } else if let Some(r) = decl.strip_prefix("@unchecked") {
            native |= UNCHECKED_TYPE;
            decl = r.trim_start();
        } else {
            break;
        }
    }
    let dollar = find_byte(decl, b'$').expect("param has $");
    let mut before = trim(&decl[..dollar]);
    let name = trim(&decl[dollar + 1..]);
    let mut by_ref = false;
    let mut variadic = false;
    if let Some(b) = before.strip_suffix("...") {
        variadic = true;
        before = trim(b);
    }
    if let Some(b) = before.strip_suffix('&') {
        by_ref = true;
        before = trim(b);
    }
    if let Some(b) = before.strip_suffix("...") {
        variadic = true;
        before = trim(b);
    }
    ParamParts {
        ty: before,
        name,
        default,
        by_ref,
        variadic,
        sensitive,
        native,
    }
}

/// Parses `name(params): ret`.
///
/// # Panics
/// Panics on malformed signatures (they are compile-time constants).
pub(crate) fn parse_sig(sig: &str) -> Sig {
    let s = parse_sig_borrowed(sig);
    Sig {
        name: s.name.to_string(),
        params: s.params,
        defaults: s.defaults,
        required: s.required,
        ret: s.ret,
        variadic: s.variadic,
        by_ref_return: s.by_ref_return,
    }
}

/// [`parse_sig`] with the name borrowed from `sig` (registration makes its
/// own `ZStr` of it).
///
/// # Panics
/// Panics on malformed signatures (they are compile-time constants).
pub(crate) fn parse_sig_borrowed(sig: &str) -> Sig<&str> {
    let open = find_byte(sig, b'(').expect("signature has (");
    let close = sig
        .as_bytes()
        .iter()
        .rposition(|&b| b == b')')
        .expect("signature has )");
    let mut name = trim(&sig[..open]);
    let by_ref_return = name.starts_with('&');
    if by_ref_return {
        name = &name[1..];
    }
    let params_s = &sig[open + 1..close];
    let ret_s = trim(&sig[close + 1..]);
    let ret = ret_s.strip_prefix(':').map(parse_type);
    let mut params = Vec::new();
    let mut defaults = Vec::new();
    let mut required = 0u32;
    let mut variadic = false;
    for (i, p) in ParamSplit::new(params_s).enumerate() {
        let p = split_param(p);
        let ty = if p.ty.is_empty() {
            None
        } else {
            Some(parse_type(p.ty))
        };
        if p.variadic {
            variadic = true;
        }
        if p.default.is_none() && !p.variadic {
            required = u32::try_from(i + 1).unwrap_or(0);
        }
        params.push(ParamInfo {
            name: ZStr::from(p.name),
            by_ref: p.by_ref,
            variadic: p.variadic,
            ty,
            has_default: p.default.is_some(),
            default_text: p.default.map(str::to_string),
            promote: p.native,
            sensitive: p.sensitive,
            default_expr: None,
            // The generated arginfo registers the marker as a real parameter
            // attribute (`zend_add_parameter_attribute(...,
            // ZSTR_KNOWN(ZEND_STR_SENSITIVEPARAMETER), 0)`), so Reflection
            // sees it.
            attributes: if p.sensitive {
                vec![crate::class::AttributeInfo {
                    name: ZStr::from("SensitiveParameter"),
                    args: Vec::new(),
                    line: 0,
                    strict: false,
                }]
            } else {
                Vec::new()
            },
        });
        defaults.push(p.default.and_then(parse_default));
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

#[cfg(test)]
mod int_literal_tests {
    use super::parse_int_literal;

    #[test]
    fn literals() {
        assert_eq!(parse_int_literal("0777"), Some(0o777));
        assert_eq!(parse_int_literal("-1"), Some(-1));
        assert_eq!(parse_int_literal("0x1F"), Some(31));
        assert_eq!(parse_int_literal("0"), Some(0));
        assert_eq!(parse_int_literal("1.5"), None);
    }
}

#[cfg(test)]
mod marker_tests {
    use super::parse_sig;
    use crate::opcode::{PREFER_REF, UNCHECKED_TYPE};
    use crate::types::mask;

    #[test]
    fn unchecked_types_are_kept_for_reflection() {
        let s = parse_sig("f(@unchecked array|object $a = [], @prefer-ref array &$b, int $c = 0)");
        let [a, b, c] = &s.params[..] else {
            panic!("three parameters");
        };
        assert_eq!(
            a.ty.as_ref().map(|t| t.mask),
            Some(mask::ARRAY | mask::OBJECT)
        );
        assert_eq!(a.promote, UNCHECKED_TYPE);
        assert_eq!(b.promote, PREFER_REF);
        assert!(b.by_ref);
        assert_eq!(c.promote, 0);
        assert_eq!(s.required, 2);
    }

    #[test]
    fn sensitive_marker_becomes_a_parameter_attribute() {
        let s = parse_sig("f(#[\\SensitiveParameter] string $n, string $p): string");
        let [n, p] = &s.params[..] else {
            panic!("two parameters");
        };
        assert!(n.sensitive);
        let names: Vec<_> = n
            .attributes
            .iter()
            .map(|a| a.name.to_string_lossy())
            .collect();
        assert_eq!(names, ["SensitiveParameter"]);
        assert!(!p.sensitive);
        assert!(p.attributes.is_empty());
    }
}

/// The allocating parser this module replaced, kept to check that the
/// byte-slice parser gives the same results.
#[cfg(test)]
mod reference {
    use php_runtime::{Value, ZArray, ZStr};

    pub(super) fn split_params(s: &str) -> Vec<String> {
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

    pub(super) fn parse_int_literal(s: &str) -> Option<i64> {
        let (neg, body) = match s.strip_prefix('-') {
            Some(b) => (true, b),
            None => (false, s),
        };
        let body = body.replace('_', "");
        let lower = body.to_ascii_lowercase();
        let v = if let Some(h) = lower.strip_prefix("0x") {
            i64::from_str_radix(h, 16).ok()?
        } else if let Some(b) = lower.strip_prefix("0b") {
            i64::from_str_radix(b, 2).ok()?
        } else if let Some(o) = lower.strip_prefix("0o") {
            i64::from_str_radix(o, 8).ok()?
        } else if lower.len() > 1
            && lower.starts_with('0')
            && lower.bytes().all(|c| c.is_ascii_digit())
        {
            i64::from_str_radix(&lower[1..], 8).ok()?
        } else if !lower.is_empty() && lower.bytes().all(|c| c.is_ascii_digit()) {
            lower.parse::<i64>().ok()?
        } else {
            return None;
        };
        Some(if neg { v.wrapping_neg() } else { v })
    }

    pub(super) fn parse_default(s: &str) -> Option<Value> {
        let s = s.trim();
        match s.to_ascii_lowercase().as_str() {
            "null" => return Some(Value::Null),
            "true" => return Some(Value::Bool(true)),
            "false" => return Some(Value::Bool(false)),
            "[]" => return Some(Value::Array(ZArray::new())),
            "php_int_max" => return Some(Value::Int(i64::MAX)),
            "php_int_min" => return Some(Value::Int(i64::MIN)),
            "e_all" => return Some(Value::Int(30719)),
            "e_error" | "php_round_half_up" => return Some(Value::Int(1)),
            "e_user_notice" => return Some(Value::Int(1024)),
            "sort_regular" | "count_normal" => return Some(Value::Int(0)),
            "ent_quotes | ent_substitute | ent_html401" => return Some(Value::Int(3 | 8)),
            _ => {}
        }
        if let Some(i) = parse_int_literal(s) {
            return Some(Value::Int(i));
        }
        if let Ok(f) = s.parse::<f64>() {
            return Some(Value::Float(f));
        }
        if (s.starts_with('"') && s.ends_with('"') || s.starts_with('\'') && s.ends_with('\''))
            && s.len() >= 2
        {
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
}

#[cfg(test)]
mod equivalence_tests {
    use super::{parse_default, parse_int_literal, parse_sig, parse_type, reference, trim};
    use super::{split_param, ParamSplit};
    use crate::types::mask;

    #[test]
    fn trim_matches_str_trim() {
        for s in [
            "",
            " ",
            "\t\n\x0B\x0C\r",
            "  int  ",
            "\x0Bx\x0B",
            "\u{a0}x\u{a0}",
            " \u{a0} x \u{2003}",
            "\u{85}",
            "é",
            " é ",
            "a b",
        ] {
            assert_eq!(trim(s), s.trim(), "{s:?}");
        }
    }

    const LISTS: &[&str] = &[
        "",
        " ",
        "string $s",
        "string $s,",
        "string $s, ",
        "array $a = [1, 2], int $b = 0",
        "string $sep = \",\", string $q = ',', $x = \"a\\\"\"",
        "(A&B)|C $x, int ...$rest",
        "Iterator|(Traversable&Countable) $it, $y = [[1, [2]], 3]",
        "$a, , $b",
        "array $x = [, $y",
        "string $s = \"\u{e9}, \u{e8}\", int $n",
        "$a) , $b",
    ];

    #[test]
    fn param_split_matches_the_allocating_splitter() {
        for s in LISTS {
            let ours: Vec<&str> = ParamSplit::new(s).collect();
            assert_eq!(ours, reference::split_params(s), "{s:?}");
        }
    }

    #[test]
    fn int_literals_match_the_allocating_parser() {
        for s in [
            "0",
            "7",
            "-1",
            "0777",
            "0x1F",
            "0X1f",
            "0b101",
            "0B1",
            "0o17",
            "0O17",
            "1_000",
            "0x_ff",
            "_1",
            "-_2",
            "08",
            "1.5",
            "",
            "-",
            "abc",
            "SORT_STRING",
            "PHP_INT_MAX",
            "9223372036854775807",
            "9223372036854775808",
            "-9223372036854775808",
            "0xZ",
            "1e3",
        ] {
            assert_eq!(
                parse_int_literal(s),
                reference::parse_int_literal(s),
                "{s:?}"
            );
        }
    }

    #[test]
    fn defaults_match_the_allocating_parser() {
        for s in [
            "null",
            "NULL",
            " Null ",
            "true",
            "FALSE",
            "[]",
            "PHP_INT_MAX",
            "php_int_min",
            "E_ALL",
            "E_ERROR",
            "E_USER_NOTICE",
            "PHP_ROUND_HALF_UP",
            "SORT_REGULAR",
            "COUNT_NORMAL",
            "ENT_QUOTES | ENT_SUBSTITUTE | ENT_HTML401",
            "ENT_QUOTES",
            "0",
            "-1",
            "0x10",
            "1.5",
            "-0.0",
            "1e3",
            "INF",
            "NAN",
            "\"\"",
            "''",
            "\" \"",
            "\"abc\"",
            "'a\\nb'",
            "\"\\\\x0B\\t\\r\\0\\\\\"",
            "\"\u{e9}\"",
            "\"",
            "'",
            "\"a'",
            "PHP_EOL",
            "[1, 2]",
            "",
        ] {
            let ours = parse_default(s).map(|v| format!("{v:?}"));
            let theirs = reference::parse_default(s).map(|v| format!("{v:?}"));
            assert_eq!(ours, theirs, "{s:?}");
        }
    }

    #[test]
    fn parameter_pieces() {
        let p = split_param(" #[\\SensitiveParameter] ?string &...$name = null ");
        assert_eq!(p.ty, "?string");
        assert_eq!(p.name, "name");
        assert_eq!(p.default, Some("null"));
        assert!(p.sensitive && p.by_ref && p.variadic);
        assert_eq!(p.native, 0);
        let p = split_param("int ...&$refs");
        assert_eq!(p.ty, "int");
        assert!(p.by_ref && p.variadic);
        let p = split_param("$x");
        assert_eq!(p.ty, "");
        assert_eq!(p.default, None);
    }

    #[test]
    fn signatures() {
        let s = parse_sig("&reset(array|object &$array): mixed");
        assert_eq!(s.name, "reset");
        assert!(s.by_ref_return);
        assert_eq!(s.required, 1);
        assert!(s.params[0].by_ref);
        assert_eq!(s.ret.as_ref().map(|t| t.mask), Some(mask::ANY));
        let s = parse_sig("sprintf(string $format, mixed ...$values): string");
        assert!(s.variadic);
        assert_eq!(s.required, 1);
        let s = parse_sig(
            "str_pad(string $string, int $length, string $pad_string = \" \", int $pad_type = STR_PAD_RIGHT): string",
        );
        assert_eq!(s.required, 2);
        assert_eq!(s.params[2].default_text.as_deref(), Some("\" \""));
        assert_eq!(
            format!("{:?}", s.defaults[2]),
            format!("{:?}", parse_default("\" \""))
        );
        assert!(s.defaults[3].is_none());
        let s = parse_sig("f(int $a = 1, int $b): void");
        assert_eq!(s.required, 2);
        let s = parse_sig("pi(): float");
        assert!(s.params.is_empty());
        assert_eq!(s.required, 0);
        let s = parse_sig("f()");
        assert!(s.ret.is_none());
    }

    #[test]
    fn builtin_type_names_ignore_case() {
        assert_eq!(parse_type("INT").mask, mask::LONG);
        assert_eq!(parse_type("?String").mask, mask::STRING | mask::NULL);
        assert_eq!(parse_type("Mixed").mask, mask::ANY);
        let t = parse_type("Integer");
        assert_eq!(t.mask, 0);
        assert_eq!(t.classes.len(), 1);
        assert_eq!(t.classes[0].as_bytes(), b"Integer");
        let t = parse_type("iterable|\\Foo");
        assert_eq!(t.mask, mask::ITERABLE);
        assert_eq!(t.classes[0].as_bytes(), b"Foo");
        let t = parse_type("Iterator|(Traversable&Countable)");
        assert_eq!(t.classes.len(), 1);
        assert_eq!(t.intersections.len(), 1);
        assert_eq!(t.intersections[0].len(), 2);
        assert!(!t.pure_intersection);
        assert!(parse_type("A&B").pure_intersection);
    }
}
