//! String building in the VM: `.=` on a compiled variable extends the
//! string in place (`zend_string_extend` on a refcount-1 string), ropes
//! (`"a $b c"`, `ZEND_ROPE_*`) build their result in one exactly sized
//! allocation, and int/float operands are converted without temporary
//! strings. The output checks are byte for byte what php-src prints; the
//! allocation counts are php-src's (`zend_string_alloc` once per result).

use php_vm::{Vm, VmOptions};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::rc::Rc;

/// Counts allocations and reallocations per thread.
struct Counting;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
}

fn bump() {
    // `try_with`: allocations during thread teardown are not counted.
    let _ = ALLOCS.try_with(|x| x.set(x.get() + 1));
}

// SAFETY: forwards every call to `System` unchanged; the counter does not
// affect the memory handed out.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump();
        // SAFETY: caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump();
        // SAFETY: as above.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System` with `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump();
        // SAFETY: caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn output_of(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// Allocations (including reallocations) of one script run.
fn allocations_of(src: &str) -> u64 {
    let a0 = ALLOCS.with(Cell::get);
    let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
    let status = vm.run_script(src.as_bytes(), "t.php");
    drop(vm);
    assert_eq!(status, 0, "script failed: {src}");
    ALLOCS.with(Cell::get) - a0
}

/// Allocations per loop iteration of `body` (two iteration counts, so
/// compilation and request startup cancel out).
fn per_iteration(setup: &str, body: &str) -> f64 {
    let script =
        |n: u64| format!("<?php\n{setup}\n$n = {n};\nfor ($i = 0; $i < $n; $i++) {{ {body} }}\n");
    let _ = allocations_of(&script(10));
    let a1 = allocations_of(&script(1_000));
    let a2 = allocations_of(&script(3_000));
    #[allow(clippy::cast_precision_loss)]
    let per = (a2 as f64 - a1 as f64) / 2_000.0;
    per
}

#[track_caller]
fn assert_allocs(setup: &str, body: &str, expected: f64) {
    let per = per_iteration(setup, body);
    assert!(
        (per - expected).abs() < 0.05,
        "`{body}`: {per:.3} allocations per iteration, expected {expected}"
    );
}

/// Interpolation reads its parts in order (`ZEND_ROPE_ADD`): conversions
/// and "Undefined variable" warnings interleave as in php-src, with each
/// part's line; `.=` with int and string operands; `precision` handling
/// of float-to-string conversion.
#[test]
fn string_building_matches_php() {
    assert_eq!(
        output_of(
            r#"<?php
$arr = [1];
$t = new class { function __toString(): string { echo "[ts]"; return "t"; } };
echo "$arr $undef\n";
echo "$t $undef2 $t\n";
$i = 42; $len = 7;
echo "item $i of {$len}\n";
echo <<<EOT
a $undef3
b $i
c $undef4
EOT;
echo "\n";
$s = "";
for ($k = 0; $k < 5; $k++) { $s .= $k; $s .= "-"; }
echo $s, "\n";
$r = ($s .= "end");
echo $r, " ", $s, "\n";
$copy = $s; $s .= "!"; echo $copy, " ", $s, "\n";
$n = -15; $x = 1.5; $two = 2.0; $big = 1e100; $p3 = 0.1 + 0.2; $min = PHP_INT_MIN;
echo "v" . $n, " ", "x" . $x, " ", $two . "", " ", $big . "", " ", $p3 . "", " ", $min . "", "\n";
$f = 1/3; $g = -2.5e-7; $h = 123456789012345678.0; $z = -0.0; $inf = -INF;
echo "$f {$g} $h $z $inf\n";
$tenth = 0.1; $q = 1234.5; $e = 0.125;
ini_set('precision', 17); echo $tenth . "", " ", $p3 . "", "\n";
ini_set('precision', -1); echo $tenth . "", " ", $f . "", "\n";
ini_set('precision', 3); echo $q . "", " ", $e . "", " ", "$q", "\n";
ini_set('precision', 0); echo $q . "", "\n";
ini_set('precision', 25); echo $tenth . "", "\n";
"#
        ),
        r"
Warning: Array to string conversion in t.php on line 4

Warning: Undefined variable $undef in t.php on line 4
Array 
[ts]
Warning: Undefined variable $undef2 in t.php on line 5
[ts]t  t
item 42 of 7

Warning: Undefined variable $undef3 in t.php on line 9

Warning: Undefined variable $undef4 in t.php on line 11
a 
b 42
c 
0-1-2-3-4-
0-1-2-3-4-end 0-1-2-3-4-end
0-1-2-3-4-end 0-1-2-3-4-end!
v-15 x1.5 2 1.0E+100 0.3 -9223372036854775808
0.33333333333333 -2.5E-7 1.2345678901235E+17 -0 -INF
0.10000000000000001 0.30000000000000004
0.1 0.3333333333333333
1.23E+3 0.125 1.23E+3
1.0E+3
0.1000000000000000055511151
"
    );
}

