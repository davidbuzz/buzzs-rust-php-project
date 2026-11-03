//! `ext/hash` for the phprust VM: binds the `php-hash` library through the
//! `php_vm::ext` extension API.
//!
//! * `hash()`, `hash_file()`, `hash_hmac()`, `hash_hmac_file()`,
//!   `hash_init()`, `hash_update()`, `hash_update_stream()`,
//!   `hash_update_file()`, `hash_final()`, `hash_copy()`, `hash_algos()`,
//!   `hash_hmac_algos()`, `hash_pbkdf2()`, `hash_equals()`, `hash_hkdf()`;
//! * the `HashContext` class (private constructor, cloning,
//!   `__serialize()`/`__unserialize()`, `__debugInfo()`);
//! * the `mhash*()` compatibility functions and the `HASH_HMAC` /
//!   `MHASH_*` constants;
//! * `md5()`, `md5_file()`, `sha1()`, `sha1_file()` and `crc32()` of
//!   `ext/standard`, which share the algorithms.
//!
//! Files are read through the `php-streams` layer (see [`files`]); stream
//! resources passed to `hash_update_stream()` are read with `fread()`.
//!
//! Known gaps: the `MHASH_*` constants are not reported as deprecated (the
//! extension API has no deprecated-constant flag), and the
//! `$stream_context` argument of `hash_update_file()` is not forwarded to
//! the opener (the context resource payload belongs to `php-ext-streams`).

mod context;
mod files;

use php_hash::{
    Diagnostic, DiagnosticLevel, ErrorClass, HashError, HashOptions, SecretOption, SeedOption,
};
use php_runtime::{Value, ZArray, ZStr};
use php_vm::ext::{Args, Extension, ModuleBuilder, Throw, VmResult};
use php_vm::Vm;

/// The `hash` extension (also providing `md5`/`sha1`/`crc32`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Hash;

/// `E_DEPRECATED`.
const E_DEPRECATED: i64 = 8192;

/// Builds the throwable PHP raises for a library error inside `func`.
pub(crate) fn throw_error(vm: &mut Vm, func: &str, e: &HashError) -> Throw {
    let class = match e.class {
        ErrorClass::ValueError => "ValueError",
        ErrorClass::TypeError => "TypeError",
        ErrorClass::Error => "Error",
        ErrorClass::Exception => "Exception",
    };
    vm.throw(class, &e.message_for(func))
}

fn fail(vm: &mut Vm, a: &Args, e: &HashError) -> Throw {
    throw_error(vm, &a.func_name(), e)
}

fn emit(vm: &mut Vm, diags: Vec<Diagnostic>) -> VmResult<()> {
    for d in diags {
        match d.level {
            DiagnosticLevel::Deprecated => vm.php_deprecated(&d.message)?,
            DiagnosticLevel::Warning => vm.php_warning(&d.message)?,
        }
    }
    Ok(())
}

fn bytes(b: impl AsRef<[u8]>) -> Value {
    Value::Str(ZStr::from(b.as_ref()))
}

/// Raw digest or its lowercase hex form.
fn digest_value(raw: &[u8], binary: bool) -> Value {
    if binary {
        bytes(raw)
    } else {
        bytes(php_hash::bin2hex(raw))
    }
}

/// Translates the `$options` array (`seed`, `secret`).
fn options(vm: &mut Vm, algo: &[u8], opts: Option<&ZArray>) -> VmResult<HashOptions> {
    let mut out = HashOptions::default();
    let Some(opts) = opts else {
        return Ok(out);
    };
    if let Some(seed) = opts.get_str(b"seed") {
        out.seed = Some(match seed.deref() {
            Value::Int(i) => SeedOption::Int(i),
            _ => SeedOption::NonInt,
        });
    }
    // Only the XXH3 family reads (and therefore converts) the secret.
    let lower = algo.to_ascii_lowercase();
    if matches!(lower.as_slice(), b"xxh3" | b"xxh128") {
        if let Some(secret) = opts.get_str(b"secret") {
            let secret = secret.deref();
            let was_string = matches!(secret, Value::Str(_));
            let s = vm.value_to_string(&secret)?;
            out.secret = Some(SecretOption {
                bytes: s.as_bytes().to_vec(),
                was_string,
            });
        }
    }
    Ok(out)
}

