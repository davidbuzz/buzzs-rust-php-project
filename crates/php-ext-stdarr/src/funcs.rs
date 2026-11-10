//! The native handlers: argument unpacking, `Z_PARAM_FUNC`-style callback
//! checks and the call into php-stdarr.
//!
//! Every handler has the [`php_vm::ext::NativeFn`] signature, so handlers
//! that cannot fail still return `VmResult`.
#![allow(clippy::unnecessary_wraps)]

use crate::bridge::{by_ref, callback, check_callable, rest, rt, with_rng};
use crate::memory;
use crate::objptr;
use crate::stack;
use crate::symtab::CallerSymbols;
use php_runtime::{Value, ZArray, ZStr};
use php_stdarr as sa;
use php_vm::ext::{Args, VmResult};
use php_vm::vm::FrameKind;
use php_vm::Vm;

/// Integer argument `n`, or `default` when it was not passed.
fn int_or(a: &Args, n: usize, default: i64) -> i64 {
    if a.has(n) {
        a.int(n)
    } else {
        default
    }
}

/// Whether the running native function was called directly from PHP code
/// (not through `call_user_func()` or another native function).
fn called_from_code(vm: &Vm) -> bool {
    let n = vm.frames.len();
    n >= 2 && vm.frames[n - 2].kind != FrameKind::Builtin
}

// ---------------------------------------------------------------------
// count, search, keys/values
// ---------------------------------------------------------------------

pub fn count(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = a.at(0);
    if a.int(1) == sa::COUNT_RECURSIVE {
        // php_count_recursive() recurses once per level below the top.
        stack::check_depth(vm, |cap| {
            stack::nesting_depth(&v, cap + 1).saturating_sub(1)
        })?;
    }
    let f = a.func_name();
    let r = sa::basic::count(vm, &f, &v, a.int(1));
    Ok(Value::Int(rt(vm, r)?))
}

// `Z_PARAM_ZVAL` + `Z_PARAM_ARRAY` read the needle and the table in
// place: no copy of either per call.
pub fn in_array(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, h) = (a.at_ref(0), a.array_ref(1));
    let r = sa::basic::in_array(vm, &n, &h, a.bool(2));
    Ok(Value::Bool(rt(vm, r)?))
}

pub fn array_search(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (n, h) = (a.at_ref(0), a.array_ref(1));
    let r = sa::basic::array_search(vm, &n, &h, a.bool(2));
    rt(vm, r)
}

pub fn array_keys(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    if a.has(2) && !a.has(1) {
        return Err(vm.throw(
            "ArgumentCountError",
            "array_keys(): Argument #2 ($filter_value) must be passed explicitly, because the default value is not known",
        ));
    }
    let h = a.array(0);
    let f = a.get(1);
    let r = sa::basic::array_keys(vm, &h, f.as_ref(), a.bool(2));
    Ok(Value::Array(rt(vm, r)?))
}

pub fn array_values(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(sa::basic::array_values(&a.array(0))))
}

pub fn array_count_values(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = a.array(0);
    let r = sa::basic::array_count_values(vm, &h);
    Ok(Value::Array(rt(vm, r)?))
}

pub fn array_flip(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = a.array(0);
    let r = sa::basic::array_flip(vm, &h);
    Ok(Value::Array(rt(vm, r)?))
}

pub fn array_change_key_case(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = a.array(0);
    let r = sa::basic::array_change_key_case(&h, int_or(a, 1, sa::CASE_LOWER));
    Ok(Value::Array(rt(vm, r)?))
}

pub fn array_key_first(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(sa::basic::array_key_first(&a.array(0)))
}

pub fn array_key_last(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(sa::basic::array_key_last(&a.array(0)))
}

pub fn array_first(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(sa::basic::array_first(&a.array(0)))
}

pub fn array_last(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(sa::basic::array_last(&a.array(0)))
}

pub fn array_is_list(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Bool(sa::basic::array_is_list(&a.array(0))))
}

