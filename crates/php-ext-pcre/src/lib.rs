// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/pcre` for the phprust VM: binds the `php-pcre` library (PHP's
//! `preg_*` semantics over PCRE2) through the `php_vm::ext` extension API.
//!
//! This crate is the glue of `ext/pcre/php_pcre.c`: argument handling,
//! array/string subjects, `$matches`/`$count` by-reference outputs, user
//! callbacks, the per-request cache and `preg_last_error()` state (see
//! [`state`]), and the `PREG_*` / `PCRE_*` constants. Pattern parsing,
//! matching, replacement and splitting live in `php-pcre`.
//!
//! The `pcre.*` INI directives are registered with on-modify handlers
//! that mark the cached match configuration stale (PHP's handlers update
//! the shared match context instead). Locale-aware character tables follow the C library's current
//! `LC_CTYPE`, which is what `setlocale()` changes.

mod convert;
mod functions;
mod memory;
mod state;

use php_runtime::{ArrayKey, Value, ZStr};
use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_vm::ext::{Args, Extension, InfoItem, ModuleBuilder, VmResult};
use php_vm::func::{BuiltinFn, Func};
use php_vm::Vm;
use std::sync::OnceLock;

static ORIG_PREG_MATCH: OnceLock<BuiltinFn> = OnceLock::new();
static ORIG_PREG_MATCH_ALL: OnceLock<BuiltinFn> = OnceLock::new();
static ORIG_PREG_SPLIT: OnceLock<BuiltinFn> = OnceLock::new();

/// When a pattern has the `/u` modifier (PCRE2_UTF), matched string captures
/// / substrings extracted from the subject can be marked valid UTF-8 with
/// `mark_valid_utf8()`.
#[inline]
pub fn mark_capture_valid_utf8(s: &ZStr, is_utf: bool) {
    if is_utf {
        s.mark_valid_utf8();
    }
}

/// Marks all string captures in a matches array / value as valid UTF-8
/// when the pattern has the `/u` modifier.
pub fn mark_captures_valid_utf8(v: &Value) {
    match v {
        Value::Str(s) => {
            s.mark_valid_utf8();
        }
        Value::Array(arr) => {
            for (key, entry) in arr.iter() {
                if let ArrayKey::Str(k) = key {
                    k.mark_valid_utf8();
                }
                mark_captures_valid_utf8(entry);
            }
        }
        Value::Reference(r) => {
            let inner = r.get();
            mark_captures_valid_utf8(&inner);
        }
        _ => {}
    }
}

/// Checks whether a PCRE regex pattern string contains the `/u` modifier (PCRE2_UTF).
pub fn pattern_has_u(pattern: &[u8]) -> bool {
    let Some(&delim) = pattern.first() else {
        return false;
    };
    if delim.is_ascii_alphanumeric() || delim == b'\\' || delim == b' ' {
        return false;
    }
    let (end_delim, paired) = match delim {
        b'(' => (b')', true),
        b'[' => (b']', true),
        b'{' => (b'}', true),
        b'<' => (b'>', true),
        d => (d, false),
    };
    let mut end = pattern.len();
    while end > 1 && pattern[end - 1] != delim && (!paired || pattern[end - 1] != end_delim) {
        end -= 1;
    }
    if end <= 1 {
        return false;
    }
    pattern[end..].contains(&b'u')
}

fn is_pattern_utf(vm: &mut Vm, a: &Args) -> bool {
    if !a.has(0) {
        return false;
    }
    let pat = a.str(0);
    pattern_has_u(pat.as_bytes())
        || state::compile(vm, pat.as_bytes())
            .map(|opt| opt.is_some_and(|re| re.is_utf()))
            .unwrap_or(false)
}

fn wrapped_preg_match(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(&handler) = ORIG_PREG_MATCH.get() else {
        return Ok(Value::Bool(false));
    };
    let res = handler(vm, a)?;
    // `false` means the pattern failed (or matching errored): recompiling it
    // for the UTF-8 check would repeat its compilation warning.
    if a.has(2) && !matches!(res, Value::Bool(false)) && is_pattern_utf(vm, a) {
        mark_captures_valid_utf8(&a.at(2));
    }
    Ok(res)
}