fn opt_array(a: &Args, n: usize) -> Option<ZArray> {
    match a.opt(n) {
        Some(Value::Array(arr)) => Some(arr),
        _ => None,
    }
}

// ------------------------------------------------------------ one-shot

fn hash(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let algo = a.str(0);
    let opts = options(vm, algo.as_bytes(), opt_array(a, 3).as_ref())?;
    match php_hash::hash(algo.as_bytes(), a.str(1).as_bytes(), &opts) {
        Ok(d) => {
            emit(vm, d.diagnostics.clone())?;
            Ok(digest_value(&d.bytes, a.bool(2)))
        }
        Err(e) => Err(fail(vm, a, &e)),
    }
}

fn hash_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let algo = a.str(0);
    if php_hash::Algorithm::find(algo.as_bytes()).is_none() {
        return Err(a.value_error(vm, 1, "must be a valid hashing algorithm"));
    }
    let path = a.str(1);
    files::check_path(vm, a, 2, path.as_bytes())?;
    let opts = options(vm, algo.as_bytes(), opt_array(a, 3).as_ref())?;
    let mut ctx = match php_hash::HashContext::new(algo.as_bytes(), 0, b"", &opts) {
        Ok(c) => c,
        Err(e) => return Err(fail(vm, a, &e)),
    };
    let func = a.func_name();
    let read = files::read_path(vm, &func, path.as_bytes(), |chunk| {
        let _ = ctx.update(chunk);
    })?;
    emit(vm, ctx.take_diagnostics())?;
    if read != Some(true) {
        return Ok(Value::Bool(false));
    }
    match ctx.finalize() {
        Ok(d) => Ok(digest_value(&d, a.bool(2))),
        Err(e) => Err(fail(vm, a, &e)),
    }
}

fn hash_hmac(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match php_hash::hash_hmac(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.str(2).as_bytes(),
    ) {
        Ok(d) => Ok(digest_value(&d, a.bool(3))),
        Err(e) => Err(fail(vm, a, &e)),
    }
}

fn hash_hmac_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut ctx = match php_hash::hmac_context(a.str(0).as_bytes(), a.str(2).as_bytes()) {
        Ok(c) => c,
        Err(e) => return Err(fail(vm, a, &e)),
    };
    let path = a.str(1);
    files::check_path(vm, a, 2, path.as_bytes())?;
    let func = a.func_name();
    let read = files::read_path(vm, &func, path.as_bytes(), |chunk| {
        let _ = ctx.update(chunk);
    })?;
    if read != Some(true) {
        return Ok(Value::Bool(false));
    }
    match ctx.finalize() {
        Ok(d) => Ok(digest_value(&d, a.bool(3))),
        Err(e) => Err(fail(vm, a, &e)),
    }
}

// --------------------------------------------------------- incremental

fn hash_init(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let algo = a.str(0);
    let opts = options(vm, algo.as_bytes(), opt_array(a, 3).as_ref())?;
    let mut ctx =
        match php_hash::HashContext::new(algo.as_bytes(), a.int(1), a.str(2).as_bytes(), &opts) {
            Ok(c) => c,
            Err(e) => return Err(fail(vm, a, &e)),
        };
    emit(vm, ctx.take_diagnostics())?;
    context::new_object(vm, ctx)
}

fn hash_update(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let data = a.str(1);
    context::with_live(vm, a, 0, |c| c.update(data.as_bytes()))?.map_err(|e| fail(vm, a, &e))?;
    Ok(Value::Bool(true))
}

fn hash_update_stream(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let stream = a.at(1);
    if !matches!(stream, Value::Resource(_)) {
        let msg = format!("must be of type resource, {} given", stream.value_name());
        return Err(a.type_error(vm, 2, &msg));
    }
    // Verify the context before reading anything.
    context::with_live(vm, a, 0, |_| ())?;
    let length = if a.has(2) { a.int(2) } else { -1 };
    let mut data = Vec::new();
    let did = files::read_stream(vm, &stream, length, |chunk| data.extend_from_slice(chunk))?;
    context::with_live(vm, a, 0, |c| c.update(&data))?.map_err(|e| fail(vm, a, &e))?;
    Ok(Value::Int(did))
}

