// Derived from php-src ext/filter/filter.c. Similarity metric to original: 38%
//   Copyright © The PHP Group and Contributors.
//   Authors: Rasmus Lerdorf <rasmus@php.net>
//            Derick Rethans <derick@php.net>
//            Pierre-A. Joye <pierre@php.net>
//            Ilia Alshanetsky <iliaa@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! The filter engine of `filter.c`: `php_zval_filter`, the recursive
//! application over arrays, `php_filter_call`, `php_filter_array_handler`
//! and the PHP-level functions.

use crate::constants::{
    filter_id_exists, find_filter, FilterKind, FILTER_CALLBACK, FILTER_DEFAULT,
    FILTER_FLAG_EMAIL_UNICODE, FILTER_FLAG_NO_ENCODE_QUOTES, FILTER_FORCE_ARRAY,
    FILTER_NULL_ON_FAILURE, FILTER_REQUIRE_ARRAY, FILTER_REQUIRE_SCALAR, FILTER_THROW_ON_FAILURE,
    FILTER_UNSAFE_RAW, INPUT_COOKIE, INPUT_ENV, INPUT_GET, INPUT_POST, INPUT_SERVER,
};
use crate::host::FilterHost;
use crate::{email, sanitize, validate};
use php_runtime::{CastTarget, ErrorLevel, RtResult, RuntimeError, Value, ZArray, ZStr};

/// The PHP function being executed (for message prefixes and argument
/// numbers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterFunction {
    /// `filter_var()`.
    FilterVar,
    /// `filter_var_array()`.
    FilterVarArray,
    /// `filter_input()`.
    FilterInput,
    /// `filter_input_array()`.
    FilterInputArray,
    /// `filter_has_var()`.
    FilterHasVar,
}

impl FilterFunction {
    /// The function name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            FilterFunction::FilterVar => "filter_var",
            FilterFunction::FilterVarArray => "filter_var_array",
            FilterFunction::FilterInput => "filter_input",
            FilterFunction::FilterInputArray => "filter_input_array",
            FilterFunction::FilterHasVar => "filter_has_var",
        }
    }
}

/// The `array|int $options` argument of the filter functions: either the
/// flags or an array with `filter`/`flags`/`options` keys (for
/// `filter_var_array`/`filter_input_array`: a filter id or a definition
/// array).
#[derive(Debug, Clone)]
pub enum FilterArgs {
    /// An integer: flags (or the filter id for the `*_array` functions).
    Long(i64),
    /// An array of options.
    Array(ZArray),
}

impl Default for FilterArgs {
    fn default() -> Self {
        FilterArgs::Long(0)
    }
}

impl From<i64> for FilterArgs {
    fn from(v: i64) -> Self {
        FilterArgs::Long(v)
    }
}

impl From<ZArray> for FilterArgs {
    fn from(a: ZArray) -> Self {
        FilterArgs::Array(a)
    }
}

/// The input arrays of `filter_input()` & co. (the raw, unfiltered copies
/// php-src keeps in `IF_G(get_array)` etc.). `None` means the storage was
/// never initialized (no such input in this request), which makes
/// `filter_input()` return `null`/`false` and `filter_has_var()` `false`.
#[derive(Debug, Clone, Default)]
pub struct FilterInputs {
    /// `INPUT_POST`.
    pub post: Option<ZArray>,
    /// `INPUT_GET`.
    pub get: Option<ZArray>,
    /// `INPUT_COOKIE`.
    pub cookie: Option<ZArray>,
    /// `INPUT_ENV` (php-src falls back to `$_ENV` when no raw copy exists;
    /// the VM should do the same when filling this field).
    pub env: Option<ZArray>,
    /// `INPUT_SERVER`.
    pub server: Option<ZArray>,
}