/// `$s .= 'x'` and `$s .= $i` on a compiled variable: amortised in-place
/// growth, no allocation per append.
#[test]
fn concat_assign_on_cv_extends_in_place() {
    assert_allocs("$s = '';", "$s .= 'x';", 0.0);
    assert_allocs("$s = '';", "$s .= $i;", 0.0);
}

/// `ZEND_ROPE_END`: one `zend_string_alloc` for the result, integer
/// parts formatted straight into it.
#[test]
fn rope_allocates_once() {
    assert_allocs("$len = 7; $t = '';", "$t = \"item $i of {$len}\";", 1.0);
}

/// `concat_function` with an integer or float operand: one allocation,
/// for the result (the number is formatted on the stack).
#[test]
fn concat_with_number_allocates_once() {
    assert_allocs("$t = '';", "$t = 'v' . $i;", 1.0);
    assert_allocs("$t = ''; $x = 1.5;", "$t = 'v' . $x;", 1.0);
    assert_allocs("$t = ''; $x = 0.1 + 0.2;", "$t = $x . 'v';", 1.0);
}

/// `ZEND_ECHO` of an integer prints its digits (no string is built).
#[test]
fn echo_of_ints_matches_php() {
    assert_eq!(
        output_of(
            r#"<?php $i = -15; echo $i, " ", 0, " ", PHP_INT_MIN, " ", PHP_INT_MAX, " ", 1234567, "\n";"#
        ),
        "-15 0 -9223372036854775808 9223372036854775807 1234567\n"
    );
}

/// `zend_compile_encaps_list`: an interpolated part is converted to a
/// string as soon as it has been evaluated (`ZEND_ROPE_ADD`), on its own
/// line and before any later part runs code, so "Array to string
/// conversion" warnings, `__toString` calls and their exceptions interleave
/// with method calls as in php-src; an opcode consuming a multi-line call's
/// result reports the last argument's line (`CG(zend_lineno)` after the
/// call); a one-part interpolation is a plain string cast. Expected output
/// from `php -n` 8.3.
#[test]
fn interpolation_converts_parts_in_php_order() {
    assert_eq!(
        output_of(
            r#"<?php
class O { function m() { echo "[m]"; return "M"; } function arr($a = 0) { return [1]; }
  function __toString() { echo "[ts]"; return "S"; } }
class B { function __toString() { throw new Exception("boom"); } }
$arr = [1]; $o = new O; $i = 7; $bb = new stdClass; $bb->x = new B;
echo "a $arr b {$o->m()} c\n";
echo "p $o q {$o->m()} r $arr\n";
$s = <<<EOT
one $arr
two $i
three {$o->m()}
four $undef
EOT;
echo $s, "\n";
try { echo "e $arr f {$bb->x} g {$o->m()} h\n"; } catch (Exception $e) { echo "caught ", $e->getMessage(), " at ", $e->getLine(), "\n"; }
echo "u $nope v {$o->m()} w\n";
echo "x {$o->arr(
  1,
  2
)} y $i\n";
echo "A $arr
B {$o->m()} C $arr
D\n";
echo "$arr", "|", "$o", "|", "$i", "\n";
$k = "idx"; $a2 = ["idx" => [5]];
echo "n {$a2["$k"]} m $arr\n";
echo $o->arr(
  "a"
), "\n";
function g(): int { return strtoupper(
  "a"
); }
try { g(); } catch (TypeError $e) { echo $e->getMessage(), " at ", $e->getLine(), "\n"; }
"#
        ),
        r"
Warning: Array to string conversion in t.php on line 6
[m]a Array b M c
[ts][m]
Warning: Array to string conversion in t.php on line 7
p S q M r Array

Warning: Array to string conversion in t.php on line 9
[m]
Warning: Undefined variable $undef in t.php on line 12
one Array
two 7
three M
four 

Warning: Array to string conversion in t.php on line 15
caught boom at 4

Warning: Undefined variable $nope in t.php on line 16
[m]u  v M w

Warning: Array to string conversion in t.php on line 19
x Array y 7

Warning: Array to string conversion in t.php on line 21
[m]
Warning: Array to string conversion in t.php on line 22
A Array
B M C Array
D

Warning: Array to string conversion in t.php on line 24
Array|[ts]S|7

Warning: Array to string conversion in t.php on line 26

Warning: Array to string conversion in t.php on line 26
n Array m Array

Warning: Array to string conversion in t.php on line 28
Array
g(): Return value must be of type int, string returned at 31
"
    );
}

/// `"$str"` is a string cast (`zend_compile_rope_finalize` with one
/// element): a reference to the same string, no allocation.
#[test]
fn single_part_interpolation_allocates_nothing() {
    assert_allocs("$s = 'abc'; $t = '';", "$t = \"$s\";", 0.0);
}
