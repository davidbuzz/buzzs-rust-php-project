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
        r#"
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
"#
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

/// `concat_function` with an integer operand: one allocation, for the
/// result.
#[test]
fn concat_with_int_allocates_once() {
    assert_allocs("$t = '';", "$t = 'v' . $i;", 1.0);
}

/// `ZEND_ASSIGN_DIM_OP` with `.=`: existing string elements (also through
/// properties and references) are extended in place; new keys, converted
/// keys (with their diagnostics) and shared arrays keep php-src's results.
#[test]
fn concat_assign_on_array_elements_matches_php() {
    assert_eq!(
        output_of(
            r#"<?php
$a = ['k' => 'a', 'n' => ['m' => 'b'], 5 => 'c'];
for ($i = 0; $i < 3; $i++) { $a['k'] .= $i; $a['n']['m'] .= 'x'; $a[5] .= "-"; }
$b = $a;
$r = ($a['k'] .= 'y');
$a['n']['m'] .= $a['n']['m'];
$a[1.5] = 's';
$a[1.5] .= 't';
$a['new'] .= 'z';
echo $r, "\n";
var_dump($a, $b['k']);
class C { public array $arr = ['p' => 'q']; public $dyn; }
$o = new C;
$o->arr['p'] .= 'r';
$o->arr['p'] .= 7;
$o->dyn = ['d' => ''];
$o->dyn['d'] .= 'e';
$ref = &$o->arr;
$ref['p'] .= '!';
echo $o->arr['p'], ' ', $o->dyn['d'], "\n";
$list = ['x'];
foreach ($list as &$v) { $list[0] .= 'y'; }
unset($v);
echo $list[0], "\n";
"#
        ),
        r#"
Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 7

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8

Warning: Undefined array key "new" in t.php on line 9
a012y
array(5) {
  ["k"]=>
  string(5) "a012y"
  ["n"]=>
  array(1) {
    ["m"]=>
    string(8) "bxxxbxxx"
  }
  [5]=>
  string(4) "c---"
  [1]=>
  string(2) "st"
  ["new"]=>
  string(1) "z"
}
string(4) "a012"
qr7! e
xy
"#
    );
}

/// `$a['k'] .= 'x'` costs no more allocations than `$a['k'] = 'x'`: the
/// element grows in place instead of being copied on every append.
#[test]
fn concat_assign_on_array_element_extends_in_place() {
    let setup = "$a = ['k' => ''];";
    let append = per_iteration(setup, "$a['k'] .= 'x';");
    let assign = per_iteration(setup, "$a['k'] = 'x';");
    assert!(
        append <= assign + 0.05,
        "`.=` on an element: {append:.3} allocations per iteration, plain assignment {assign:.3}"
    );
}