impl FilterInputs {
    /// `php_filter_get_storage`: `Err` (a `ValueError`) for an unknown input
    /// type, `Ok(None)` for an uninitialized storage.
    fn storage(&self, func: FilterFunction, input_type: i64) -> RtResult<Option<&ZArray>> {
        Ok(match input_type {
            INPUT_GET => self.get.as_ref(),
            INPUT_POST => self.post.as_ref(),
            INPUT_COOKIE => self.cookie.as_ref(),
            INPUT_SERVER => self.server.as_ref(),
            INPUT_ENV => self.env.as_ref(),
            _ => {
                let arg = if func == FilterFunction::FilterHasVar {
                    "input_type"
                } else {
                    "type"
                };
                return Err(RuntimeError::ValueError(format!(
                    "{}(): Argument #1 (${arg}) must be an INPUT_* constant",
                    func.name()
                )));
            }
        })
    }
}

/// The filter engine for one call of a PHP-level filter function.
struct Engine<'h, H: FilterHost + ?Sized> {
    host: &'h mut H,
    func: FilterFunction,
    /// Addresses of the references whose arrays are being filtered
    /// (`Z_PROTECT_RECURSION`).
    guard: Vec<usize>,
    /// Nesting of `php_zval_filter_recursive` (for the stack limit).
    depth: usize,
}

fn sv(v: Vec<u8>) -> Value {
    Value::Str(ZStr::new(v))
}

fn validation_failed(flags: i64) -> Value {
    if flags & FILTER_NULL_ON_FAILURE != 0 {
        Value::Null
    } else {
        Value::Bool(false)
    }
}

/// The C string view of a PHP string (`%s` stops at the first NUL).
fn c_str(s: &[u8]) -> String {
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..end]).into_owned()
}

