// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `preg_*` functions (the `PHP_FUNCTION`s of `ext/pcre/php_pcre.c`).

use php_pcre::{limit_from_php, MatchFailure, PregError, Regex, Replacement};
use php_runtime::{ArrayKey, HashTable, Value, ZArray, ZStr};
use php_vm::ext::{Args, ModuleBuilder, Throw, VmResult};
use php_vm::vm::PendingCall;
use php_vm::Vm;

use crate::convert::{to_zarray, unnamed_subpats};
use crate::memory;
use crate::state;

/// Registers the functions of the extension.
pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    m.function_static(
        "preg_match(string $pattern, string $subject, &$matches = null, int $flags = 0, int $offset = 0): int|false",
        preg_match,
    )
    .function_static(
        "preg_match_all(string $pattern, string $subject, &$matches = null, int $flags = 0, int $offset = 0): int|false",
        preg_match_all,
    )
    .function_static(
        "preg_replace(string|array $pattern, string|array $replacement, string|array $subject, int $limit = -1, &$count = null): string|array|null",
        preg_replace,
    )
    .function_static(
        "preg_filter(string|array $pattern, string|array $replacement, string|array $subject, int $limit = -1, &$count = null): string|array|null",
        preg_filter,
    )
    .function_static(
        "preg_replace_callback(string|array $pattern, callable $callback, string|array $subject, int $limit = -1, &$count = null, int $flags = 0): string|array|null",
        preg_replace_callback,
    )
    .function_static(
        "preg_replace_callback_array(array $pattern, string|array $subject, int $limit = -1, &$count = null, int $flags = 0): string|array|null",
        preg_replace_callback_array,
    )
    .function_static(
        "preg_split(string $pattern, string $subject, int $limit = -1, int $flags = 0): array|false",
        preg_split,
    )
    .function_static(
        "preg_quote(string $str, ?string $delimiter = null): string",
        preg_quote,
    )
    .function_static(
        "preg_grep(string $pattern, array $array, int $flags = 0): array|false",
        preg_grep,
    )
    .function_static("preg_last_error(): int", preg_last_error)
    .function_static("preg_last_error_msg(): string", preg_last_error_msg);
}

/// The configuration the last [`state::compile`] synchronised.
fn config() -> php_pcre::MatchConfig {
    state::with(|p| p.config())
}

// ---------------------------------------------------------------------------
// preg_match / preg_match_all
// ---------------------------------------------------------------------------

fn preg_match(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    do_match(vm, a, false)
}

fn preg_match_all(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    do_match(vm, a, true)
}

/// `php_do_pcre_match()`.
///
/// Matching first records only capture offsets; the size of the PHP array
/// they imply is reserved against `memory_limit` before any capture is
/// copied, so an oversized result raises PHP's "Allowed memory size"
/// fatal instead of being built.
fn do_match(vm: &mut Vm, a: &Args, global: bool) -> VmResult<Value> {
    let pattern = a.str(0);
    let subject = a.str(1);
    let capture = a.has(2);
    let flags = a.int(3);
    let offset = a.int(4);
    let Some(re) = state::compile(vm, &pattern)? else {
        return Ok(Value::Bool(false));
    };
    let cfg = state::config_for(&subject);
    if global && capture {
        memory::precheck_match_all(vm, &re, &cfg, subject.as_bytes())?;
    }
    let spans = if global {
        php_pcre::match_all_spans(&re, &cfg, subject.as_bytes(), capture, flags, offset)
    } else {
        php_pcre::match_one_spans(&re, &cfg, subject.as_bytes(), capture, flags, offset)
    };
    if capture {
        memory::check(vm, memory::spans_size(&spans, &re, global, flags))?;
    }
    // `$matches` is `None` when PHP leaves the variable untouched (only for
    // an invalid offset). The common single match without named groups is
    // built straight from the offsets; everything else goes through
    // `php-pcre`'s general array model.
    let (result, last_error, warning, valid_utf8, matches) =
        if capture && !global && re.name_count() == 0 {
            let result = spans.result();
            let matches = (result != Err(MatchFailure::InvalidOffset)).then(|| {
                spans.matches().next().map_or_else(
                    || ZArray::from(HashTable::new()),
                    |m| unnamed_subpats(m, re.capture_count() + 1, flags, re.is_utf()),
                )
            });
            (
                result,
                spans.last_error(),
                spans.warning(),
                spans.subject_valid_utf8(),
                matches,
            )
        } else {
            let out = spans.into_outcome();
            let matches = out
                .matches
                .filter(|_| capture)
                .map(|m| to_zarray(m, re.is_utf()));
            (
                out.result,
                out.last_error,
                out.warning,
                out.subject_valid_utf8,
                matches,
            )
        };
    if valid_utf8 {
        state::remember_valid_utf8(&subject);
    }
    if let Some(e) = last_error {
        state::set_last_error(e);
    }
    if let Some(m) = matches {
        a.set_ref(2, Value::Array(m));
    }
    if let Some(w) = warning {
        vm.php_warning(w)?;
    }
    match result {
        Ok(n) => Ok(Value::Int(i64::try_from(n).unwrap_or(i64::MAX))),
        Err(MatchFailure::InvalidFlags) => Err(a.value_error(vm, 4, "must be a PREG_* constant")),
        Err(MatchFailure::InvalidOffset) => {
            Err(a.value_error(vm, 5, &format!("must be greater than {}", i64::MIN)))
        }
        Err(MatchFailure::Failed) => Ok(Value::Bool(false)),
    }
}

