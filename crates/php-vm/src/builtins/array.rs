//! Array functions: bindings of `php-stdarr` (`ext/standard/array.c`).

use super::util::{arr, b, i, opt, s};
use super::{constant, reg};
use crate::func::Args;
use crate::object;
use crate::vm::{Vm, VmResult};
use php_runtime::{Obj, PhpRef, RtResult, Value, ZArray, ZStr};
use php_stdarr as sa;

impl sa::ArrayContext for Vm {
    fn is_enum(&self, obj: &Obj) -> bool {
        object::class_of(obj).is_enum()
    }

    fn count_object(&mut self, obj: &Obj) -> RtResult<Option<i64>> {
        php_runtime::RuntimeContext::count_object(self, obj)
    }

    fn column_property(&mut self, obj: &Obj, name: &ZStr) -> RtResult<Option<Value>> {
        let has = self.has_property(obj, name, false).map_err(|t| self.to_rt(t))?;
        if !has {
            return Ok(None);
        }
        self.read_property(obj, name, crate::opcode::FetchMode::Is)
            .map(|v| Some(v.deref()))
            .map_err(|t| self.to_rt(t))
    }

    fn this_object(&mut self) -> Option<Value> {
        let n = self.frames.len();
        if n < 2 {
            return None;
        }
        self.frames[n - 2].this.clone().map(Value::Object)
    }
}

fn rt<T>(vm: &mut Vm, r: RtResult<T>) -> VmResult<T> {
    r.map_err(|e| vm.rt_throw(e))
}

fn cb_of(cb: Value) -> impl FnMut(&mut Vm, &[Value]) -> RtResult<Value> {
    move |vm: &mut Vm, args: &[Value]| vm.call_value(&cb, args.to_vec()).map_err(|t| vm.to_rt(t))
}

fn byref(a: &Args, n: usize) -> PhpRef {
    a.reference(n).unwrap_or_else(|| PhpRef::new(a.at(n)))
}

fn fname(a: &Args) -> String {
    a.func_name()
}

fn count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = a.at(0);
    let mode = i(a, 1);
    let f = fname(a);
    let r = sa::basic::count(vm, &f, &v, mode);
    Ok(Value::Int(rt(vm, r)?))
}

fn in_array(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = a.at(0);
    let h = arr(a, 1);
    let r = sa::basic::in_array(vm, &n, &h, b(a, 2));
    Ok(Value::Bool(rt(vm, r)?))
}

fn array_search(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let n = a.at(0);
    let h = arr(a, 1);
    let r = sa::basic::array_search(vm, &n, &h, b(a, 2));
    rt(vm, r)
}

fn array_keys(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let f = a.get(1);
    let r = sa::basic::array_keys(vm, &h, f.as_ref(), b(a, 2));
    Ok(Value::Array(rt(vm, r)?))
}

fn array_values(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(sa::basic::array_values(&arr(a, 0))))
}

fn array_count_values(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let r = sa::basic::array_count_values(vm, &h);
    Ok(Value::Array(rt(vm, r)?))
}

fn array_flip(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let r = sa::basic::array_flip(vm, &h);
    Ok(Value::Array(rt(vm, r)?))
}

fn array_change_key_case(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let r = sa::basic::array_change_key_case(&h, i(a, 1));
    Ok(Value::Array(rt(vm, r)?))
}

fn array_key_first(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(sa::basic::array_key_first(&arr(a, 0)))
}
fn array_key_last(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(sa::basic::array_key_last(&arr(a, 0)))
}
fn array_first(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(sa::basic::array_first(&arr(a, 0)))
}
fn array_last(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(sa::basic::array_last(&arr(a, 0)))
}
fn array_is_list(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(sa::basic::array_is_list(&arr(a, 0))))
}

fn array_key_exists(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let k = a.at(0);
    let h = arr(a, 1);
    let f = fname(a);
    let r = sa::basic::array_key_exists(vm, &f, &k, &h);
    Ok(Value::Bool(rt(vm, r)?))
}

fn array_fill(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = sa::basic::array_fill(i(a, 0), i(a, 1), &a.at(2));
    Ok(Value::Array(rt(vm, r)?))
}

