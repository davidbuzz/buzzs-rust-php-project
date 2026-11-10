//! Sorting: `sort`, `rsort`, `asort`, `arsort`, `ksort`, `krsort`,
//! `natsort`, `natcasesort`, `usort`, `uasort`, `uksort` and
//! `array_multisort`.
//!
//! All PHP 8 sorts are stable: ties keep the original order
//! (`stable_sort_fallback`), which [`HashTable::sort_by`] provides.

use crate::compare::{compare_data_by, compare_keys_by, SortKind, UserCompare};
use crate::util::{arg_type_error, arg_value_error, not_array_error, ErrSlot};
use crate::{ArrayContext, Callback, SORT_ASC, SORT_DESC, SORT_FLAG_CASE};
use crate::{SORT_LOCALE_STRING, SORT_NATURAL, SORT_NUMERIC, SORT_REGULAR, SORT_STRING};
use php_runtime::sort::zend_sort;
use php_runtime::{ArrayKey, HashTable, PhpRef, RtResult, RuntimeError, Value, ZArray};

/// Sorts a table in place with one of PHP's internal comparisons
/// (`zend_array_sort` with the function chosen by `php_get_*_compare_func`).
///
/// * `by_key`: compare keys (`ksort`/`krsort`) instead of values;
/// * `reverse`: use the `php_array_reverse_*` comparison;
/// * `renumber`: discard keys and renumber `0..n` (`sort`/`rsort`).
///
/// The table stays sorted (as far as the sort got) even when a comparison
/// throws; the first error is returned.
pub fn sort_table<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    ht: &mut HashTable,
    by_key: bool,
    reverse: bool,
    kind: SortKind,
    renumber: bool,
) -> RtResult<()> {
    let mut slot = ErrSlot::default();
    let regular_values = !by_key && kind == SortKind::Regular;
    ht.sort_by(
        |ka, va, kb, vb| {
            // `zend_compare`'s `TYPE_PAIR(IS_LONG, IS_LONG)` case inline:
            // what `compare_data_by` returns for two ints, without its
            // calls (no comparison runs once one has failed).
            if regular_values && !slot.failed() {
                if let (Value::Int(x), Value::Int(y)) = (va, vb) {
                    let r = i32::from(x > y) - i32::from(x < y);
                    return if reverse { r.wrapping_neg() } else { r };
                }
            }
            slot.run(|| {
                if by_key {
                    compare_keys_by(ctx, kind, reverse, ka, kb)
                } else {
                    compare_data_by(ctx, kind, reverse, va, vb)
                }
            })
        },
        renumber,
    );
    slot.into_result()
}

fn php_sort<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    array: &PhpRef,
    flags: i64,
    by_key: bool,
    reverse: bool,
    renumber: bool,
) -> RtResult<()> {
    let kind = SortKind::from_flags(flags);
    let live = match array.get() {
        Value::Array(a) => !by_key && can_observe_sort(&a),
        other => return Err(not_array_error(func, 1, Some("array"), &other)),
    };
    if live {
        return sort_live(ctx, array, kind, reverse, renumber);
    }
    // No PHP code can run and nothing can reach the array while it is
    // sorted, so it is sorted outside the reference, separated first when
    // shared (`Z_PARAM_ARRAY_HT_EX(array, 0, 1)`).
    let mut work = match array.set(Value::Null) {
        Value::Array(a) => a,
        other => {
            array.set(other);
            return Ok(());
        }
    };
    let res = sort_table(ctx, work.make_mut(), by_key, reverse, kind, renumber);
    array.set(Value::Array(work));
    res
}

/// Whether anything can observe a value sort of `a` while it runs: PHP
/// code started by a comparison (`__toString`, a compare handler that
/// initializes a lazy object, an error handler for "Array to string
/// conversion") or a comparison reaching the array through a reference
/// (`$a[0] =& $a`). Only objects, arrays and references can lead there;
/// comparing scalars never runs PHP code. Key sorts compare keys only.
fn can_observe_sort(a: &ZArray) -> bool {
    a.values()
        .any(|v| matches!(v, Value::Array(_) | Value::Object(_) | Value::Reference(_)))
}

/// A value sort that PHP code or a self-reference may observe: php-src
/// sorts the live table that the variable keeps holding, with its hash
/// index reset (`zend_array_sort` on the by-reference argument), which
/// [`HashTable::sort_in_ref`] reproduces. Comparisons reaching the array
/// mid-sort see its current, half-sorted order and find none of its keys
/// (`Zend/tests/bug63882_2.phpt`); a write from PHP code separates it and
/// the sorted table is dropped (`Zend/tests/gh16648.phpt`).
#[inline(never)]
fn sort_live<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    array: &PhpRef,
    kind: SortKind,
    reverse: bool,
    renumber: bool,
) -> RtResult<()> {
    let mut slot = ErrSlot::default();
    HashTable::sort_in_ref(
        array,
        |_, va, _, vb| slot.run(|| compare_data_by(ctx, kind, reverse, va, vb)),
        renumber,
    );
    slot.into_result()
}

