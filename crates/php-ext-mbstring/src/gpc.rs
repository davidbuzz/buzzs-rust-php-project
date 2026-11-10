//! `mb_parse_str()` (`_php_mb_encoding_handler_ex` in `mb_gpc.c`).

use php_mbstring::convert::fast_convert;
use php_mbstring::{encoding, Encoding, EncodingNo};
use php_runtime::{Value, ZArray, ZStr};
use php_stdmisc::query::{register_variable_ex, url_decode, RegisterOptions};
use php_vm::ext::{Args, ModuleBuilder, RequestInput, VmResult};
use php_vm::Vm;

use crate::state;

/// `E_WARNING`.
const E_WARNING: i64 = 2;

/// `php_strtok_r` over a separator set: the non-empty tokens.
fn tokens<'a>(s: &'a [u8], seps: &'a [u8]) -> impl Iterator<Item = &'a [u8]> {
    s.split(move |c| seps.contains(c)).filter(|t| !t.is_empty())
}

fn mb_parse_str(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let input = a.str(0);
    // The C code works on a NUL-terminated copy.
    let res = input
        .as_bytes()
        .split(|&b| b == 0)
        .next()
        .unwrap_or_default()
        .to_vec();
    let seps = vm
        .ini_string("arg_separator.input")
        .unwrap_or_else(|| b"&".to_vec());
    let mut track = ZArray::new();
    let detected = if res.is_empty() {
        None
    } else {
        let h = Handler {
            separators: &seps,
            report_errors: true,
            cookie: false,
            prefix: "mb_parse_str(): ",
        };
        encoding_handler(vm, &h, &res, &mut track)?
    };
    state::with(|s| s.http_input_identify = detected);
    a.set_ref(1, Value::Array(track));
    Ok(Value::Bool(detected.is_some()))
}

/// `php_mb_encoding_handler_info_t`: how one input is split and reported.
struct Handler<'a> {
    separators: &'a [u8],
    /// "Unable to detect encoding" is reported (`mb_parse_str()` only).
    report_errors: bool,
    /// Cookie name mangling of `php_register_variable_ex`.
    cookie: bool,
    /// Warning prefix (`"mb_parse_str(): "`, `"PHP Request Startup: "`).
    prefix: &'a str,
}

/// `_php_mb_encoding_handler_ex()`: splits `res`, url-decodes, detects the
/// source encoding among `http_input_list`, converts to the internal
/// encoding and registers every pair into `track`; returns the encoding
/// used (`None` when nothing was registered).
fn encoding_handler(
    vm: &mut Vm,
    h: &Handler<'_>,
    res: &[u8],
    track: &mut ZArray,
) -> VmResult<Option<&'static Encoding>> {
    let max_vars = if vm.ini_string("max_input_vars").is_some() {
        vm.ini_long("max_input_vars")
    } else {
        1000
    };
    let nesting = if vm.ini_string("max_input_nesting_level").is_some() {
        vm.ini_long("max_input_nesting_level")
    } else {
        64
    };
    // val_list + len_list of the C implementation.
    let seps_count = res.iter().filter(|b| h.separators.contains(b)).count();
    crate::mem::check(vm, crate::mem::zstr(res.len()), (seps_count + 1) * 2 * 16)?;
    // Split and url-decode into (name, value) pairs.
    let mut list: Vec<Vec<u8>> = Vec::new();
    for var in tokens(res, h.separators) {
        if let Some(i) = var.iter().position(|&b| b == b'=') {
            list.push(url_decode(&var[..i], false));
            list.push(url_decode(&var[i + 1..], false));
        } else {
            list.push(url_decode(var, false));
            list.push(Vec::new());
        }
    }
    if i64::try_from(list.len()).unwrap_or(i64::MAX) > max_vars.saturating_mul(2) {
        vm.raise(
            E_WARNING,
            &format!(
                "{}Input variables exceeded {max_vars}. To increase the limit change max_input_vars in php.ini.",
                h.prefix
            ),
        )?;
        return Ok(None);
    }
    register_all(vm, h, &list, track, nesting)
}