fn array_fill_keys(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let k = arr(a, 0);
    let v = a.at(1);
    let r = sa::basic::array_fill_keys(vm, &k, &v);
    Ok(Value::Array(rt(vm, r)?))
}

fn array_pad(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = sa::basic::array_pad(&arr(a, 0), i(a, 1), &a.at(2));
    Ok(Value::Array(rt(vm, r)?))
}

fn array_reverse(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(sa::basic::array_reverse(&arr(a, 0), b(a, 1))))
}

fn array_slice(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let len = opt(a, 2).map(|_| i(a, 2));
    Ok(Value::Array(sa::basic::array_slice(&arr(a, 0), i(a, 1), len, b(a, 3))))
}

fn array_chunk(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = sa::basic::array_chunk(&arr(a, 0), i(a, 1), b(a, 2));
    Ok(Value::Array(rt(vm, r)?))
}

fn array_combine(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let k = arr(a, 0);
    let v = arr(a, 1);
    let r = sa::basic::array_combine(vm, &k, &v);
    Ok(Value::Array(rt(vm, r)?))
}

fn column_key(v: Option<Value>) -> Option<sa::basic::ColumnKey> {
    match v? {
        Value::Int(i) => Some(sa::basic::ColumnKey::Int(i)),
        Value::Str(s) => Some(sa::basic::ColumnKey::Str(s)),
        _ => None,
    }
}

fn array_column(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let c = column_key(opt(a, 1));
    let idx = column_key(opt(a, 2));
    let r = sa::basic::array_column(vm, &h, c.as_ref(), idx.as_ref());
    Ok(Value::Array(rt(vm, r)?))
}

fn array_sum(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let r = sa::basic::array_sum(vm, &h);
    rt(vm, r)
}

fn array_product(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let r = sa::basic::array_product(vm, &h);
    rt(vm, r)
}

fn min(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let vals: Vec<Value> = a.values.iter().map(Value::deref).collect();
    let r = sa::basic::min(vm, &vals);
    rt(vm, r)
}

fn max(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let vals: Vec<Value> = a.values.iter().map(Value::deref).collect();
    let r = sa::basic::max(vm, &vals);
    rt(vm, r)
}

macro_rules! sort_fn {
    ($name:ident, $f:path) => {
        fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let r0 = byref(a, 0);
            let r = $f(vm, &r0, i(a, 1));
            rt(vm, r)?;
            Ok(Value::Bool(true))
        }
    };
}
sort_fn!(sort, sa::sort::sort);
sort_fn!(rsort, sa::sort::rsort);
sort_fn!(asort, sa::sort::asort);
sort_fn!(arsort, sa::sort::arsort);
sort_fn!(ksort, sa::sort::ksort);
sort_fn!(krsort, sa::sort::krsort);

fn natsort(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r0 = byref(a, 0);
    let r = sa::sort::natsort(vm, &r0);
    rt(vm, r)?;
    Ok(Value::Bool(true))
}

fn natcasesort(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r0 = byref(a, 0);
    let r = sa::sort::natcasesort(vm, &r0);
    rt(vm, r)?;
    Ok(Value::Bool(true))
}

macro_rules! usort_fn {
    ($name:ident, $f:path) => {
        fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let r0 = byref(a, 0);
            let mut cb = cb_of(a.at(1));
            let r = $f(vm, &r0, &mut cb);
            rt(vm, r)?;
            Ok(Value::Bool(true))
        }
    };
}
usort_fn!(usort, sa::sort::usort);
usort_fn!(uasort, sa::sort::uasort);
usort_fn!(uksort, sa::sort::uksort);

fn array_multisort(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let vals = a.values.clone();
    let r = sa::sort::array_multisort(vm, &vals);
    rt(vm, r)?;
    Ok(Value::Bool(true))
}

fn array_push(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r0 = byref(a, 0);
    let vals: Vec<Value> = a.values[1..].iter().map(Value::deref).collect();
    let r = sa::stack::array_push(&r0, &vals);
    Ok(Value::Int(rt(vm, r)?))
}

