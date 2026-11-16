// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Small pieces of per-request state: `assert_options()` (the `assert.*`
//! INI-backed table, `ext/standard/assert.c`), `php_ini_loaded_file()`,
//! `php_ini_scanned_files()` and `get_cfg_var()`.

use php_runtime::{RtResult, RuntimeContext, RuntimeError, Value, ZStr};

/// `ASSERT_*` constants.
pub const ASSERT_CONSTANTS: &[(&str, i64)] = &[
    ("ASSERT_ACTIVE", 1),
    ("ASSERT_CALLBACK", 2),
    ("ASSERT_BAIL", 3),
    ("ASSERT_WARNING", 4),
    ("ASSERT_EXCEPTION", 5),
];

/// `zend_ini_parse_bool()`: "true"/"yes"/"on" (any case) or a non-zero
/// integer prefix.
#[must_use]
pub fn ini_parse_bool(s: &[u8]) -> bool {
    let l = s.to_ascii_lowercase();
    if matches!(l.as_slice(), b"true" | b"yes" | b"on") {
        return true;
    }
    // atoi()
    let t: Vec<u8> = s
        .iter()
        .skip_while(|c| c.is_ascii_whitespace())
        .copied()
        .collect();
    let (neg, digits) = match t.first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, &t[..]),
    };
    let n: i64 = digits
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .fold(0i64, |a, &c| {
            a.wrapping_mul(10).wrapping_add(i64::from(c - b'0'))
        });
    (if neg { -n } else { n }) != 0
}

/// The `assert.*` settings (`ASSERTG`; one bool per INI directive).
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct AssertState {
    /// `assert.active`.
    pub active: bool,
    /// `assert.bail`.
    pub bail: bool,
    /// `assert.warning`.
    pub warning: bool,
    /// `assert.exception`.
    pub exception: bool,
    /// The callback set through `assert_options(ASSERT_CALLBACK, ...)`.
    pub callback: Option<Value>,
    /// `assert.callback` INI value.
    pub cb: Option<ZStr>,
}

impl Default for AssertState {
    fn default() -> Self {
        AssertState {
            active: true,
            bail: false,
            warning: true,
            exception: true,
            callback: None,
            cb: None,
        }
    }
}

/// An INI change requested by `assert_options()` (applied by the VM with
/// `ZEND_INI_STAGE_ASSERT_OPTIONS`, which also updates [`AssertState`]
/// through [`AssertState::on_ini_update`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IniChange {
    /// Directive name.
    pub name: &'static str,
    /// New value.
    pub value: ZStr,
}

impl AssertState {
    /// The `OnUpdate` handler of the `assert.*` directives.
    pub fn on_ini_update(&mut self, name: &str, value: &[u8]) {
        let b = ini_parse_bool(value);
        match name {
            "assert.active" => self.active = b,
            "assert.bail" => self.bail = b,
            "assert.warning" => self.warning = b,
            "assert.exception" => self.exception = b,
            "assert.callback" => self.cb = (!value.is_empty()).then(|| ZStr::new(value)),
            _ => {}
        }
    }

    /// `assert_options($option, $value)`: the old value and the INI change
    /// to apply (if a value was passed). The callback option is stored
    /// directly.
    pub fn assert_options(
        &mut self,
        what: i64,
        value: Option<&Value>,
        ctx: &mut (impl RuntimeContext + ?Sized),
    ) -> RtResult<(Value, Option<IniChange>)> {
        let (old, name) = match what {
            1 => (self.active, "assert.active"),
            3 => (self.bail, "assert.bail"),
            4 => (self.warning, "assert.warning"),
            5 => (self.exception, "assert.exception"),
            2 => {
                let old = match (&self.callback, &self.cb) {
                    (Some(v), _) => v.clone(),
                    (None, Some(s)) => Value::Str(s.clone()),
                    (None, None) => Value::Null,
                };
                if let Some(v) = value {
                    self.callback = (!v.is_null()).then(|| v.clone());
                }
                return Ok((old, None));
            }
            _ => {
                return Err(RuntimeError::ValueError(
                    "assert_options(): Argument #1 ($option) must be an ASSERT_* constant".into(),
                ))
            }
        };
        let change = match value {
            Some(v) => {
                let s = v.to_zstr(ctx)?;
                self.on_ini_update(name, s.as_bytes());
                Some(IniChange { name, value: s })
            }
            None => None,
        };
        Ok((Value::Int(i64::from(old)), change))
    }
}

/// Which configuration files were loaded (`php_ini_opened_path`,
/// `php_ini_scanned_files`).
#[derive(Debug, Clone, Default)]
pub struct IniFiles {
    /// The loaded `php.ini`.
    pub opened_path: Option<Vec<u8>>,
    /// Comma-separated additional `.ini` files (as PHP builds it: each
    /// file followed by `",\n"`, the last separator removed).
    pub scanned_files: Option<Vec<u8>>,
}

impl IniFiles {
    /// `php_ini_loaded_file()`.
    #[must_use]
    pub fn php_ini_loaded_file(&self) -> Value {
        self.opened_path
            .as_ref()
            .map_or(Value::Bool(false), |p| Value::Str(ZStr::new(p.as_slice())))
    }

    /// `php_ini_scanned_files()`.
    #[must_use]
    pub fn php_ini_scanned_files(&self) -> Value {
        self.scanned_files
            .as_ref()
            .map_or(Value::Bool(false), |p| Value::Str(ZStr::new(p.as_slice())))
    }
}

/// `get_cfg_var($option)` over the parsed configuration (`cfg_get_entry`):
/// strings and arrays are returned as they are, missing entries are
/// `false`.
#[must_use]
pub fn get_cfg_var(entry: Option<&Value>) -> Value {
    match entry {
        Some(v @ (Value::Str(_) | Value::Array(_))) => v.clone(),
        Some(other) => other.clone(),
        None => Value::Bool(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use php_runtime::CollectingContext;

    #[test]
    fn assert_options_table() {
        let mut ctx = CollectingContext::new();
        let mut st = AssertState::default();
        let (old, ch) = st
            .assert_options(1, Some(&Value::Int(0)), &mut ctx)
            .unwrap();
        assert_eq!(old.as_int(), Some(1));
        assert_eq!(ch.unwrap().value.as_bytes(), b"0");
        assert!(!st.active);
        let (old, ch) = st.assert_options(1, None, &mut ctx).unwrap();
        assert_eq!(old.as_int(), Some(0));
        assert!(ch.is_none());
        let (old, _) = st
            .assert_options(2, Some(&Value::from("cb")), &mut ctx)
            .unwrap();
        assert!(old.is_null());
        let (old, _) = st.assert_options(2, Some(&Value::Null), &mut ctx).unwrap();
        assert_eq!(old.as_str().unwrap().as_bytes(), b"cb");
        let e = st.assert_options(99, None, &mut ctx).unwrap_err();
        assert_eq!(
            e.message(),
            Some("assert_options(): Argument #1 ($option) must be an ASSERT_* constant")
        );
        assert!(
            ini_parse_bool(b"On")
                && ini_parse_bool(b" 2")
                && !ini_parse_bool(b"off")
                && !ini_parse_bool(b"0")
        );
    }

    #[test]
    fn ini_files() {
        let f = IniFiles::default();
        assert!(matches!(f.php_ini_loaded_file(), Value::Bool(false)));
        assert!(matches!(get_cfg_var(None), Value::Bool(false)));
    }
}