fn wrapped_preg_match_all(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(&handler) = ORIG_PREG_MATCH_ALL.get() else {
        return Ok(Value::Bool(false));
    };
    let res = handler(vm, a)?;
    // `false` means the pattern failed (or matching errored): recompiling it
    // for the UTF-8 check would repeat its compilation warning.
    if a.has(2) && !matches!(res, Value::Bool(false)) && is_pattern_utf(vm, a) {
        mark_captures_valid_utf8(&a.at(2));
    }
    Ok(res)
}

fn wrapped_preg_split(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let Some(&handler) = ORIG_PREG_SPLIT.get() else {
        return Ok(Value::Bool(false));
    };
    let res = handler(vm, a)?;
    if !matches!(res, Value::Bool(false)) && is_pattern_utf(vm, a) {
        mark_captures_valid_utf8(&res);
    }
    Ok(res)
}

fn wrap_pcre_functions(vm: &mut Vm) {
    let list: [(&str, &OnceLock<BuiltinFn>, BuiltinFn); 3] = [
        ("preg_match", &ORIG_PREG_MATCH, wrapped_preg_match),
        (
            "preg_match_all",
            &ORIG_PREG_MATCH_ALL,
            wrapped_preg_match_all,
        ),
        ("preg_split", &ORIG_PREG_SPLIT, wrapped_preg_split),
    ];
    for (name, slot, wrapper) in list {
        let key = ZStr::from(name);
        if let Some(Func::Builtin(b)) = vm.functions.get(&key).cloned() {
            // The first registration's handler: the original builtin.
            let _ = slot.set(b.handler);
            let mut nb = (*b).clone();
            nb.handler = wrapper;
            vm.functions
                .insert(key, Func::Builtin(std::rc::Rc::new(nb)));
        }
    }
}

/// The `pcre` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pcre;

impl Extension for Pcre {
    fn name(&self) -> &'static str {
        "pcre"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        functions::register(m);
        wrap_pcre_functions(m.vm());
        register_constants(m);
        register_ini(m);
        m.info(module_info);
        m.on_request_startup(|_| state::reset());
        m.on_request_shutdown(|_| state::reset());
    }
}

/// `PHP_INI_BEGIN()` of `php_pcre.c`. (Directives the VM core already
/// registered keep their existing definition.)
fn register_ini(m: &mut ModuleBuilder<'_>) {
    let d = |name: &str, default: &str, handler| {
        IniDirective::new(name, Some(default), Access::ALL, handler, "pcre")
    };
    m.ini(d("pcre.backtrack_limit", "1000000", ModifyHandler::Long))
        .ini(d("pcre.recursion_limit", "100000", ModifyHandler::Long))
        .ini(d("pcre.jit", "1", ModifyHandler::Bool));
    for name in ["pcre.backtrack_limit", "pcre.recursion_limit", "pcre.jit"] {
        m.ini_on_modify(name, on_modify);
    }
}

/// `OnUpdateBacktrackLimit` / `OnUpdateRecursionLimit` / `OnUpdateJit`:
/// the value itself is parsed by the registry; the cached match
/// configuration is refreshed before the next operation.
#[allow(clippy::unnecessary_wraps)] // the `IniModifyFn` signature
fn on_modify(_vm: &mut Vm, _value: &[u8]) -> VmResult<bool> {
    state::invalidate_config();
    Ok(true)
}

/// `PHP_MINFO_FUNCTION(pcre)`.
fn module_info(_vm: &mut Vm) -> Vec<InfoItem> {
    info_items()
}

/// The rows of the `pcre` `phpinfo()` section.
fn info_items() -> Vec<InfoItem> {
    let row = |k: &str, v: String| InfoItem::Row(vec![k.to_string(), v]);
    let mut items = vec![
        InfoItem::TableStart,
        row(
            "PCRE (Perl Compatible Regular Expressions) Support",
            "enabled".into(),
        ),
        row("PCRE Library Version", php_pcre::pcre_version()),
        row("PCRE Unicode Version", php_pcre::unicode_version()),
        row(
            "PCRE JIT Support",
            if php_pcre::jit_support() {
                "enabled"
            } else {
                "disabled"
            }
            .into(),
        ),
    ];
    if let Some(target) = php_pcre::jit_target() {
        items.push(row("PCRE JIT Target", target));
    }
    items.push(InfoItem::TableEnd);
    items.push(InfoItem::IniEntries);
    items
}