fn array_pop(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r0 = byref(a, 0);
    let r = sa::stack::array_pop(&r0);
    rt(vm, r)
}

fn array_shift(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r0 = byref(a, 0);
    let r = sa::stack::array_shift(&r0);
    rt(vm, r)
}

fn array_unshift(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r0 = byref(a, 0);
    let vals: Vec<Value> = a.values[1..].iter().map(Value::deref).collect();
    let r = sa::stack::array_unshift(&r0, &vals);
    Ok(Value::Int(rt(vm, r)?))
}

fn array_splice(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r0 = byref(a, 0);
    let len = opt(a, 2).map(|_| i(a, 2));
    let rep = a.get(3);
    let r = sa::stack::array_splice(vm, &r0, i(a, 1), len, rep.as_ref());
    Ok(Value::Array(rt(vm, r)?))
}

macro_rules! args_fn {
    ($name:ident, $f:path) => {
        fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let vals: Vec<Value> = a.values.iter().map(Value::deref).collect();
            let r = $f(vm, &vals);
            Ok(Value::Array(rt(vm, r)?))
        }
    };
}
args_fn!(array_merge, sa::merge::array_merge);
args_fn!(array_merge_recursive, sa::merge::array_merge_recursive);
args_fn!(array_intersect, sa::setops::array_intersect);
args_fn!(array_intersect_key, sa::setops::array_intersect_key);
args_fn!(array_intersect_assoc, sa::setops::array_intersect_assoc);
args_fn!(array_diff, sa::setops::array_diff);
args_fn!(array_diff_key, sa::setops::array_diff_key);
args_fn!(array_diff_assoc, sa::setops::array_diff_assoc);

fn array_replace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let vals: Vec<Value> = a.values.iter().map(Value::deref).collect();
    let r = sa::merge::array_replace(&vals);
    Ok(Value::Array(rt(vm, r)?))
}

fn array_replace_recursive(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let vals: Vec<Value> = a.values.iter().map(Value::deref).collect();
    let r = sa::merge::array_replace_recursive(&vals);
    Ok(Value::Array(rt(vm, r)?))
}

macro_rules! ucb_fn {
    ($name:ident, $f:path) => {
        fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let n = a.values.len();
            if n < 2 {
                return Ok(Value::Array(ZArray::new()));
            }
            let vals: Vec<Value> = a.values[..n - 1].iter().map(Value::deref).collect();
            let mut cb = cb_of(a.values[n - 1].deref());
            let r = $f(vm, &vals, &mut cb);
            Ok(Value::Array(rt(vm, r)?))
        }
    };
}
ucb_fn!(array_udiff, sa::setops::array_udiff);
ucb_fn!(array_uintersect, sa::setops::array_uintersect);
ucb_fn!(array_diff_ukey, sa::setops::array_diff_ukey);
ucb_fn!(array_intersect_ukey, sa::setops::array_intersect_ukey);
ucb_fn!(array_diff_uassoc, sa::setops::array_diff_uassoc);
ucb_fn!(array_intersect_uassoc, sa::setops::array_intersect_uassoc);
ucb_fn!(array_udiff_assoc, sa::setops::array_udiff_assoc);
ucb_fn!(array_uintersect_assoc, sa::setops::array_uintersect_assoc);

fn array_unique(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let fl = if a.has(1) { i(a, 1) } else { 2 };
    let r = sa::setops::array_unique(vm, &h, fl);
    Ok(Value::Array(rt(vm, r)?))
}

fn array_walk(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r0 = byref(a, 0);
    let mut cb = cb_of(a.at(1));
    let ud = a.get(2);
    let r = sa::callback::array_walk(vm, &r0, &mut cb, ud.as_ref());
    rt(vm, r)?;
    Ok(Value::Bool(true))
}

fn array_walk_recursive(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r0 = byref(a, 0);
    let mut cb = cb_of(a.at(1));
    let ud = a.get(2);
    let r = sa::callback::array_walk_recursive(vm, &r0, &mut cb, ud.as_ref());
    rt(vm, r)?;
    Ok(Value::Bool(true))
}

