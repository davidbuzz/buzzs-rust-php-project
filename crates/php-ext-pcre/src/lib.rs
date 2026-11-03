//! `ext/pcre` for the phprust VM: binds the `php-pcre` library (PHP's
//! `preg_*` semantics over PCRE2) through the `php_vm::ext` extension API.
//!
//! This crate is the glue of `ext/pcre/php_pcre.c`: argument handling,
//! array/string subjects, `$matches`/`$count` by-reference outputs, user
//! callbacks, the per-request cache and `preg_last_error()` state (see
//! [`state`]), and the `PREG_*` / `PCRE_*` constants. Pattern parsing,
//! matching, replacement and splitting live in `php-pcre`.
//!
//! The `pcre.*` INI directives are registered by the VM core; they are
//! re-read before each operation, which is equivalent to PHP's on-modify
//! handlers. Locale-aware character tables follow the C library's current
//! `LC_CTYPE`, which is what `setlocale()` changes.

mod convert;
mod functions;
mod memory;
mod state;

use php_runtime::Value;
use php_vm::ext::{Extension, ModuleBuilder};

/// The `pcre` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pcre;

impl Extension for Pcre {
    fn name(&self) -> &'static str {
        "pcre"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        functions::register(m);
        register_constants(m);
        m.on_request_shutdown(|_| state::reset());
    }
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
