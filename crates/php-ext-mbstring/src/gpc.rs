//! `mb_parse_str()` (`_php_mb_encoding_handler_ex` in `mb_gpc.c`).

use php_mbstring::convert::fast_convert;
use php_mbstring::{encoding, Encoding, EncodingNo};
use php_runtime::{Value, ZArray, ZStr};
use php_stdmisc::query::{register_variable_ex, url_decode, RegisterOptions};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

use crate::state;

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
        .unwrap_or_default();
    let seps = vm
        .ini_string("arg_separator.input")
        .unwrap_or_else(|| b"&".to_vec());
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
    let mut track = ZArray::new();
    let detected = if res.is_empty() {
        None
    } else {
        // Split and url-decode into (name, value) pairs.
        let mut list: Vec<Vec<u8>> = Vec::new();
        for var in tokens(res, &seps) {
            if let Some(i) = var.iter().position(|&b| b == b'=') {
                list.push(url_decode(&var[..i], false));
                list.push(url_decode(&var[i + 1..], false));
            } else {
                list.push(url_decode(var, false));
                list.push(Vec::new());
            }
        }
        // val_list + len_list of the C implementation.
        let seps_count = res.iter().filter(|b| seps.contains(b)).count();
        crate::mem::check(vm, crate::mem::zstr(res.len()), (seps_count + 1) * 2 * 16)?;
        if i64::try_from(list.len()).unwrap_or(i64::MAX) > max_vars.saturating_mul(2) {
            vm.php_warning(&format!(
                "Input variables exceeded {max_vars}. To increase the limit change max_input_vars in php.ini."
            ))?;
            None
        } else {
            register_all(vm, &list, &mut track, nesting)?
        }
    };
    state::with(|s| s.http_input_identify = detected);
    a.set_ref(1, Value::Array(track));
    Ok(Value::Bool(detected.is_some()))
}

/// Detects the source encoding, converts and registers every pair;
/// returns the encoding used.
fn register_all(
    vm: &mut Vm,
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
                vm.php_warning("Unable to detect encoding")?;
                pass
            }
        }
    };
    let opts = RegisterOptions {
        max_nesting_level: nesting,
        display_errors: vm.ini_bool("display_errors"),
        cookie: false,
        warning_prefix: "mb_parse_str(): ",
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
        if track.len().is_power_of_two() && track.len() >= 1024 {
            let size = crate::mem::value_size(&Value::Array(track.clone()));
            crate::mem::check(vm, list.len() * 16, size)?;
        }
    }
    Ok(Some(from_enc))
}

/// Registers `mb_parse_str`.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function("mb_parse_str(string $string, &$result): bool", mb_parse_str);
}