pub fn array_key_exists(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let key = a.at(0);
    let h = a.array(1);
    let f = a.func_name();
    // A direct call to `array_key_exists()` is compiled to the
    // ZEND_ARRAY_KEY_EXISTS opcode, whose error for illegal keys differs.
    if f == "array_key_exists"
        && matches!(key, Value::Array(_) | Value::Object(_))
        && called_from_code(vm)
    {
        let msg = format!("Cannot access offset of type {} on array", key.value_name());
        return Err(vm.throw("TypeError", &msg));
    }
    let r = sa::basic::array_key_exists(vm, &f, &key, &h);
    Ok(Value::Bool(rt(vm, r)?))
}

// ---------------------------------------------------------------------
// construction / slicing
// ---------------------------------------------------------------------

pub fn array_fill(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (start, num) = (a.int(0), a.int(1));
    if num > 0 && num <= i64::from(i32::MAX) && start <= i64::MAX - num + 1 {
        // `array_finit_size_packed` / `array_finit_size_mixed`.
        if (0..num).contains(&start) {
            let size = start.unsigned_abs() + num.unsigned_abs();
            memory::check_table(vm, size)?;
            if !memory::reserve(memory::packed_data(size)) {
                return Ok(Value::Null);
            }
        } else {
            memory::check_table(vm, num.unsigned_abs())?;
            if !memory::reserve(memory::hash_data(num.unsigned_abs())) {
                return Ok(Value::Null);
            }
        }
    }
    let r = sa::basic::array_fill(start, num, &a.at(2));
    Ok(Value::Array(rt(vm, r)?))
}

pub fn array_fill_keys(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (k, v) = (a.array(0), a.at(1));
    let r = sa::basic::array_fill_keys(vm, &k, &v);
    Ok(Value::Array(rt(vm, r)?))
}

pub fn array_pad(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (h, len) = (a.array(0), a.int(1));
    let n = len.unsigned_abs();
    if n > h.len() as u64 && n <= memory::HT_MAX_SIZE && !memory::reserve(memory::packed_data(n)) {
        return Ok(Value::Null);
    }
    let r = sa::basic::array_pad(&h, len, &a.at(2));
    Ok(Value::Array(rt(vm, r)?))
}

pub fn array_reverse(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    Ok(Value::Array(sa::basic::array_reverse(
        &a.array(0),
        a.bool(1),
    )))
}

pub fn array_slice(_vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let len = a.opt(2).map(|_| a.int(2));
    Ok(Value::Array(sa::basic::array_slice(
        &a.array(0),
        a.int(1),
        len,
        a.bool(3),
    )))
}

pub fn array_chunk(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (h, size) = (a.array(0), a.int(1));
    if size >= 1 && !h.is_empty() {
        let (len, size) = (h.len() as u64, size.unsigned_abs());
        if !memory::reserve_nested(len.div_ceil(size), size.min(len)) {
            return Ok(Value::Null);
        }
    }
    let r = sa::basic::array_chunk(&h, size, a.bool(2));
    Ok(Value::Array(rt(vm, r)?))
}

pub fn array_combine(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (k, v) = (a.array(0), a.array(1));
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

pub fn array_column(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = a.array(0);
    let c = column_key(a.opt(1));
    let idx = column_key(a.opt(2));
    let r = sa::basic::array_column(vm, &h, c.as_ref(), idx.as_ref());
    Ok(Value::Array(rt(vm, r)?))
}

pub fn array_sum(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = a.array(0);
    let r = sa::basic::array_sum(vm, &h);
    rt(vm, r)
}

pub fn array_product(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = a.array(0);
    let r = sa::basic::array_product(vm, &h);
    rt(vm, r)
}

pub fn min(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let vals = rest(a, 0);
    let r = match vals.as_slice() {
        [x, y] => sa::basic::min2(vm, x, y),
        _ => sa::basic::min(vm, &vals),
    };
    rt(vm, r)
}

pub fn max(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let vals = rest(a, 0);
    let r = match vals.as_slice() {
        [x, y] => sa::basic::max2(vm, x, y),
        _ => sa::basic::max(vm, &vals),
    };
    rt(vm, r)
}

pub fn clamp(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = sa::basic::clamp(vm, &a.at(0), &a.at(1), &a.at(2));
    rt(vm, r)
}

/// The number of elements of a numeric `range()`, if cheaply known.
#[allow(clippy::cast_precision_loss)]
fn range_len(start: &Value, end: &Value, step: Option<&Value>) -> Option<f64> {
    let num = |v: &Value| match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    };
    let step = step.map_or(Some(1.0), num)?.abs();
    let span = (num(end)? - num(start)?).abs();
    (step > 0.0 && span.is_finite()).then(|| (span / step).floor() + 1.0)
}