impl<H: FilterHost + ?Sized> Engine<'_, H> {
    fn fname(&self) -> &'static str {
        self.func.name()
    }

    fn warning(&mut self, message: &str) -> RtResult<()> {
        let text = format!("{}(): {message}", self.fname());
        self.host.diagnostic(ErrorLevel::Warning, &text)
    }

    /// Runs one filter implementation on a string. `Ok(None)` means
    /// `RETURN_VALIDATION_FAILED`.
    fn run_filter(
        &mut self,
        kind: FilterKind,
        s: ZStr,
        flags: i64,
        options: Option<&Value>,
    ) -> RtResult<Option<Value>> {
        let fname = self.fname();
        let opts = match options {
            Some(Value::Array(a)) => Some(a),
            _ => None,
        };
        let b = s.as_bytes();
        Ok(match kind {
            FilterKind::Int => validate::filter_int(self.host, b, flags, opts)?,
            FilterKind::Boolean => validate::filter_boolean(b),
            FilterKind::Float => validate::filter_float(self.host, b, flags, opts, fname)?,
            FilterKind::ValidateRegexp => validate::validate_regexp(self.host, &s, opts, fname)?,
            FilterKind::ValidateDomain => validate::validate_domain(&s, flags),
            FilterKind::ValidateUrl => validate::validate_url(self.host, &s, flags, opts, fname)?,
            FilterKind::ValidateEmail => {
                email::validate_email(b, flags & FILTER_FLAG_EMAIL_UNICODE != 0)
                    .then(|| Value::Str(s.clone()))
            }
            FilterKind::ValidateIp => validate::validate_ip(&s, flags),
            FilterKind::ValidateMac => validate::validate_mac(&s, opts, fname)?,
            FilterKind::String => Some(sanitize::filter_string(b, flags)),
            FilterKind::Encoded => Some(sv(sanitize::filter_encoded(b, flags))),
            FilterKind::SpecialChars => Some(sv(sanitize::filter_special_chars(b, flags))),
            FilterKind::FullSpecialChars => Some(sv(self
                .host
                .escape_html_entities(b, flags & FILTER_FLAG_NO_ENCODE_QUOTES == 0))),
            FilterKind::UnsafeRaw => Some(sanitize::filter_unsafe_raw(b, flags)),
            FilterKind::Email => Some(sv(sanitize::filter_email(b))),
            FilterKind::Url => Some(sv(sanitize::filter_url(b))),
            FilterKind::NumberInt => Some(sv(sanitize::filter_number_int(b))),
            FilterKind::NumberFloat => Some(sv(sanitize::filter_number_float(b, flags))),
            FilterKind::AddSlashes => Some(sv(sanitize::add_slashes(b))),
            FilterKind::Callback => {
                let callable = options.cloned().unwrap_or(Value::Null);
                if options.is_none() || !self.host.is_callable(&callable) {
                    return Err(RuntimeError::TypeError(format!(
                        "{fname}(): Option must be a valid callback"
                    )));
                }
                Some(self.host.call_callback(&callable, Value::Str(s))?)
            }
        })
    }

    /// `php_zval_filter`: filters one non-array value.
    fn zval_filter(
        &mut self,
        value: &Value,
        filter: i64,
        flags: i64,
        options: Option<&Value>,
    ) -> RtResult<Value> {
        let entry = find_filter(filter);
        let value = value.deref();
        let result = 'filtered: {
            let s = match &value {
                Value::Object(o) => {
                    if let Some(Value::Str(s)) = self.host.cast_object(o, CastTarget::String)? {
                        s
                    } else {
                        // #49274: objects without __toString fail nicely.
                        if flags & FILTER_THROW_ON_FAILURE != 0 {
                            let msg = format!(
                            "filter validation failed: object of type {} has no __toString() method",
                            o.class_name()
                        );
                            return Err(self.host.filter_failed_exception(msg));
                        }
                        break 'filtered validation_failed(flags);
                    }
                }
                other => other.to_zstr(self.host)?,
            };
            let copy = (flags & FILTER_THROW_ON_FAILURE != 0).then(|| s.clone());
            if let Some(v) = self.run_filter(entry.kind, s, flags, options)? {
                v
            } else {
                if let Some(copy) = copy {
                    let msg = format!(
                        "filter validation failed: filter {} not satisfied by '{}'",
                        entry.name,
                        c_str(copy.as_bytes())
                    );
                    return Err(self.host.filter_failed_exception(msg));
                }
                validation_failed(flags)
            }
        };
        // handle_default:
        if let Some(Value::Array(opts)) = options {
            let failed = if flags & FILTER_NULL_ON_FAILURE != 0 {
                result.is_null()
            } else {
                matches!(result, Value::Bool(false))
            };
            if failed {
                if let Some(default) = opts.get_str(b"default") {
                    return Ok(default.deref());
                }
            }
        }
        Ok(result)
    }

    /// `php_zval_filter_recursive` on an array.
    fn filter_array(
        &mut self,
        mut arr: ZArray,
        filter: i64,
        flags: i64,
        options: Option<&Value>,
    ) -> RtResult<ZArray> {
        if let Some(e) = self.host.stack_overflowed(self.depth) {
            return Err(e);
        }
        self.depth += 1;
        let r = self.filter_array_elements(&mut arr, filter, flags, options);
        self.depth -= 1;
        r.map(|()| arr)
    }

    /// The loop of [`Self::filter_array`] (`ZEND_HASH_FOREACH_VAL`).
    fn filter_array_elements(
        &mut self,
        arr: &mut ZArray,
        filter: i64,
        flags: i64,
        options: Option<&Value>,
    ) -> RtResult<()> {
        let table = arr.make_mut();
        for (_, slot) in table.iter_mut() {
            match slot {
                Value::Reference(r) => {
                    if self.guard.contains(&r.addr()) {
                        continue;
                    }
                    let r = r.clone();
                    let inner = r.get();
                    let new = if let Value::Array(a) = inner {
                        self.guard.push(r.addr());
                        let res = self.filter_array(a, filter, flags, options);
                        self.guard.pop();
                        Value::Array(res?)
                    } else {
                        self.zval_filter(&inner, filter, flags, options)?
                    };
                    r.set(new);
                }
                Value::Array(a) => {
                    let a = std::mem::take(a);
                    *slot = Value::Array(self.filter_array(a, filter, flags, options)?);
                }
                other => {
                    let v = std::mem::replace(other, Value::Null);
                    *other = self.zval_filter(&v, filter, flags, options)?;
                }
            }
        }
        Ok(())
    }

    /// `php_filter_call`. `filter` is `None` for the "array apply" mode
    /// (`-1` in php-src), where `args` gives the filter.
    fn filter_call(
        &mut self,
        filtered: &Value,
        filter: Option<i64>,
        args: &FilterArgs,
        mut flags: i64,
        options_arg_num: u32,
    ) -> RtResult<Value> {
        let mut filter = filter;
        let mut options: Option<Value> = None;
        match args {
            FilterArgs::Long(l) => {
                if filter.is_some() {
                    flags = *l;
                    if flags & (FILTER_REQUIRE_ARRAY | FILTER_FORCE_ARRAY) == 0 {
                        flags |= FILTER_REQUIRE_SCALAR;
                    }
                } else {
                    filter = Some(*l);
                }
            }
            FilterArgs::Array(ht) => {
                if let Some(f) = ht.get_str(b"filter") {
                    filter = Some(f.to_long(self.host)?);
                }
                if let Some(opt) = ht.get_str(b"options") {
                    let opt = opt.deref();
                    if filter == Some(FILTER_CALLBACK) {
                        options = Some(opt);
                        flags = 0;
                    } else if matches!(opt, Value::Array(_)) {
                        options = Some(opt);
                    }
                }
                if let Some(f) = ht.get_str(b"flags") {
                    flags = f.to_long(self.host)?;
                    if flags & (FILTER_REQUIRE_ARRAY | FILTER_FORCE_ARRAY) == 0 {
                        flags |= FILTER_REQUIRE_SCALAR;
                    }
                }
            }
        }
        let filter = filter.unwrap_or(-1);

        if flags & FILTER_NULL_ON_FAILURE != 0 && flags & FILTER_THROW_ON_FAILURE != 0 {
            return Err(RuntimeError::ValueError(format!(
                "{}(): Argument #{options_arg_num} ($options) cannot use both FILTER_NULL_ON_FAILURE and FILTER_THROW_ON_FAILURE",
                self.fname()
            )));
        }

        let options = options.as_ref();
        match filtered.deref() {
            Value::Array(a) => {
                if flags & FILTER_REQUIRE_SCALAR != 0 {
                    if flags & FILTER_THROW_ON_FAILURE != 0 {
                        return Err(self.host.filter_failed_exception(
                            "filter validation failed: not a scalar value (got an array)"
                                .to_string(),
                        ));
                    }
                    return Ok(validation_failed(flags));
                }
                Ok(Value::Array(self.filter_array(a, filter, flags, options)?))
            }
            other => {
                if flags & FILTER_REQUIRE_ARRAY != 0 {
                    if flags & FILTER_THROW_ON_FAILURE != 0 {
                        let msg = format!(
                            "filter validation failed: not an array (got {})",
                            other.value_name()
                        );
                        return Err(self.host.filter_failed_exception(msg));
                    }
                    return Ok(validation_failed(flags));
                }
                let v = self.zval_filter(&other, filter, flags, options)?;
                if flags & FILTER_FORCE_ARRAY != 0 {
                    Ok(Value::Array(ZArray::from_values([v])))
                } else {
                    Ok(v)
                }
            }
        }
    }

    /// `php_filter_array_handler`.
    fn array_handler(
        &mut self,
        input: &ZArray,
        op: &FilterArgs,
        add_empty: bool,
    ) -> RtResult<Value> {
        let op_ht = match op {
            FilterArgs::Long(l) => {
                return self.filter_call(
                    &Value::Array(input.clone()),
                    None,
                    &FilterArgs::Long(*l),
                    FILTER_REQUIRE_ARRAY,
                    2,
                );
            }
            FilterArgs::Array(ht) => ht,
        };
        let mut out = ZArray::new();
        for (key, def) in op_ht.iter() {
            let php_runtime::ArrayKey::Str(name) = key else {
                return Err(RuntimeError::TypeError(format!(
                    "{}(): Argument #2 ($options) must contain only string keys",
                    self.fname()
                )));
            };
            if name.is_empty() {
                return Err(RuntimeError::ValueError(format!(
                    "{}(): Argument #2 ($options) cannot contain empty keys",
                    self.fname()
                )));
            }
            match input.get_str(name.as_bytes()) {
                None => {
                    if add_empty {
                        out.make_mut().insert(key.clone(), Value::Null);
                    }
                }
                Some(v) => {
                    let nval = v.deref();
                    let def = def.deref();
                    let args = if let Value::Array(a) = &def {
                        FilterArgs::Array(a.clone())
                    } else {
                        let id = def.to_long(self.host)?;
                        if !filter_id_exists(id) {
                            self.warning(&format!("Unknown filter with ID {id}"))?;
                        }
                        FilterArgs::Long(id)
                    };
                    let res = self.filter_call(&nval, None, &args, FILTER_REQUIRE_SCALAR, 2)?;
                    out.make_mut().insert(key.clone(), res);
                }
            }
        }
        Ok(Value::Array(out))
    }
}

