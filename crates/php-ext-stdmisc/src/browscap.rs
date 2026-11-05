//! `get_browser()` (`ext/standard/browscap.c`): loads the `browscap` INI
//! file once and matches user agents with [`php_stdmisc::browscap`].

use php_runtime::{HashTable, Value, ZArray, ZStr};
use php_stdmisc::browscap::{Browscap, LoadDiagnostic};
use php_support::ini::{parse_ini, EnvHooks, IniParserCallback, IniScalar, ScannerMode};
use php_vm::ext::{Args, ModuleBuilder, VmResult};
use php_vm::Vm;
use std::cell::RefCell;
use std::rc::Rc;

/// `E_CORE_ERROR`.
const E_CORE_ERROR: i64 = 16;
/// `E_CORE_WARNING`.
const E_CORE_WARNING: i64 = 32;

/// The `browscap` INI file name and its parsed contents (`None` when it
/// could not be read).
type LoadedBrowscap = (Vec<u8>, Option<Rc<Browscap>>);

thread_local! {
    /// The data read from the `browscap` file (`global_bdata`), keyed by
    /// the file name; `None` when it could not be read.
    static DATA: RefCell<Option<LoadedBrowscap>> = const { RefCell::new(None) };
}

/// `php_browscap_parser_cb` over the INI parser callbacks.
struct Loader(Browscap);

impl IniParserCallback for Loader {
    fn section(&mut self, name: &[u8]) {
        self.0.section(name);
    }

    fn entry(&mut self, key: &[u8], value: Option<IniScalar>) {
        if let Some(v) = value {
            self.0.entry(key, &v.to_bytes());
        }
    }

    // `ZEND_INI_PARSER_POP_ENTRY` is ignored.
    fn pop_entry(&mut self, _key: &[u8], _value: IniScalar, _offset: &[u8]) {}
}

/// `browscap_read_file` (`ZEND_INI_SCANNER_RAW`); `None` when the file
/// cannot be opened.
fn read_file(vm: &mut Vm, file: &[u8]) -> VmResult<Option<Rc<Browscap>>> {
    let name = String::from_utf8_lossy(file).into_owned();
    let Ok(data) = std::fs::read(&name) else {
        vm.raise(
            E_CORE_WARNING,
            &format!("Cannot open \"{name}\" for reading"),
        )?;
        return Ok(None);
    };
    let mut loader = Loader(Browscap::new());
    let _ = parse_ini(&data, &name, ScannerMode::Raw, &mut EnvHooks, &mut loader);
    let mut table = loader.0;
    for d in std::mem::take(&mut table.diagnostics) {
        match d {
            LoadDiagnostic::LongPattern(n) => vm.php_warning(&format!(
                "Skipping excessively long pattern of length {n}"
            ))?,
            LoadDiagnostic::SelfParent(section) => vm.raise(
                E_CORE_ERROR,
                &format!(
                    "Invalid browscap ini file: 'Parent' value cannot be same as the section name: {} (in file {name})",
                    String::from_utf8_lossy(&section)
                ),
            )?,
        }
    }
    Ok(Some(Rc::new(table)))
}

/// The loaded table for `file`, reading it on first use.
fn load_table(vm: &mut Vm, file: &[u8]) -> VmResult<Option<Rc<Browscap>>> {
    let cached = DATA.with(|d| {
        d.borrow()
            .as_ref()
            .filter(|(f, _)| f == file)
            .map(|(_, t)| t.clone())
    });
    if let Some(t) = cached {
        return Ok(t);
    }
    let t = read_file(vm, file)?;
    DATA.with(|d| *d.borrow_mut() = Some((file.to_vec(), t.clone())));
    Ok(t)
}

/// `$_SERVER['HTTP_USER_AGENT']`.
fn server_user_agent(vm: &Vm) -> Option<Value> {
    let mut g = vm.globals.borrow_mut();
    let i = g.find(b"_SERVER")?;
    match g.slots.get(i as usize)?.as_ref()?.deref() {
        Value::Array(a) => a.get_str(b"HTTP_USER_AGENT").map(Value::deref),
        _ => None,
    }
}

/// `get_browser(?string $user_agent = null, bool $return_array = false)`.
#[allow(clippy::single_match_else)] // php_get_browser's two-level fallback
fn get_browser(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let agent = a.get(0).filter(|v| !v.is_null());
    let return_array = a.get(1).is_some() && a.bool(1);
    let file = vm.ini_string("browscap").unwrap_or_default();
    let table = if file.is_empty() {
        None
    } else {
        load_table(vm, &file)?
    };
    let Some(table) = table else {
        vm.php_warning("browscap ini directive not set")?;
        return Ok(Value::Bool(false));
    };
    let agent = match agent {
        Some(v) => vm.value_to_string(&v)?,
        None => match server_user_agent(vm) {
            Some(v) => vm.value_to_string(&v)?,
            None => {
                vm.php_warning(
                    "HTTP_USER_AGENT variable is not set, cannot determine user agent name",
                )?;
                return Ok(Value::Bool(false));
            }
        },
    };
    let Some(caps) = table.lookup(agent.as_bytes()) else {
        return Ok(Value::Bool(false));
    };
    if return_array {
        let mut t = HashTable::new();
        for (k, v) in caps {
            t.insert_str_raw(ZStr::from(k), Value::Str(ZStr::from(v)));
        }
        return Ok(Value::Array(ZArray::from_table(t)));
    }
    let Some(class) = vm.find_class(b"stdClass") else {
        return Err(vm.throw("Error", "Class \"stdClass\" not found"));
    };
    let o = vm.new_object(&class)?;
    for (k, v) in caps {
        vm.set_property(&o, &String::from_utf8_lossy(&k), Value::Str(ZStr::from(v)))?;
    }
    Ok(Value::Object(o))
}

/// Registers `get_browser()`.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function(
        "get_browser(?string $user_agent = null, bool $return_array = false): object|array|false",
        get_browser,
    );
}
