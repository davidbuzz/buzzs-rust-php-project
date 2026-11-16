// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/standard/html.c`: `htmlspecialchars()`, `htmlentities()` and
//! their decoders, `get_html_translation_table()`.

use php_runtime::{Value, ZStr};
use php_stdstr::error::Notices;
use php_stdstr::html;
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::{assoc, bytes, emit};

/// The charset hint of an `?string $encoding` argument at index `n`:
/// the argument if non-empty, else `internal_encoding`, else
/// `default_charset` (`get_default_charset()`).
fn charset_hint(vm: &Vm, arg: Option<&ZStr>) -> Option<Vec<u8>> {
    let internal = vm.ini_string("internal_encoding");
    let default = vm.ini_string("default_charset");
    html::effective_charset_hint(
        arg.map(ZStr::as_bytes),
        internal.as_deref(),
        default.as_deref(),
    )
    .map(<[u8]>::to_vec)
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
