//! `.=` on properties, array elements and static properties extends the
//! string in place: php-src's `ZEND_ASSIGN_OBJ_OP`, `ZEND_ASSIGN_DIM_OP` and
//! `ZEND_ASSIGN_STATIC_PROP_OP` with `ZEND_CONCAT` run `concat_function` on
//! the fetched slot (`result == op1`), so `zend_string_extend` grows a string
//! nobody else holds, instead of copying it into a new string that a second
//! walk writes back. The outputs are byte for byte what `/usr/bin/php -n`
//! (8.3) prints; the property-hook and `private(set)` script, which 8.3
//! cannot run, is checked against the debug build before this change. The
//! allocation counts compare `.=` with the plain assignment of the same
//! place: in-place growth costs no allocation per append.

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

/// Allocations per iteration of the loop `script` builds for an iteration
/// count (two counts, so compilation and request startup cancel out).
fn per_iteration(script: impl Fn(u64) -> String) -> f64 {
    let _ = allocations_of(&script(10));
    let a1 = allocations_of(&script(1_000));
    let a2 = allocations_of(&script(3_000));
    #[allow(clippy::cast_precision_loss)]
    let per = (a2 as f64 - a1 as f64) / 2_000.0;
    per
}

/// `body` in a top-level loop after `setup`.
fn top_level<'a>(setup: &'a str, body: &'a str) -> impl Fn(u64) -> String + 'a {
    move |n| format!("<?php\n{setup}\n$n = {n};\nfor ($i = 0; $i < $n; $i++) {{ {body} }}\n")
}

/// `body` in a loop inside a method of a class with `members`, after
/// `prelude`.
fn in_method<'a>(prelude: &'a str, members: &'a str, body: &'a str) -> impl Fn(u64) -> String + 'a {
    move |n| {
        format!(
            "<?php\n{prelude}\nclass A {{ {members}\n  function run($n) {{ for ($i = 0; $i < $n; $i++) {{ {body} }} }} }}\n(new A)->run({n});\n"
        )
    }
}

#[track_caller]
fn assert_allocs(script: impl Fn(u64) -> String, expected: f64) {
    let per = per_iteration(&script);
    assert!(
        (per - expected).abs() < 0.05,
        "{}: {per:.3} allocations per iteration, expected {expected}",
        script(1).trim()
    );
}

/// Asserts that `.=` on a place (loop body `concat`) costs no more
/// allocations per iteration than the plain assignment of the same place
/// (loop body `assign`).
#[track_caller]
fn assert_no_costlier_than_assign(make: impl Fn(&str) -> f64, concat: &str, assign: &str) {
    let c = make(concat);
    let a = make(assign);
    assert!(
        c <= a + 0.05,
        "`{concat}`: {c:.3} allocations per iteration, `{assign}`: {a:.3}"
    );
}

