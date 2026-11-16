// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `IntlBreakIterator`, `IntlRuleBasedBreakIterator`,
//! `IntlCodePointBreakIterator` and `IntlPartsIterator`.

use php_intl::breakiterator::{Class, IntlBreakIterator, Kind, PartsCursor};
use php_runtime::{Obj, PropertyPurpose, Value, ZArray, ZStr};
use php_vm::ext::{Args, NativeFn, NativeIterator, VmResult};
use php_vm::Vm;

use crate::core::{self, s};
use crate::iter::IterSource;
use crate::locale::text;

/// Wraps an iterator in a new object of its PHP class.
fn object(vm: &mut Vm, bi: IntlBreakIterator) -> VmResult<Value> {
    let class = match bi.class() {
        Class::RuleBased => "IntlRuleBasedBreakIterator",
        Class::CodePoint => "IntlCodePointBreakIterator",
    };
    let o = core::instantiate(vm, class)?;
    core::set_data(&o, bi);
    Ok(Value::Object(o))
}

fn construct(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Err(vm.throw(
        "Exception",
        "An object of this type cannot be created with the new operator",
    ))
}

fn factory(vm: &mut Vm, a: &Args, kind: Kind) -> VmResult<Value> {
    core::reset_global();
    let loc = a.opt(0).map(|v| match v {
        Value::Str(z) => text(&z),
        _ => String::new(),
    });
    match IntlBreakIterator::create(kind, loc.as_deref()) {
        Ok((bi, st)) => {
            core::with_global(|g| g.code = st);
            object(vm, bi)
        }
        Err(e) => core::fail_with(vm, a, None, e, 0, Value::Null),
    }
}

fn create_word(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    factory(vm, a, Kind::Word)
}

fn create_line(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    factory(vm, a, Kind::Line)
}

fn create_character(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    factory(vm, a, Kind::Character)
}

fn create_sentence(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    factory(vm, a, Kind::Sentence)
}

fn create_title(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    factory(vm, a, Kind::Title)
}

fn create_code_point(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    core::reset_global();
    object(vm, IntlBreakIterator::create_code_point())
}

fn fetch(vm: &mut Vm, a: &Args) -> VmResult<Obj> {
    let o = a.this_obj();
    core::reset_both(&o);
    if !core::is_constructed::<IntlBreakIterator>(&o) {
        return Err(core::unconstructed(vm, "BreakIterator"));
    }
    Ok(o)
}

fn run<T>(
    vm: &mut Vm,
    a: &Args,
    f: impl FnOnce(&mut IntlBreakIterator) -> php_intl::Result<T>,
    ok: impl FnOnce(T) -> Value,
) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    match core::with::<IntlBreakIterator, _>(&o, f) {
        Some(Ok(v)) => Ok(ok(v)),
        Some(Err(e)) => core::fail_with(vm, a, Some(&o), e, 0, Value::Bool(false)),
        None => Ok(Value::Bool(false)),
    }
}

fn get_text(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run(
        vm,
        a,
        |b| Ok(b.text().map(<[u8]>::to_vec)),
        |t| t.map_or(Value::Null, |t| Value::Str(ZStr::from(t))),
    )
}

fn set_text(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let t = a.str(0);
    run(vm, a, |b| b.set_text(t.as_bytes()), |()| Value::Bool(true))
}

fn first(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run(vm, a, |b| Ok(b.first()), Value::Int)
}

fn last(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run(vm, a, |b| Ok(b.last()), Value::Int)
}

fn previous(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run(vm, a, |b| Ok(b.previous()), Value::Int)
}

fn next(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = a.opt(0).map(|v| if let Value::Int(i) = v { i } else { 0 });
    run(vm, a, |b| b.next(n), Value::Int)
}

fn current(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run(vm, a, |b| Ok(b.current()), Value::Int)
}

fn following(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = a.int(0);
    run(vm, a, |b| b.following(n), Value::Int)
}

fn preceding(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = a.int(0);
    run(vm, a, |b| b.preceding(n), Value::Int)
}

fn is_boundary(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = a.int(0);
    run(vm, a, |b| b.is_boundary(n), Value::Bool)
}

fn get_locale(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = a.int(0);
    run(vm, a, |b| b.locale(n), s)
}

fn get_last_code_point(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run(vm, a, |b| Ok(b.last_code_point()), Value::Int)
}

fn get_rules(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run(vm, a, IntlBreakIterator::rules, s)
}

fn get_rule_status(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run(vm, a, |b| Ok(b.rule_status()), Value::Int)
}

fn get_rule_status_vec(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run(vm, a, IntlBreakIterator::rule_status_vec, |v| {
        Value::Array(ZArray::from_values(v.into_iter().map(Value::Int)))
    })
}

fn get_binary_rules(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    run(vm, a, IntlBreakIterator::binary_rules, |v| {
        Value::Str(ZStr::from(v))
    })
}

