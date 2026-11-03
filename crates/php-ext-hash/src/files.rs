//! Reading files through the `php-streams` layer for `hash_file()`,
//! `hash_hmac_file()`, `hash_update_file()`, `md5_file()` and
//! `sha1_file()`, and stream resources for `hash_update_stream()`.
//!
//! Paths are opened directly with [`php_streams::open::open_wrapper`]
//! (`php_stream_open_wrapper_ex(path, "rb", REPORT_ERRORS, ...)`), so every
//! registered wrapper (`file://`, `php://`, `data:`, ...) works. The
//! library's queued diagnostics are re-emitted as `func(): message`
//! warnings.
//!
//! Stream *resources* belong to `php-ext-streams`; `hash_update_stream()`
//! reads them through the registered `fread()` function so that this crate
//! does not depend on that crate's resource payload type.

use php_runtime::Value;
use php_streams::diag::{self, Severity};
use php_streams::wrapper::REPORT_ERRORS;
use php_vm::ext::{Args, VmResult};
use php_vm::Vm;

/// `E_DEPRECATED`, `E_NOTICE`, `E_WARNING`, `E_ERROR`.
const E_ERROR: i64 = 1;
const E_WARNING: i64 = 2;
const E_NOTICE: i64 = 8;
const E_DEPRECATED: i64 = 8192;

/// The chunk size of the read loops (`char buf[1024]` in `hash.c`).
const CHUNK: usize = 1024;

/// Copies the INI settings the streams layer consults (the same subset
/// `php-ext-streams` keeps in sync).
fn sync_settings(vm: &Vm) {
    let get = |n: &str| vm.ini_string(n).unwrap_or_default();
    let include_path = get("include_path");
    let open_basedir = get("open_basedir");
    let allow_url_fopen = vm.ini_bool("allow_url_fopen");
    php_streams::state::update_settings(|s| {
        s.include_path = include_path;
        s.open_basedir = open_basedir;
        s.allow_url_fopen = allow_url_fopen;
        s.sapi_is_cli = true;
    });
}

/// Emits the diagnostics queued by the streams layer, prefixed with the
/// running function (and the `php_error_docref1/2` parameters, if any).
pub(crate) fn flush_diagnostics(vm: &mut Vm, func: &str) -> VmResult<()> {
    for d in diag::take() {
        let level = match d.severity {
            Severity::Warning => E_WARNING,
            Severity::Notice => E_NOTICE,
            Severity::Deprecated => E_DEPRECATED,
            Severity::Error => E_ERROR,
        };
        let params: Vec<String> = d
            .params
            .iter()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        vm.raise(
            level,
            &format!("{func}({}): {}", params.join(","), d.text()),
        )?;
    }
    Ok(())
}

/// Checks a path argument (`Z_PARAM_PATH`): no NUL bytes.
pub(crate) fn check_path(vm: &mut Vm, a: &Args, n: usize, path: &[u8]) -> VmResult<()> {
    if path.contains(&0) {
        return Err(a.value_error(vm, n, "must not contain any null bytes"));
    }
    Ok(())
}

/// Opens `path` read-only and feeds its contents to `sink` in 1 KiB chunks.
///
/// Returns `Ok(None)` when the file could not be opened (the warning has
/// been emitted), otherwise `Ok(Some(ok))` where `ok` is `false` after a
/// read error.
pub(crate) fn read_path(
    vm: &mut Vm,
    func: &str,
    path: &[u8],
    mut sink: impl FnMut(&[u8]),
) -> VmResult<Option<bool>> {
    if path.is_empty() {
        return Err(vm.throw("ValueError", "Path cannot be empty"));
    }
    sync_settings(vm);
    let mut opened = None;
    let stream = php_streams::open::open_wrapper(path, "rb", REPORT_ERRORS, &mut opened, None);
    flush_diagnostics(vm, func)?;
    let Some(mut stream) = stream else {
        return Ok(None);
    };
    let mut buf = vec![0u8; CHUNK];
    let ok = loop {
        match stream.read(&mut buf) {
            Ok(0) => break true,
            Ok(n) => sink(&buf[..n]),
            Err(_) => break false,
        }
    };
    stream.close();
    flush_diagnostics(vm, func)?;
    Ok(Some(ok))
}

/// Reads up to `length` bytes (`-1`: until EOF) from a stream resource with
/// `fread()`, feeding them to `sink`; returns the number of bytes read.
pub(crate) fn read_stream(
    vm: &mut Vm,
    stream: &Value,
    mut length: i64,
    mut sink: impl FnMut(&[u8]),
) -> VmResult<i64> {
    let fread = Value::string("fread");
    let mut did = 0i64;
    while length != 0 {
        let chunk = if length > 0 {
            length.min(CHUNK as i64)
        } else {
            CHUNK as i64
        };
        let got = vm.call_value(&fread, vec![stream.clone(), Value::Int(chunk)])?;
        let Value::Str(s) = got else {
            break;
        };
        if s.is_empty() {
            break;
        }
        sink(s.as_bytes());
        let n = i64::try_from(s.len()).unwrap_or(i64::MAX);
        if length > 0 {
            length -= n;
        }
        did += n;
    }
    Ok(did)
}