// ---------------------------------------------------------------------------
// preg_replace / preg_filter
// ---------------------------------------------------------------------------

fn preg_replace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    replace_common(vm, a, false)
}

fn preg_filter(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    replace_common(vm, a, true)
}

/// `_preg_replace_common()`.
fn replace_common(vm: &mut Vm, a: &Args, is_filter: bool) -> VmResult<Value> {
    let regex = a.at(0);
    let replacement = a.at(1);
    let subject = a.at(2);
    let limit = limit_from_php(a.int(3));
    if matches!(replacement, Value::Array(_)) && !matches!(regex, Value::Array(_)) {
        return Err(a.type_error(
            vm,
            1,
            "must be of type array when argument #2 ($replacement) is an array, string given",
        ));
    }
    let mut count = 0usize;
    let result = if let Value::Array(subjects) = &subject {
        let mut out = HashTable::with_capacity(subjects.len());
        let mut pending = Pending::default();
        for (key, entry) in entries(subjects) {
            let old = count;
            let Some(s) = pending.absorb(vm.value_to_string(&entry))? else {
                continue;
            };
            if pending.is_set() {
                continue;
            }
            let r = replace_in_subject(vm, &regex, &replacement, &s, limit, &mut count);
            if let Some(Some(r)) = pending.absorb(r)? {
                if !is_filter || count > old {
                    out.insert(key, Value::Str(exact(r)));
                }
            }
        }
        pending.finish(Value::Array(ZArray::from(out)))?
    } else {
        let s = vm.value_to_string(&subject)?;
        match replace_in_subject(vm, &regex, &replacement, &s, limit, &mut count)? {
            Some(r) if !is_filter || count > 0 => Value::Str(exact(r)),
            _ => Value::Null,
        }
    };
    if a.has(4) {
        a.set_ref(4, Value::Int(count_value(count)));
    }
    Ok(result)
}

/// An exception raised while looping over an array subject.
///
/// PHP keeps iterating with the exception pending: later elements are
/// still converted to strings (emitting their conversion warnings) but no
/// more matching or callback calls happen. Other unwinds (`exit`, fatal
/// errors) propagate immediately.
#[derive(Default)]
struct Pending(Option<Throw>);

impl Pending {
    /// Records an exception from `r`; `Ok(None)` when `r` threw one.
    fn absorb<T>(&mut self, r: VmResult<T>) -> VmResult<Option<T>> {
        match r {
            Ok(v) => Ok(Some(v)),
            Err(Throw::Exception(e)) => {
                if self.0.is_none() {
                    self.0 = Some(Throw::Exception(e));
                }
                Ok(None)
            }
            Err(t) => Err(t),
        }
    }

    /// Whether an exception is pending.
    fn is_set(&self) -> bool {
        self.0.is_some()
    }

    /// Rethrows the pending exception, or returns `v`.
    fn finish<T>(self, v: T) -> VmResult<T> {
        match self.0 {
            Some(t) => Err(t),
            None => Ok(v),
        }
    }
}

