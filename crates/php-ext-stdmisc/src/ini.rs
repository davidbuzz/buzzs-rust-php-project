//! Configuration functions of `basic_functions.c` that no other binding
//! provides: `ini_get_all`, `parse_ini_string`, `parse_ini_file`, and
//! `get_included_files` (`zend_builtin_functions.c`).

use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};
use php_support::ini::{self, IniHooks, IniKey, IniMap, IniScalar, IniValue, ScannerMode};
use php_support::{Diagnostic, ErrorLevel};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// Emits a `php-support` diagnostic through the VM.
fn emit(vm: &mut Vm, d: &Diagnostic) -> VmResult<()> {
    let level = i64::from(d.level.0);
    if !d.docref {
        return vm.raise(level, &d.message);
    }
    match d.level {
        ErrorLevel::WARNING => vm.php_warning(&d.message),
        ErrorLevel::NOTICE => vm.php_notice(&d.message),
        ErrorLevel::DEPRECATED => vm.php_deprecated(&d.message),
        _ => vm.raise(level, &d.message),
    }
}

fn opt_str(v: Option<Vec<u8>>) -> Value {
    v.map_or(Value::Null, |v| Value::Str(ZStr::from(v)))
}

fn ini_get_all(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let ext = a.opt(0).map(|_| a.str(0).to_string_lossy());
    let details = !a.has(1) || a.bool(1);
    // Every loaded extension is a module for `ini_get_all($extension)`,
    // even without INI entries.
    let names: Vec<String> = php_vm::LOADED_EXTENSIONS
        .iter()
        .map(|s| (*s).to_string())
        .chain(vm.loaded_extensions.iter().cloned())
        .collect();
    for n in names {
        vm.ini.register_module(&n);
    }
    let rows = match vm.ini.ini_get_all(ext.as_deref()) {
        Ok(r) => r,
        Err(d) => {
            emit(vm, &d)?;
            return Ok(Value::Bool(false));
        }
    };
    let mut t = HashTable::new();
    for r in rows {
        let key = ArrayKey::from_bytes(r.name.as_bytes());
        if details {
            let entry = ZArray::from_pairs([
                ("global_value", opt_str(r.global_value)),
                ("local_value", opt_str(r.local_value)),
                ("builtin_default_value", opt_str(r.builtin_default_value)),
                ("access", Value::Int(i64::from(r.access))),
            ]);
            t.insert(key, Value::Array(entry));
        } else {
            t.insert(key, opt_str(r.local_value));
        }
    }
    Ok(Value::Array(ZArray::from_table(t)))
}

/// Constants and `${var}` lookups for the INI parser.
struct VmHooks<'a> {
    vm: &'a Vm,
}

impl IniHooks for VmHooks<'_> {
    fn constant(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        let v = self.vm.constants.get(&ZStr::from(name))?;
        Some(match v {
            Value::Str(s) => s.as_bytes().to_vec(),
            Value::Int(i) => i.to_string().into_bytes(),
            Value::Bool(true) => b"1".to_vec(),
            Value::Bool(false) | Value::Null => Vec::new(),
            Value::Float(f) => {
                php_runtime::float::double_to_string(*f, self.vm.precision).into_bytes()
            }
            _ => return None,
        })
    }

    fn variable(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        let n = String::from_utf8_lossy(name);
        if let Some(v) = self.vm.ini.config_value(&n) {
            return Some(v.to_vec());
        }
        if name.is_empty() || name.contains(&0) || name.contains(&b'=') {
            return None;
        }
        std::env::var_os(std::ffi::OsStr::from_bytes(name)).map(OsStringExt::into_vec)
    }
}

fn scalar(s: &IniScalar) -> Value {
    match s {
        IniScalar::Str(b) => Value::Str(ZStr::from(b.as_slice())),
        IniScalar::Long(i) => Value::Int(*i),
        IniScalar::Double(f) => Value::Float(*f),
        IniScalar::Bool(b) => Value::Bool(*b),
        IniScalar::Null => Value::Null,
    }
}