fn engine<H: FilterHost + ?Sized>(host: &mut H, func: FilterFunction) -> Engine<'_, H> {
    Engine {
        host,
        func,
        guard: Vec::new(),
        depth: 0,
    }
}

/// `filter_var(mixed $value, int $filter = FILTER_DEFAULT, array|int
/// $options = 0): mixed`.
///
/// An unknown `$filter` emits "`filter_var()`: Unknown filter with ID N" and
/// returns `false`.
pub fn filter_var<H: FilterHost + ?Sized>(
    host: &mut H,
    value: &Value,
    filter: i64,
    options: &FilterArgs,
) -> RtResult<Value> {
    let mut e = engine(host, FilterFunction::FilterVar);
    if !filter_id_exists(filter) {
        e.warning(&format!("Unknown filter with ID {filter}"))?;
        return Ok(Value::Bool(false));
    }
    e.filter_call(value, Some(filter), options, FILTER_REQUIRE_SCALAR, 3)
}

/// `filter_var_array(array $array, array|int $options = FILTER_DEFAULT,
/// bool $add_empty = true): array|false`.
pub fn filter_var_array<H: FilterHost + ?Sized>(
    host: &mut H,
    array: &ZArray,
    options: &FilterArgs,
    add_empty: bool,
) -> RtResult<Value> {
    let mut e = engine(host, FilterFunction::FilterVarArray);
    if let FilterArgs::Long(id) = options {
        if !filter_id_exists(*id) {
            e.warning(&format!("Unknown filter with ID {id}"))?;
            return Ok(Value::Bool(false));
        }
    }
    e.array_handler(array, options, add_empty)
}