fn array_map(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cbv = a.at(0);
    let arrays: Vec<Value> = a.values[1..].iter().map(Value::deref).collect();
    let r = if cbv.is_null() {
        sa::callback::array_map::<Vm>(vm, None, &arrays)
    } else {
        let mut cb = cb_of(cbv);
        sa::callback::array_map(vm, Some(&mut cb), &arrays)
    };
    Ok(Value::Array(rt(vm, r)?))
}

fn array_filter(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let mode = i(a, 2);
    let r = match opt(a, 1) {
        None => sa::callback::array_filter::<Vm>(vm, &h, None, mode),
        Some(c) => {
            let mut cb = cb_of(c);
            sa::callback::array_filter(vm, &h, Some(&mut cb), mode)
        }
    };
    Ok(Value::Array(rt(vm, r)?))
}

fn array_reduce(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let mut cb = cb_of(a.at(1));
    let init = a.get(2);
    let r = sa::callback::array_reduce(vm, &h, &mut cb, init.as_ref());
    rt(vm, r)
}

fn array_find(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let mut cb = cb_of(a.at(1));
    let r = sa::callback::array_find(vm, &h, &mut cb);
    rt(vm, r)
}

fn array_find_key(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let mut cb = cb_of(a.at(1));
    let r = sa::callback::array_find_key(vm, &h, &mut cb);
    rt(vm, r)
}

fn array_any(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let mut cb = cb_of(a.at(1));
    let r = sa::callback::array_any(vm, &h, &mut cb);
    Ok(Value::Bool(rt(vm, r)?))
}

fn array_all(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let mut cb = cb_of(a.at(1));
    let r = sa::callback::array_all(vm, &h, &mut cb);
    Ok(Value::Bool(rt(vm, r)?))
}

macro_rules! ptr_fn {
    ($name:ident, $f:path) => {
        fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let r0 = byref(a, 0);
            let r = $f(vm, &r0);
            rt(vm, r)
        }
    };
}
ptr_fn!(end, sa::pointer::end);
ptr_fn!(prev, sa::pointer::prev);
ptr_fn!(next, sa::pointer::next);
ptr_fn!(reset, sa::pointer::reset);

fn current(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = a.at(0);
    let f = fname(a);
    let r = sa::pointer::current(vm, &f, &v);
    rt(vm, r)
}

fn key(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = a.at(0);
    let r = sa::pointer::key(vm, &v);
    rt(vm, r)
}

fn range(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s0 = a.at(0);
    let e = a.at(1);
    let st = a.get(2);
    let r = sa::range::range(vm, &s0, &e, st.as_ref());
    Ok(Value::Array(rt(vm, r)?))
}

impl Vm {
    pub(crate) fn random_globals(&mut self) -> &mut php_random::RandomGlobals {
        self.ext.random.get_or_insert_with(php_random::RandomGlobals::new)
    }
}

fn shuffle(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r0 = byref(a, 0);
    let mut g = vm.random_globals().clone();
    let r = {
        let mut rng = |min: i64, max: i64| Ok(g.mt_rand_range_unchecked(min, max));
        sa::random::shuffle(&r0, &mut rng)
    };
    vm.ext.random = Some(g);
    rt(vm, r)?;
    Ok(Value::Bool(true))
}

fn array_rand(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = arr(a, 0);
    let n = if a.has(1) { i(a, 1) } else { 1 };
    let mut g = vm.random_globals().clone();
    let r = {
        let mut rng = |min: i64, max: i64| Ok(g.mt_rand_range_unchecked(min, max));
        sa::random::array_rand(&h, n, &mut rng)
    };
    vm.ext.random = Some(g);
    rt(vm, r)
}

fn str_shuffle(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let st = s(a, 0);
    let mut g = vm.random_globals().clone();
    let r = {
        let mut rng = |min: i64, max: i64| Ok(g.mt_rand_range_unchecked(min, max));
        sa::random::str_shuffle(st.as_bytes(), &mut rng)
    };
    vm.ext.random = Some(g);
    Ok(Value::Str(ZStr::from(rt(vm, r)?)))
}

