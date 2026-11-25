// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ext/standard` array functions (`array.c`) for the phprust VM.
//!
//! The algorithms live in `php-stdarr`; this crate binds them through the
//! `php_vm::ext` API: stub signatures (by-reference `&$array` parameters,
//! variadics), `Z_PARAM_FUNC`-style callback validation, the caller's
//! symbol table for `extract()` / `compact()`, the request RNG for
//! `shuffle()` / `array_rand()` / `str_shuffle()`, and the `SORT_*`,
//! `COUNT_*`, `EXTR_*`, `CASE_*` and `ARRAY_FILTER_USE_*` constants.
//!
//! Signatures follow `basic_functions.stub.php`, with two deliberate
//! deviations where PHP's argument order matters:
//!
//! * variadic array parameters (`array ...$arrays`) are declared untyped so
//!   php-stdarr reports "Argument #N must be of type array" without a
//!   parameter name, as PHP does; `array_map()`'s `$array` likewise, so the
//!   callback is validated first;
//! * the callback of `usort()`/`uasort()`/`uksort()`/`array_walk()`/
//!   `array_walk_recursive()` is untyped and validated in the handler,
//!   because the by-reference array before it must be checked first.

mod bridge;
mod funcs;
mod memory;
mod objptr;
mod stack;
mod symtab;

use funcs as f;
use php_runtime::Value;
use php_vm::ext::{ClassBuilder, Extension, ModuleBuilder, NativeFn};

/// The array part of `ext/standard`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdArray;

