//! `mb_ereg*` (`ext/mbstring/php_mbregex.c`) running on PCRE2.
//!
//! PHP implements these functions with Oniguruma in Ruby syntax. phprust
//! routes them through `php-pcre` instead:
//!
//! 1. the pattern and subject are checked against the regex encoding
//!    (`mb_regex_encoding()`), then converted to UTF-8 when that encoding is
//!    not UTF-8, keeping a byte-offset map back to the original string;
//! 2. the Ruby-syntax pattern is rewritten to PCRE2 syntax ([`translate`]):
//!    `\h`/`\H` (hex digits), `\uHHHH`, `{,n}`, inline `(?m)` (= PCRE `s`),
//!    and — as Oniguruma does when a pattern has named groups — plain
//!    `(...)` groups become non-capturing;
//! 3. the Oniguruma options map to modifiers: `i` → `i`, `x` → `x`,
//!    `m` (dot matches newline) → `s`, absence of `s` (`SINGLELINE`) → `m`
//!    (`^`/`$` match at line boundaries); `u` (UTF-8 + Unicode properties)
//!    is always on and duplicate names are allowed (`(?J)`);
//! 4. offsets are mapped back and substrings cut from the original string.
//!
//! Not supported: `l` (find longest) and `n` (find not empty) options, the
//! non-Ruby syntaxes selected by `j u g c z b d` (patterns are always read
//! as Ruby/PCRE syntax), class intersections (`[a&&b]`), and Oniguruma's
//! exact error texts beyond the common ones. `mbstring.regex_retry_limit`
//! maps to PCRE2's match limit (scaled ×2); `mbstring.regex_stack_limit`
//! has no PCRE2 (JIT) equivalent and is ignored.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use php_mbstring::mbregex::{
    parse_options, Syntax, OPTION_EXTEND, OPTION_IGNORECASE, OPTION_MULTILINE, OPTION_SINGLELINE,
};
use php_mbstring::strings::check_str_encoding;
use php_mbstring::Encoding;
use php_runtime::{Value, ZArray, ZStr};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::state::{self, bytes, opt_str};

/// The Oniguruma version reported by `MB_ONIGURUMA_VERSION`.
pub const ONIGURUMA_VERSION: &str = "6.9.9";

/// `#[\Deprecated]` of every regex function (PHP 8.6).
const SINCE: &str = "8.6";
/// The deprecation message.
const WHY: &str = "because the underlying library is no longer maintained";

/// Group names with their group numbers.
type NameList = Vec<(Vec<u8>, Vec<usize>)>;

/// A compiled pattern.
struct Compiled {
    re: php_pcre::Regex,
    /// Group names in definition order with their group numbers.
    names: NameList,
    /// Whether plain groups capture (`onig_noname_group_capture_is_active`).
    noname_capture: bool,
}

impl Compiled {
    fn num_regs(&self) -> usize {
        self.re.capture_count() + 1
    }
}

/// Match registers (`OnigRegion`) in original-string byte offsets.
#[derive(Clone)]
struct Regs(Vec<Option<(usize, usize)>>);

impl Regs {
    fn get(&self, i: usize) -> Option<(usize, usize)> {
        self.0.get(i).copied().flatten()
    }

    /// `onig_name_to_backref_number` with a region: the last matched group
    /// of that name (or the last group).
    fn name_group(&self, groups: &[usize]) -> usize {
        groups
            .iter()
            .rev()
            .find(|&&g| self.get(g).is_some())
            .or_else(|| groups.last())
            .copied()
            .unwrap_or(0)
    }
}

/// The `mb_ereg_search*` state.
#[derive(Default)]
struct Search {
    string: Option<ZStr>,
    pos: usize,
    re: Option<Rc<Compiled>>,
    regs: Option<Regs>,
}

#[derive(Default)]
struct RegexGlobals {
    /// `MBREX(ht_rc)`: keyed by pattern bytes only, like PHP.
    cache: HashMap<Vec<u8>, (u32, u8, &'static str, Rc<Compiled>)>,
    search: Search,
}

thread_local! {
    static GLOBALS: RefCell<RegexGlobals> = RefCell::new(RegexGlobals::default());
}

/// `RSHUTDOWN`.
pub(crate) fn reset() {
    GLOBALS.with(|g| *g.borrow_mut() = RegexGlobals::default());
}

// ---- encoding conversion --------------------------------------------------

/// A subject converted to UTF-8.
struct Subject {
    text: Vec<u8>,
    /// For converted subjects: original offset of every UTF-8 offset that
    /// starts a character (and the end).
    back: Option<Vec<usize>>,
    /// For converted subjects: UTF-8 offset of every original character
    /// boundary.
    fwd: Option<Vec<usize>>,
}

impl Subject {
    fn to_orig(&self, off: usize) -> usize {
        self.back
            .as_ref()
            .map_or(off, |m| m.get(off).copied().unwrap_or(off))
    }

