//! `ext/mbstring` for the phprust VM: binds the `php-mbstring` library
//! (libmbfl's encodings and the `mb_*` algorithms) through the
//! `php_vm::ext` extension API.
//!
//! This crate is the glue of `ext/mbstring/mbstring.c` and
//! `php_mbregex.c`: argument handling, array walking
//! (`mb_convert_encoding`/`mb_check_encoding` on arrays, by-reference
//! `mb_convert_variables`), the `mbstring.*` INI table with its on-modify
//! handlers, `mb_parse_str`, `mb_send_mail`, `mb_output_handler`, and the
//! `mb_ereg*` family, which runs on PCRE2 (see [`regex`]).

mod arrays;
mod funcs;
mod gpc;
mod ini;
mod mail;
mod mem;
mod output;
pub mod regex;
mod state;

use php_runtime::Value;
use php_vm::ext::{Extension, ModuleBuilder};

/// The `mbstring` extension.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mbstring;

impl Extension for Mbstring {
    fn name(&self) -> &'static str {
        "mbstring"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        ini::register(m);
        register_constants(m);
        funcs::register(m);
        arrays::register(m);
        mail::register(m);
        gpc::register(m);
        output::register(m);
        regex::register(m);
        ini::install_hooks(m);
        output::install_hooks(m);
        ini::startup(m.vm());
        m.on_request_shutdown(|_| {
            state::with(php_mbstring::MbState::request_shutdown);
            output::reset();
            regex::reset();
        });
    }
}

fn register_constants(m: &mut ModuleBuilder<'_>) {
    let cases = [
        ("MB_CASE_UPPER", 0),
        ("MB_CASE_LOWER", 1),
        ("MB_CASE_TITLE", 2),
        ("MB_CASE_FOLD", 3),
        ("MB_CASE_UPPER_SIMPLE", 4),
        ("MB_CASE_LOWER_SIMPLE", 5),
        ("MB_CASE_TITLE_SIMPLE", 6),
        ("MB_CASE_FOLD_SIMPLE", 7),
    ];
    for (name, v) in cases {
        m.constant(name, Value::Int(v));
    }
    m.constant(
        "MB_ONIGURUMA_VERSION",
        Value::from(regex::ONIGURUMA_VERSION),
    );
}