/// Properties: declared (untyped, typed, private), static (untyped and
/// typed), the chains `$this->o->s` and `$list[$i]->name`, an element of
/// a property, stdClass, the result value, self-append, copy-on-write with
/// a shared string and with a clone; and the cases that keep the generic
/// path: a nullable property holding null, an int property (coercion),
/// readonly, other right-hand types, `__get`/`__set` after `unset`, a
/// property held by reference, an undefined dynamic property, an error
/// handler, and the memory released after a long loop.
#[test]
fn concat_assign_on_properties_matches_php() {
    assert_eq!(
        output_of(
            r#"<?php
class P {
    public $s = 'a';
    public string $t = 'b';
    public ?string $n = null;
    public int $i = 1;
    public readonly string $ro;
    private $priv = 'p';
    public static $st = 's';
    public static string $ts = 'T';
    public $o;
    public $arr = ['k' => 'v'];
    function __construct() { $this->ro = 'r'; $this->o = new stdClass; $this->o->s = 'o'; }
    function run() {
        for ($k = 0; $k < 3; $k++) {
            $this->s .= $k; $this->t .= 'x'; $this->priv .= '!';
            self::$st .= '.'; self::$ts .= '-'; $this->o->s .= 'O'; $this->arr['k'] .= $k;
        }
        $r = ($this->s .= 'R');
        $this->s .= $this->s;
        echo $r, ' ', $this->s, ' ', $this->t, ' ', $this->priv, ' ', self::$st, ' ', self::$ts, ' ', $this->o->s, ' ', $this->arr['k'], "\n";
        $this->n .= 'z';
        var_dump($this->n);
        $this->i .= '5';
    }
}
$p = new P;
try { $p->run(); } catch (TypeError $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
try { $p->ro .= 'x'; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
$p->s = 'a';
$p->s .= [];
$p->s .= 1.5;
$p->s .= null; $p->s .= true; $p->s .= false; $p->s .= -7;
var_dump($p->s);
class M { public $p = 'm'; function __get($n) { echo "get $n\n"; return 'G'; } function __set($n, $v) { echo "set $n=$v\n"; } }
$m = new M; $m->p .= 'x'; echo $m->p, "\n"; unset($m->p); $m->p .= 'y'; var_dump(isset($m->p));
$o = new stdClass; $o->s = 'a'; $x = &$o->s; $o->s .= 'b'; echo $x, "\n";
$o->t = 'q'; $copy = $o->t; $o->t .= 'w'; echo $copy, ' ', $o->t, "\n";
$c = clone $o; $o->t .= '!'; echo $c->t, ' ', $o->t, "\n";
$d = new stdClass; $d->v = ''; for ($k = 0; $k < 5; $k++) { $d->v .= 'k'; } echo $d->v, "\n";
$d->u .= 'first';
var_dump($d->u);
$list = [new stdClass, new stdClass];
foreach ($list as $i => $e) { $e->name = "n$i"; }
for ($k = 0; $k < 2; $k++) { foreach ($list as $i => $e) { $list[$i]->name .= $k; } }
echo $list[0]->name, ' ', $list[1]->name, "\n";
class T { public $v = 'tv'; function __toString(): string { return '[T]'; } }
$p->s = 'x'; $p->s .= new T; echo $p->s, "\n";
$u = new stdClass; $u->s = 'u';
function h($no, $msg) { echo "handler: $msg\n"; return true; }
set_error_handler('h');
$u->s .= $undef;
$u->w .= 'w';
echo $u->s, ' ', $u->w, "\n";
restore_error_handler();
for ($pass = 0; $pass < 2; $pass++) {
    if ($pass == 1) { $mem0 = memory_get_usage(); }
    $q = new stdClass; $q->s = '';
    for ($k = 0; $k < 10000; $k++) { $q->s .= 'abcdefgh'; }
    $len = strlen($q->s);
    unset($q);
}
var_dump($len, memory_get_usage() - $mem0 < 4096);
"#
        ),
        r#"a012R a012Ra012R bxxx p!!! s... T--- oOOO v012
string(1) "z"
Error: Cannot modify readonly property P::$ro

Warning: Array to string conversion in t.php on line 31
string(12) "aArray1.51-7"
mx
get p
set p=Gy
bool(false)
ab
q qw
qw qw!
kkkkk

Warning: Undefined property: stdClass::$u in t.php on line 41
string(5) "first"
n001 n101
x[T]
handler: Undefined variable $undef
handler: Undefined property: stdClass::$w
u w
int(80000)
bool(true)
"#
    );
}

/// Elements: existing elements (also nested, through a typed array
/// property, a reference to the array, a `foreach` by reference, and a
/// static array property), the result value, self-append, copy-on-write,
/// and the cases that keep the generic path: new keys (with their
/// warning), converted keys (with their deprecation, also under an error
/// handler), `[]`, string offsets, an element held by reference, an array
/// right-hand side, and `ArrayAccess`.
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
$a['n']['new2'] .= 'y';
$a[] .= 'app';
echo $r, "\n";
var_dump($a, $b['k']);
class C {
    public array $arr = ['p' => 'q'];
    public $dyn;
    public static $st = 's';
    public static string $ts = 'T';
    public static array $sa = ['k' => 'v'];
    function run() {
        for ($i = 0; $i < 3; $i++) { $this->arr['p'] .= $i; self::$st .= '.'; static::$ts .= '-'; self::$sa['k'] .= '+'; }
        echo $this->arr['p'], ' ', self::$st, ' ', self::$ts, ' ', self::$sa['k'], "\n";
    }
}
(new C)->run();
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
$s = 'str';
try { $s[0] .= 'x'; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
$rk = &$a['k']; $a['k'] .= 'R'; echo $rk, "\n";
$a['k'] .= $a['k'];
echo $a['k'], "\n";
$a['arr'] = [1]; $a['arr'] .= 'x';
var_dump($a['arr']);
function h($no, $msg) { echo "handler: $msg\n"; return true; }
set_error_handler('h');
$u = []; $u['x'] .= 'hx'; var_dump($u);
$u[1.5] = 's'; $u[1.5] .= 't'; var_dump($u);
$u['x'] .= $undef2;
var_dump($u['x']);
restore_error_handler();
$ao = new ArrayObject(['k' => 'v']);
$ao['k'] .= 'w';
var_dump($ao['k']);
"#
        ),
        r#"
Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 7

Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8

Warning: Undefined array key "new" in t.php on line 9

Warning: Undefined array key "new2" in t.php on line 10
a012y
array(6) {
  ["k"]=>
  string(5) "a012y"
  ["n"]=>
  array(2) {
    ["m"]=>
    string(8) "bxxxbxxx"
    ["new2"]=>
    string(1) "y"
  }
  [5]=>
  string(4) "c---"
  [1]=>
  string(2) "st"
  ["new"]=>
  string(1) "z"
  [6]=>
  string(3) "app"
}
string(4) "a012"
q012 s... T--- v+++
qr7! e
xy
Error: Cannot use assign-op operators with string offsets
a012yR
a012yRa012yR

Warning: Array to string conversion in t.php on line 43
string(6) "Arrayx"
handler: Undefined array key "x"
array(1) {
  ["x"]=>
  string(2) "hx"
}
handler: Implicit conversion from float 1.5 to int loses precision
handler: Implicit conversion from float 1.5 to int loses precision
array(2) {
  ["x"]=>
  string(2) "hx"
  [1]=>
  string(2) "st"
}
handler: Undefined variable $undef2
string(2) "hx"
string(2) "vw"
"#
    );
}

