//! Heap-allocation budgets of interpreter hot paths.
//!
//! php-src executes these paths without touching the allocator: the VM stack
//! holds call frames (`zend_vm_stack_push_call_frame`), CVs and TMPs live in
//! the frame (`ZEND_CALL_VAR_NUM`), longs and doubles are stored inline in the
//! zval, declared properties are slots of the object (`OBJ_PROP` through the
//! `CACHE_ADDR` offset), and packed-array writes on an unshared array do not
//! separate. Each test runs a PHP loop at two iteration counts and asserts the
//! exact number of allocations per iteration (the difference cancels compile
//! and request startup/shutdown). Tests named `*_target` encode the php-src
//! count that phprust does not meet yet; they are `#[ignore]`d with the
//! perf/PLAN.md stage that fixes them, so `cargo test` stays green and
//! `cargo test -p php-vm --test alloc_hot_paths -- --include-ignored` shows
//! the gap. When a stage lands, remove its `#[ignore]`.

use php_vm::{Vm, VmOptions};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// Counts allocations (`alloc`, `alloc_zeroed`) and growth (`realloc`) per
/// thread, so the test harness's parallel threads do not disturb each other.
struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static REALLOCS: Cell<u64> = const { Cell::new(0) };
}

fn bump(c: &'static std::thread::LocalKey<Cell<u64>>) {
    // `try_with`: allocations during thread teardown are not counted.
    let _ = c.try_with(|x| x.set(x.get() + 1));
}

// SAFETY: forwards every call to `System` unchanged; the counters do not
// affect the memory handed out.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump(&ALLOCS);
        // SAFETY: caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump(&ALLOCS);
        // SAFETY: as above.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System` with `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump(&REALLOCS);
        // SAFETY: caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations and reallocations of one complete script run (VM creation,
/// compile, execution, shutdown).
fn run(src: &str) -> (u64, u64) {
    let a0 = ALLOCS.with(Cell::get);
    let r0 = REALLOCS.with(Cell::get);
    let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
    let status = vm.run_script(src.as_bytes(), "alloc_hot_paths.php");
    drop(vm);
    assert_eq!(status, 0, "script failed: {src}");
    (ALLOCS.with(Cell::get) - a0, REALLOCS.with(Cell::get) - r0)
}

const N1: u64 = 1_000;
const N2: u64 = 3_000;

/// Heap allocations (alloc + realloc) per loop iteration of `body`, with
/// `setup` before the loop. `$n` is the iteration count.
fn per_iteration(setup: &str, body: &str) -> f64 {
    let script =
        |n: u64| format!("<?php\n{setup}\n$n = {n};\nfor ($i = 0; $i < $n; $i++) {{ {body} }}\n");
    // Warm-up run: first-use caches (interned strings, per-thread tables).
    let _ = run(&script(10));
    let (a1, r1) = run(&script(N1));
    let (a2, r2) = run(&script(N2));
    #[allow(clippy::cast_precision_loss)]
    let per = ((a2 + r2) as f64 - (a1 + r1) as f64) / (N2 - N1) as f64;
    per
}

#[track_caller]
fn assert_allocs(setup: &str, body: &str, expected: u32) {
    let per = per_iteration(setup, body);
    assert!(
        (per - f64::from(expected)).abs() < 0.05,
        "`{body}`: {per:.3} heap allocations per iteration, php-src fast path does {expected}"
    );
}

// ---- paths that already meet the php-src count (must stay green) --------

/// `ZEND_PRE_INC_LONG` / `ZEND_IS_SMALLER_LONG` + `ZEND_JMPNZ`: the loop itself.
#[test]
fn zend_empty_loop_allocates_nothing() {
    assert_allocs("", "", 0);
}

/// `ZEND_ADD`/`ZEND_MUL`/`ZEND_MOD` long fast paths (`fast_long_add_function`).
#[test]
fn zend_add_long_allocates_nothing() {
    assert_allocs("$s = 0;", "$s += $i * 2 % 7;", 0);
}

/// `ZEND_MUL_DOUBLE`/`ZEND_ADD_DOUBLE`: doubles live in the zval.
#[test]
fn zend_add_double_allocates_nothing() {
    assert_allocs("$x = 1.5; $y = 0.0;", "$y = $y + $x * 1.0001;", 0);
}

/// `ZEND_FETCH_DIM_R` (`zend_fetch_dimension_address_inner_R`, packed fast path).
#[test]
#[ignore = "perf target: perf/proposals/01 (place segments without a heap Vec), then PLAN.md stage 6"]
fn zend_fetch_dim_r_packed_allocates_nothing_target() {
    assert_allocs(
        "$a = [1, 2, 3, 4, 5, 6, 7, 8]; $s = 0;",
        "$s = $a[$i & 7];",
        0,
    );
}

/// `ZEND_FETCH_DIM_R` with a constant string key (`zend_hash_find_known_hash`).
#[test]
#[ignore = "perf target: perf/proposals/01 (place segments without a heap Vec), then PLAN.md stage 6"]
fn zend_fetch_dim_r_const_key_allocates_nothing_target() {
    assert_allocs(
        "$a = ['alpha' => 1, 'beta' => 2]; $s = 0;",
        "$s = $a['beta'];",
        0,
    );
}

/// `ZEND_ASSIGN_DIM` on an unshared packed array (no separation).
#[test]
#[ignore = "perf target: perf/proposals/01 (place segments without a heap Vec), then PLAN.md stage 6"]
fn zend_assign_dim_packed_allocates_nothing_target() {
    assert_allocs("$a = [0, 0, 0, 0, 0, 0, 0, 0];", "$a[$i & 7] = $i;", 0);
}