/// `sort(array &$array, int $flags = SORT_REGULAR): true`.
pub fn sort<C: ArrayContext + ?Sized>(ctx: &mut C, array: &PhpRef, flags: i64) -> RtResult<()> {
    php_sort(ctx, "sort", array, flags, false, false, true)
}

/// `rsort(array &$array, int $flags = SORT_REGULAR): true`.
pub fn rsort<C: ArrayContext + ?Sized>(ctx: &mut C, array: &PhpRef, flags: i64) -> RtResult<()> {
    php_sort(ctx, "rsort", array, flags, false, true, true)
}

/// `asort(array &$array, int $flags = SORT_REGULAR): true`.
pub fn asort<C: ArrayContext + ?Sized>(ctx: &mut C, array: &PhpRef, flags: i64) -> RtResult<()> {
    php_sort(ctx, "asort", array, flags, false, false, false)
}

/// `arsort(array &$array, int $flags = SORT_REGULAR): true`.
pub fn arsort<C: ArrayContext + ?Sized>(ctx: &mut C, array: &PhpRef, flags: i64) -> RtResult<()> {
    php_sort(ctx, "arsort", array, flags, false, true, false)
}

/// `ksort(array &$array, int $flags = SORT_REGULAR): true`.
pub fn ksort<C: ArrayContext + ?Sized>(ctx: &mut C, array: &PhpRef, flags: i64) -> RtResult<()> {
    php_sort(ctx, "ksort", array, flags, true, false, false)
}

/// `krsort(array &$array, int $flags = SORT_REGULAR): true`.
pub fn krsort<C: ArrayContext + ?Sized>(ctx: &mut C, array: &PhpRef, flags: i64) -> RtResult<()> {
    php_sort(ctx, "krsort", array, flags, true, true, false)
}

/// `natsort(array &$array): true`.
pub fn natsort<C: ArrayContext + ?Sized>(ctx: &mut C, array: &PhpRef) -> RtResult<()> {
    php_sort(ctx, "natsort", array, SORT_NATURAL, false, false, false)
}

/// `natcasesort(array &$array): true`.
pub fn natcasesort<C: ArrayContext + ?Sized>(ctx: &mut C, array: &PhpRef) -> RtResult<()> {
    php_sort(
        ctx,
        "natcasesort",
        array,
        SORT_NATURAL | SORT_FLAG_CASE,
        false,
        false,
        false,
    )
}

/// Sorts a table with a user comparison (`zend_array_sort` with
/// `php_array_user_compare` / `php_array_user_key_compare`), for reuse by
/// SPL's `uasort`/`uksort`. `func` names the PHP function for the
/// bool-return deprecation.
pub fn user_sort_table<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    ht: &mut HashTable,
    callback: &mut Callback<'_, C>,
    by_key: bool,
    renumber: bool,
) -> RtResult<()> {
    let mut uc = UserCompare::new(func, callback);
    let mut slot = ErrSlot::default();
    ht.sort_by(
        |ka, va, kb, vb| {
            slot.run(|| {
                if by_key {
                    uc.compare(ctx, &ka.to_value(), &kb.to_value())
                } else {
                    uc.compare(ctx, va, vb)
                }
            })
        },
        renumber,
    );
    slot.into_result()
}

fn php_usort<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    func: &str,
    array: &PhpRef,
    callback: &mut Callback<'_, C>,
    by_key: bool,
    renumber: bool,
) -> RtResult<()> {
    let arr = match array.get() {
        Value::Array(a) => a,
        other => return Err(not_array_error(func, 1, Some("array"), &other)),
    };
    if arr.is_empty() {
        return Ok(());
    }
    // Copy the array so that in-place modifications are not visible to the
    // callback (`zend_array_dup`).
    let mut copy: HashTable = (*arr).clone();
    drop(arr);
    let res = user_sort_table(ctx, func, &mut copy, callback, by_key, renumber);
    array.set(Value::Array(ZArray::from_table(copy)));
    res
}

/// `usort(array &$array, callable $callback): true`.
pub fn usort<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    array: &PhpRef,
    callback: &mut Callback<'_, C>,
) -> RtResult<()> {
    php_usort(ctx, "usort", array, callback, false, true)
}

/// `uasort(array &$array, callable $callback): true`.
pub fn uasort<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    array: &PhpRef,
    callback: &mut Callback<'_, C>,
) -> RtResult<()> {
    php_usort(ctx, "uasort", array, callback, false, false)
}

/// `uksort(array &$array, callable $callback): true`.
pub fn uksort<C: ArrayContext + ?Sized>(
    ctx: &mut C,
    array: &PhpRef,
    callback: &mut Callback<'_, C>,
) -> RtResult<()> {
    php_usort(ctx, "uksort", array, callback, true, false)
}