/// An error handler that throws on the diagnostic of the fetch (a
/// converted key, an undefined key) or of the right-hand side (an
/// undefined variable, an array conversion) leaves the place as it was:
/// the in-place path only runs when nothing is pending, so the exception
/// still comes before the operation.
#[test]
fn throwing_error_handler_leaves_the_place_unchanged() {
    assert_eq!(
        output_of(
            r#"<?php
function thrower($no, $msg) { throw new Exception("handler: $msg"); }
set_error_handler('thrower');
$a = [1 => 'one'];
try { $a[1.5] .= 'x'; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
var_dump($a);
try { $a['nope'] .= 'x'; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
var_dump($a);
$o = new stdClass;
$o->s = 's';
try { $o->s .= $undef; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
var_dump($o->s);
try { $o->s .= []; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
var_dump($o->s);
class W { public $w = 'w'; public $arr = ['k' => 'v']; function run() {
    try { $this->w .= $nope; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
    try { $this->arr['k'] .= [1]; } catch (Exception $e) { echo $e->getMessage(), "\n"; }
    var_dump($this->w, $this->arr);
} }
(new W)->run();
"#
        ),
        r#"handler: Implicit conversion from float 1.5 to int loses precision
array(1) {
  [1]=>
  string(3) "one"
}
handler: Undefined array key "nope"
array(1) {
  [1]=>
  string(3) "one"
}
handler: Undefined variable $undef
string(1) "s"
handler: Array to string conversion
string(1) "s"
handler: Undefined variable $nope
handler: Array to string conversion
string(1) "w"
array(1) {
  ["k"]=>
  string(1) "v"
}
"#
    );
}

/// PHP 8.4 properties: a `set` hook still runs for `.=` (the hook writes
/// the backing value), `private(set)` still forbids `.=` from outside the
/// class and allows it inside, after the places have been used in loops.
/// Expected output from the debug build before this change (8.3 cannot
/// run hooks).
#[test]
fn hooked_and_set_restricted_properties_take_the_generic_path() {
    assert_eq!(
        output_of(
            r#"<?php
class H {
    public string $h = '' { set { echo "hook\n"; $this->h = strtoupper($value); } }
    public private(set) string $ps = 'p';
    public string $plain = '';
    function run() { for ($i = 0; $i < 2; $i++) { $this->ps .= 'x'; $this->plain .= 'y'; } echo $this->ps, ' ', $this->plain, "\n"; }
}
$h = new H;
for ($i = 1; $i <= 2; $i++) { $h->h .= "a$i"; echo $h->h, "\n"; }
$h->run();
try { $h->ps .= 'z'; } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
"#
        ),
        "hook\nA1\nhook\nA1A2\npxx yy\n\
Error: Cannot modify private(set) property H::$ps from global scope\n"
    );
}

/// `.=` on a cached declared property (untyped and typed) and on a
/// stdClass property: amortised in-place growth, no allocation per append
/// (the plain assignment of a literal allocates nothing either).
#[test]
fn concat_assign_on_cached_properties_extends_in_place() {
    assert_allocs(in_method("", "public $s = '';", "$this->s .= 'x';"), 0.0);
    assert_allocs(
        in_method("", "public string $t = '';", "$this->t .= 'x';"),
        0.0,
    );
    assert_allocs(
        top_level("$o = new stdClass; $o->v = '';", "$o->v .= 'x';"),
        0.0,
    );
}

/// Elements, nested elements, elements of a cached property, a static
/// property and a property of a walked object: `.=` costs no more
/// allocations than `=` of the same place (the string grows in place
/// instead of being copied and written back by a second walk).
#[test]
fn concat_assign_on_elements_statics_and_chains_extends_in_place() {
    assert_no_costlier_than_assign(
        |body| per_iteration(top_level("$a = ['k' => ''];", body)),
        "$a['k'] .= 'x';",
        "$a['k'] = 'x';",
    );
    assert_no_costlier_than_assign(
        |body| per_iteration(top_level("$a = ['n' => ['m' => '']];", body)),
        "$a['n']['m'] .= 'x';",
        "$a['n']['m'] = 'x';",
    );
    assert_no_costlier_than_assign(
        |body| per_iteration(in_method("", "public $arr = ['k' => ''];", body)),
        "$this->arr['k'] .= 'x';",
        "$this->arr['k'] = 'x';",
    );
    assert_no_costlier_than_assign(
        |body| per_iteration(in_method("", "public static $st = '';", body)),
        "self::$st .= 'x';",
        "self::$st = 'x';",
    );
    assert_no_costlier_than_assign(
        |body| {
            per_iteration(in_method(
                "class B { public $s = ''; }",
                "public $o; function __construct() { $this->o = new B; }",
                body,
            ))
        },
        "$this->o->s .= 'x';",
        "$this->o->s = 'x';",
    );
}