/// `mbstr_treat_data()` and `php_mb_post_handler()` with
/// `mbstring.encoding_translation`: the request input converted from the
/// detected HTTP input encoding; `None` leaves it to the default parser.
fn treat_data(
    vm: &mut Vm,
    kind: RequestInput,
    data: &[u8],
    track: &mut ZArray,
) -> Option<VmResult<()>> {
    if !vm.ini_bool("mbstring.encoding_translation") {
        return None;
    }
    let res = data.split(|&b| b == 0).next().unwrap_or_default();
    if res.is_empty() {
        return Some(Ok(()));
    }
    state::with(|s| match kind {
        RequestInput::Get => s.http_input_identify_get = None,
        RequestInput::Post => s.http_input_identify_post = None,
        RequestInput::Cookie => s.http_input_identify_cookie = None,
    });
    let seps = if kind == RequestInput::Cookie {
        b";".to_vec()
    } else {
        vm.ini_string("arg_separator.input")
            .unwrap_or_else(|| b"&".to_vec())
    };
    state::with(|s| s.illegalchars = 0);
    let h = Handler {
        separators: &seps,
        report_errors: false,
        cookie: kind == RequestInput::Cookie,
        prefix: "PHP Request Startup: ",
    };
    let detected = match encoding_handler(vm, &h, res, track) {
        Ok(d) => d,
        Err(t) => return Some(Err(t)),
    };
    state::with(|s| {
        s.http_input_identify = detected;
        if let Some(d) = detected {
            match kind {
                RequestInput::Get => s.http_input_identify_get = Some(d),
                RequestInput::Post => s.http_input_identify_post = Some(d),
                RequestInput::Cookie => s.http_input_identify_cookie = Some(d),
            }
        }
    });
    Some(Ok(()))
}

/// Detects the source encoding, converts and registers every pair;
/// returns the encoding used.
fn register_all(
    vm: &mut Vm,
    h: &Handler<'_>,
    list: &[Vec<u8>],
    track: &mut ZArray,
    nesting: i64,
) -> VmResult<Option<&'static Encoding>> {
    let (from, to, sub, mode, strict) = state::with(|s| {
        (
            s.http_input_list().to_vec(),
            s.internal_encoding(),
            s.illegal_substchar(),
            s.illegal_mode(),
            s.strict_detection,
        )
    });
    let pass = encoding::pass();
    let from_enc = match from.len() {
        0 => pass,
        1 => from[0],
        _ => {
            let strs: Vec<&[u8]> = list.iter().map(Vec::as_slice).collect();
            if let Some(e) =
                php_mbstring::detect::guess_encoding_for_strings(&strs, &from, strict, false)
            {
                e
            } else {
                if h.report_errors {
                    vm.php_warning("Unable to detect encoding")?;
                }
                pass
            }
        }
    };
    let opts = RegisterOptions {
        max_nesting_level: nesting,
        display_errors: vm.ini_bool("display_errors"),
        cookie: h.cookie,
        warning_prefix: h.prefix,
    };
    let convert = from_enc.no != EncodingNo::Pass && to.no != EncodingNo::Pass;
    for pair in list.chunks(2) {
        let (name, val) = if convert {
            let (n, e1) = fast_convert(&pair[0], from_enc, to, sub, mode);
            let (v, e2) = fast_convert(&pair[1], from_enc, to, sub, mode);
            state::with(|s| s.illegalchars += u64::from(e1) + u64::from(e2));
            (n, v)
        } else {
            (pair[0].clone(), pair[1].clone())
        };
        register_variable_ex(&name, Value::Str(ZStr::new(val)), track, &opts, vm)
            .map_err(|e| state::rt_throw(vm, e))?;
        if crate::mem::pending() {
            return Ok(None);
        }
    }
    Ok(Some(from_enc))
}

/// Registers `mb_parse_str`.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function_static("mb_parse_str(string $string, &$result): bool", mb_parse_str);
    php_vm::ext::set_request_input_hook(treat_data);
}