/// `filter_input(int $type, string $var_name, int $filter = FILTER_DEFAULT,
/// array|int $options = 0): mixed`, reading from `inputs`.
pub fn filter_input<H: FilterHost + ?Sized>(
    host: &mut H,
    inputs: &FilterInputs,
    input_type: i64,
    var_name: &[u8],
    filter: i64,
    options: &FilterArgs,
) -> RtResult<Value> {
    let mut e = engine(host, FilterFunction::FilterInput);
    if !filter_id_exists(filter) {
        e.warning(&format!("Unknown filter with ID {filter}"))?;
        return Ok(Value::Bool(false));
    }
    let storage = inputs.storage(FilterFunction::FilterInput, input_type)?;
    let found = storage.and_then(|s| s.get_str(var_name));
    let Some(found) = found else {
        let flags = match options {
            FilterArgs::Long(l) => *l,
            FilterArgs::Array(ht) => match ht.get_str(b"flags") {
                Some(f) => f.to_long(e.host)?,
                None => 0,
            },
        };
        if flags & FILTER_NULL_ON_FAILURE != 0 && flags & FILTER_THROW_ON_FAILURE != 0 {
            return Err(RuntimeError::ValueError(
                "filter_input(): Argument #4 ($options) cannot use both FILTER_NULL_ON_FAILURE and FILTER_THROW_ON_FAILURE"
                    .to_string(),
            ));
        }
        if flags & FILTER_THROW_ON_FAILURE != 0 {
            let msg = format!("input value '{}' not found", c_str(var_name));
            return Err(e.host.filter_failed_exception(msg));
        }
        if let FilterArgs::Array(ht) = options {
            if let Some(Value::Array(opt)) = ht.get_str(b"options").map(Value::deref) {
                if let Some(def) = opt.get_str(b"default") {
                    return Ok(def.deref());
                }
            }
        }
        // FILTER_NULL_ON_FAILURE inverts the "missing" and "invalid" results.
        return Ok(if flags & FILTER_NULL_ON_FAILURE != 0 {
            Value::Bool(false)
        } else {
            Value::Null
        });
    };
    let value = found.deref();
    e.filter_call(&value, Some(filter), options, FILTER_REQUIRE_SCALAR, 4)
}