/// Registers the array functions.
pub(crate) fn register(vm: &mut Vm) {
    reg(vm, "count(Countable|array $value, int $mode = COUNT_NORMAL): int", count);
    reg(vm, "sizeof(Countable|array $value, int $mode = COUNT_NORMAL): int", count);
    reg(vm, "in_array(mixed $needle, array $haystack, bool $strict = false): bool", in_array);
    reg(vm, "array_search(mixed $needle, array $haystack, bool $strict = false): int|string|false", array_search);
    reg(vm, "array_keys(array $array, mixed $filter_value = UNKNOWN, bool $strict = false): array", array_keys);
    reg(vm, "array_values(array $array): array", array_values);
    reg(vm, "array_count_values(array $array): array", array_count_values);
    reg(vm, "array_flip(array $array): array", array_flip);
    reg(vm, "array_change_key_case(array $array, int $case = CASE_LOWER): array", array_change_key_case);
    reg(vm, "array_key_first(array $array): int|string|null", array_key_first);
    reg(vm, "array_key_last(array $array): int|string|null", array_key_last);
    reg(vm, "array_first(array $array): mixed", array_first);
    reg(vm, "array_last(array $array): mixed", array_last);
    reg(vm, "array_is_list(array $array): bool", array_is_list);
    reg(vm, "array_key_exists($key, array $array): bool", array_key_exists);
    reg(vm, "key_exists($key, array $array): bool", array_key_exists);
    reg(vm, "array_fill(int $start_index, int $count, mixed $value): array", array_fill);
    reg(vm, "array_fill_keys(array $keys, mixed $value): array", array_fill_keys);
    reg(vm, "array_pad(array $array, int $length, mixed $value): array", array_pad);
    reg(vm, "array_reverse(array $array, bool $preserve_keys = false): array", array_reverse);
    reg(vm, "array_slice(array $array, int $offset, ?int $length = null, bool $preserve_keys = false): array", array_slice);
    reg(vm, "array_chunk(array $array, int $length, bool $preserve_keys = false): array", array_chunk);
    reg(vm, "array_combine(array $keys, array $values): array", array_combine);
    reg(vm, "array_column(array $array, int|string|null $column_key, int|string|null $index_key = null): array", array_column);
    reg(vm, "array_sum(array $array): int|float", array_sum);
    reg(vm, "array_product(array $array): int|float", array_product);
    reg(vm, "min(mixed $value, mixed ...$values): mixed", min);
    reg(vm, "max(mixed $value, mixed ...$values): mixed", max);
    reg(vm, "sort(array &$array, int $flags = SORT_REGULAR): true", sort);
    reg(vm, "rsort(array &$array, int $flags = SORT_REGULAR): true", rsort);
    reg(vm, "asort(array &$array, int $flags = SORT_REGULAR): true", asort);
    reg(vm, "arsort(array &$array, int $flags = SORT_REGULAR): true", arsort);
    reg(vm, "ksort(array &$array, int $flags = SORT_REGULAR): true", ksort);
    reg(vm, "krsort(array &$array, int $flags = SORT_REGULAR): true", krsort);
    reg(vm, "natsort(array &$array): true", natsort);
    reg(vm, "natcasesort(array &$array): true", natcasesort);
    reg(vm, "usort(array &$array, callable $callback): true", usort);
    reg(vm, "uasort(array &$array, callable $callback): true", uasort);
    reg(vm, "uksort(array &$array, callable $callback): true", uksort);
    reg(vm, "array_multisort(&$array, &...$rest): bool", array_multisort);
    reg(vm, "array_push(array &$array, mixed ...$values): int", array_push);
    reg(vm, "array_pop(array &$array): mixed", array_pop);
    reg(vm, "array_shift(array &$array): mixed", array_shift);
    reg(vm, "array_unshift(array &$array, mixed ...$values): int", array_unshift);
    reg(vm, "array_splice(array &$array, int $offset, ?int $length = null, mixed $replacement = []): array", array_splice);
    reg(vm, "array_merge(array ...$arrays): array", array_merge);
    reg(vm, "array_merge_recursive(array ...$arrays): array", array_merge_recursive);
    reg(vm, "array_replace(array $array, array ...$replacements): array", array_replace);
    reg(vm, "array_replace_recursive(array $array, array ...$replacements): array", array_replace_recursive);
    reg(vm, "array_unique(array $array, int $flags = SORT_STRING): array", array_unique);
    reg(vm, "array_intersect(array $array, array ...$arrays): array", array_intersect);
    reg(vm, "array_intersect_key(array $array, array ...$arrays): array", array_intersect_key);
    reg(vm, "array_intersect_assoc(array $array, array ...$arrays): array", array_intersect_assoc);
    reg(vm, "array_diff(array $array, array ...$arrays): array", array_diff);
    reg(vm, "array_diff_key(array $array, array ...$arrays): array", array_diff_key);
    reg(vm, "array_diff_assoc(array $array, array ...$arrays): array", array_diff_assoc);
    reg(vm, "array_udiff(array $array, ...$rest): array", array_udiff);
    reg(vm, "array_uintersect(array $array, ...$rest): array", array_uintersect);
    reg(vm, "array_diff_ukey(array $array, ...$rest): array", array_diff_ukey);
    reg(vm, "array_intersect_ukey(array $array, ...$rest): array", array_intersect_ukey);
    reg(vm, "array_diff_uassoc(array $array, ...$rest): array", array_diff_uassoc);
    reg(vm, "array_intersect_uassoc(array $array, ...$rest): array", array_intersect_uassoc);
    reg(vm, "array_udiff_assoc(array $array, ...$rest): array", array_udiff_assoc);
    reg(vm, "array_uintersect_assoc(array $array, ...$rest): array", array_uintersect_assoc);
    reg(vm, "array_walk(array|object &$array, callable $callback, mixed $arg = UNKNOWN): true", array_walk);
    reg(vm, "array_walk_recursive(array|object &$array, callable $callback, mixed $arg = UNKNOWN): true", array_walk_recursive);
    reg(vm, "array_map(?callable $callback, array $array, array ...$arrays): array", array_map);
    reg(vm, "array_filter(array $array, ?callable $callback = null, int $mode = 0): array", array_filter);
    reg(vm, "array_reduce(array $array, callable $callback, mixed $initial = null): mixed", array_reduce);
    reg(vm, "array_find(array $array, callable $callback): mixed", array_find);
    reg(vm, "array_find_key(array $array, callable $callback): mixed", array_find_key);
    reg(vm, "array_any(array $array, callable $callback): bool", array_any);
    reg(vm, "array_all(array $array, callable $callback): bool", array_all);
    reg(vm, "end(array|object &$array): mixed", end);
    reg(vm, "prev(array|object &$array): mixed", prev);
    reg(vm, "next(array|object &$array): mixed", next);
    reg(vm, "reset(array|object &$array): mixed", reset);
    reg(vm, "current(array|object $array): mixed", current);
    reg(vm, "pos(array|object $array): mixed", current);
    reg(vm, "key(array|object $array): int|string|null", key);
    reg(vm, "range($start, $end, int|float $step = 1): array", range);
    reg(vm, "shuffle(array &$array): true", shuffle);
    reg(vm, "array_rand(array $array, int $num = 1): int|string|array", array_rand);
    reg(vm, "str_shuffle(string $string): string", str_shuffle);
    for (n, v) in [
        ("COUNT_NORMAL", 0),
        ("COUNT_RECURSIVE", 1),
        ("SORT_ASC", 4),
        ("SORT_DESC", 3),
        ("SORT_REGULAR", 0),
        ("SORT_NUMERIC", 1),
        ("SORT_STRING", 2),
        ("SORT_LOCALE_STRING", 5),
        ("SORT_NATURAL", 6),
        ("SORT_FLAG_CASE", 8),
        ("CASE_LOWER", 0),
        ("CASE_UPPER", 1),
        ("ARRAY_FILTER_USE_KEY", 2),
        ("ARRAY_FILTER_USE_BOTH", 1),
    ] {
        constant(vm, n, Value::Int(v));
    }
}
