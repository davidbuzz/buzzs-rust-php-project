//! `setcookie()` / `setrawcookie()` / `header_remove()` (`ext/standard/head.c`).
//!
//! The CLI SAPI keeps no response headers, so after php-src's argument and
//! option validation the cookie header is not stored; the functions return
//! `true` like `php_setcookie()` + `sapi_header_op()` do under the CLI.

use php_runtime::{Value, ZArray, ZStr};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;

/// The `ILLEGAL_COOKIE_CHARACTER` list of the error messages.
const ILLEGAL: &str = r#"",", ";", " ", "\t", "\r", "\n", "\013", or "\014""#;

/// `strpbrk(s, set) != NULL` on the C string (up to the first NUL).
fn has_any(s: &[u8], set: &[u8]) -> bool {
    s.iter().take_while(|&&c| c != 0).any(|c| set.contains(c))
}

const COOKIE_SEPARATORS: &[u8] = b",; \t\r\n\x0b\x0c";

/// The cookie attributes (`php_setcookie()` parameters).
#[derive(Default)]
struct Cookie {
    expires: i64,
    path: Option<ZStr>,
    domain: Option<ZStr>,
    secure: bool,
    samesite: Option<ZStr>,
    partitioned: bool,
}

/// `php_head_parse_cookie_options_array()`.
fn parse_options(vm: &mut Vm, func: &str, opts: &ZArray, c: &mut Cookie) -> VmResult<()> {
    for (k, v) in opts.iter() {
        let php_runtime::ArrayKey::Str(key) = k else {
            return Err(vm.throw(
                "ValueError",
                &format!("{func}(): option array cannot have numeric keys"),
            ));
        };
        let v = v.deref();
        match key.as_bytes().to_ascii_lowercase().as_slice() {
            b"expires" => c.expires = vm.value_to_int(&v)?,
            b"path" => c.path = Some(vm.value_to_string(&v)?),
            b"domain" => c.domain = Some(vm.value_to_string(&v)?),
            b"secure" => c.secure = vm.value_to_bool(&v)?,
            b"httponly" => {
                vm.value_to_bool(&v)?;
            }
            b"samesite" => c.samesite = Some(vm.value_to_string(&v)?),
            b"partitioned" => c.partitioned = vm.value_to_bool(&v)?,
            _ => {
                return Err(vm.throw(
                    "ValueError",
                    &format!("{func}(): option \"{}\" is invalid", key.to_string_lossy()),
                ))
            }
        }
    }
    Ok(())
}

/// `php_setcookie()`'s validation.
fn validate(vm: &mut Vm, a: &Args, func: &str, raw: bool, c: &Cookie) -> VmResult<()> {
    let name = a.str(0);
    if name.is_empty() {
        return Err(a.value_error(vm, 1, "must not be empty"));
    }
    if has_any(name.as_bytes(), b"=,; \t\r\n\x0b\x0c") {
        return Err(a.value_error(vm, 1, &format!("cannot contain \"=\", {ILLEGAL}")));
    }
    if raw && a.has(1) && has_any(a.str(1).as_bytes(), COOKIE_SEPARATORS) {
        return Err(a.value_error(vm, 2, &format!("cannot contain {ILLEGAL}")));
    }
    for (label, v) in [("path", &c.path), ("domain", &c.domain)] {
        if v.as_ref()
            .is_some_and(|s| has_any(s.as_bytes(), COOKIE_SEPARATORS))
        {
            return Err(vm.throw(
                "ValueError",
                &format!("{func}(): \"{label}\" option cannot contain {ILLEGAL}"),
            ));
        }
    }
    if c.expires >= 253_402_300_800 {
        return Err(vm.throw(
            "ValueError",
            &format!("{func}(): \"expires\" option cannot have a year greater than 9999"),
        ));
    }
    if c.partitioned && !c.secure {
        return Err(vm.throw(
            "ValueError",
            &format!("{func}(): \"partitioned\" option cannot be used without \"secure\" option"),
        ));
    }
    if let Some(s) = &c.samesite {
        let ok = s.is_empty()
            || [&b"strict"[..], b"lax", b"none"]
                .contains(&s.as_bytes().to_ascii_lowercase().as_slice());
        if !ok {
            return Err(vm.throw(
                "ValueError",
                &format!(
                    "{func}(): \"samesite\" option must be \"Strict\", \"Lax\", \"None\", or \"\""
                ),
            ));
        }
    }
    Ok(())
}

