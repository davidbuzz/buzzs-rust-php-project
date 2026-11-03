//! `Locale` (all static) and the `locale_*()` functions.

use php_intl::locale::{self as lib, LookupEntry, Subtag};
use php_intl::Result;
use php_runtime::{Value, ZArray, ZStr};
use php_vm::ext::{Args, NativeFn, VmResult};
use php_vm::Vm;

use crate::core::{self, s};

/// Lossy UTF-8 view of a string argument.
pub(crate) fn text(z: &ZStr) -> String {
    String::from_utf8_lossy(z.as_bytes()).into_owned()
}

/// An optional string argument.
pub(crate) fn opt_text(a: &Args, n: usize) -> Option<String> {
    a.opt(n).map(|v| match v {
        Value::Str(z) => text(&z),
        _ => String::new(),
    })
}

/// Common tail: `Ok(Some(s))` → string, `Ok(None)` → `none`, `Err` →
/// intl error then `fail`.
fn finish(vm: &mut Vm, a: &Args, r: Result<Option<String>>, none: Value, fail: Value) -> VmResult<Value> {
    match r {
        Ok(Some(v)) => Ok(s(v)),
        Ok(None) => Ok(none),
        Err(e) => core::fail_with(vm, a, None, e, 0, fail),
    }
}

fn get_default(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(s(lib::get_default()))
}

fn set_default(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let z = a.str(0);
    if z.as_bytes().contains(&0) {
        return Err(a.value_error(vm, 1, "must not contain any null bytes"));
    }
    let v = lib::set_default_value(&text(&z));
    crate::set_default_locale_ini(vm, &v);
    Ok(Value::Bool(true))
}

fn getter(vm: &mut Vm, a: &Args, f: fn(&str) -> Result<Option<String>>) -> VmResult<Value> {
    core::reset_global();
    let r = f(&text(&a.str(0)));
    finish(vm, a, r, Value::Null, Value::Null)
}

fn primary_language(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    getter(vm, a, lib::get_primary_language)
}

fn script(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    getter(vm, a, lib::get_script)
}

fn region(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    getter(vm, a, lib::get_region)
}

fn canonicalize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    getter(vm, a, lib::canonicalize)
}

fn keywords(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match lib::get_keywords(&text(&a.str(0))) {
        Ok(Some(kw)) => {
            let mut arr = ZArray::new();
            for (k, v) in kw {
                arr.make_mut().insert(ZStr::from(k), s(v));
            }
            Ok(Value::Array(arr))
        }
        Ok(None) => Ok(Value::Null),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

type DisplayFn = fn(&str, Option<&str>) -> Result<Option<String>>;

fn display(vm: &mut Vm, a: &Args, f: DisplayFn) -> VmResult<Value> {
    core::reset_global();
    let disp = opt_text(a, 1);
    let r = f(&text(&a.str(0)), disp.as_deref());
    finish(vm, a, r, Value::Bool(false), Value::Bool(false))
}

fn display_name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    display(vm, a, lib::get_display_name)
}

fn display_language(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    display(vm, a, lib::get_display_language)
}

fn display_script(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    display(vm, a, lib::get_display_script)
}

fn display_region(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    display(vm, a, lib::get_display_region)
}

fn display_variant(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    display(vm, a, lib::get_display_variant)
}

fn display_keyword(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let disp = opt_text(a, 1);
    let r = lib::get_display_keyword(&text(&a.str(0)), disp.as_deref()).map(Some);
    finish(vm, a, r, Value::Bool(false), Value::Bool(false))
}

fn display_keyword_value(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let disp = opt_text(a, 2);
    let r = lib::get_display_keyword_value(&text(&a.str(0)), &text(&a.str(1)), disp.as_deref()).map(Some);
    finish(vm, a, r, Value::Bool(false), Value::Bool(false))
}

fn compose(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let arr = a.array(0);
    // Materialise the string views first (the closure borrows them).
    let mut entries: Vec<(String, Option<Vec<Option<String>>>, Option<String>)> = Vec::new();
    for (k, v) in arr.iter() {
        let key = match k {
            php_runtime::ArrayKey::Int(i) => i.to_string(),
            php_runtime::ArrayKey::Str(z) => text(z),
        };
        match v.deref() {
            Value::Str(z) => entries.push((key, None, Some(text(&z)))),
            Value::Array(items) => {
                let list = items
                    .values()
                    .map(|x| match x.deref() {
                        Value::Str(z) => Some(text(&z)),
                        _ => None,
                    })
                    .collect();
                entries.push((key, Some(list), None));
            }
            _ => entries.push((key, None, None)),
        }
    }
    let get = |key: &str| -> Option<Subtag<'_>> {
        let (_, list, st) = entries.iter().find(|e| e.0 == key)?;
        Some(match (list, st) {
            (_, Some(s)) => Subtag::Str(s.as_str()),
            (Some(l), None) => Subtag::List(l.iter().map(Option::as_deref).collect()),
            (None, None) => Subtag::Other,
        })
    };
    let r = lib::compose(arr.len(), &get).map(Some);
    finish(vm, a, r, Value::Bool(false), Value::Bool(false))
}