fn hash_update_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = a.str(1);
    files::check_path(vm, a, 2, path.as_bytes())?;
    if let Some(ctx) = a.get(2) {
        if !matches!(ctx, Value::Resource(_) | Value::Null) {
            let msg = format!(
                "must be of type resource or null, {} given",
                ctx.value_name()
            );
            return Err(a.type_error(vm, 3, &msg));
        }
    }
    context::with_live(vm, a, 0, |_| ())?;
    let func = a.func_name();
    let mut data = Vec::new();
    let read = files::read_path(vm, &func, path.as_bytes(), |chunk| {
        data.extend_from_slice(chunk)
    })?;
    let Some(ok) = read else {
        return Ok(Value::Bool(false));
    };
    context::with_live(vm, a, 0, |c| c.update(&data))?.map_err(|e| fail(vm, a, &e))?;
    Ok(Value::Bool(ok))
}

fn hash_final(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let d = context::with_live(vm, a, 0, php_hash::HashContext::finalize)?
        .map_err(|e| fail(vm, a, &e))?;
    Ok(digest_value(&d, a.bool(1)))
}

fn hash_copy(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let copy = context::with_live(vm, a, 0, |c| c.try_clone())?;
    match copy {
        Ok(c) => context::new_object(vm, c),
        Err(_) => Err(vm.throw("Error", "Cannot copy hash")),
    }
}

fn hash_algos(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(ZArray::from_values(
        php_hash::hash_algos().into_iter().map(Value::string),
    )))
}

fn hash_hmac_algos(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(ZArray::from_values(
        php_hash::hash_hmac_algos().into_iter().map(Value::string),
    )))
}

// ------------------------------------------------------- derivation

fn hash_pbkdf2(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match php_hash::hash_pbkdf2(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.str(2).as_bytes(),
        a.int(3),
        a.int(4),
        a.bool(5),
    ) {
        Ok(out) => Ok(bytes(out)),
        Err(e) => Err(fail(vm, a, &e)),
    }
}

fn hash_hkdf(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    match php_hash::hash_hkdf(
        a.str(0).as_bytes(),
        a.str(1).as_bytes(),
        a.int(2),
        a.str(3).as_bytes(),
        a.str(4).as_bytes(),
    ) {
        Ok(out) => Ok(bytes(out)),
        Err(e) => Err(fail(vm, a, &e)),
    }
}

fn hash_equals(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (known, user) = (a.at(0), a.at(1));
    for (n, v) in [(1, &known), (2, &user)] {
        if !matches!(v, Value::Str(_)) {
            let msg = format!("must be of type string, {} given", v.value_name());
            return Err(a.type_error(vm, n, &msg));
        }
    }
    let (Value::Str(k), Value::Str(u)) = (known, user) else {
        return Ok(Value::Bool(false));
    };
    Ok(Value::Bool(php_hash::hash_equals(
        k.as_bytes(),
        u.as_bytes(),
    )))
}

// ------------------------------------------------------------- mhash

fn mhash_deprecated(vm: &mut Vm, a: &Args) -> VmResult<()> {
    vm.raise(
        E_DEPRECATED,
        &format!("Function {}() is deprecated since 8.1", a.func_name()),
    )
}

fn mhash(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    mhash_deprecated(vm, a)?;
    let key = a.opt(2).map(|_| a.str(2));
    match php_hash::mhash(
        a.int(0),
        a.str(1).as_bytes(),
        key.as_ref().map(ZStr::as_bytes),
    ) {
        Ok(Some(d)) => Ok(bytes(d)),
        Ok(None) => Ok(Value::Bool(false)),
        Err(e) => Err(fail(vm, a, &e)),
    }
}

fn mhash_get_hash_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    mhash_deprecated(vm, a)?;
    Ok(php_hash::mhash_get_hash_name(a.int(0)).map_or(Value::Bool(false), Value::string))
}

fn mhash_get_block_size(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    mhash_deprecated(vm, a)?;
    Ok(
        php_hash::mhash_get_block_size(a.int(0)).map_or(Value::Bool(false), |n| {
            Value::Int(i64::try_from(n).unwrap_or(i64::MAX))
        }),
    )
}

fn mhash_count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    mhash_deprecated(vm, a)?;
    Ok(Value::Int(php_hash::mhash_count()))
}