fn rbbi_construct(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    core::reset_both(&o);
    if core::is_constructed::<IntlBreakIterator>(&o) {
        return Err(vm.throw(
            "Error",
            "IntlRuleBasedBreakIterator object is already constructed",
        ));
    }
    match IntlBreakIterator::from_rules(a.str(0).as_bytes(), a.bool(1)) {
        Ok(bi) => {
            core::set_data(&o, bi);
            Ok(Value::Null)
        }
        Err(e) => Err(core::throw_engine(vm, a, e, 0)),
    }
}

/// The `IntlPartsIterator` source.
struct Parts {
    bi: Obj,
    cursor: PartsCursor,
}

impl IterSource for Parts {
    fn rewind(&mut self, _vm: &mut Vm, index: &mut i64) -> Option<VmResult<()>> {
        let cursor = &mut self.cursor;
        if let Some(Some(i)) = core::with::<IntlBreakIterator, _>(&self.bi, |b| cursor.rewind(b)) {
            *index = i;
        }
        Some(Ok(()))
    }

    fn valid(&self) -> bool {
        self.cursor.valid()
    }

    fn current(&self) -> Value {
        self.cursor
            .current()
            .map_or(Value::Null, |c| Value::Str(ZStr::from(c)))
    }

    fn key(&self, index: i64) -> Value {
        Value::Int(self.cursor.key(index))
    }

    fn move_forward(&mut self, _vm: &mut Vm, index: &mut i64) -> VmResult<()> {
        let cursor = &mut self.cursor;
        if let Some(Some(i)) =
            core::with::<IntlBreakIterator, _>(&self.bi, |b| cursor.move_forward(b))
        {
            *index = i;
        }
        Ok(())
    }

    fn wrapped(&self) -> Option<Obj> {
        Some(self.bi.clone())
    }
}

fn get_parts_iterator(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = fetch(vm, a)?;
    let key = match IntlBreakIterator::check_parts_key_type(a.int(0)) {
        Ok(k) => k,
        Err(e) => return Err(core::throw_engine(vm, a, e, 0)),
    };
    let src = Parts {
        bi: o,
        cursor: PartsCursor::new(key),
    };
    Ok(Value::Object(crate::iter::wrap(
        vm,
        "IntlPartsIterator",
        Box::new(src),
    )?))
}

/// The boundary iterator of `foreach ($bi as ...)` / `getIterator()`.
struct Boundaries {
    bi: Obj,
    current: Option<i64>,
}

impl IterSource for Boundaries {
    fn rewind(&mut self, _vm: &mut Vm, _index: &mut i64) -> Option<VmResult<()>> {
        self.current = core::with::<IntlBreakIterator, _>(&self.bi, IntlBreakIterator::first);
        Some(Ok(()))
    }

    fn valid(&self) -> bool {
        self.current.is_some()
    }

    fn current(&self) -> Value {
        self.current.map_or(Value::Null, Value::Int)
    }

    fn move_forward(&mut self, _vm: &mut Vm, _index: &mut i64) -> VmResult<()> {
        let n = core::with::<IntlBreakIterator, _>(&self.bi, |b| b.next(None).unwrap_or(-1));
        self.current = n.filter(|&p| p != -1);
        Ok(())
    }
}

/// Drives an [`IterSource`] as a `foreach` iterator.
struct Driver {
    src: Boundaries,
    index: i64,
}

impl NativeIterator for Driver {
    fn rewind(&mut self, vm: &mut Vm) -> VmResult<()> {
        self.index = 0;
        self.src.rewind(vm, &mut self.index).unwrap_or(Ok(()))
    }

    fn valid(&mut self, _vm: &mut Vm) -> VmResult<bool> {
        Ok(self.src.valid())
    }

    fn current(&mut self, _vm: &mut Vm) -> VmResult<Value> {
        Ok(self.src.current())
    }

    fn key(&mut self, _vm: &mut Vm) -> VmResult<Value> {
        Ok(Value::Int(self.index))
    }

    fn next(&mut self, vm: &mut Vm) -> VmResult<()> {
        self.index += 1;
        self.src.move_forward(vm, &mut self.index)
    }
}

/// The `get_iterator` hook.
pub(crate) fn get_iterator(
    vm: &mut Vm,
    o: &Obj,
    by_ref: bool,
) -> VmResult<Box<dyn NativeIterator>> {
    if by_ref {
        return Err(vm.throw("Exception", "Iteration by reference is not supported"));
    }
    if !core::is_constructed::<IntlBreakIterator>(o) {
        return Err(vm.throw("Exception", "The BreakIterator is not properly constructed"));
    }
    Ok(Box::new(Driver {
        src: Boundaries {
            bi: o.clone(),
            current: None,
        },
        index: 0,
    }))
}

fn get_iterator_method(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    if !core::is_constructed::<IntlBreakIterator>(&o) {
        return Err(vm.throw("Exception", "The BreakIterator is not properly constructed"));
    }
    let src = Boundaries {
        bi: o,
        current: None,
    };
    Ok(Value::Object(crate::iter::wrap(
        vm,
        "IntlIterator",
        Box::new(src),
    )?))
}

fn parts_get_break_iterator(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    match core::with::<crate::iter::IterState, _>(&o, |st| st.src.wrapped()).flatten() {
        Some(bi) => Ok(Value::Object(bi)),
        None => Err(core::unconstructed(vm, "IntlIterator")),
    }
}