fn pairs_to_array(p: Vec<(String, String)>) -> Value {
    let mut arr = ZArray::new();
    for (k, v) in p {
        arr.make_mut().insert(ZStr::from(k), s(v));
    }
    Value::Array(arr)
}

fn parse(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match lib::parse(&text(&a.str(0))) {
        Ok(p) => Ok(pairs_to_array(p)),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Null),
    }
}

fn all_variants(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match lib::get_all_variants(&text(&a.str(0))) {
        Ok(v) => {
            let mut arr = ZArray::new();
            for x in v {
                let _ = arr.make_mut().push(s(x));
            }
            Ok(Value::Array(arr))
        }
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Null),
    }
}

fn filter_matches(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    match lib::filter_matches(&text(&a.str(0)), &text(&a.str(1)), a.bool(2)) {
        Ok(b) => Ok(Value::Bool(b)),
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Bool(false)),
    }
}

fn entries(arr: &ZArray) -> Vec<Option<ZStr>> {
    arr.values()
        .map(|v| match v.deref() {
            Value::Str(z) => Some(z),
            _ => None,
        })
        .collect()
}

fn as_lookup(e: &[Option<ZStr>]) -> Vec<LookupEntry<'_>> {
    e.iter()
        .map(|x| match x {
            Some(z) => LookupEntry::Str(z.as_bytes()),
            None => LookupEntry::NonString,
        })
        .collect()
}

fn lookup(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let list = entries(&a.array(0));
    let fallback = opt_text(a, 3);
    let r = lib::lookup(&as_lookup(&list), &text(&a.str(1)), a.bool(2), fallback.as_deref());
    finish(vm, a, r, Value::Null, Value::Null)
}

fn accept_from_http(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    let avail = a.opt(1).map(|v| match v {
        Value::Array(arr) => entries(&arr),
        _ => Vec::new(),
    });
    let view = avail.as_deref().map(as_lookup);
    let r = lib::accept_from_http(a.str(0).as_bytes(), view.as_deref());
    finish(vm, a, r, Value::Bool(false), Value::Bool(false))
}

fn is_rtl(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(lib::is_right_to_left(&text(&a.str(0)))))
}

fn subtags(vm: &mut Vm, a: &Args, f: fn(&str) -> Result<String>) -> VmResult<Value> {
    core::reset_global();
    let r = f(&text(&a.str(0))).map(Some);
    finish(vm, a, r, Value::Bool(false), Value::Bool(false))
}

fn add_likely(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    subtags(vm, a, lib::add_likely_subtags)
}

fn minimize(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    subtags(vm, a, lib::minimize_subtags)
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        ("Locale::getDefault", get_default),
        ("Locale::setDefault", set_default),
        ("Locale::getPrimaryLanguage", primary_language),
        ("Locale::getScript", script),
        ("Locale::getRegion", region),
        ("Locale::getKeywords", keywords),
        ("Locale::getDisplayScript", display_script),
        ("Locale::getDisplayRegion", display_region),
        ("Locale::getDisplayName", display_name),
        ("Locale::getDisplayLanguage", display_language),
        ("Locale::getDisplayVariant", display_variant),
        ("Locale::getDisplayKeyword", display_keyword),
        ("Locale::getDisplayKeywordValue", display_keyword_value),
        ("Locale::composeLocale", compose),
        ("Locale::parseLocale", parse),
        ("Locale::getAllVariants", all_variants),
        ("Locale::filterMatches", filter_matches),
        ("Locale::lookup", lookup),
        ("Locale::canonicalize", canonicalize),
        ("Locale::acceptFromHttp", accept_from_http),
        ("Locale::isRightToLeft", is_rtl),
        ("Locale::addLikelySubtags", add_likely),
        ("Locale::minimizeSubtags", minimize),
    ]
}
