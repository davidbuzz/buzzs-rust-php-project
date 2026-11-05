//! PHP's INI system.
//!
//! * Parsing: a faithful port of the re2c scanner and Bison grammar in
//!   `Zend/zend_ini_scanner.l` / `Zend/zend_ini_parser.y` (sections, labels,
//!   quoted and raw strings, `${VAR:-fallback}` expansion, constants,
//!   `|`/`&`/`^`/`~`/`!` expressions, `a[]=`/`a[k]=` entries, comments,
//!   `INI_SCANNER_NORMAL`/`RAW`/`TYPED`) with byte-identical syntax error
//!   messages.  [`parse_ini_string`] builds the array that PHP's
//!   `parse_ini_string()` returns; [`parse_ini`] drives an arbitrary
//!   [`IniParserCallback`] (as `php.ini` loading does).
//! * [`registry`]: the INI directive registry (`ini_get`, `ini_set`,
//!   `ini_restore`, `ini_get_all`) with PHP's core directives.
//! * [`quantity`]: `zend_ini_parse_quantity()` and `zend_ini_parse_bool()`.

pub mod configuration;
mod parser;
pub mod quantity;
pub mod registry;
mod scanner;
mod tables;
mod value;

pub use quantity::{parse_bool, parse_quantity, parse_uquantity};
pub use registry::{
    core_directives, Access, IniDirective, IniEntryInfo, IniRegistry, IniTyped, ModifyHandler,
    Stage, CLI_OVERRIDES,
};
pub use value::{IniKey, IniMap, IniScalar, IniValue};

use crate::diag::{Diagnostic, ErrorLevel};
use crate::numeric::is_numeric_long_type_only;

/// Scanner mode (`INI_SCANNER_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScannerMode {
    /// `INI_SCANNER_NORMAL` (0): booleans become `"1"`/`""`, constants and
    /// expressions are evaluated.
    #[default]
    Normal,
    /// `INI_SCANNER_RAW` (1): values are taken verbatim.
    Raw,
    /// `INI_SCANNER_TYPED` (2): booleans, `null` and numbers get PHP types.
    Typed,
}

impl ScannerMode {
    /// Converts the userland constant value; `None` for invalid modes
    /// (PHP throws "must be one of `INI_SCANNER_NORMAL`, `INI_SCANNER_RAW`, or
    /// `INI_SCANNER_TYPED`").
    pub fn from_php(mode: i64) -> Option<Self> {
        match mode {
            0 => Some(Self::Normal),
            1 => Some(Self::Raw),
            2 => Some(Self::Typed),
            _ => None,
        }
    }
}

/// Environment the parser consults while evaluating values.
pub trait IniHooks {
    /// Value of the PHP constant `name` converted to a string, or `None` if
    /// it is not defined (the name is then used literally).
    fn constant(&mut self, _name: &[u8]) -> Option<Vec<u8>> {
        None
    }

    /// Value for `${name}`: PHP first looks at the loaded configuration
    /// (`zend_get_configuration_directive()`), then at the environment.
    /// The default looks only at the process environment.
    fn variable(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        env_var(name)
    }
}

/// Looks up an environment variable as bytes.
fn env_var(name: &[u8]) -> Option<Vec<u8>> {
    if name.is_empty() || name.contains(&0) || name.contains(&b'=') {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        std::env::var_os(std::ffi::OsStr::from_bytes(name)).map(OsStringExt::into_vec)
    }
    #[cfg(not(unix))]
    {
        let n = std::str::from_utf8(name).ok()?;
        std::env::var(n).ok().map(String::into_bytes)
    }
}

/// [`IniHooks`] with no constants and process-environment variables.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnvHooks;

impl IniHooks for EnvHooks {}

/// [`IniHooks`] backed by closures/maps, convenient for embedding.
pub struct FnHooks<C, V> {
    /// Constant lookup.
    pub constant: C,
    /// Variable lookup.
    pub variable: V,
}

impl<C, V> std::fmt::Debug for FnHooks<C, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FnHooks")
    }
}

impl<C, V> IniHooks for FnHooks<C, V>
where
    C: FnMut(&[u8]) -> Option<Vec<u8>>,
    V: FnMut(&[u8]) -> Option<Vec<u8>>,
{
    fn constant(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        (self.constant)(name)
    }
    fn variable(&mut self, name: &[u8]) -> Option<Vec<u8>> {
        (self.variable)(name)
    }
}

/// Receiver of parsed entries (`zend_ini_parser_cb_t`).
pub trait IniParserCallback {
    /// `ZEND_INI_PARSER_SECTION`: `[name]`.
    fn section(&mut self, name: &[u8]);
    /// `ZEND_INI_PARSER_ENTRY`: `key = value`, or a bare `key` (`None`).
    fn entry(&mut self, key: &[u8], value: Option<IniScalar>);
    /// `ZEND_INI_PARSER_POP_ENTRY`: `key[offset] = value` (`offset` is empty
    /// for `key[] = value`).
    fn pop_entry(&mut self, key: &[u8], value: IniScalar, offset: &[u8]);
}