/// `(stub signature, handler)` of every array function.
const FUNCTIONS: &[(&str, NativeFn)] = &[
    ("count(Countable|array $value, int $mode = COUNT_NORMAL): int", f::count),
    ("sizeof(Countable|array $value, int $mode = COUNT_NORMAL): int", f::count),
    ("in_array(mixed $needle, array $haystack, bool $strict = false): bool", f::in_array),
    ("array_search(mixed $needle, array $haystack, bool $strict = false): int|string|false", f::array_search),
    ("array_keys(array $array, mixed $filter_value = UNKNOWN, bool $strict = false): array", f::array_keys),
    ("array_values(array $array): array", f::array_values),
    ("array_count_values(array $array): array", f::array_count_values),
    ("array_flip(array $array): array", f::array_flip),
    ("array_change_key_case(array $array, int $case = CASE_LOWER): array", f::array_change_key_case),
    ("array_key_first(array $array): int|string|null", f::array_key_first),
    ("array_key_last(array $array): int|string|null", f::array_key_last),
    ("array_first(array $array): mixed", f::array_first),
    ("array_last(array $array): mixed", f::array_last),
    ("array_is_list(array $array): bool", f::array_is_list),
    ("array_key_exists($key, array $array): bool", f::array_key_exists),
    ("key_exists($key, array $array): bool", f::array_key_exists),
    ("array_fill(int $start_index, int $count, mixed $value): array", f::array_fill),
    ("array_fill_keys(array $keys, mixed $value): array", f::array_fill_keys),
    ("array_pad(array $array, int $length, mixed $value): array", f::array_pad),
    ("array_reverse(array $array, bool $preserve_keys = false): array", f::array_reverse),
    ("array_slice(array $array, int $offset, ?int $length = null, bool $preserve_keys = false): array", f::array_slice),
    ("array_chunk(array $array, int $length, bool $preserve_keys = false): array", f::array_chunk),
    ("array_combine(array $keys, array $values): array", f::array_combine),
    ("array_column(array $array, int|string|null $column_key, int|string|null $index_key = null): array", f::array_column),
    ("array_sum(array $array): int|float", f::array_sum),
    ("array_product(array $array): int|float", f::array_product),
    ("min(mixed $value, mixed ...$values): mixed", f::min),
    ("max(mixed $value, mixed ...$values): mixed", f::max),
    ("clamp(mixed $value, mixed $min, mixed $max): mixed", f::clamp),
    ("range(string|int|float $start, string|int|float $end, int|float $step = 1): array", f::range),
    ("sort(array &$array, int $flags = SORT_REGULAR): true", f::sort),
    ("rsort(array &$array, int $flags = SORT_REGULAR): true", f::rsort),
    ("asort(array &$array, int $flags = SORT_REGULAR): true", f::asort),
    ("arsort(array &$array, int $flags = SORT_REGULAR): true", f::arsort),
    ("ksort(array &$array, int $flags = SORT_REGULAR): true", f::ksort),
    ("krsort(array &$array, int $flags = SORT_REGULAR): true", f::krsort),
    ("natsort(array &$array): true", f::natsort),
    ("natcasesort(array &$array): true", f::natcasesort),
    ("usort(array &$array, $callback): true", f::usort),
    ("uasort(array &$array, $callback): true", f::uasort),
    ("uksort(array &$array, $callback): true", f::uksort),
    ("array_multisort(@prefer-ref &$array, @prefer-ref &...$rest): true", f::array_multisort),
    ("array_push(array &$array, mixed ...$values): int", f::array_push),
    ("array_pop(array &$array): mixed", f::array_pop),
    ("array_shift(array &$array): mixed", f::array_shift),
    ("array_unshift(array &$array, mixed ...$values): int", f::array_unshift),
    ("array_splice(array &$array, int $offset, ?int $length = null, mixed $replacement = []): array", f::array_splice),
    ("array_merge(...$arrays): array", f::array_merge),
    ("array_merge_recursive(@unchecked array ...$arrays): array", f::array_merge_recursive),
    ("array_replace(array $array, @unchecked array ...$replacements): array", f::array_replace),
    ("array_replace_recursive(array $array, @unchecked array ...$replacements): array", f::array_replace_recursive),
    ("array_unique(array $array, int $flags = SORT_STRING): array", f::array_unique),
    ("array_intersect(array $array, @unchecked array ...$arrays): array", f::array_intersect),
    ("array_intersect_key(array $array, @unchecked array ...$arrays): array", f::array_intersect_key),
    ("array_intersect_assoc(array $array, @unchecked array ...$arrays): array", f::array_intersect_assoc),
    ("array_diff(array $array, @unchecked array ...$arrays): array", f::array_diff),
    ("array_diff_key(array $array, @unchecked array ...$arrays): array", f::array_diff_key),
    ("array_diff_assoc(array $array, @unchecked array ...$arrays): array", f::array_diff_assoc),
    ("array_udiff(@unchecked array $array, ...$rest): array", f::array_udiff),
    ("array_uintersect(@unchecked array $array, ...$rest): array", f::array_uintersect),
    ("array_diff_ukey(@unchecked array $array, ...$rest): array", f::array_diff_ukey),
    ("array_intersect_ukey(@unchecked array $array, ...$rest): array", f::array_intersect_ukey),
    ("array_diff_uassoc(@unchecked array $array, ...$rest): array", f::array_diff_uassoc),
    ("array_intersect_uassoc(@unchecked array $array, ...$rest): array", f::array_intersect_uassoc),
    ("array_udiff_assoc(@unchecked array $array, ...$rest): array", f::array_udiff_assoc),
    ("array_uintersect_assoc(@unchecked array $array, ...$rest): array", f::array_uintersect_assoc),
    ("array_udiff_uassoc(@unchecked array $array, ...$rest): array", f::array_udiff_uassoc),
    ("array_uintersect_uassoc(@unchecked array $array, ...$rest): array", f::array_uintersect_uassoc),
    ("array_walk(array|object &$array, $callback, mixed $arg = UNKNOWN): true", f::array_walk),
    ("array_walk_recursive(array|object &$array, $callback, mixed $arg = UNKNOWN): true", f::array_walk_recursive),
    ("array_map(?callable $callback, $array, ...$arrays): array", f::array_map),
    ("array_filter(array $array, ?callable $callback = null, int $mode = 0): array", f::array_filter),
    ("array_reduce(array $array, callable $callback, mixed $initial = null): mixed", f::array_reduce),
    ("array_find(array $array, callable $callback): mixed", f::array_find),
    ("array_find_key(array $array, callable $callback): mixed", f::array_find_key),
    ("array_any(array $array, callable $callback): bool", f::array_any),
    ("array_all(array $array, callable $callback): bool", f::array_all),
    ("end(array|object &$array): mixed", f::end),
    ("prev(array|object &$array): mixed", f::prev),
    ("next(array|object &$array): mixed", f::next),
    ("reset(array|object &$array): mixed", f::reset),
    ("current(array|object $array): mixed", f::current),
    ("pos(array|object $array): mixed", f::current),
    ("key(array|object $array): int|string|null", f::key),
    ("shuffle(array &$array): true", f::shuffle),
    ("array_rand(array $array, int $num = 1): int|string|array", f::array_rand),
    ("str_shuffle(string $string): string", f::str_shuffle),
    ("extract(@prefer-ref array &$array, int $flags = 0, string $prefix = \"\"): int", f::extract),
    ("compact($var_name, ...$var_names): array", f::compact),
];

