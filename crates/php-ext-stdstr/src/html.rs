// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/standard/html.c`: `htmlspecialchars()`, `htmlentities()` and
//! their decoders, `get_html_translation_table()`.

use std::cell::RefCell;
use std::rc::Rc;

use php_runtime::{Value, ZStr};
use php_stdstr::error::Notices;
use php_stdstr::html;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::{assoc, bytes, emit};

thread_local! {
    /// The charset hint of a call without `$encoding`
    /// (`get_default_charset()`) and the INI generation it was worked out
    /// for: php-src reads `PG(internal_encoding)` and `SG(default_charset)`
    /// from globals, where reading both settings here copied each into a
    /// new vector on every call. Process memory, like the INI values it
    /// mirrors (`php_memlimit::persistent`).
    static DEFAULT_HINT: RefCell<(u64, Option<Rc<[u8]>>)> = const { RefCell::new((u64::MAX, None)) };
}

/// The charset hint of an `?string $encoding` argument: the argument if
/// non-empty, else `internal_encoding`, else `default_charset`
/// (`get_default_charset()`), the last two worked out once per INI
/// generation. The caller drops the hint before anything can run user
/// code (a notice's error handler may change the settings).
fn charset_hint(vm: &Vm, arg: Option<&ZStr>) -> Option<Rc<[u8]>> {
    if let Some(h) = html::effective_charset_hint(arg.map(ZStr::as_bytes), None, None) {
        return Some(Rc::from(h));
    }
    let generation = vm.ini.generation();
    DEFAULT_HINT.with(|cell| {
        let mut c = cell.borrow_mut();
        if c.0 != generation {
            let hint = html::effective_charset_hint(
                None,
                vm.ini.get("internal_encoding"),
                vm.ini.get("default_charset"),
            );
            php_memlimit::persistent(|| {
                *c = (generation, hint.map(Rc::from));
            });
        }
        c.1.clone()
    })
}

fn flags(a: &Args, n: usize) -> i64 {
    if a.has(n) {
        a.int(n)
    } else {
        html::DEFAULT_FLAGS
    }
}

fn htmlspecialchars(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let enc = a.opt(2).map(|_| a.str(2));
    let cs = charset_hint(vm, enc.as_ref());
    let double = !a.has(3) || a.bool(3);
    let input = a.str(0);
    let mut n = Notices::new();
    let r = html::htmlspecialchars(input.as_bytes(), flags(a, 1), cs.as_deref(), double, &mut n);
    drop(cs);
    emit(vm, n)?;
    // php_escape_html_entities_ex() returns the input itself when nothing
    // needed encoding.
    if r == input.as_bytes() {
        return Ok(Value::Str(input));
    }
    Ok(bytes(r))
}

fn htmlentities(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let enc = a.opt(2).map(|_| a.str(2));
    let cs = charset_hint(vm, enc.as_ref());
    let double = !a.has(3) || a.bool(3);
    let mut n = Notices::new();
    let r = html::htmlentities(
        a.str(0).as_bytes(),
        flags(a, 1),
        cs.as_deref(),
        double,
        &mut n,
    );
    drop(cs);
    emit(vm, n)?;
    Ok(bytes(r))
}

fn htmlspecialchars_decode(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(bytes(html::htmlspecialchars_decode(
        a.str(0).as_bytes(),
        flags(a, 1),
    )))
}

fn html_entity_decode(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let enc = a.opt(2).map(|_| a.str(2));
    let cs = charset_hint(vm, enc.as_ref());
    let mut n = Notices::new();
    let r = html::html_entity_decode(a.str(0).as_bytes(), flags(a, 1), cs.as_deref(), &mut n);
    drop(cs);
    emit(vm, n)?;
    Ok(bytes(r))
}

fn get_html_translation_table(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let table = if a.has(0) {
        a.int(0)
    } else {
        html::HTML_SPECIALCHARS
    };
    let enc = if a.has(2) {
        a.str(2)
    } else {
        ZStr::from("UTF-8")
    };
    let cs = charset_hint(vm, Some(&enc));
    let mut n = Notices::new();
    let r = html::get_html_translation_table(table, flags(a, 1), cs.as_deref(), &mut n);
    drop(cs);
    emit(vm, n)?;
    Ok(assoc(r.into_iter().map(|(k, v)| (ZStr::from(k), bytes(v)))))
}

/// Registers the `html.c` functions and constants.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function_static("htmlspecialchars(string $string, int $flags = ENT_QUOTES | ENT_SUBSTITUTE | ENT_HTML401, ?string $encoding = null, bool $double_encode = true): string", htmlspecialchars)
        .function_static("htmlspecialchars_decode(string $string, int $flags = ENT_QUOTES | ENT_SUBSTITUTE | ENT_HTML401): string", htmlspecialchars_decode)
        .function_static("html_entity_decode(string $string, int $flags = ENT_QUOTES | ENT_SUBSTITUTE | ENT_HTML401, ?string $encoding = null): string", html_entity_decode)
        .function_static("htmlentities(string $string, int $flags = ENT_QUOTES | ENT_SUBSTITUTE | ENT_HTML401, ?string $encoding = null, bool $double_encode = true): string", htmlentities)
        .function_static("get_html_translation_table(int $table = HTML_SPECIALCHARS, int $flags = ENT_QUOTES | ENT_SUBSTITUTE | ENT_HTML401, string $encoding = \"UTF-8\"): array", get_html_translation_table);
    for (name, v) in [
        ("HTML_SPECIALCHARS", html::HTML_SPECIALCHARS),
        ("HTML_ENTITIES", html::HTML_ENTITIES),
        ("ENT_COMPAT", html::ENT_COMPAT),
        ("ENT_QUOTES", html::ENT_QUOTES),
        ("ENT_NOQUOTES", html::ENT_NOQUOTES),
        ("ENT_IGNORE", html::ENT_IGNORE),
        ("ENT_SUBSTITUTE", html::ENT_SUBSTITUTE),
        ("ENT_DISALLOWED", html::ENT_DISALLOWED),
        ("ENT_HTML401", html::ENT_HTML401),
        ("ENT_XML1", html::ENT_XML1),
        ("ENT_XHTML", html::ENT_XHTML),
        ("ENT_HTML5", html::ENT_HTML5),
    ] {
        m.constant(name, Value::Int(v));
    }
}