/// `php_setcookie_common()`.
fn setcookie_common(vm: &mut Vm, a: &Args, raw: bool) -> VmResult<Value> {
    let func = a.func_name();
    let mut c = Cookie::default();
    match a.get(2) {
        Some(Value::Array(opts)) => {
            let given = a.passed.iter().filter(|&&p| p).count();
            if given > 3 {
                return Err(vm.throw(
                    "ArgumentCountError",
                    &format!(
                        "{func}(): Expects exactly 3 arguments when argument #3 ($expires_or_options) is an array"
                    ),
                ));
            }
            parse_options(vm, &func, &opts, &mut c)?;
        }
        Some(v) => c.expires = vm.value_to_int(&v)?,
        None => {}
    }
    if !matches!(a.get(2), Some(Value::Array(_))) {
        c.path = a.has(3).then(|| a.str(3));
        c.domain = a.has(4).then(|| a.str(4));
        c.secure = a.has(5) && a.bool(5);
    }
    validate(vm, a, &func, raw, &c)?;
    if a.has(1) {
        check_memory(vm, &a.str(1), raw)?;
    }
    Ok(Value::Bool(true))
}

/// `memory_limit` in bytes (`None` = unlimited).
fn memory_limit(vm: &Vm) -> Option<u64> {
    let raw = vm.ini_string("memory_limit")?;
    let s = std::str::from_utf8(&raw).ok()?.trim();
    let (digits, mult) = match s.as_bytes().last()? {
        b'k' | b'K' => (&s[..s.len() - 1], 1u64 << 10),
        b'm' | b'M' => (&s[..s.len() - 1], 1 << 20),
        b'g' | b'G' => (&s[..s.len() - 1], 1 << 30),
        _ => (s, 1),
    };
    let n: i64 = digits.trim().parse().ok()?;
    u64::try_from(n)
        .ok()
        .filter(|&n| n > 0)
        .map(|n| n.saturating_mul(mult))
}

/// Models the allocations of building the `Set-Cookie` header (the VM
/// does not account memory): the value, its URL-encoded copy and the
/// header copy on top of the idle 2 MiB heap must fit in `memory_limit`,
/// else the engine's out-of-memory fatal error is raised.
fn check_memory(vm: &mut Vm, value: &ZStr, raw: bool) -> VmResult<()> {
    let Some(limit) = memory_limit(vm) else {
        return Ok(());
    };
    let encoded: u64 = if raw {
        value.len() as u64
    } else {
        value
            .as_bytes()
            .iter()
            .map(|&c| {
                if c.is_ascii_alphanumeric() || b"-_.~".contains(&c) {
                    1
                } else {
                    3
                }
            })
            .sum()
    };
    let need = (2u64 << 20) + value.len() as u64 + 2 * encoded;
    if need > limit {
        return Err(vm.fatal(&format!(
            "Allowed memory size of {limit} bytes exhausted (tried to allocate {encoded} bytes)"
        )));
    }
    Ok(())
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn header_remove(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

fn setcookie(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    setcookie_common(vm, a, false)
}

fn setrawcookie(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    setcookie_common(vm, a, true)
}

/// Registers the functions.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function(
        "setcookie(string $name, string $value = \"\", array|int $expires_or_options = 0, string $path = \"\", string $domain = \"\", bool $secure = false, bool $httponly = false): bool",
        setcookie,
    )
    .function(
        "setrawcookie(string $name, string $value = \"\", array|int $expires_or_options = 0, string $path = \"\", string $domain = \"\", bool $secure = false, bool $httponly = false): bool",
        setrawcookie,
    )
    .function("header_remove(?string $name = null): void", header_remove);
}