fn parts_get_rule_status(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let o = a.this_obj();
    match core::with::<crate::iter::IterState, _>(&o, |st| st.src.wrapped()).flatten() {
        Some(bi) => vm.call_method_by_name(&bi, "getRuleStatus", Vec::new()),
        None => Err(core::unconstructed(vm, "IntlIterator")),
    }
}

/// `get_debug_info`.
pub(crate) fn debug_info(_vm: &mut Vm, o: &Obj, p: PropertyPurpose) -> VmResult<Option<ZArray>> {
    if p != PropertyPurpose::Debug {
        return Ok(None);
    }
    let mut arr = ZArray::new();
    let h = arr.make_mut();
    match core::with::<IntlBreakIterator, _>(o, |b| (b.text().map(<[u8]>::to_vec), b.class())) {
        None => {
            h.insert("valid", Value::Bool(false));
        }
        Some((t, class)) => {
            h.insert("valid", Value::Bool(true));
            h.insert("text", t.map_or(Value::Null, |t| Value::Str(ZStr::from(t))));
            let ty = match class {
                Class::RuleBased => "N6icu_7422RuleBasedBreakIteratorE",
                Class::CodePoint => "N3PHP22CodePointBreakIteratorE",
            };
            h.insert("type", s(ty));
        }
    }
    Ok(Some(arr))
}

/// `clone_obj`.
pub(crate) fn clone(vm: &mut Vm, old: &Obj, new: &Obj) -> VmResult<()> {
    core::create(vm, new)?;
    match core::with::<IntlBreakIterator, _>(old, |b| b.try_clone()) {
        Some(Ok(b)) => core::set_data(new, b),
        Some(Err(_)) => return Err(vm.throw("Error", "Failed to clone BreakIterator")),
        None => return Err(vm.throw("Error", "Cannot clone uninitialized BreakIterator")),
    }
    Ok(())
}

/// `compare`: equal when both wrap equal ICU iterators (same text).
pub(crate) fn compare(_vm: &mut Vm, x: &Value, y: &Value) -> VmResult<Option<i32>> {
    let (Value::Object(a), Value::Object(b)) = (x, y) else {
        return Ok(None);
    };
    if a == b {
        return Ok(Some(0));
    }
    let ta =
        core::with::<IntlBreakIterator, _>(a, |bi| (bi.class(), bi.text().map(<[u8]>::to_vec)));
    let tb =
        core::with::<IntlBreakIterator, _>(b, |bi| (bi.class(), bi.text().map(<[u8]>::to_vec)));
    match (ta, tb) {
        (Some(x), Some(y)) if x == y => Ok(Some(0)),
        (Some(_), Some(_)) => Ok(Some(1)),
        _ => Ok(None),
    }
}

/// Handlers by stub key.
pub(crate) fn handlers() -> Vec<(&'static str, NativeFn)> {
    vec![
        (
            "IntlBreakIterator::createCharacterInstance",
            create_character,
        ),
        (
            "IntlBreakIterator::createCodePointInstance",
            create_code_point,
        ),
        ("IntlBreakIterator::createLineInstance", create_line),
        ("IntlBreakIterator::createSentenceInstance", create_sentence),
        ("IntlBreakIterator::createTitleInstance", create_title),
        ("IntlBreakIterator::createWordInstance", create_word),
        ("IntlBreakIterator::__construct", construct),
        ("IntlBreakIterator::current", current),
        ("IntlBreakIterator::first", first),
        ("IntlBreakIterator::following", following),
        ("IntlBreakIterator::getErrorCode", core::get_error_code),
        (
            "IntlBreakIterator::getErrorMessage",
            core::get_error_message,
        ),
        ("IntlBreakIterator::getLocale", get_locale),
        ("IntlBreakIterator::getPartsIterator", get_parts_iterator),
        ("IntlBreakIterator::getText", get_text),
        ("IntlBreakIterator::isBoundary", is_boundary),
        ("IntlBreakIterator::last", last),
        ("IntlBreakIterator::next", next),
        ("IntlBreakIterator::preceding", preceding),
        ("IntlBreakIterator::previous", previous),
        ("IntlBreakIterator::setText", set_text),
        ("IntlBreakIterator::getIterator", get_iterator_method),
        ("IntlRuleBasedBreakIterator::__construct", rbbi_construct),
        (
            "IntlRuleBasedBreakIterator::getBinaryRules",
            get_binary_rules,
        ),
        ("IntlRuleBasedBreakIterator::getRules", get_rules),
        ("IntlRuleBasedBreakIterator::getRuleStatus", get_rule_status),
        (
            "IntlRuleBasedBreakIterator::getRuleStatusVec",
            get_rule_status_vec,
        ),
        (
            "IntlCodePointBreakIterator::getLastCodePoint",
            get_last_code_point,
        ),
        (
            "IntlPartsIterator::getBreakIterator",
            parts_get_break_iterator,
        ),
        ("IntlPartsIterator::getRuleStatus", parts_get_rule_status),
    ]
}