/// `ZEND_IS_SMALLER` + `ZEND_JMPZ` (smart branch) with a CV assignment.
#[test]
fn zend_smart_branch_allocates_nothing() {
    assert_allocs("$s = 0;", "if ($i < 10) { $s = 1; }", 0);
}

// ---- php-src counts phprust does not meet yet (perf/PLAN.md stages) ----

/// `ZEND_INIT_FCALL` + `ZEND_SEND_VAL` + `ZEND_DO_UCALL` + `ZEND_RECV` +
/// `ZEND_RETURN`: the frame is pushed on the VM stack, args are written into
/// its slots. No heap allocation.
#[test]
#[ignore = "perf target: PLAN.md stage 3 (frames on a VM stack) and stage 4 (call protocol)"]
fn zend_do_ucall_allocates_nothing_target() {
    assert_allocs("function f($a, $b) { return $a; }", "f($i, 2);", 0);
}

/// Typed parameters and return (`ZEND_RECV` with `ZEND_VERIFY_RETURN_TYPE`).
#[test]
#[ignore = "perf target: PLAN.md stages 3-4"]
fn zend_do_ucall_typed_allocates_nothing_target() {
    assert_allocs(
        "function f(int $a, int $b): int { return $a; }",
        "f($i, 2);",
        0,
    );
}

/// `ZEND_INIT_METHOD_CALL` (runtime-cache hit) + `ZEND_DO_UCALL`.
#[test]
#[ignore = "perf target: PLAN.md stages 3-5"]
fn zend_init_method_call_allocates_nothing_target() {
    assert_allocs(
        "class C { function m($x) { return $x; } } $o = new C;",
        "$o->m(1);",
        0,
    );
}

/// `ZEND_INIT_STATIC_METHOD_CALL` (runtime-cache hit) + `ZEND_DO_UCALL`.
#[test]
#[ignore = "perf target: PLAN.md stages 3-5"]
fn zend_init_static_method_call_allocates_nothing_target() {
    assert_allocs(
        "class C { static function m($x) { return $x; } }",
        "C::m(1);",
        0,
    );
}

/// Internal function call (`ZEND_DO_ICALL`): php-src passes the arguments
/// in the callee frame on the VM stack.
#[test]
fn zend_do_icall_allocates_nothing() {
    assert_allocs("$s = 'hello';", "$n = strlen($s);", 0);
}

/// `ZEND_FETCH_OBJ_R` of a declared property through the `CACHE_ADDR`
/// property-offset slot.
#[test]
#[ignore = "perf target: perf/proposals/01 (place segments without a heap Vec)"]
fn zend_fetch_obj_r_declared_allocates_nothing_target() {
    assert_allocs(
        "class Q { public $p = 3; } $o = new Q; $s = 0;",
        "$s = $o->p;",
        0,
    );
}

/// `ZEND_ASSIGN_OBJ` of a declared property (`zend_assign_to_variable` into
/// `OBJ_PROP`).
#[test]
#[ignore = "perf target: perf/proposals/01 (place segments without a heap Vec)"]
fn zend_assign_obj_declared_allocates_nothing_target() {
    assert_allocs("class Q { public $p = 3; } $o = new Q;", "$o->p = $i;", 0);
}

/// `ZEND_CONCAT` of two short strings: exactly one `zend_string_alloc`.
#[test]
#[ignore = "perf target: PLAN.md stage 2 (single-allocation zend_string layout)"]
fn zend_concat_allocates_once_target() {
    assert_allocs("$a = 'abc'; $b = 'def';", "$c = $a . $b;", 1);
}

/// `ZEND_NEW` + constructor: one allocation for the object with its
/// property table inline (`zend_objects_new` + `object_properties_init`).
#[test]
#[ignore = "perf target: PLAN.md stages 2, 3 and 5"]
fn zend_new_allocates_once_target() {
    assert_allocs(
        "class R { public $a; function __construct($a) { $this->a = $a; } }",
        "$o = new R($i);",
        1,
    );
}

/// `ZEND_FE_FETCH_R` over a packed array: no allocation per element.
#[test]
fn zend_fe_fetch_r_allocates_nothing() {
    // One foreach of 8 elements per iteration: FE_RESET + 8 FE_FETCH + FE_FREE.
    assert_allocs(
        "$a = [1, 2, 3, 4, 5, 6, 7, 8]; $s = 0;",
        "foreach ($a as $v) { $s += $v; }",
        0,
    );
}

/// Packed-array append (`zend_hash_next_index_insert` doubling the table):
/// building an N-element array needs O(log N) allocations, not O(N).
#[test]
#[ignore = "perf target: perf/proposals/01 (place segments without a heap Vec), then PLAN.md stage 6"]
fn zend_hash_packed_append_amortises_target() {
    let build =
        |n: u32| format!("<?php\n$a = [];\nfor ($i = 0; $i < {n}; $i++) {{ $a[] = $i; }}\n");
    let _ = run(&build(8));
    let (a1, r1) = run(&build(1 << 10));
    let (a2, r2) = run(&build(1 << 16));
    let extra = (a2 + r2).saturating_sub(a1 + r1);
    // Six more doublings, plus slack for the hash-index side table.
    assert!(
        extra <= 8,
        "64x more appends cost {extra} more allocations (expected <= 8)"
    );
}