    fn to_utf8(&self, off: usize) -> usize {
        self.fwd.as_ref().map_or(off, |m| {
            m.get(off)
                .copied()
                .unwrap_or_else(|| *m.last().unwrap_or(&0))
        })
    }
}

/// Byte length of the character at `s[i..]` in `enc`.
fn char_len(enc: &'static Encoding, s: &[u8], i: usize) -> usize {
    let rest = s.len() - i;
    let n = if let Some(tab) = enc.mblen_table {
        usize::from(tab[usize::from(s[i])])
    } else if let Some(w) = enc.fixed_width() {
        w
    } else if enc.name.starts_with("UTF-16") {
        let le = enc.name == "UTF-16LE";
        let hi = if le { s.get(i + 1) } else { s.get(i) };
        if hi.is_some_and(|&h| (0xD8..0xDC).contains(&h)) {
            4
        } else {
            2
        }
    } else {
        1
    };
    n.clamp(1, rest)
}

fn push_utf8(out: &mut Vec<u8>, cp: u32) {
    let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

/// Converts `s` from the regex encoding to UTF-8 with offset maps.
fn to_subject(s: &[u8], enc: &'static Encoding) -> Subject {
    if enc.is_utf8() || enc.name == "ASCII" {
        return Subject {
            text: s.to_vec(),
            back: None,
            fwd: None,
        };
    }
    let mut text = Vec::with_capacity(s.len());
    let mut back = Vec::with_capacity(s.len() + 1);
    let mut fwd = vec![0usize; s.len() + 1];
    let mut i = 0;
    while i < s.len() {
        let n = char_len(enc, s, i);
        fwd[i] = text.len();
        let start = text.len();
        for cp in php_mbstring::convert::decode_all(&s[i..i + n], enc) {
            push_utf8(&mut text, cp);
        }
        back.resize(text.len(), i);
        if let Some(b) = back.get_mut(start) {
            *b = i;
        }
        for k in 1..n {
            fwd[i + k] = text.len();
        }
        i += n;
    }
    fwd[s.len()] = text.len();
    back.push(s.len());
    Subject {
        text,
        back: Some(back),
        fwd: Some(fwd),
    }
}

// ---- pattern translation -------------------------------------------------

fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

/// Whether the (UTF-8) pattern defines named groups.
fn has_named_groups(p: &[u8]) -> bool {
    let mut i = 0;
    let mut in_class = 0usize;
    while i < p.len() {
        match p[i] {
            b'\\' => i += 1,
            b'[' => in_class += 1,
            b']' if in_class > 0 => in_class -= 1,
            b'(' if in_class == 0 && p.get(i + 1) == Some(&b'?') => {
                match (p.get(i + 2), p.get(i + 3)) {
                    (Some(b'<'), Some(c)) if *c != b'=' && *c != b'!' => return true,
                    (Some(b'\''), _) | (Some(b'P'), Some(b'<')) => return true,
                    _ => {}
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// The name of a `(?<name>` / `(?'name'` group (`p` starts after `(?`).
fn group_name(p: &[u8]) -> Option<&[u8]> {
    let close = match p.first()? {
        b'<' if !matches!(p.get(1), Some(b'=' | b'!')) => b'>',
        b'\'' => b'\'',
        _ => return None,
    };
    let end = p[1..].iter().position(|&b| b == close)?;
    Some(&p[1..=end])
}

/// Rewrites Oniguruma Ruby syntax into PCRE2 syntax. `Err` carries an
/// Oniguruma-style error message.
#[allow(clippy::too_many_lines)]
fn translate(p: &[u8], names: bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(p.len() + 8);
    let mut i = 0;
    let mut class_depth = 0usize;
    while i < p.len() {
        let c = p[i];
        match c {
            b'\\' => {
                let Some(&n) = p.get(i + 1) else {
                    return Err("end pattern at escape".into());
                };
                match n {
                    b'h' if class_depth == 0 => out.extend_from_slice(b"[0-9a-fA-F]"),
                    b'h' => out.extend_from_slice(b"0-9a-fA-F"),
                    b'H' if class_depth == 0 => out.extend_from_slice(b"[^0-9a-fA-F]"),
                    b'u' if p.len() >= i + 6 && p[i + 2..i + 6].iter().all(|&b| is_hex(b)) => {
                        out.extend_from_slice(b"\\x{");
                        out.extend_from_slice(&p[i + 2..i + 6]);
                        out.push(b'}');
                        i += 6;
                        continue;
                    }
                    1 => out.extend_from_slice(b"\\x01"),
                    _ => {
                        out.push(b'\\');
                        out.push(n);
                    }
                }
                i += 2;
                continue;
            }
            1 => out.extend_from_slice(b"\\x01"),
            b'[' => {
                if class_depth > 0 && p.get(i + 1) == Some(&b':') {
                    // POSIX bracket: copy through ":]".
                    if let Some(end) = p[i + 2..].windows(2).position(|w| w == b":]") {
                        out.extend_from_slice(&p[i..i + 2 + end + 2]);
                        i += 2 + end + 2;
                        continue;
                    }
                }
                class_depth += 1;
                out.push(b'[');
                i += 1;
                // A leading ']' (after an optional '^') is literal.
                if p.get(i) == Some(&b'^') {
                    out.push(b'^');
                    i += 1;
                }
                if p.get(i) == Some(&b']') {
                    out.extend_from_slice(b"\\]");
                    i += 1;
                }
                continue;
            }
            b']' if class_depth > 0 => {
                class_depth -= 1;
                out.push(b']');
            }
            b'(' if class_depth == 0 => {
                if p.get(i + 1) == Some(&b'?') {
                    if let Some(name) = group_name(&p[i + 2..]) {
                        if !name
                            .first()
                            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_' || *b >= 0x80)
                        {
                            return Err(format!(
                                "invalid group name <{}>",
                                String::from_utf8_lossy(name)
                            ));
                        }
                    }
                    // Inline options: Ruby's `m` is PCRE's `s`.
                    let mut j = i + 2;
                    let mut flags = Vec::new();
                    while let Some(&f) = p.get(j) {
                        if f.is_ascii_alphabetic() || f == b'-' {
                            flags.push(if f == b'm' { b's' } else { f });
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    if !flags.is_empty() && matches!(p.get(j), Some(b')' | b':')) {
                        out.extend_from_slice(b"(?");
                        out.extend_from_slice(&flags);
                        i = j;
                        continue;
                    }
                    out.push(b'(');
                } else if names {
                    out.extend_from_slice(b"(?:");
                } else {
                    out.push(b'(');
                }
            }
            b'{' if class_depth == 0 && p.get(i + 1) == Some(&b',') => {
                // `{,n}` = `{0,n}`.
                let digits = p[i + 2..].iter().take_while(|b| b.is_ascii_digit()).count();
                if digits > 0 && p.get(i + 2 + digits) == Some(&b'}') {
                    out.extend_from_slice(b"{0,");
                } else {
                    out.push(b'{');
                    out.push(b',');
                }
                i += 2;
                continue;
            }
            _ => out.push(c),
        }
        i += 1;
    }
    Ok(out)
}

/// Rewrites the GNU/POSIX basic syntaxes (`b`, `g`, `c`), where `\(`,
/// `\)`, `\{`, `\}` and `\|` are operators and the bare characters are
/// literals, into Ruby-style syntax for [`translate`].
fn from_basic(p: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(p.len() + 4);
    let mut i = 0;
    let mut class = false;
    while i < p.len() {
        let c = p[i];
        if class {
            if c == b']' {
                class = false;
            }
            out.push(c);
        } else if c == b'\\' && i + 1 < p.len() {
            let n = p[i + 1];
            if matches!(n, b'(' | b')' | b'{' | b'}' | b'|' | b'+' | b'?') {
                out.push(n);
            } else {
                out.push(b'\\');
                out.push(n);
            }
            i += 2;
            continue;
        } else if matches!(c, b'(' | b')' | b'{' | b'}' | b'|' | b'+' | b'?') {
            out.push(b'\\');
            out.push(c);
        } else {
            if c == b'[' {
                class = true;
            }
            out.push(c);
        }
        i += 1;
    }
    out
}

/// Maps a PCRE2 compile error to Oniguruma's wording where known.
fn onig_message(pcre: &[u8]) -> String {
    let m = String::from_utf8_lossy(pcre);
    let table = [
        (
            "missing closing parenthesis",
            "end pattern with unmatched parenthesis",
        ),
        (
            "unmatched closing parenthesis",
            "unmatched close parenthesis",
        ),
        (
            "quantifier does not follow a repeatable item",
            "target of repeat operator is not specified",
        ),
        (
            "missing terminating ] for character class",
            "premature end of char-class",
        ),
        ("\\ at end of pattern", "end pattern at escape"),
        (
            "reference to non-existent subpattern",
            "invalid backref number/name",
        ),
        (
            "range out of order in character class",
            "empty range in char class",
        ),
        (
            "numbers out of order in {} quantifier",
            "upper bound must be greater than lower bound",
        ),
        (
            "lookbehind assertion is not fixed length",
            "invalid pattern in look-behind",
        ),
        ("unknown property", "invalid character property name"),
        (
            "unrecognized character after (? or (?-",
            "undefined group option",
        ),
        (
            "two named subpatterns have the same name",
            "multiplex defined name",
        ),
    ];
    for (pcre_msg, onig) in table {
        if m.starts_with(pcre_msg) {
            return onig.to_string();
        }
    }
    m.into_owned()
}

/// Oniguruma's `str_end_hash` for its name table.
fn onig_name_hash(name: &[u8]) -> u32 {
    let val = name
        .iter()
        .fold(0u32, |v, &b| v.wrapping_mul(997).wrapping_add(u32::from(b)));
    val.wrapping_add(val >> 5)
}

/// Sorts names in the order `onig_foreach_name` visits them: by bucket of
/// the (11-bin) `st` hash table, most recently inserted first within a bin.
fn onig_name_order(names: &mut [(Vec<u8>, Vec<usize>)]) {
    let bins = if names.len() > 55 { 19 } else { 11 };
    let mut order: Vec<(u32, usize)> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (onig_name_hash(&n.0) % bins, usize::MAX - i))
        .collect();
    order.sort_unstable();
    let sorted: NameList = order
        .iter()
        .map(|&(_, rev)| names[usize::MAX - rev].clone())
        .collect();
    for (slot, n) in names.iter_mut().zip(sorted) {
        *slot = n;
    }
}

/// `php_mbregex_compile_pattern`; warnings are raised here.
fn compile(
    vm: &mut Vm,
    pattern: &[u8],
    options: u32,
    syntax: Syntax,
) -> VmResult<Option<Rc<Compiled>>> {
    let (enc, enc_name) = state::with(|s| (s.regex_encoding(), s.mb_regex_encoding_get()));
    if !check_str_encoding(pattern, enc) {
        vm.php_warning(&format!("Pattern is not valid under {enc_name} encoding"))?;
        return Ok(None);
    }
    let cached = GLOBALS.with(|g| g.borrow().cache.get(pattern).cloned());
    if let Some((o, s, e, c)) = &cached {
        if *o == options && *s == syntax as u8 && *e == enc.name {
            return Ok(Some(c.clone()));
        }
    }
    let mut utf8 = to_subject(pattern, enc).text;
    if matches!(syntax, Syntax::PosixBasic | Syntax::Grep | Syntax::Emacs) {
        utf8 = from_basic(&utf8);
    }
    let names = has_named_groups(&utf8);
    let body = match translate(&utf8, names) {
        Ok(b) => b,
        Err(e) => {
            vm.php_warning(&format!("mbregex compile err: {e}"))?;
            return Ok(None);
        }
    };
    let mut full = vec![1u8];
    full.extend_from_slice(b"(?J)");
    full.extend_from_slice(&body);
    full.push(1);
    full.push(b'u');
    if options & OPTION_IGNORECASE != 0 {
        full.push(b'i');
    }
    if options & OPTION_EXTEND != 0 {
        full.push(b'x');
    }
    if options & OPTION_MULTILINE != 0 {
        full.push(b's');
    }
    if options & OPTION_SINGLELINE == 0 {
        full.push(b'm');
    }
    let re = match php_pcre::Regex::compile(&full) {
        Ok(re) => re,
        Err(php_pcre::CompileError::Compilation { message, .. }) => {
            vm.php_warning(&format!("mbregex compile err: {}", onig_message(&message)))?;
            return Ok(None);
        }
        Err(e) => {
            vm.php_warning_bytes(&[b"mbregex compile err: ".as_slice(), &e.message()].concat())?;
            return Ok(None);
        }
    };
    let mut name_list: NameList = Vec::new();
    for (g, n) in re.group_names().iter().enumerate() {
        if let Some(n) = n {
            if let Some(e) = name_list.iter_mut().find(|(k, _)| k.as_slice() == &n[..]) {
                e.1.push(g);
            } else {
                name_list.push((n.to_vec(), vec![g]));
            }
        }
    }
    onig_name_order(&mut name_list);
    let c = Rc::new(Compiled {
        re,
        names: name_list,
        noname_capture: !names,
    });
    GLOBALS.with(|g| {
        let mut g = g.borrow_mut();
        // Replacing the pattern the search functions use drops their
        // state (bug #72399).
        if let Some((.., old)) = &cached {
            if g.search.re.as_ref().is_some_and(|r| Rc::ptr_eq(r, old)) {
                g.search.re = None;
                g.search.regs = None;
            }
        }
        g.cache.insert(
            pattern.to_vec(),
            (options, syntax as u8, enc.name, c.clone()),
        );
    });
    Ok(Some(c))
}

/// Outcome of a search.
enum Found {
    Match(Regs),
    NoMatch,
    /// A match-time error (Oniguruma error text).
    Error(String),
}

fn match_config(vm: &Vm) -> php_pcre::MatchConfig {
    // PCRE2's match limit counts about twice as many steps as
    // Oniguruma's retry counter on backtracking-heavy patterns; a
    // non-positive or too large limit keeps Oniguruma's default (10M).
    let retry = vm.ini_long("mbstring.regex_retry_limit");
    let retry = if retry > 0 && retry < i64::from(u32::MAX) {
        retry
    } else {
        10_000_000
    };
    let backtrack = retry.saturating_mul(2).min(i64::from(u32::MAX));
    php_pcre::MatchConfig::from_ini(backtrack, 100_000, true)
}

/// `onig_search` from original offset `start` (`anchored`: `onig_match`).
fn search(vm: &Vm, c: &Compiled, subj: &Subject, start: usize, anchored: bool) -> Found {
    let cfg = match_config(vm);
    let flags = php_pcre::PREG_OFFSET_CAPTURE | php_pcre::PREG_UNMATCHED_AS_NULL;
    let off = subj.to_utf8(start);
    let out = php_pcre::match_one(
        &c.re,
        &cfg,
        &subj.text,
        true,
        flags,
        i64::try_from(off).unwrap_or(0),
    );
    match out.result {
        Ok(0) => return Found::NoMatch,
        Ok(_) => {}
        Err(_) => {
            return match out.last_error {
                Some(php_pcre::PregError::BacktrackLimit) => {
                    Found::Error("retry-limit-in-match over".into())
                }
                Some(php_pcre::PregError::RecursionLimit | php_pcre::PregError::JitStackLimit) => {
                    Found::Error("match-stack limit over".into())
                }
                _ => Found::NoMatch,
            };
        }
    }
    let Some(m) = out.matches else {
        return Found::NoMatch;
    };
    let regs = regs_from(c, subj, &m);
    if anchored && regs.get(0).map(|r| r.0) != Some(start) {
        return Found::NoMatch;
    }
    Found::Match(regs)
}

/// Converts one PCRE match set (`[string|null, offset]` pairs) to registers.
#[allow(clippy::many_single_char_names)]
fn regs_from(c: &Compiled, subj: &Subject, m: &php_pcre::MatchArray) -> Regs {
    let n = c.num_regs();
    let mut regs = Vec::with_capacity(n);
    for g in 0..n {
        let pair = m
            .get(&php_pcre::Key::Index(i64::try_from(g).unwrap_or(0)))
            .and_then(php_pcre::Value::as_array);
        let r = pair.and_then(|p| {
            let s = p.get_index(0)?.as_bytes()?;
            let o = usize::try_from(p.get_index(1)?.as_int()?).ok()?;
            Some((subj.to_orig(o), subj.to_orig(o + s.len())))
        });
        regs.push(r);
    }
    Regs(regs)
}

/// All successive matches (`preg_match_all` iteration), for the global
/// operations; `Err` is a match-time error.
fn search_all(vm: &Vm, c: &Compiled, subj: &Subject) -> Result<Vec<Regs>, String> {
    let cfg = match_config(vm);
    let flags =
        php_pcre::PREG_SET_ORDER | php_pcre::PREG_OFFSET_CAPTURE | php_pcre::PREG_UNMATCHED_AS_NULL;
    let out = php_pcre::match_all(&c.re, &cfg, &subj.text, true, flags, 0);
    if out.result.is_err() {
        return match out.last_error {
            Some(php_pcre::PregError::BacktrackLimit) => Err("retry-limit-in-match over".into()),
            Some(php_pcre::PregError::RecursionLimit | php_pcre::PregError::JitStackLimit) => {
                Err("match-stack limit over".into())
            }
            _ => Ok(Vec::new()),
        };
    }
    let Some(m) = out.matches else {
        return Ok(Vec::new());
    };
    Ok(m.values()
        .filter_map(php_pcre::Value::as_array)
        .map(|set| regs_from(c, subj, set))
        .collect())
}

// ---- result arrays ---------------------------------------------------------

/// Numbered + named groups as `mb_ereg` reports them (`false` for empty or
/// unmatched groups when `empty_is_false`).
#[allow(clippy::many_single_char_names)]
fn regs_array(c: &Compiled, regs: &Regs, s: &[u8], empty_is_false: bool) -> ZArray {
    let mut out = ZArray::with_capacity(regs.0.len());
    let t = out.make_mut();
    let ok = |r: Option<(usize, usize)>| {
        r.filter(|&(b, e)| if empty_is_false { b < e } else { b <= e } && e <= s.len())
    };
    for (i, r) in regs.0.iter().enumerate() {
        let v = ok(*r).map_or(Value::Bool(false), |(b, e)| bytes(&s[b..e]));
        t.insert(i64::try_from(i).unwrap_or(0), v);
    }
    named_into(c, regs, s, t);
    out
}

/// `mb_regex_groups_iter`: named groups (non-empty match → string, else
/// `false`).
#[allow(clippy::many_single_char_names)]
fn named_into(c: &Compiled, regs: &Regs, s: &[u8], t: &mut php_runtime::HashTable) {
    for (name, groups) in &c.names {
        let g = regs.name_group(groups);
        let v = regs
            .get(g)
            .filter(|&(b, e)| b < e && e <= s.len())
            .map_or(Value::Bool(false), |(b, e)| bytes(&s[b..e]));
        t.insert(php_runtime::ArrayKey::from_bytes(name), v);
    }
}

fn regex_encoding() -> &'static Encoding {
    state::with(|s| s.regex_encoding())
}

fn defaults() -> (u32, Syntax) {
    state::with(|s| {
        let o = s.mb_regex_set_options(None).unwrap_or_default();
        parse_options(o.as_bytes()).unwrap_or((OPTION_MULTILINE | OPTION_SINGLELINE, Syntax::Ruby))
    })
}

/// Parses an `$options` argument (`_php_mb_regex_init_options`).
fn options_arg(vm: &mut Vm, opt: Option<&ZStr>) -> VmResult<(u32, Syntax)> {
    match opt {
        Some(o) => parse_options(o.as_bytes()).map_err(|e| vm.throw(e.class(), &e.to_string())),
        None => Ok(defaults()),
    }
}

// ---- functions ---------------------------------------------------------------

fn mb_regex_encoding(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match opt_str(a, 0) {
        None => Ok(Value::from(state::with(|s| s.mb_regex_encoding_get()))),
        Some(e) => state::call(vm, a, |s| s.mb_regex_encoding_set(e.as_bytes())).map(Value::Bool),
    }
}

fn mb_regex_set_options(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = opt_str(a, 0);
    state::call(vm, a, |s| {
        s.mb_regex_set_options(o.as_ref().map(ZStr::as_bytes))
    })
    .map(Value::from)
}

fn ereg_common(vm: &mut Vm, a: &mut Args, icase: bool) -> VmResult<Value> {
    let (pattern, string) = (a.str(0), a.str(1));
    if pattern.as_bytes().is_empty() {
        return Err(a.value_error(vm, 1, "must not be empty"));
    }
    let want = a.has(2);
    if want {
        a.set_ref(2, Value::Array(ZArray::new()));
    }
    let enc = regex_encoding();
    if !check_str_encoding(string.as_bytes(), enc) {
        return Ok(Value::Bool(false));
    }
    let (mut opts, syntax) = defaults();
    if icase {
        opts |= OPTION_IGNORECASE;
    }
    let Some(c) = compile(vm, pattern.as_bytes(), opts, syntax)? else {
        return Ok(Value::Bool(false));
    };
    let subj = to_subject(string.as_bytes(), enc);
    match search(vm, &c, &subj, 0, false) {
        Found::Match(regs) => {
            if want {
                a.set_ref(
                    2,
                    Value::Array(regs_array(&c, &regs, string.as_bytes(), true)),
                );
            }
            Ok(Value::Bool(true))
        }
        _ => Ok(Value::Bool(false)),
    }
}

fn mb_ereg(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    ereg_common(vm, a, false)
}

fn mb_eregi(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    ereg_common(vm, a, true)
}

/// `mb_regex_substitute`: expands `\0`-`\9` and `\k<name>` references.
#[allow(clippy::too_many_lines)]
fn substitute(
    out: &mut Vec<u8>,
    subject: &[u8],
    replace: &[u8],
    c: &Compiled,
    regs: &Regs,
    enc: &'static Encoding,
) {
    let mut p = 0;
    let eos = replace.len();
    let clen = |i: usize| -> usize {
        if i < eos {
            char_len(enc, replace, i)
        } else {
            1
        }
    };
    while p < eos {
        let l = clen(p);
        if l != 1 || replace[p] != b'\\' {
            out.extend_from_slice(&replace[p..p + l]);
            p += l;
            continue;
        }
        let sp = p;
        p += 1;
        if p >= eos || clen(p) != 1 {
            out.extend_from_slice(&replace[sp..p]);
            continue;
        }
        let no: Option<usize>;
        match replace[p] {
            b'0' => {
                no = Some(0);
                p += 1;
            }
            d @ b'1'..=b'9' => {
                p += 1;
                if !c.noname_capture {
                    out.extend_from_slice(&replace[sp..p]);
                    continue;
                }
                no = Some(usize::from(d - b'0'));
            }
            b'k' => {
                p += 1;
                if p >= eos || clen(p) != 1 || (replace[p] != b'<' && replace[p] != b'\'') {
                    p = (p + if p < eos { clen(p) } else { 0 }).min(eos);
                    out.extend_from_slice(&replace[sp..p]);
                    continue;
                }
                let delim = if replace[p] == b'<' { b'>' } else { b'\'' };
                let name = p + 1;
                let mut name_end = name;
                let mut maybe_num = true;
                while name_end < eos {
                    let l = clen(name_end);
                    if l != 1 {
                        name_end += l;
                        maybe_num = false;
                        continue;
                    }
                    if replace[name_end] == delim {
                        break;
                    }
                    if maybe_num && !replace[name_end].is_ascii_digit() {
                        maybe_num = false;
                    }
                    name_end += 1;
                }
                if name_end <= name || name_end >= eos {
                    p = (name_end + 1).min(eos);
                    out.extend_from_slice(&replace[sp..p]);
                    continue;
                }
                p = name_end + 1;
                let nm = &replace[name..name_end];
                if maybe_num {
                    if !c.noname_capture {
                        out.extend_from_slice(&replace[sp..p]);
                        continue;
                    }
                    if nm.len() == 1 {
                        no = Some(usize::from(nm[0] - b'0'));
                    } else if nm[0] == b'0' {
                        no = None;
                    } else {
                        no = std::str::from_utf8(nm).ok().and_then(|s| s.parse().ok());
                    }
                } else {
                    no = c
                        .names
                        .iter()
                        .find(|(n, _)| n.as_slice() == nm)
                        .map(|(_, g)| regs.name_group(g));
                }
            }
            _ => {
                out.extend_from_slice(&replace[sp..p]);
                continue;
            }
        }
        let Some(no) = no.filter(|&n| n < regs.0.len()) else {
            out.extend_from_slice(&replace[sp..p]);
            continue;
        };
        if let Some((b, e)) = regs.get(no).filter(|&(b, e)| b < e && e <= subject.len()) {
            out.extend_from_slice(&subject[b..e]);
        }
    }
}

#[allow(clippy::many_single_char_names)]
fn replace_common(vm: &mut Vm, a: &mut Args, base_opts: u32, callable: bool) -> VmResult<Value> {
    let pattern = a.str(0);
    let replacement = if callable { None } else { Some(a.str(1)) };
    let callback = if callable { Some(a.at(1)) } else { None };
    let string = a.str(2);
    let opt = opt_str(a, 3);
    let enc = regex_encoding();
    if !check_str_encoding(string.as_bytes(), enc) {
        return Ok(Value::Null);
    }
    let (opts, syntax) = options_arg(vm, opt.as_ref())?;
    let opts = opts | base_opts;
    let Some(c) = compile(vm, pattern.as_bytes(), opts, syntax)? else {
        return Ok(Value::Bool(false));
    };
    let s = string.as_bytes();
    let subj = to_subject(s, enc);
    let mut out = Vec::with_capacity(s.len());
    let mut pos = 0usize;
    let mut next_check = s.len().max(1 << 16);
    let matches = match search_all(vm, &c, &subj) {
        Ok(m) => m,
        Err(e) => {
            vm.php_warning(&format!(
                "mbregex search failure in php_mbereg_replace_exec(): {e}"
            ))?;
            return Ok(Value::Bool(false));
        }
    };
    let nullable = matches!(
        search(vm, &c, &to_subject(b"", enc), 0, false),
        Found::Match(_)
    );
    // Appends the replacement (or the callback's result) for one match.
    let emit = |vm: &mut Vm, out: &mut Vec<u8>, regs: &Regs| -> VmResult<()> {
        if let Some(rep) = &replacement {
            substitute(out, s, rep.as_bytes(), &c, regs, enc);
        }
        if let Some(cb) = &callback {
            let mut subpats = ZArray::with_capacity(regs.0.len());
            let t = subpats.make_mut();
            for r in &regs.0 {
                let v = r.map_or_else(|| bytes(Vec::new()), |(b, e)| bytes(&s[b..e.max(b)]));
                let _ = t.push(v);
            }
            named_into(&c, regs, s, t);
            let ret = vm.call_value(cb, vec![Value::Array(subpats)])?;
            let r = vm.value_to_string(&ret)?;
            out.extend_from_slice(r.as_bytes());
        }
        Ok(())
    };
    // Replay Oniguruma's search loop over the match list: each search
    // from `pos` finds the first listed match starting at or after it.
    let mut next = matches.into_iter().peekable();
    loop {
        while next
            .peek()
            .is_some_and(|r| r.get(0).is_none_or(|(b, _)| b < pos))
        {
            next.next();
        }
        let Some(regs) = next.next() else {
            break;
        };
        let (beg, end) = regs.get(0).unwrap_or((pos, pos));
        out.extend_from_slice(&s[pos..beg]);
        emit(vm, &mut out, &regs)?;
        if pos < end {
            pos = end;
        } else if pos < s.len() {
            // Oniguruma advances one *byte* after an empty match; a pattern
            // that matches the empty string anywhere then also matches
            // between the bytes of a multibyte character.
            let n = char_len(enc, s, pos);
            for k in 1..n {
                out.push(s[pos + k - 1]);
                if nullable {
                    let mut mid = Regs(vec![None; regs.0.len()]);
                    mid.0[0] = Some((pos + k, pos + k));
                    emit(vm, &mut out, &mid)?;
                }
            }
            out.push(s[pos + n - 1]);
            pos += n;
        } else {
            break;
        }
        if out.len() >= next_check {
            // smart_str growth.
            crate::mem::check(vm, crate::mem::zstr(s.len()), crate::mem::zstr(out.len()))?;
            next_check = out.len() * 2;
        }
    }
    if pos < s.len() {
        out.extend_from_slice(&s[pos..]);
    }
    crate::mem::result(vm, crate::mem::zstr(s.len()), bytes(out))
}

fn mb_ereg_replace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    replace_common(vm, a, 0, false)
}

fn mb_eregi_replace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    replace_common(vm, a, OPTION_IGNORECASE, false)
}

fn mb_ereg_replace_callback(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    replace_common(vm, a, 0, true)
}

#[allow(clippy::many_single_char_names)]
fn mb_split(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (pattern, string) = (a.str(0), a.str(1));
    #[allow(clippy::cast_sign_loss)]
    let mut count = a.int(2) as u64;
    count = count.saturating_sub(1);
    let enc = regex_encoding();
    let s = string.as_bytes();
    if !check_str_encoding(s, enc) {
        return Ok(Value::Bool(false));
    }
    let (opts, syntax) = defaults();
    let Some(c) = compile(vm, pattern.as_bytes(), opts, syntax)? else {
        return Ok(Value::Bool(false));
    };
    let subj = to_subject(s, enc);
    let mut out = ZArray::new();
    let t = out.make_mut();
    let mut pos = 0usize;
    let mut chunk = 0usize;
    let matches = match search_all(vm, &c, &subj) {
        Ok(m) => m,
        Err(e) => {
            vm.php_warning(&format!("mbregex search failure in mbsplit(): {e}"))?;
            return Ok(Value::Bool(false));
        }
    };
    let mut next = matches.into_iter().peekable();
    while count != 0 && pos < s.len() {
        while next
            .peek()
            .is_some_and(|r| r.get(0).is_none_or(|(b, _)| b < pos))
        {
            next.next();
        }
        let Some(regs) = next.next() else {
            break;
        };
        let (beg, end) = regs.get(0).unwrap_or((pos, pos));
        if pos < end {
            if beg < s.len() && beg >= chunk {
                let _ = t.push(bytes(&s[chunk..beg]));
                count = count.wrapping_sub(1);
                if t.len().is_power_of_two() && t.len() >= 1024 {
                    crate::mem::check(vm, crate::mem::zstr(s.len()), crate::mem::packed(t.len()))?;
                }
            } else {
                vm.php_warning("mbregex search failure in mbsplit(): ")?;
                return Ok(Value::Bool(false));
            }
            pos = end;
            chunk = end;
        } else {
            pos += char_len(enc, s, pos);
        }
    }
    if s.len() > chunk {
        let _ = t.push(bytes(&s[chunk..]));
    } else {
        let _ = t.push(bytes(Vec::new()));
    }
    crate::mem::result(vm, crate::mem::zstr(s.len()), Value::Array(out))
}

fn mb_ereg_match(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (pattern, string) = (a.str(0), a.str(1));
    let opt = opt_str(a, 2);
    let (opts, syntax) = options_arg(vm, opt.as_ref())?;
    let enc = regex_encoding();
    if !check_str_encoding(string.as_bytes(), enc) {
        return Ok(Value::Bool(false));
    }
    let Some(c) = compile(vm, pattern.as_bytes(), opts, syntax)? else {
        return Ok(Value::Bool(false));
    };
    let subj = to_subject(string.as_bytes(), enc);
    Ok(Value::Bool(matches!(
        search(vm, &c, &subj, 0, true),
        Found::Match(_)
    )))
}

/// `_php_mb_regex_ereg_search_exec`; `mode`: 0 bool, 1 pos, 2 regs.
fn search_exec(vm: &mut Vm, a: &mut Args, mode: u8) -> VmResult<Value> {
    let pattern = opt_str(a, 0);
    let opt = opt_str(a, 1);
    let (opts, syntax) = options_arg(vm, opt.as_ref())?;
    GLOBALS.with(|g| g.borrow_mut().search.regs = None);
    if let Some(p) = &pattern {
        let Some(c) = compile(vm, p.as_bytes(), opts, syntax)? else {
            return Ok(Value::Bool(false));
        };
        GLOBALS.with(|g| g.borrow_mut().search.re = Some(c));
    }
    let (re, string, pos) = GLOBALS.with(|g| {
        let g = g.borrow();
        (g.search.re.clone(), g.search.string.clone(), g.search.pos)
    });
    let Some(re) = re else {
        return Err(vm.throw("Error", "No pattern was provided"));
    };
    let Some(string) = string else {
        return Err(vm.throw("Error", "No string was provided"));
    };
    let s = string.as_bytes();
    let enc = regex_encoding();
    let subj = to_subject(s, enc);
    let found = if pos > s.len() {
        Found::NoMatch
    } else {
        search(vm, &re, &subj, pos, false)
    };
    match found {
        Found::NoMatch => {
            GLOBALS.with(|g| g.borrow_mut().search.pos = s.len());
            Ok(Value::Bool(false))
        }
        Found::Error(e) => {
            vm.php_warning(&format!("mbregex search failure in mbregex_search(): {e}"))?;
            Ok(Value::Bool(false))
        }
        Found::Match(regs) => {
            let (beg, end) = regs.get(0).unwrap_or((pos, pos));
            let ret = match mode {
                1 => Value::Array(ZArray::from_values([
                    Value::Int(i64::try_from(beg).unwrap_or(0)),
                    Value::Int(i64::try_from(end - beg).unwrap_or(0)),
                ])),
                2 => Value::Array(regs_array(&re, &regs, s, false)),
                _ => Value::Bool(true),
            };
            GLOBALS.with(|g| {
                let mut g = g.borrow_mut();
                g.search.pos = if pos <= end { end } else { pos + 1 };
                g.search.regs = Some(regs);
            });
            Ok(ret)
        }
    }
}

fn mb_ereg_search(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    search_exec(vm, a, 0)
}

fn mb_ereg_search_pos(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    search_exec(vm, a, 1)
}

fn mb_ereg_search_regs(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    search_exec(vm, a, 2)
}

fn mb_ereg_search_init(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let string = a.str(0);
    let pattern = opt_str(a, 1);
    let opt = opt_str(a, 2);
    if pattern.as_ref().is_some_and(|p| p.as_bytes().is_empty()) {
        return Err(a.value_error(vm, 2, "must not be empty"));
    }
    let (opts, syntax) = options_arg(vm, opt.as_ref())?;
    if let Some(p) = &pattern {
        let Some(c) = compile(vm, p.as_bytes(), opts, syntax)? else {
            return Ok(Value::Bool(false));
        };
        GLOBALS.with(|g| g.borrow_mut().search.re = Some(c));
    }
    let valid = check_str_encoding(string.as_bytes(), regex_encoding());
    GLOBALS.with(|g| {
        let mut g = g.borrow_mut();
        g.search.pos = if valid { 0 } else { string.as_bytes().len() };
        g.search.string = Some(string);
        g.search.regs = None;
    });
    Ok(Value::Bool(valid))
}

#[allow(clippy::unnecessary_wraps)] // NativeFn signature
fn mb_ereg_search_getregs(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let (re, regs, string) = GLOBALS.with(|g| {
        let g = g.borrow();
        (
            g.search.re.clone(),
            g.search.regs.clone(),
            g.search.string.clone(),
        )
    });
    match (re, regs, string) {
        (Some(re), Some(regs), Some(s)) => {
            Ok(Value::Array(regs_array(&re, &regs, s.as_bytes(), false)))
        }
        _ => Ok(Value::Bool(false)),
    }
}

#[allow(clippy::unnecessary_wraps)] // NativeFn signature
fn mb_ereg_search_getpos(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let pos = GLOBALS.with(|g| g.borrow().search.pos);
    Ok(Value::Int(i64::try_from(pos).unwrap_or(i64::MAX)))
}

fn mb_ereg_search_setpos(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut position = a.int(0);
    let len = GLOBALS.with(|g| {
        g.borrow()
            .search
            .string
            .as_ref()
            .map(|s| i64::try_from(s.as_bytes().len()).unwrap_or(i64::MAX))
    });
    if position < 0 {
        if let Some(l) = len {
            position += l;
        }
    }
    if position < 0 || len.is_some_and(|l| position > l) {
        return Err(a.value_error(vm, 1, "is out of range"));
    }
    GLOBALS.with(|g| g.borrow_mut().search.pos = usize::try_from(position).unwrap_or(0));
    Ok(Value::Bool(true))
}

/// Matches `subject` against a bare PCRE pattern (no delimiters), as
/// `_php_mb_match_regex` does for `mbstring.http_output_conv_mimetypes`.
pub(crate) fn simple_match(pattern: &[u8], subject: &[u8], caseless: bool) -> bool {
    let mut full = vec![1u8];
    for &b in pattern {
        if b == 1 {
            full.extend_from_slice(b"\\x01");
        } else {
            full.push(b);
        }
    }
    full.push(1);
    if caseless {
        full.push(b'i');
    }
    let Ok(re) = php_pcre::Regex::compile(&full) else {
        return false;
    };
    let cfg = php_pcre::MatchConfig::default();
    matches!(php_pcre::match_one(&re, &cfg, subject, false, 0, 0).result, Ok(n) if n > 0)
}

/// Registers the `mb_ereg*` functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function("mb_regex_encoding(?string $encoding = null): string|bool", mb_regex_encoding)
        .deprecated(SINCE, WHY)
        .function("mb_ereg(string $pattern, string $string, &$matches = null): bool", mb_ereg)
        .deprecated(SINCE, WHY)
        .function("mb_eregi(string $pattern, string $string, &$matches = null): bool", mb_eregi)
        .deprecated(SINCE, WHY)
        .function(
            "mb_ereg_replace(string $pattern, string $replacement, string $string, ?string $options = null): string|false|null",
            mb_ereg_replace,
        )
        .deprecated(SINCE, WHY)
        .function(
            "mb_eregi_replace(string $pattern, string $replacement, string $string, ?string $options = null): string|false|null",
            mb_eregi_replace,
        )
        .deprecated(SINCE, WHY)
        .function(
            "mb_ereg_replace_callback(string $pattern, callable $callback, string $string, ?string $options = null): string|false|null",
            mb_ereg_replace_callback,
        )
        .deprecated(SINCE, WHY)
        .function("mb_split(string $pattern, string $string, int $limit = -1): array|false", mb_split)
        .deprecated(SINCE, WHY)
        .function("mb_ereg_match(string $pattern, string $string, ?string $options = null): bool", mb_ereg_match)
        .deprecated(SINCE, WHY)
        .function("mb_ereg_search(?string $pattern = null, ?string $options = null): bool", mb_ereg_search)
        .deprecated(SINCE, WHY)
        .function("mb_ereg_search_pos(?string $pattern = null, ?string $options = null): array|false", mb_ereg_search_pos)
        .deprecated(SINCE, WHY)
        .function("mb_ereg_search_regs(?string $pattern = null, ?string $options = null): array|false", mb_ereg_search_regs)
        .deprecated(SINCE, WHY)
        .function(
            "mb_ereg_search_init(string $string, ?string $pattern = null, ?string $options = null): bool",
            mb_ereg_search_init,
        )
        .deprecated(SINCE, WHY)
        .function("mb_ereg_search_getregs(): array|false", mb_ereg_search_getregs)
        .deprecated(SINCE, WHY)
        .function("mb_ereg_search_getpos(): int", mb_ereg_search_getpos)
        .deprecated(SINCE, WHY)
        .function("mb_ereg_search_setpos(int $offset): bool", mb_ereg_search_setpos)
        .deprecated(SINCE, WHY)
        .function("mb_regex_set_options(?string $options = null): string", mb_regex_set_options)
        .deprecated(SINCE, WHY);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(p: &str) -> String {
        let p = p.as_bytes();
        String::from_utf8(translate(p, has_named_groups(p)).unwrap()).unwrap()
    }

    #[test]
    fn translates_ruby_syntax() {
        assert_eq!(tr(r"\h+"), "[0-9a-fA-F]+");
        assert_eq!(tr(r"[\h_]"), "[0-9a-fA-F_]");
        assert_eq!(tr(r"\u00e9"), r"\x{00e9}");
        assert_eq!(tr("a{,2}"), "a{0,2}");
        assert_eq!(tr("(?m)a.b"), "(?s)a.b");
        assert_eq!(tr("(?mi:x)"), "(?si:x)");
        assert_eq!(tr("(a)(b)"), "(a)(b)");
        assert_eq!(tr("(a)(?<n>b)"), "(?:a)(?<n>b)");
        assert_eq!(tr("(?<=a)(b)"), "(?<=a)(b)");
        assert_eq!(tr("[(]"), "[(]");
        assert_eq!(tr("[]a]"), r"[\]a]");
        assert_eq!(tr("[[:alpha:]]"), "[[:alpha:]]");
        assert!(translate(br"abc\", false).is_err());
        assert_eq!(
            translate(b"(?<0>a)", true).unwrap_err(),
            "invalid group name <0>"
        );
    }

    #[test]
    fn named_group_detection() {
        assert!(has_named_groups(b"(?<x>a)"));
        assert!(has_named_groups(b"(?'x'a)"));
        assert!(!has_named_groups(b"(?<=a)(?<!b)"));
        assert!(!has_named_groups(br"\(?<x>a\)"));
        assert!(!has_named_groups(b"[(?<x>]"));
    }

    #[test]
    fn posix_basic_syntax() {
        assert_eq!(from_basic(br"a\{1,2\}"), b"a{1,2}");
        assert_eq!(from_basic(br"\(a\)|b+"), br"(a)\|b\+");
        assert_eq!(from_basic(b"[(]"), b"[(]");
    }

    #[test]
    fn subject_offsets_map_back() {
        let enc = php_mbstring::encoding::by_name(b"EUC-JP").unwrap();
        // "a" + HIRAGANA A (EUC-JP A4A2) + "b"
        let s = to_subject(b"a\xA4\xA2b", enc);
        assert_eq!(s.text, "aあb".as_bytes());
        assert_eq!(s.to_orig(1), 1);
        assert_eq!(s.to_orig(4), 3);
        assert_eq!(s.to_orig(5), 4);
        assert_eq!(s.to_utf8(3), 4);
        let utf8 = to_subject("aあ".as_bytes(), php_mbstring::encoding::utf8());
        assert!(utf8.back.is_none());
        assert_eq!(utf8.to_orig(4), 4);
    }

    #[test]
    fn name_order_matches_oniguruma() {
        let mut n: Vec<(Vec<u8>, Vec<usize>)> = ["b", "a", "c", "a2"]
            .iter()
            .map(|s| (s.as_bytes().to_vec(), vec![]))
            .collect();
        onig_name_order(&mut n);
        let got: Vec<&[u8]> = n.iter().map(|x| x.0.as_slice()).collect();
        assert_eq!(got, [&b"a2"[..], b"a", b"b", b"c"]);
    }

    #[test]
    fn onig_messages() {
        assert_eq!(
            onig_message(b"missing closing parenthesis"),
            "end pattern with unmatched parenthesis"
        );
        assert_eq!(onig_message(b"something else"), "something else");
    }

    #[test]
    fn simple_match_is_caseless() {
        assert!(simple_match(
            br"^(text/|application/xhtml\+xml)",
            b"TEXT/html",
            true
        ));
        assert!(!simple_match(b"^text/", b"image/png", true));
    }
}