/// Converts the parser's ordered map to a PHP array.
fn to_array(m: &IniMap) -> ZArray {
    let mut t = HashTable::new();
    for (k, v) in m.iter() {
        let key = match k {
            IniKey::Int(i) => ArrayKey::Int(*i),
            IniKey::Str(s) => ArrayKey::Str(ZStr::from(s.as_slice())),
        };
        let val = match v {
            IniValue::Scalar(s) => scalar(s),
            IniValue::Array(inner) => Value::Array(to_array(inner)),
        };
        t.insert(key, val);
    }
    ZArray::from_table(t)
}

/// The `$scanner_mode` argument (argument #3).
fn scanner_mode(vm: &mut Vm, a: &Args) -> VmResult<ScannerMode> {
    let mode = if a.has(2) { a.int(2) } else { 0 };
    ScannerMode::from_php(mode).ok_or_else(|| {
        a.value_error(
            vm,
            3,
            "must be one of INI_SCANNER_NORMAL, INI_SCANNER_RAW, or INI_SCANNER_TYPED",
        )
    })
}

fn parse_result(vm: &mut Vm, r: Result<IniMap, Diagnostic>) -> VmResult<Value> {
    match r {
        Ok(m) => Ok(Value::Array(to_array(&m))),
        Err(d) => {
            emit(vm, &d)?;
            Ok(Value::Bool(false))
        }
    }
}

fn parse_ini_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    let sections = a.has(1) && a.bool(1);
    let mode = scanner_mode(vm, a)?;
    let r = ini::parse_ini_string(s.as_bytes(), sections, mode, &mut VmHooks { vm });
    parse_result(vm, r)
}

fn parse_ini_file(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let path = a.str(0);
    if path.is_empty() {
        return Err(a.value_error(vm, 1, "must not be empty"));
    }
    if path.as_bytes().contains(&0) {
        return Err(a.value_error(vm, 1, "must not contain any null bytes"));
    }
    let sections = a.has(1) && a.bool(1);
    let mode = scanner_mode(vm, a)?;
    let p = std::ffi::OsStr::from_bytes(path.as_bytes());
    let contents = match std::fs::read(p) {
        Ok(c) => c,
        Err(e) => {
            let reason = match e.kind() {
                std::io::ErrorKind::PermissionDenied => "Permission denied",
                _ => "No such file or directory",
            };
            vm.php_warning(&format!("Failed to open stream: {reason}"))?;
            return Ok(Value::Bool(false));
        }
    };
    let name = path.to_string_lossy();
    let r = ini::parse_ini_contents(&contents, &name, sections, mode, &mut VmHooks { vm });
    parse_result(vm, r)
}

#[allow(clippy::unnecessary_wraps)] // native handler signature
fn get_included_files(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(vm.included.len() + 1);
    let main = &vm.script_path;
    let is_file = !main.is_empty()
        && main.as_bytes() != b"Standard input code"
        && main.as_bytes() != b"Command line code";
    if is_file && !vm.included.iter().any(|f| f == main) {
        let real = std::fs::canonicalize(std::ffi::OsStr::new(&main.to_string_lossy()))
            .map_or_else(
                |_| main.clone(),
                |p| ZStr::from(p.to_string_lossy().as_ref()),
            );
        out.push(Value::Str(real));
    }
    out.extend(vm.included.iter().cloned().map(Value::Str));
    Ok(Value::Array(ZArray::from_values(out)))
}

/// Registers the functions and constants.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.constant("INI_SCANNER_NORMAL", Value::Int(0))
        .constant("INI_SCANNER_RAW", Value::Int(1))
        .constant("INI_SCANNER_TYPED", Value::Int(2));
    m.function(
        "ini_get_all(?string $extension = null, bool $details = true): array|false",
        ini_get_all,
    )
    .function(
        "parse_ini_string(string $ini_string, bool $process_sections = false, int $scanner_mode = INI_SCANNER_NORMAL): array|false",
        parse_ini_string,
    )
    .function(
        "parse_ini_file(string $filename, bool $process_sections = false, int $scanner_mode = INI_SCANNER_NORMAL): array|false",
        parse_ini_file,
    )
    .function("get_included_files(): array", get_included_files)
    .function("get_required_files(): array", get_included_files);
}