/// `filter_input_array(int $type, array|int $options = FILTER_DEFAULT,
/// bool $add_empty = true): array|false|null`.
pub fn filter_input_array<H: FilterHost + ?Sized>(
    host: &mut H,
    inputs: &FilterInputs,
    input_type: i64,
    options: &FilterArgs,
    add_empty: bool,
) -> RtResult<Value> {
    let mut e = engine(host, FilterFunction::FilterInputArray);
    if let FilterArgs::Long(id) = options {
        if !filter_id_exists(*id) {
            e.warning(&format!("Unknown filter with ID {id}"))?;
            return Ok(Value::Bool(false));
        }
    }
    let Some(storage) = inputs.storage(FilterFunction::FilterInputArray, input_type)? else {
        return Ok(Value::Null);
    };
    e.array_handler(storage, options, add_empty)
}

/// `filter_has_var(int $input_type, string $var_name): bool`.
pub fn filter_has_var(inputs: &FilterInputs, input_type: i64, var_name: &[u8]) -> RtResult<bool> {
    let storage = inputs.storage(FilterFunction::FilterHasVar, input_type)?;
    Ok(storage.is_some_and(|s| s.get_str(var_name).is_some()))
}

/// The `filter.default` ini setting resolved to a filter id
/// (`UpdateDefaultFilter`): a case-insensitive filter name; anything else
/// means `FILTER_DEFAULT`. The flag is `true` when the setting selects a
/// non-default filter, in which case the engine emits `E_DEPRECATED` "The
/// filter.default ini setting is deprecated".
#[must_use]
pub fn parse_default_filter_ini(value: &[u8]) -> (i64, bool) {
    let value = value.split(|&b| b == 0).next().unwrap_or_default();
    crate::constants::FILTER_LIST
        .iter()
        .find(|e| e.name.as_bytes().eq_ignore_ascii_case(value))
        .map_or((FILTER_DEFAULT, false), |e| (e.id, e.id != FILTER_DEFAULT))
}

/// The `filter.default_flags` ini setting (`OnUpdateFlags`): `atoi()` of
/// the value; unset means `FILTER_FLAG_NO_ENCODE_QUOTES`.
#[must_use]
pub fn parse_default_flags_ini(value: Option<&[u8]>) -> i64 {
    let Some(v) = value else {
        return crate::constants::FILTER_FLAG_NO_ENCODE_QUOTES;
    };
    let mut i = 0;
    while i < v.len() && (v[i] == b' ' || (0x09..=0x0d).contains(&v[i])) {
        i += 1;
    }
    let neg = v.get(i) == Some(&b'-');
    if matches!(v.get(i), Some(b'-' | b'+')) {
        i += 1;
    }
    let mut n: i32 = 0;
    while let Some(d) = v.get(i).filter(|d| d.is_ascii_digit()) {
        n = n.wrapping_mul(10).wrapping_add(i32::from(d - b'0'));
        i += 1;
    }
    i64::from(if neg { n.wrapping_neg() } else { n })
}

/// `php_sapi_filter`: the value registered in `$_GET`/`$_POST`/... for a raw
/// request variable under the `filter.default`/`filter.default_flags`
/// settings (the raw value is kept separately for `filter_input`). Also used
/// for `parse_str()` (`PARSE_STRING`), where the result replaces the value.
pub fn apply_default_filter<H: FilterHost + ?Sized>(
    host: &mut H,
    raw: &[u8],
    default_filter: i64,
    default_flags: i64,
) -> RtResult<Value> {
    if raw.is_empty() || default_filter == FILTER_UNSAFE_RAW {
        return Ok(Value::from(raw));
    }
    let mut e = engine(host, FilterFunction::FilterVar);
    e.zval_filter(&Value::from(raw), default_filter, default_flags, None)
}