/// The `(key, value)` pairs of an array, copied out so the VM can be
/// re-entered while iterating.
fn entries(arr: &ZArray) -> Vec<(ArrayKey, Value)> {
    arr.iter().map(|(k, v)| (k.clone(), v.deref())).collect()
}

fn count_value(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

/// `php_replace_in_subject()`: `Ok(None)` is PHP's `NULL` result.
fn replace_in_subject(
    vm: &mut Vm,
    regex: &Value,
    replacement: &Value,
    subject_str: &ZStr,
    limit: usize,
    count: &mut usize,
) -> VmResult<Option<Vec<u8>>> {
    let subject = subject_str.as_bytes();
    let Value::Array(patterns) = regex else {
        let pattern = vm.value_to_string(regex)?;
        let rep = vm.value_to_string(replacement)?;
        return replace_one(
            vm,
            &pattern,
            rep.as_bytes(),
            subject,
            state::known_valid_utf8(subject_str),
            limit,
            count,
        );
    };
    // php_pcre_replace_array()
    let replacements = match replacement {
        Value::Array(r) => Some(entries(r)),
        _ => None,
    };
    let single = match replacement {
        Value::Array(_) => None,
        other => Some(vm.value_to_string(other)?),
    };
    let mut next_replacement = replacements.map(Vec::into_iter);
    let mut current = subject.to_vec();
    for (_, entry) in entries(patterns) {
        let pattern = vm.value_to_string(&entry)?;
        let rep = match (&mut next_replacement, &single) {
            (Some(it), _) => match it.next() {
                Some((_, v)) => vm.value_to_string(&v)?,
                None => ZStr::from(""),
            },
            (None, Some(s)) => s.clone(),
            (None, None) => ZStr::from(""),
        };
        match replace_one(vm, &pattern, rep.as_bytes(), &current, false, limit, count)? {
            Some(r) => current = r,
            None => return Ok(None),
        }
    }
    Ok(Some(current))
}

/// `php_pcre_replace()` for one pattern and one replacement string.
fn replace_one(
    vm: &mut Vm,
    pattern: &ZStr,
    replacement: &[u8],
    subject: &[u8],
    valid_utf8: bool,
    limit: usize,
    count: &mut usize,
) -> VmResult<Option<Vec<u8>>> {
    let Some(re) = state::compile(vm, pattern)? else {
        return Ok(None);
    };
    let cfg = config().with_known_valid_utf8(valid_utf8);
    let replacement = Replacement::parse(replacement);
    memory::precheck_replace(vm, &re, &cfg, subject, &replacement, limit)?;
    let out = php_pcre::replace(&re, &cfg, subject, &replacement, limit);
    *count += out.count;
    state::set_last_error(out.last_error());
    checked(vm, out.result.ok())
}

/// A PHP string from a result buffer, without its spare capacity (PHP
/// strings are allocated at their exact size; `memory_get_usage()` sees it).
fn exact(mut r: Vec<u8>) -> ZStr {
    r.shrink_to_fit();
    ZStr::from(r)
}

/// Applies the memory limit to a replacement result.
fn checked(vm: &mut Vm, result: Option<Vec<u8>>) -> VmResult<Option<Vec<u8>>> {
    if let Some(r) = &result {
        memory::check(vm, memory::string_size(r.len()))?;
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// preg_replace_callback / preg_replace_callback_array
// ---------------------------------------------------------------------------

fn preg_replace_callback(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let regex = a.at(0);
    let callback = resolve_callback(vm, a, &a.at(1))?;
    let subject = a.at(2);
    let limit = limit_from_php(a.int(3));
    let flags = a.int(5);
    let mut count = 0usize;
    let result = replace_func_impl(vm, &regex, &callback, &subject, limit, flags, &mut count)?;
    if a.has(4) {
        a.set_ref(4, Value::Int(count_value(count)));
    }
    Ok(result)
}

fn preg_replace_callback_array(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let patterns = a.array(0);
    let mut subject = a.at(1);
    let limit = limit_from_php(a.int(2));
    let flags = a.int(4);
    let mut count = 0usize;
    for (key, callback) in entries(&patterns) {
        let ArrayKey::Str(pattern) = key else {
            return Err(a.type_error(vm, 1, "must contain only string patterns as keys"));
        };
        // `zend_is_callable(replace, &fcc, NULL)`: each entry is resolved
        // (and deprecated) once, before its pattern runs.
        let call = match vm.resolve_callable(&callback)? {
            Ok(call) => dynamic(call),
            Err(_) => return Err(a.type_error(vm, 1, "must contain only valid callbacks")),
        };
        let result = replace_func_impl(
            vm,
            &Value::Str(pattern),
            &call,
            &subject,
            limit,
            flags,
            &mut count,
        )?;
        if matches!(result, Value::Null) {
            return Ok(Value::Null);
        }
        subject = result;
    }
    if a.has(3) {
        a.set_ref(3, Value::Int(count_value(count)));
    }
    Ok(subject)
}

/// `Z_PARAM_FUNC` for `preg_replace_callback()`'s `$callback`: resolved
/// once, in the calling user frame's scope, raising the callable
/// deprecations there (`zend_fcall_info_init`); every match calls that
/// resolution (`zend_call_known_fcc`). The `callable` parameter type
/// already rejected what does not resolve, unless resolving it again
/// failed.
fn resolve_callback(vm: &mut Vm, a: &Args, callback: &Value) -> VmResult<PendingCall> {
    match vm.resolve_callable(callback)? {
        Ok(call) => Ok(dynamic(call)),
        Err(reason) => Err(a.type_error(vm, 2, &format!("must be a valid callback, {reason}"))),
    }
}

/// `zend_call_function()` marks the call `ZEND_CALL_DYNAMIC` (`compact()`,
/// `func_get_args()` & co. refuse it).
fn dynamic(mut call: PendingCall) -> PendingCall {
    call.dynamic = true;
    call
}

/// `php_preg_replace_func_impl()`.
fn replace_func_impl(
    vm: &mut Vm,
    regex: &Value,
    callback: &PendingCall,
    subject: &Value,
    limit: usize,
    flags: i64,
    count: &mut usize,
) -> VmResult<Value> {
    if let Value::Array(subjects) = subject {
        let mut out = HashTable::with_capacity(subjects.len());
        let mut pending = Pending::default();
        for (key, entry) in entries(subjects) {
            let Some(s) = pending.absorb(vm.value_to_string(&entry))? else {
                continue;
            };
            if pending.is_set() {
                continue;
            }
            let r = replace_func_in_subject(vm, regex, callback, &s, limit, flags, count);
            if let Some(Some(r)) = pending.absorb(r)? {
                out.insert(key, Value::Str(exact(r)));
            }
        }
        return pending.finish(Value::Array(ZArray::from(out)));
    }
    let s = vm.value_to_string(subject)?;
    Ok(
        match replace_func_in_subject(vm, regex, callback, &s, limit, flags, count)? {
            Some(r) => Value::Str(exact(r)),
            None => Value::Null,
        },
    )
}

/// `php_replace_in_subject_func()`.
fn replace_func_in_subject(
    vm: &mut Vm,
    regex: &Value,
    callback: &PendingCall,
    subject_str: &ZStr,
    limit: usize,
    flags: i64,
    count: &mut usize,
) -> VmResult<Option<Vec<u8>>> {
    let subject = subject_str.as_bytes();
    let Value::Array(patterns) = regex else {
        let pattern = vm.value_to_string(regex)?;
        return replace_func(
            vm,
            &pattern,
            callback,
            subject,
            state::known_valid_utf8(subject_str),
            limit,
            flags,
            count,
        );
    };
    let mut current = subject.to_vec();
    for (_, entry) in entries(patterns) {
        let pattern = vm.value_to_string(&entry)?;
        match replace_func(vm, &pattern, callback, &current, false, limit, flags, count)? {
            Some(r) => current = r,
            None => return Ok(None),
        }
    }
    Ok(Some(current))
}

/// `php_pcre_replace_func()`: one pattern, one subject.
#[allow(clippy::too_many_arguments)]
fn replace_func(
    vm: &mut Vm,
    pattern: &ZStr,
    callback: &PendingCall,
    subject: &[u8],
    valid_utf8: bool,
    limit: usize,
    flags: i64,
    count: &mut usize,
) -> VmResult<Option<Vec<u8>>> {
    let Some(re) = state::compile(vm, pattern)? else {
        return Ok(None);
    };
    let cfg = config().with_known_valid_utf8(valid_utf8);
    state::set_last_error(PregError::NoError);
    let mut thrown = None;
    let out = php_pcre::replace_callback(&re, &cfg, subject, limit, flags, |m| {
        if thrown.is_some() {
            return None;
        }
        match call_replacement(vm, callback, m) {
            Ok(s) => Some(s),
            Err(t) => {
                thrown = Some(t);
                None
            }
        }
    });
    *count += out.count;
    state::set_last_error(out.last_error());
    if let Some(t) = thrown {
        return Err(t);
    }
    checked(vm, out.result.ok())
}

/// `preg_do_repl_func()`: calls the resolved user callback with the match
/// array and converts its return value to a string.
fn call_replacement(
    vm: &mut Vm,
    callback: &PendingCall,
    m: php_pcre::MatchArray,
) -> VmResult<Vec<u8>> {
    let ret = vm.call_resolved(callback.clone(), vec![Value::Array(to_zarray(m, false))])?;
    match ret.deref() {
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        other => Ok(vm.value_to_string(&other)?.as_bytes().to_vec()),
    }
}

// ---------------------------------------------------------------------------
// preg_split / preg_grep / preg_quote / preg_last_error
// ---------------------------------------------------------------------------

fn preg_split(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let pattern = a.str(0);
    let subject = a.str(1);
    let limit = a.int(2);
    let flags = a.int(3);
    let Some(re) = state::compile(vm, &pattern)? else {
        return Ok(Value::Bool(false));
    };
    let cfg = state::config_for(&subject);
    let no_empty = flags & php_pcre::PREG_SPLIT_NO_EMPTY != 0;
    memory::precheck_split(vm, &re, &cfg, subject.as_bytes(), no_empty)?;
    match php_pcre::split(&re, &cfg, subject.as_bytes(), limit, flags) {
        Ok(parts) => {
            state::set_last_error(PregError::NoError);
            memory::check(vm, memory::array_size(&parts))?;
            Ok(Value::Array(to_zarray(parts, re.is_utf())))
        }
        Err(e) => {
            state::set_last_error(e);
            Ok(Value::Bool(false))
        }
    }
}

fn preg_grep(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let pattern = a.str(0);
    let input = a.array(1);
    let invert = a.int(2) & php_pcre::PREG_GREP_INVERT != 0;
    let Some(re) = state::compile(vm, &pattern)? else {
        return Ok(Value::Bool(false));
    };
    grep_impl(vm, &re, &input, invert)
}

/// `php_pcre_grep_impl()`.
fn grep_impl(vm: &mut Vm, re: &Regex, input: &ZArray, invert: bool) -> VmResult<Value> {
    let cfg = config();
    state::set_last_error(PregError::NoError);
    let mut out = HashTable::new();
    // Entries are copied as they are, so references stay references.
    let raw: Vec<(ArrayKey, Value)> = input.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut pending = Pending::default();
    for (key, entry) in raw {
        let Some(s) = pending.absorb(vm.value_to_string(&entry.deref()))? else {
            continue;
        };
        if pending.is_set() {
            continue;
        }
        match php_pcre::grep(re, &cfg, [s.as_bytes()], invert) {
            Ok(kept) => {
                if !kept.is_empty() {
                    out.insert(key, entry);
                }
            }
            Err(e) => {
                state::set_last_error(e);
                return Ok(Value::Bool(false));
            }
        }
    }
    pending.finish(Value::Array(ZArray::from(out)))
}

#[allow(clippy::unnecessary_wraps)] // the `NativeFn` signature
fn preg_quote(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    let delim = match a.opt(1) {
        Some(Value::Str(d)) => Some(d),
        _ => None,
    };
    let out = php_pcre::quote(s.as_bytes(), delim.as_ref().map(ZStr::as_bytes));
    Ok(Value::Str(ZStr::from(out)))
}

#[allow(clippy::unnecessary_wraps)] // the `NativeFn` signature
fn preg_last_error(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Int(state::last_error().code()))
}

#[allow(clippy::unnecessary_wraps)] // the `NativeFn` signature
fn preg_last_error_msg(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::from(state::last_error().message()))
}