fn mhash_keygen_s2k(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    mhash_deprecated(vm, a)?;
    match php_hash::mhash_keygen_s2k(a.int(0), a.str(1).as_bytes(), a.str(2).as_bytes(), a.int(3)) {
        Ok(Some(d)) => Ok(bytes(d)),
        Ok(None) => Ok(Value::Bool(false)),
        Err(e) => Err(fail(vm, a, &e)),
    }
}

// ------------------------------------------------------ ext/standard

fn md5(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(digest_value(&php_hash::md5(a.str(0).as_bytes()), a.bool(1)))
}

fn sha1(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(digest_value(
        &php_hash::sha1(a.str(0).as_bytes()),
        a.bool(1),
    ))
}

fn crc32(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(i64::from(php_hash::crc32(a.str(0).as_bytes()))))
}

/// `md5_file()` / `sha1_file()`: the digest of a file's contents.
fn file_digest(vm: &mut Vm, a: &Args, algo: &str) -> VmResult<Value> {
    let path = a.str(0);
    files::check_path(vm, a, 1, path.as_bytes())?;
    let mut ctx = match php_hash::HashContext::new(algo.as_bytes(), 0, b"", &HashOptions::default())
    {
        Ok(c) => c,
        Err(e) => return Err(fail(vm, a, &e)),
    };
    let func = a.func_name();
    let read = files::read_path(vm, &func, path.as_bytes(), |chunk| {
        let _ = ctx.update(chunk);
    })?;
    if read != Some(true) {
        return Ok(Value::Bool(false));
    }
    match ctx.finalize() {
        Ok(d) => Ok(digest_value(&d, a.bool(1))),
        Err(e) => Err(fail(vm, a, &e)),
    }
}

fn md5_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    file_digest(vm, a, "md5")
}

fn sha1_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    file_digest(vm, a, "sha1")
}

impl Extension for Hash {
    fn name(&self) -> &'static str {
        "hash"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        context::register(m);
        m.constant("HASH_HMAC", Value::Int(php_hash::HASH_HMAC));
        for (name, id) in php_hash::mhash_constants() {
            m.constant(&format!("MHASH_{name}"), Value::Int(id));
        }
        m.function("hash(string $algo, string $data, bool $binary = false, array $options = []): string", hash)
            .function("hash_file(string $algo, string $filename, bool $binary = false, array $options = []): string|false", hash_file)
            .function("hash_hmac(string $algo, string $data, string $key, bool $binary = false): string", hash_hmac)
            .function("hash_hmac_file(string $algo, string $filename, string $key, bool $binary = false): string|false", hash_hmac_file)
            .function("hash_init(string $algo, int $flags = 0, string $key = \"\", array $options = []): HashContext", hash_init)
            .function("hash_update(HashContext $context, string $data): true", hash_update)
            .function("hash_update_stream(HashContext $context, $stream, int $length = -1): int", hash_update_stream)
            .function("hash_update_file(HashContext $context, string $filename, $stream_context = null): bool", hash_update_file)
            .function("hash_final(HashContext $context, bool $binary = false): string", hash_final)
            .function("hash_copy(HashContext $context): HashContext", hash_copy)
            .function("hash_algos(): array", hash_algos)
            .function("hash_hmac_algos(): array", hash_hmac_algos)
            .function("hash_pbkdf2(string $algo, string $password, string $salt, int $iterations, int $length = 0, bool $binary = false, array $options = []): string", hash_pbkdf2)
            .function("hash_equals($known_string, $user_string): bool", hash_equals)
            .function("hash_hkdf(string $algo, string $key, int $length = 0, string $info = \"\", string $salt = \"\"): string", hash_hkdf)
            .function("mhash_get_block_size(int $algo): int|false", mhash_get_block_size)
            .function("mhash_get_hash_name(int $algo): string|false", mhash_get_hash_name)
            .function("mhash_keygen_s2k(int $algo, string $password, string $salt, int $length): string|false", mhash_keygen_s2k)
            .function("mhash_count(): int", mhash_count)
            .function("mhash(int $algo, string $data, ?string $key = null): string|false", mhash)
            .function("md5(string $string, bool $binary = false): string", md5)
            .function("md5_file(string $filename, bool $binary = false): string|false", md5_file)
            .function("sha1(string $string, bool $binary = false): string", sha1)
            .function("sha1_file(string $filename, bool $binary = false): string|false", sha1_file)
            .function("crc32(string $string): int", crc32);
    }
}