pub fn range(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let (s, e) = (a.at(0), a.at(1));
    let st = a.get(2);
    if let Some(n) = range_len(&s, &e, st.as_ref()) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = n as u64;
        if n > 1024 && n < memory::HT_MAX_SIZE && !memory::reserve(memory::packed_data(n)) {
            return Ok(Value::Null);
        }
    }
    let r = sa::range::range(vm, &s, &e, st.as_ref());
    Ok(Value::Array(rt(vm, r)?))
}

// ---------------------------------------------------------------------
// sorting
// ---------------------------------------------------------------------

macro_rules! sort_fn {
    ($name:ident, $f:path) => {
        pub fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let arr = by_ref(a, 0);
            let r = $f(vm, &arr, int_or(a, 1, sa::SORT_REGULAR));
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

pub fn natsort(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arr = by_ref(a, 0);
    let r = sa::sort::natsort(vm, &arr);
    rt(vm, r)?;
    Ok(Value::Bool(true))
}

pub fn natcasesort(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arr = by_ref(a, 0);
    let r = sa::sort::natcasesort(vm, &arr);
    rt(vm, r)?;
    Ok(Value::Bool(true))
}

/// The by-reference `array` (or `array|object`) parameter type check, which
/// PHP's zpp performs before validating the callback that follows it.
fn check_array_arg(vm: &mut Vm, a: &Args, allow_object: bool) -> VmResult<()> {
    let v = a.at(0);
    if matches!(v, Value::Array(_)) || (allow_object && matches!(v, Value::Object(_))) {
        return Ok(());
    }
    let msg = format!(
        "{}(): Argument #1 ($array) must be of type array, {} given",
        a.func_name(),
        v.value_name()
    );
    Err(vm.throw("TypeError", &msg))
}

macro_rules! usort_fn {
    ($name:ident, $f:path) => {
        pub fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            check_array_arg(vm, a, false)?;
            let cb = a.at(1);
            check_callable(vm, a, 2, true, false, &cb)?;
            let arr = by_ref(a, 0);
            let mut cb = callback(cb);
            let r = $f(vm, &arr, &mut cb);
            rt(vm, r)?;
            Ok(Value::Bool(true))
        }
    };
}
usort_fn!(usort, sa::sort::usort);
usort_fn!(uasort, sa::sort::uasort);
usort_fn!(uksort, sa::sort::uksort);

pub fn array_multisort(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let vals = a.values.clone();
    let r = sa::sort::array_multisort(vm, &vals);
    rt(vm, r)?;
    Ok(Value::Bool(true))
}

// ---------------------------------------------------------------------
// stack operations
// ---------------------------------------------------------------------

pub fn array_push(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arr = by_ref(a, 0);
    let vals = rest(a, 1);
    let r = sa::stack::array_push(&arr, &vals);
    Ok(Value::Int(rt(vm, r)?))
}

pub fn array_pop(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = sa::stack::array_pop(&by_ref(a, 0));
    rt(vm, r)
}

pub fn array_shift(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let r = sa::stack::array_shift(&by_ref(a, 0));
    rt(vm, r)
}

pub fn array_unshift(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arr = by_ref(a, 0);
    let vals = rest(a, 1);
    let r = sa::stack::array_unshift(&arr, &vals);
    Ok(Value::Int(rt(vm, r)?))
}

pub fn array_splice(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    // `USED_RET()`: read before anything else can start another call.
    let used_ret = vm.pending_call_dst.is_some() || !called_from_code(vm);
    let arr = by_ref(a, 0);
    let len = a.opt(2).map(|_| a.int(2));
    let offset = a.int(1);
    if used_ret {
        let rep = a.get(3);
        let r = sa::stack::array_splice(vm, &arr, offset, len, rep.as_ref());
        return Ok(Value::Array(rt(vm, r)?));
    }
    // php_splice() without a `removed` table: the removed elements are
    // destroyed in place, and iterator positions are computed without
    // counting them (unlike the `USED_RET()` path php-stdarr implements).
    let repl = match a.get(3) {
        Some(v) => {
            let r = v.to_array(vm);
            Some(rt(vm, r)?)
        }
        None => None,
    };
    let r = sa::util::with_array(&arr, "array_splice", 1, "array", |t| {
        let length = len.unwrap_or_else(|| i64::try_from(t.len()).unwrap_or(i64::MAX));
        let removed = t.make_mut().splice(offset, length, repl.as_deref(), false);
        Ok(removed.unwrap_or_default())
    });
    let removed = rt(vm, r)?;
    // Without a used result PHP destroys the removed elements while the
    // array is being rebuilt (holding an extra reference to it), and fails
    // if a destructor modified or replaced the array meanwhile.
    let held = match &*arr.borrow() {
        Value::Array(z) => Some(z.clone()),
        _ => None,
    };
    drop(removed);
    vm.run_released_destructors()?;
    if let Some(held) = held {
        let same = matches!(&*arr.borrow(), Value::Array(z) if z.addr() == held.addr());
        if !same {
            return Err(vm.throw("Error", "Array was modified during array_splice operation"));
        }
    }
    Ok(Value::Array(ZArray::new()))
}

// ---------------------------------------------------------------------
// merge / set operations
// ---------------------------------------------------------------------

macro_rules! args_fn {
    ($name:ident, $f:path) => {
        pub fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let vals = rest(a, 0);
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

pub fn array_replace(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let vals = rest(a, 0);
    let r = sa::merge::array_replace(&vals);
    Ok(Value::Array(rt(vm, r)?))
}

pub fn array_replace_recursive(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let args = rest(a, 0);
    if args.len() > 1 {
        // The recursion only follows keys holding arrays on both sides.
        stack::check_depth(vm, |cap| {
            let dest = stack::nesting_depth(&args[0], cap + 1);
            let src = args[1..]
                .iter()
                .map(|v| stack::nesting_depth(v, cap + 1))
                .max()
                .unwrap_or(0);
            dest.min(src).saturating_sub(1)
        })?;
    }
    let r = sa::merge::array_replace_recursive(&args);
    Ok(Value::Array(rt(vm, r)?))
}

/// `zend_parse_parameters("+f")` / `("+ff")` for the `u*` diff/intersect
/// variants: at least `ncb + 1` arguments, the trailing `ncb` callbacks
/// validated (unnamed: `Argument #n must be a valid callback`).
fn split_callbacks(vm: &mut Vm, a: &Args, ncb: usize) -> VmResult<(Vec<Value>, Vec<Value>)> {
    let n = a.values.len();
    if n < ncb + 1 {
        let msg = format!(
            "{}() expects at least {} arguments, {n} given",
            a.func_name(),
            ncb + 1
        );
        return Err(vm.throw("ArgumentCountError", &msg));
    }
    let all = rest(a, 0);
    let (arrays, cbs) = all.split_at(n - ncb);
    for (i, cb) in cbs.iter().enumerate() {
        check_callable(vm, a, n - ncb + i + 1, false, false, cb)?;
    }
    let mut arrays = arrays.to_vec();
    // The result starts as `zend_array_dup()` of the first array, which
    // unwraps references only that array holds. php-stdarr copies the
    // elements for sorting first (raising their refcounts), so hand it an
    // already duplicated first array.
    if let Some(Value::Array(first)) = arrays.first_mut() {
        *first = ZArray::from_table((**first).clone());
    }
    Ok((arrays, cbs.to_vec()))
}

macro_rules! ucb_fn {
    ($name:ident, $f:path) => {
        pub fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let (arrays, mut cbs) = split_callbacks(vm, a, 1)?;
            let mut cb = callback(cbs.remove(0));
            let r = $f(vm, &arrays, &mut cb);
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

macro_rules! uucb_fn {
    ($name:ident, $f:path) => {
        pub fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let (arrays, mut cbs) = split_callbacks(vm, a, 2)?;
            let mut key_cb = callback(cbs.remove(1));
            let mut data_cb = callback(cbs.remove(0));
            let r = $f(vm, &arrays, &mut data_cb, &mut key_cb);
            Ok(Value::Array(rt(vm, r)?))
        }
    };
}
uucb_fn!(array_udiff_uassoc, sa::setops::array_udiff_uassoc);
uucb_fn!(array_uintersect_uassoc, sa::setops::array_uintersect_uassoc);

pub fn array_unique(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = a.array(0);
    let r = sa::setops::array_unique(vm, &h, int_or(a, 1, sa::SORT_STRING));
    Ok(Value::Array(rt(vm, r)?))
}

// ---------------------------------------------------------------------
// callbacks
// ---------------------------------------------------------------------

pub fn array_walk(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    walk(vm, a, false)
}

pub fn array_walk_recursive(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    walk(vm, a, true)
}

fn walk(vm: &mut Vm, a: &Args, recursive: bool) -> VmResult<Value> {
    check_array_arg(vm, a, true)?;
    let cb = a.at(1);
    check_callable(vm, a, 2, true, false, &cb)?;
    let arr = by_ref(a, 0);
    if recursive {
        let v = arr.get();
        stack::check_depth(vm, |cap| stack::nesting_depth(&v, cap))?;
    }
    let mut cb = callback(cb);
    let ud = a.get(2);
    let r = if recursive {
        sa::callback::array_walk_recursive(vm, &arr, &mut cb, ud.as_ref())
    } else {
        sa::callback::array_walk(vm, &arr, &mut cb, ud.as_ref())
    };
    rt(vm, r)?;
    Ok(Value::Bool(true))
}

pub fn array_map(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let cbv = a.at(0);
    let arrays = rest(a, 1);
    if cbv.is_null() && arrays.len() > 1 {
        let longest = arrays
            .iter()
            .map(|v| match v {
                Value::Array(x) => x.len() as u64,
                _ => 0,
            })
            .max()
            .unwrap_or(0);
        if !memory::reserve_nested(longest, arrays.len() as u64) {
            return Ok(Value::Null);
        }
    }
    let r = if cbv.is_null() {
        sa::callback::array_map::<Vm>(vm, None, &arrays)
    } else {
        let mut cb = callback(cbv);
        sa::callback::array_map(vm, Some(&mut cb), &arrays)
    };
    Ok(Value::Array(rt(vm, r)?))
}

pub fn array_filter(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = a.array(0);
    let cbv = a.at(1);
    let mode = int_or(a, 2, sa::ARRAY_FILTER_USE_VALUE);
    let r = if cbv.is_null() {
        sa::callback::array_filter::<Vm>(vm, &h, None, mode)
    } else {
        let mut cb = callback(cbv);
        sa::callback::array_filter(vm, &h, Some(&mut cb), mode)
    };
    Ok(Value::Array(rt(vm, r)?))
}

pub fn array_reduce(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = a.array(0);
    let cb = a.at(1);
    let mut cb = callback(cb);
    let init = a.get(2);
    let r = sa::callback::array_reduce(vm, &h, &mut cb, init.as_ref());
    rt(vm, r)
}

macro_rules! find_fn {
    ($name:ident, $f:path, $wrap:expr) => {
        pub fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let h = a.array(0);
            let cb = a.at(1);
            let mut cb = callback(cb);
            let r = $f(vm, &h, &mut cb);
            Ok($wrap(rt(vm, r)?))
        }
    };
}
find_fn!(array_find, sa::callback::array_find, std::convert::identity);
find_fn!(
    array_find_key,
    sa::callback::array_find_key,
    std::convert::identity
);
find_fn!(array_any, sa::callback::array_any, Value::Bool);
find_fn!(array_all, sa::callback::array_all, Value::Bool);

// ---------------------------------------------------------------------
// internal pointer
// ---------------------------------------------------------------------

macro_rules! ptr_fn {
    ($name:ident, $f:path, $op:ident) => {
        pub fn $name(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
            let arr = by_ref(a, 0);
            if let Value::Object(o) = arr.get() {
                return objptr::run(vm, &a.func_name(), &o, objptr::Op::$op);
            }
            let r = $f(vm, &arr);
            rt(vm, r)
        }
    };
}
ptr_fn!(end, sa::pointer::end, End);
ptr_fn!(prev, sa::pointer::prev, Prev);
ptr_fn!(next, sa::pointer::next, Next);
ptr_fn!(reset, sa::pointer::reset, Reset);

pub fn current(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = a.at(0);
    let f = a.func_name();
    if let Value::Object(o) = &v {
        return objptr::run(vm, &f, o, objptr::Op::Current);
    }
    let r = sa::pointer::current(vm, &f, &v);
    rt(vm, r)
}

pub fn key(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let v = a.at(0);
    if let Value::Object(o) = &v {
        return objptr::run(vm, "key", o, objptr::Op::Key);
    }
    let r = sa::pointer::key(vm, &v);
    rt(vm, r)
}

// ---------------------------------------------------------------------
// randomness
// ---------------------------------------------------------------------

pub fn shuffle(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let arr = by_ref(a, 0);
    let r = with_rng(vm, |rng| sa::random::shuffle(&arr, rng));
    rt(vm, r)?;
    Ok(Value::Bool(true))
}

pub fn array_rand(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let h = a.array(0);
    let n = int_or(a, 1, 1);
    let r = with_rng(vm, |rng| sa::random::array_rand(&h, n, rng));
    rt(vm, r)
}

pub fn str_shuffle(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let s = a.str(0);
    let r = with_rng(vm, |rng| sa::random::str_shuffle(s.as_bytes(), rng));
    Ok(Value::Str(ZStr::from(rt(vm, r)?)))
}

// ---------------------------------------------------------------------
// symbol table
// ---------------------------------------------------------------------

/// `zend_forbid_dynamic_call()`.
fn forbid_dynamic_call(vm: &mut Vm, a: &Args) -> VmResult<CallerSymbols> {
    if let Some(s) = CallerSymbols::of_caller(vm) {
        return Ok(s);
    }
    let msg = format!("Cannot call {}() dynamically", a.func_name());
    Err(vm.throw("Error", &msg))
}

pub fn extract(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let sym = forbid_dynamic_call(vm, a)?;
    let table = sym.handle();
    let arr = a.values.first().cloned().unwrap_or_default();
    let prefix = a.get(2).map(|_| a.str(2));
    let r = sa::symbols::extract(
        &mut sym.checked(vm),
        &arr,
        int_or(a, 1, sa::EXTR_OVERWRITE),
        prefix.as_ref(),
    );
    table.finish(vm);
    Ok(Value::Int(rt(vm, r)?))
}

pub fn compact(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let mut sym = forbid_dynamic_call(vm, a)?;
    let vals = rest(a, 0);
    for v in &vals {
        stack::check_depth(vm, |cap| stack::nesting_depth(v, cap))?;
    }
    let r = sa::symbols::compact(vm, &mut sym, &vals);
    Ok(Value::Array(rt(vm, r)?))
}
