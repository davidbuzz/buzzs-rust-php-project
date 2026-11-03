//! Source highlighting/stripping, query strings and locale functions.

use crate::ctx::{RtExt, VmQuery};
use php_lexer::WarningKind;
use php_runtime::{Value, ZStr};
use php_stdmisc::highlight::{self, HighlightColors};
use php_stdmisc::locale::{self, LocaleState};
use php_stdmisc::query::{self, InputSettings};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;
use std::cell::RefCell;

thread_local! {
    static LOCALE: RefCell<LocaleState> = RefCell::new(LocaleState::default());
}

/// `E_COMPILE_WARNING`.
const E_COMPILE_WARNING: i64 = 128;
/// `E_DEPRECATED`.
const E_DEPRECATED: i64 = 8192;

/// `php_get_highlight_struct()`: the `highlight.*` INI colours.
fn colors(vm: &Vm) -> HighlightColors {
    let d = HighlightColors::default();
    let get = |name: &str, def: Vec<u8>| vm.ini_string(name).unwrap_or(def);
    HighlightColors {
        comment: get("highlight.comment", d.comment),
        default: get("highlight.default", d.default),
        html: get("highlight.html", d.html),
        keyword: get("highlight.keyword", d.keyword),
        string: get("highlight.string", d.string),
    }
}

fn short_tags(vm: &Vm) -> bool {
    vm.ini_bool("short_open_tag")
}

/// Prints or returns highlighted HTML (`$return`).
fn emit(vm: &mut Vm, html: &[u8], ret: bool) -> Value {
    if ret {
        Value::Str(ZStr::from(html))
    } else {
        vm.output(html);
        Value::Bool(true)
    }
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn highlight_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let src = a.str(0);
    let ret = a.has(1) && a.bool(1);
    let html = highlight::highlight(src.as_bytes(), &colors(vm), short_tags(vm));
    Ok(emit(vm, &html, ret))
}

/// Reads a script for highlighting/stripping like `zend_stream_open`
/// (reporting "Failed to open stream" on failure).
fn read_script(vm: &mut Vm, func: &str, path: &[u8]) -> VmResult<Option<Vec<u8>>> {
    use std::os::unix::ffi::OsStrExt;
    let p = std::ffi::OsStr::from_bytes(path);
    match std::fs::read(p) {
        Ok(s) => Ok(Some(s)),
        Err(e) => {
            let reason = match e.kind() {
                std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
                std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
                _ if std::fs::metadata(p).is_ok_and(|m| m.is_dir()) => "Is a directory".to_string(),
                _ => e.to_string(),
            };
            vm.raise(
                2,
                &format!(
                    "{func}({}): Failed to open stream: {reason}",
                    String::from_utf8_lossy(path)
                ),
            )?;
            Ok(None)
        }
    }
}

fn highlight_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = a.str(0);
    let ret = a.has(1) && a.bool(1);
    let func = a.func_name();
    if path.as_bytes().contains(&0) {
        return Err(a.value_error(vm, 1, "must not contain any null bytes"));
    }
    let Some(src) = read_script(vm, &func, path.as_bytes())? else {
        vm.php_warning(&format!(
            "Failed opening '{}' for highlighting",
            String::from_utf8_lossy(path.as_bytes())
        ))?;
        return Ok(Value::Bool(false));
    };
    let html = highlight::highlight(&src, &colors(vm), short_tags(vm));
    Ok(emit(vm, &html, ret))
}

fn php_strip_whitespace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = a.str(0);
    if path.as_bytes().contains(&0) {
        return Err(a.value_error(vm, 1, "must not contain any null bytes"));
    }
    let Some(src) = read_script(vm, "php_strip_whitespace", path.as_bytes())? else {
        return Ok(Value::from(""));
    };
    let (out, warnings) = highlight::strip_whitespace(&src, short_tags(vm));
    for w in warnings {
        let level = match w.kind {
            WarningKind::CompileWarning => E_COMPILE_WARNING,
            WarningKind::Deprecated => E_DEPRECATED,
        };
        vm.error_at(level, &w.message, &path, w.line)?;
    }
    Ok(Value::Str(ZStr::from(out)))
}

fn http_build_query(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.at(0);
    let prefix = if a.has(1) { a.str(1) } else { ZStr::from("") };
    let sep = a.opt(2).map(|_| a.str(2));
    let enc = if a.has(3) {
        a.int(3)
    } else {
        query::PHP_QUERY_RFC1738
    };
    let default_sep = vm.ini_string("arg_separator.output").unwrap_or_default();
    let r = {
        let mut q = VmQuery::new(vm);
        query::http_build_query(
            &mut q,
            &data,
            prefix.as_bytes(),
            sep.as_ref().map(ZStr::as_bytes),
            enc,
            &default_sep,
        )
    };
    r.vm(vm).map(Value::Str)
}

/// The request-variable INI settings used by `parse_str()`.
fn input_settings(vm: &Vm) -> InputSettings {
    let d = InputSettings::default();
    InputSettings {
        arg_separator_input: vm
            .ini_string("arg_separator.input")
            .unwrap_or(d.arg_separator_input),
        max_input_vars: vm
            .ini_string("max_input_vars")
            .map_or(d.max_input_vars, |_| vm.ini_long("max_input_vars")),
        max_input_nesting_level: vm
            .ini_string("max_input_nesting_level")
            .map_or(d.max_input_nesting_level, |_| {
                vm.ini_long("max_input_nesting_level")
            }),
        display_errors: vm.ini_bool("display_errors"),
    }
}

fn parse_str(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    let settings = input_settings(vm);
    let arr = query::parse_str(s.as_bytes(), &settings, vm).vm(vm)?;
    a.set_ref(1, Value::Array(arr));
    Ok(Value::Null)
}

fn setlocale(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cat = a.int(0);
    let args: Vec<Value> = a.values.iter().skip(1).map(Value::deref).collect();
    LOCALE
        .with(|l| {
            let mut st = l.borrow().clone();
            let r = st.setlocale(cat, &args, vm);
            *l.borrow_mut() = st;
            r
        })
        .vm(vm)
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn localeconv(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(locale::localeconv()))
}

fn nl_langinfo(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    locale::nl_langinfo(a.int(0), vm).vm(vm)
}

/// Registers the functions and constants.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    for (name, v) in locale::CONSTANTS {
        m.constant(name, Value::Int(*v));
    }
    m.constant("PHP_QUERY_RFC1738", Value::Int(query::PHP_QUERY_RFC1738))
        .constant("PHP_QUERY_RFC3986", Value::Int(query::PHP_QUERY_RFC3986));
    m.function(
        "highlight_string(string $string, bool $return = false): string|true",
        highlight_string,
    )
    .function(
        "highlight_file(string $filename, bool $return = false): string|bool",
        highlight_file,
    )
    .function(
        "show_source(string $filename, bool $return = false): string|bool",
        highlight_file,
    )
    .function("php_strip_whitespace(string $filename): string", php_strip_whitespace)
    .function(
        "http_build_query(array|object $data, string $numeric_prefix = \"\", ?string $arg_separator = null, int $encoding_type = PHP_QUERY_RFC1738): string",
        http_build_query,
    )
    .function("parse_str(string $string, &$result): void", parse_str)
    .function("setlocale(int $category, $locales, ...$rest): string|false", setlocale)
    .function("localeconv(): array", localeconv)
    .function("nl_langinfo(int $item): string|false", nl_langinfo)
    .on_request_shutdown(|_| LOCALE.with(|l| l.borrow_mut().request_shutdown()));
    locale::reset_lc_ctype();
}