/// Parses INI `input` and feeds `callback`.
///
/// On a syntax error returns the warning PHP raises:
/// `"syntax error, unexpected …  in <filename> on line N\n"` (note the
/// trailing newline that PHP includes).  Entries before the error have
/// already been delivered.
pub fn parse_ini(
    input: &[u8],
    filename: &str,
    mode: ScannerMode,
    hooks: &mut dyn IniHooks,
    callback: &mut dyn IniParserCallback,
) -> Result<(), Diagnostic> {
    let mut p = parser::Parser::new(input, mode, hooks);
    p.parse(callback).map_err(|e| {
        Diagnostic::new(
            ErrorLevel::WARNING,
            format!("{} in {filename} on line {}\n", e.message, e.lineno),
        )
    })
}

/// Builds the array returned by `parse_ini_string()`/`parse_ini_file()`
/// (`php_simple_ini_parser_cb` / `php_ini_parser_cb_with_sections`).
#[derive(Debug, Default)]
pub struct ArrayBuilder {
    /// The result.
    pub result: IniMap,
    process_sections: bool,
    active_section: Option<IniKey>,
}

impl ArrayBuilder {
    /// A builder; `process_sections` nests entries under their section.
    pub fn new(process_sections: bool) -> Self {
        Self {
            result: IniMap::new(),
            process_sections,
            active_section: None,
        }
    }

    /// Runs `f` on the active section (or the top-level array).
    fn with_target(&mut self, f: impl FnOnce(&mut IniMap)) {
        if let Some(k) = &self.active_section {
            if let Some(IniValue::Array(a)) = self.result.get_mut(k) {
                f(a);
                return;
            }
        }
        f(&mut self.result);
    }
}

/// `php_simple_ini_parser_cb()` applied to `arr`.
fn simple_entry(arr: &mut IniMap, key: &[u8], value: IniScalar) {
    arr.insert(IniKey::symtable(key), IniValue::Scalar(value));
}

fn simple_pop_entry(arr: &mut IniMap, key: &[u8], value: IniScalar, offset: &[u8]) {
    let k = if !(key.len() > 1 && key[0] == b'0') && is_numeric_long_type_only(key) {
        // ZEND_STRTOUL(key, NULL, 0) reinterpreted as a signed index.
        IniKey::Int(strtoul_as_index(key))
    } else {
        IniKey::Str(key.to_vec())
    };
    if !matches!(arr.get(&k), Some(IniValue::Array(_))) {
        arr.insert(k.clone(), IniValue::Array(IniMap::new()));
    }
    if let Some(IniValue::Array(inner)) = arr.get_mut(&k) {
        if offset.is_empty() {
            inner.push(IniValue::Scalar(value));
        } else {
            inner.insert(IniKey::symtable(offset), IniValue::Scalar(value));
        }
    }
}

/// `strtoul()` of a decimal numeric string, reinterpreted as a signed
/// `zend_long` index (negative inputs wrap, overflow gives `ULONG_MAX`).
fn strtoul_as_index(key: &[u8]) -> i64 {
    let start = key.iter().take_while(|c| c.is_ascii_whitespace()).count();
    let rest = &key[start..];
    let neg = rest.first() == Some(&b'-');
    let rest = rest
        .strip_prefix(b"-")
        .or_else(|| rest.strip_prefix(b"+"))
        .unwrap_or(rest);
    let mag = rest
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .try_fold(0u64, |acc, &d| {
            acc.checked_mul(10)?.checked_add(u64::from(d - b'0'))
        });
    let v = match mag {
        None => u64::MAX,
        Some(m) if neg => m.wrapping_neg(),
        Some(m) => m,
    };
    i64::from_ne_bytes(v.to_ne_bytes())
}

impl IniParserCallback for ArrayBuilder {
    fn section(&mut self, name: &[u8]) {
        if self.process_sections {
            let k = IniKey::symtable(name);
            self.result
                .insert(k.clone(), IniValue::Array(IniMap::new()));
            self.active_section = Some(k);
        }
    }

    fn entry(&mut self, key: &[u8], value: Option<IniScalar>) {
        if let Some(v) = value {
            self.with_target(|arr| simple_entry(arr, key, v));
        }
    }

    fn pop_entry(&mut self, key: &[u8], value: IniScalar, offset: &[u8]) {
        self.with_target(|arr| simple_pop_entry(arr, key, value, offset));
    }
}

/// `parse_ini_string($ini, $process_sections, $scanner_mode)`.
///
/// The input is cut at the first NUL byte, as PHP scans it with
/// `strlen()`.  Returns the warning on a syntax error (PHP then returns
/// `false`).
pub fn parse_ini_string(
    input: &[u8],
    process_sections: bool,
    mode: ScannerMode,
    hooks: &mut dyn IniHooks,
) -> Result<IniMap, Diagnostic> {
    let input = input
        .iter()
        .position(|&c| c == 0)
        .map_or(input, |p| &input[..p]);
    parse_ini_contents(input, "Unknown", process_sections, mode, hooks)
}

/// `parse_ini_file()` given the file's contents and its name (used in
/// error messages).  Unlike [`parse_ini_string`] NUL bytes are kept.
pub fn parse_ini_contents(
    input: &[u8],
    filename: &str,
    process_sections: bool,
    mode: ScannerMode,
    hooks: &mut dyn IniHooks,
) -> Result<IniMap, Diagnostic> {
    let mut b = ArrayBuilder::new(process_sections);
    parse_ini(input, filename, mode, hooks, &mut b)?;
    Ok(b.result)
}

#[cfg(test)]
mod tests;