fn register_constants(m: &mut ModuleBuilder<'_>) {
    let ints: [(&str, i64); 15] = [
        ("PREG_PATTERN_ORDER", php_pcre::PREG_PATTERN_ORDER),
        ("PREG_SET_ORDER", php_pcre::PREG_SET_ORDER),
        ("PREG_OFFSET_CAPTURE", php_pcre::PREG_OFFSET_CAPTURE),
        ("PREG_UNMATCHED_AS_NULL", php_pcre::PREG_UNMATCHED_AS_NULL),
        ("PREG_SPLIT_NO_EMPTY", php_pcre::PREG_SPLIT_NO_EMPTY),
        (
            "PREG_SPLIT_DELIM_CAPTURE",
            php_pcre::PREG_SPLIT_DELIM_CAPTURE,
        ),
        (
            "PREG_SPLIT_OFFSET_CAPTURE",
            php_pcre::PREG_SPLIT_OFFSET_CAPTURE,
        ),
        ("PREG_GREP_INVERT", php_pcre::PREG_GREP_INVERT),
        ("PREG_NO_ERROR", php_pcre::PREG_NO_ERROR),
        ("PREG_INTERNAL_ERROR", php_pcre::PREG_INTERNAL_ERROR),
        (
            "PREG_BACKTRACK_LIMIT_ERROR",
            php_pcre::PREG_BACKTRACK_LIMIT_ERROR,
        ),
        (
            "PREG_RECURSION_LIMIT_ERROR",
            php_pcre::PREG_RECURSION_LIMIT_ERROR,
        ),
        ("PREG_BAD_UTF8_ERROR", php_pcre::PREG_BAD_UTF8_ERROR),
        (
            "PREG_BAD_UTF8_OFFSET_ERROR",
            php_pcre::PREG_BAD_UTF8_OFFSET_ERROR,
        ),
        (
            "PREG_JIT_STACKLIMIT_ERROR",
            php_pcre::PREG_JIT_STACKLIMIT_ERROR,
        ),
    ];
    for (name, v) in ints {
        m.constant(name, Value::Int(v));
    }
    let (major, minor) = php_pcre::version_numbers();
    m.constant("PCRE_VERSION", Value::from(php_pcre::pcre_version()))
        .constant("PCRE_VERSION_MAJOR", Value::Int(major))
        .constant("PCRE_VERSION_MINOR", Value::Int(minor))
        .constant("PCRE_JIT_SUPPORT", Value::Bool(php_pcre::jit_support()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phpinfo_rows() {
        let items = info_items();
        assert_eq!(items.first(), Some(&InfoItem::TableStart));
        assert_eq!(items.last(), Some(&InfoItem::IniEntries));
        let row = |k: &str| {
            items
                .iter()
                .any(|i| matches!(i, InfoItem::Row(c) if c[0] == k))
        };
        assert!(row("PCRE (Perl Compatible Regular Expressions) Support"));
        assert!(row("PCRE Library Version"));
        assert!(row("PCRE Unicode Version"));
        assert!(row("PCRE JIT Support"));
    }

    #[test]
    fn test_mark_captures_valid_utf8() {
        let s = ZStr::from("test capture");
        assert!(!s.is_valid_utf8());
        mark_capture_valid_utf8(&s, true);
        assert!(s.is_valid_utf8());

        let non_utf = ZStr::from("other");
        mark_capture_valid_utf8(&non_utf, false);
        assert!(!non_utf.is_valid_utf8());

        let pat_u = b"/test/u";
        assert!(pattern_has_u(pat_u));
        let pat_no_u = b"/test/i";
        assert!(!pattern_has_u(pat_no_u));
    }
}
