//! `ext/gettext` for the phprust VM: binds the `php-gettext` library
//! through the `php_vm::ext` extension API (ref: `ext/gettext/gettext.c`).
//!
//! The library performs master's argument checks and the libintl calls;
//! this crate only converts PHP values and turns [`GettextError`]s into
//! thrown `ValueError`s / `Error`s.

// Native handlers share one signature (`fn(&mut Vm, &mut Args) ->
// VmResult<Value>`).
#![allow(clippy::unnecessary_wraps)]

use php_gettext::{GettextError, GettextResult};
use php_runtime::{Value, ZStr};
use php_vm::ext::{Args, Extension, ModuleBuilder, VmResult};
use php_vm::Vm;

/// The `gettext` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Gettext;

/// The running function's name (for `ValueError` prefixes).
fn fname(a: &Args) -> String {
    a.func
        .as_ref()
        .map_or_else(String::new, |f| f.name.to_string_lossy())
}

/// Throws the PHP exception for a library error.
fn fail(vm: &mut Vm, a: &Args, e: &GettextError) -> php_vm::ext::Throw {
    match e {
        GettextError::ValueError(m) => vm.throw("ValueError", &format!("{}(): {m}", fname(a))),
        GettextError::Error(m) => vm.throw("Error", m),
    }
}

/// A string result.
fn string(vm: &mut Vm, a: &Args, r: GettextResult<Vec<u8>>) -> VmResult<Value> {
    match r {
        Ok(s) => Ok(Value::Str(ZStr::from(s.as_slice()))),
        Err(e) => Err(fail(vm, a, &e)),
    }
}

/// A `string|false` result.
fn string_or_false(vm: &mut Vm, a: &Args, r: GettextResult<Option<Vec<u8>>>) -> VmResult<Value> {
    match r {
        Ok(Some(s)) => Ok(Value::Str(ZStr::from(s.as_slice()))),
        Ok(None) => Ok(Value::Bool(false)),
        Err(e) => Err(fail(vm, a, &e)),
    }
}

/// An optional (`?string`) argument.
fn opt_str(a: &Args, n: usize) -> Option<ZStr> {
    match a.opt(n) {
        Some(Value::Str(s)) => Some(s),
        _ => None,
    }
}

fn textdomain(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let d = opt_str(a, 0);
    let r = php_gettext::textdomain(d.as_ref().map(ZStr::as_bytes));
    string(vm, a, r)
}

fn gettext(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = php_gettext::gettext(a.str(0).as_bytes());
    string(vm, a, r)
}

fn dgettext(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = php_gettext::dgettext(a.str(0).as_bytes(), a.str(1).as_bytes());
    string(vm, a, r)
}

fn dcgettext(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = php_gettext::dcgettext(a.str(0).as_bytes(), a.str(1).as_bytes(), a.int(2));
    string(vm, a, r)
}

fn bindtextdomain(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let dir = opt_str(a, 1);
    let r = php_gettext::bindtextdomain(a.str(0).as_bytes(), dir.as_ref().map(ZStr::as_bytes));
    string_or_false(vm, a, r)
}

fn ngettext(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = php_gettext::ngettext(a.str(0).as_bytes(), a.str(1).as_bytes(), a.int(2));
    string(vm, a, r)
}

fn dngettext(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = php_gettext::dngettext(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.str(2).as_bytes(),
        a.int(3),
    );
    string(vm, a, r)
}

fn dcngettext(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = php_gettext::dcngettext(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.str(2).as_bytes(),
        a.int(3),
        a.int(4),
    );
    string(vm, a, r)
}

fn bind_textdomain_codeset(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cs = opt_str(a, 1);
    let r =
        php_gettext::bind_textdomain_codeset(a.str(0).as_bytes(), cs.as_ref().map(ZStr::as_bytes));
    string_or_false(vm, a, r)
}

impl Extension for Gettext {
    fn name(&self) -> &'static str {
        "gettext"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        m.function("textdomain(?string $domain = null): string", textdomain)
            .function("gettext(string $message): string", gettext)
            .function("_(string $message): string", gettext)
            .function("dgettext(string $domain, string $message): string", dgettext)
            .function(
                "dcgettext(string $domain, string $message, int $category): string",
                dcgettext,
            )
            .function(
                "bindtextdomain(string $domain, ?string $directory = null): string|false",
                bindtextdomain,
            )
            .function(
                "ngettext(string $singular, string $plural, int $count): string",
                ngettext,
            )
            .function(
                "dngettext(string $domain, string $singular, string $plural, int $count): string",
                dngettext,
            )
            .function(
                "dcngettext(string $domain, string $singular, string $plural, int $count, int $category): string",
                dcngettext,
            )
            .function(
                "bind_textdomain_codeset(string $domain, ?string $codeset = null): string|false",
                bind_textdomain_codeset,
            );
    }
}