/// `array_multisort(&$array, &...$rest): true`.
///
/// `args` are the arguments as passed: arrays (by reference when the
/// caller passed a variable, `Value::Reference`, so the sorted result is
/// written back) and `SORT_*` flag integers. Arrays passed by value are
/// sorted but the result is discarded, as in PHP.
#[allow(clippy::too_many_lines)]
pub fn array_multisort<C: ArrayContext + ?Sized>(ctx: &mut C, args: &[Value]) -> RtResult<()> {
    const F: &str = "array_multisort";
    let name = |i: usize| if i == 0 { Some("array") } else { None };
    // (argument index, comparison kind, reverse)
    let mut arrays: Vec<usize> = Vec::new();
    let mut funcs: Vec<(SortKind, bool)> = Vec::new();
    let mut order_allowed = false;
    let mut type_allowed = false;
    let mut sort_order = SORT_ASC;
    let mut sort_type = SORT_REGULAR;
    for (i, arg) in args.iter().enumerate() {
        match crate::util::deref(arg) {
            Value::Array(_) => {
                if i > 0 {
                    funcs.push((SortKind::from_flags(sort_type), sort_order != SORT_ASC));
                    sort_order = SORT_ASC;
                    sort_type = SORT_REGULAR;
                }
                arrays.push(i);
                order_allowed = true;
                type_allowed = true;
            }
            Value::Int(flag) => match flag & !SORT_FLAG_CASE {
                SORT_ASC | SORT_DESC => {
                    if order_allowed {
                        sort_order = if flag == SORT_DESC {
                            SORT_DESC
                        } else {
                            SORT_ASC
                        };
                        order_allowed = false;
                    } else {
                        return Err(arg_type_error(
                            F,
                            i + 1,
                            name(i),
                            "must be an array or a sort flag that has not already been specified",
                        ));
                    }
                }
                SORT_REGULAR | SORT_NUMERIC | SORT_STRING | SORT_NATURAL | SORT_LOCALE_STRING => {
                    if type_allowed {
                        sort_type = flag;
                        type_allowed = false;
                    } else {
                        return Err(arg_type_error(
                            F,
                            i + 1,
                            name(i),
                            "must be an array or a sort flag that has not already been specified",
                        ));
                    }
                }
                _ => {
                    return Err(arg_value_error(
                        F,
                        i + 1,
                        name(i),
                        "must be a valid sort flag",
                    ));
                }
            },
            _ => {
                return Err(arg_type_error(
                    F,
                    i + 1,
                    name(i),
                    "must be an array or a sort flag",
                ))
            }
        }
    }
    let tables: Vec<ZArray> = arrays
        .iter()
        .map(|&i| match crate::util::deref(&args[i]) {
            Value::Array(a) => a,
            _ => ZArray::new(),
        })
        .collect();
    let Some(first) = tables.first() else {
        return Ok(());
    };
    let size = first.len();
    if tables.iter().any(|t| t.len() != size) {
        return Err(RuntimeError::ValueError(
            "Array sizes are inconsistent".to_string(),
        ));
    }
    if size == 0 {
        return Ok(());
    }
    funcs.push((SortKind::from_flags(sort_type), sort_order != SORT_ASC));

    // rows[k][i] = (key, value) of the k-th element of array i.
    let columns: Vec<Vec<(ArrayKey, Value)>> = tables
        .iter()
        .map(|t| t.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .collect();
    let mut order: Vec<usize> = (0..size).collect();
    let mut slot = ErrSlot::default();
    zend_sort(&mut order, &mut |&a: &usize, &b: &usize| {
        slot.run(|| {
            for (i, &(kind, reverse)) in funcs.iter().enumerate() {
                let r = compare_data_by(ctx, kind, reverse, &columns[i][a].1, &columns[i][b].1)?;
                if r != 0 {
                    return Ok(if r > 0 { 1 } else { -1 });
                }
            }
            Ok(i32::from(a > b) - i32::from(a < b))
        })
    });
    if let Some(e) = slot.0 {
        return Err(e);
    }
    for (i, &argi) in arrays.iter().enumerate() {
        let packed = tables[i].is_packed();
        let mut out = HashTable::with_capacity(size);
        let mut n: i64 = 0;
        for &k in &order {
            let (key, val) = &columns[i][k];
            match key {
                ArrayKey::Str(s) if !packed => {
                    out.insert(ArrayKey::Str(s.clone()), val.clone());
                }
                _ => {
                    out.insert(ArrayKey::Int(n), val.clone());
                    n += 1;
                }
            }
        }
        out.set_next_free_element(i64::try_from(size).unwrap_or(i64::MAX));
        if let Value::Reference(r) = &args[argi] {
            // PHP restructures the original tables in place; when user code
            // (an object comparison) replaced the variable meanwhile, the
            // result is lost with the old table (gh22678).
            let unchanged = r
                .try_borrow()
                .is_some_and(|v| matches!(&*v, Value::Array(a) if ZArray::ptr_eq(a, &tables[i])));
            if unchanged {
                r.set(Value::Array(ZArray::from_table(out)));
            }
        }
    }
    Ok(())
}