/// The integer constants of `array.c`.
const CONSTANTS: &[(&str, i64)] = &[
    ("EXTR_OVERWRITE", php_stdarr::EXTR_OVERWRITE),
    ("EXTR_SKIP", php_stdarr::EXTR_SKIP),
    ("EXTR_PREFIX_SAME", php_stdarr::EXTR_PREFIX_SAME),
    ("EXTR_PREFIX_ALL", php_stdarr::EXTR_PREFIX_ALL),
    ("EXTR_PREFIX_INVALID", php_stdarr::EXTR_PREFIX_INVALID),
    ("EXTR_PREFIX_IF_EXISTS", php_stdarr::EXTR_PREFIX_IF_EXISTS),
    ("EXTR_IF_EXISTS", php_stdarr::EXTR_IF_EXISTS),
    ("EXTR_REFS", php_stdarr::EXTR_REFS),
    ("SORT_ASC", php_stdarr::SORT_ASC),
    ("SORT_DESC", php_stdarr::SORT_DESC),
    ("SORT_REGULAR", php_stdarr::SORT_REGULAR),
    ("SORT_NUMERIC", php_stdarr::SORT_NUMERIC),
    ("SORT_STRING", php_stdarr::SORT_STRING),
    ("SORT_LOCALE_STRING", php_stdarr::SORT_LOCALE_STRING),
    ("SORT_NATURAL", php_stdarr::SORT_NATURAL),
    ("SORT_FLAG_CASE", php_stdarr::SORT_FLAG_CASE),
    ("CASE_LOWER", php_stdarr::CASE_LOWER),
    ("CASE_UPPER", php_stdarr::CASE_UPPER),
    ("COUNT_NORMAL", php_stdarr::COUNT_NORMAL),
    ("COUNT_RECURSIVE", php_stdarr::COUNT_RECURSIVE),
    ("ARRAY_FILTER_USE_VALUE", php_stdarr::ARRAY_FILTER_USE_VALUE),
    ("ARRAY_FILTER_USE_BOTH", php_stdarr::ARRAY_FILTER_USE_BOTH),
    ("ARRAY_FILTER_USE_KEY", php_stdarr::ARRAY_FILTER_USE_KEY),
];

impl Extension for StdArray {
    fn name(&self) -> &'static str {
        "standard"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        // The array functions are part of ext/standard, which the VM
        // already reports as loaded: do not list it twice.
        let vm = m.vm();
        if php_vm::LOADED_EXTENSIONS.contains(&"standard") {
            if let Some(p) = vm.loaded_extensions.iter().rposition(|e| e == "standard") {
                vm.loaded_extensions.remove(p);
            }
        }
        for &(sig, handler) in FUNCTIONS {
            m.function_static(sig, handler);
        }
        for &(name, value) in CONSTANTS {
            if name == "SORT_LOCALE_STRING" {
                m.constant_deprecated(
                    name,
                    Value::Int(value),
                    "8.6",
                    "use one of the Collator::*sort*() methods instead",
                );
            } else {
                m.constant(name, Value::Int(value));
            }
        }
        register_sort_direction(m);
    }
}

#[cfg(test)]
mod tests;

/// Declares the `SortDirection` unit enum (`basic_functions.stub.php`).
fn register_sort_direction(m: &mut ModuleBuilder<'_>) {
    let mut b = ClassBuilder::new("SortDirection").unit_enum();
    for case in ["Ascending", "Descending"] {
        b = b.case(case, None);
    }
    b.register(m.vm());
}
