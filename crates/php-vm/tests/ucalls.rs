//! The user-call path: arguments bound into CVs when the frame is pushed
//! (`i_init_func_execute_data`), `RECV` skipping and in-place verification,
//! the `ZEND_INIT_METHOD_CALL` run-time cache and closure variable binding.
//! Expected outputs are php-src's (`/usr/bin/php -n`).
use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::Write;
use std::rc::Rc;

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

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    let output = String::from_utf8(output).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

/// What `arguments_bound_at_frame_push_match_recv_semantics` prints in
/// php-src (`/usr/bin/php -n`).
const BOUND_ARGS_EXPECTED: &str = concat!(
    r#"1:a:1:{i:0;i:1;}:a:3:{i:0;i:1;i:1;i:2;i:2;a:1:{i:0;i:3;}}
2:a:2:{i:0;i:1;i:1;i:5;}:a:3:{i:0;i:1;i:1;i:5;i:2;a:1:{i:0;i:3;}}
4:a:4:{i:0;i:1;i:1;i:5;i:2;i:6;i:3;i:7;}:a:3:{i:0;i:1;i:1;i:5;i:2;i:6;}
3:a:3:{i:0;i:0;i:1;i:2;i:2;i:9;}:a:3:{i:0;i:0;i:1;i:2;i:2;i:9;}
Too few arguments to function plain(), 0 passed in t.php on line 4 and at least 1 expected
gap(): Argument #2 ($b) not passed
a:2:{i:0;s:7:"changed";i:1;i:2;}
int(5)
string(1) "d"
NULL
int(7)
string(1) "8"
NULL
int(1)
string(1) "x"
array(1) {
  [0]=>
  int(1)
}
typed(): Argument #1 ($a) must be of type int, string given, called in t.php on line 11
a:1:{i:0;s:1:"x";}
typed(): Argument #2 ($b) must be of type string, array given, called in t.php on line 12
a:2:{i:0;i:1;i:1;a:0:{}}
array(3) {
  [0]=>
  int(1)
  [1]=>
  int(2)
  [2]=>
  int(3)
}
int(1)
array(0) {
}
int(1)
int(1)
array(2) {
  [0]=>
  int(2)
  [1]=>
  int(3)
}
int(3)
int(1)
array(1) {
  ["x"]=>
  int(2)
}
int(1)
abc
[toString]string(3) "str"
int(1)
"#,
    // `echo $v, " "` ends both generator lines in a space: spelled out
    // so that no editor strips it.
    "10 2 1 \n10 20 2 \n",
    r#"a:1:{i:0;i:0;}abc01
a:1:{i:0;i:1;}abc0111
a:1:{i:0;i:2;}abc011121
69
3
2
O:1:"P":2:{s:1:"x";i:1;s:1:"y";i:3;}O:1:"P":2:{s:1:"x";s:1:"a";s:1:"y";i:4;}
20
1275
"#
);

#[test]
fn arguments_bound_at_frame_push_match_recv_semantics() {
    assert_eq!(
        run(r#"<?php
function plain($a, $b = 2, $c = [3]) { echo func_num_args(), ":", serialize(func_get_args()), ":", serialize([$a, $b, $c]), "\n"; }
plain(1); plain(1, 5); plain(1, 5, 6, 7); plain(c: 9, a: 0);
try { plain(); } catch (ArgumentCountError $e) { echo $e->getMessage(), "\n"; }
function gap($a, $b, $c = 3) { echo $a, $b, $c, "\n"; }
try { gap(1, c: 4); } catch (ArgumentCountError $e) { echo $e->getMessage(), "\n"; }
function trace($a, $b) { $a = "changed"; return debug_backtrace()[0]["args"]; }
echo serialize(trace(1, 2)), "\n";
function typed(int $a, string $b = "d", ?array $c = null) { var_dump($a, $b, $c); }
typed("5"); typed(7, 8); typed(1.0, "x", [1]);
try { typed("x"); } catch (TypeError $e) { echo $e->getMessage(), "\n", serialize($e->getTrace()[0]["args"]), "\n"; }
try { typed(1, []); } catch (TypeError $e) { echo $e->getMessage(), "\n", serialize($e->getTrace()[0]["args"]), "\n"; }
function mixed_($a, int $b, $c) { var_dump(func_get_args()); }
mixed_(1, "2", 3);
function variadic($a, ...$rest) { var_dump($a, $rest, func_num_args()); }
variadic(1); variadic(1, 2, 3); variadic(1, x: 2);
function byref(&$a, $b) { $a .= $b; }
$s = "a"; byref($s, "b"); byref($s, "c"); echo $s, "\n";
class S { function __toString(): string { echo "[toString]"; return "str"; } }
function strparam(string $s, $t) { var_dump($s, $t); }
strparam(new S, 1);
function gen($a, $b = 2) { yield $a; yield $b; yield func_num_args(); }
foreach (gen(10) as $v) echo $v, " "; echo "\n";
foreach (gen(10, 20) as $v) echo $v, " "; echo "\n";
$f = function ($x, $y = 1) use (&$s) { $s .= $x . $y; return func_get_args(); };
for ($i = 0; $i < 3; $i++) { echo serialize($f($i)), $s, "\n"; }
$g = fn($x) => $x * $i;
echo $g(2), $g(3), "\n";
function counter() { static $n = 0; return ++$n; }
counter(); counter(); echo counter(), "\n";
$h = function () { static $k = 0; return ++$k; };
$h(); echo $h(), "\n";
class P { public function __construct(public $x, public int $y = 3) {} }
echo serialize(new P(1)), serialize(new P("a", "4")), "\n";
function many($a, $b) { return $a + $b; }
$t = 0; for ($i = 0; $i < 5; $i++) { $t += many($i, $i); } echo $t, "\n";
function rec($n) { return $n ? $n + rec($n - 1) : 0; }
echo rec(50), "\n";
"#),
        BOUND_ARGS_EXPECTED
    );
}

/// What `return_releases_in_zend_leave_helper_order` prints in php-src
/// (`/usr/bin/php -n`).
const LEAVE_ORDER_EXPECTED: &str = r#"in f
d:arg-a
d:cv-b
d:cv-c
d:extra1
d:extra2
after f
in m
d:m-arg
d:m-cv-b
d:m-extra
M gone
after m
d:g-cv
d:g-extra
after g
d:g-arg
in closure
d:cl-arg
d:cl-cv
d:cl-extra
after closure call
d:use-x
in v
d:v-a
d:v-cv
d:v-r1
d:v-r2
after v
in n
d:n-a
d:n-c
d:n-cv
after n
leave 0
d:rec-0
leave 1
d:rec-1
leave 2
d:rec-2
leave 3
d:rec-3
leave 4
d:rec-4
leave 5
d:rec-5
leave 6
d:rec-6
leave 7
d:rec-7
leave 8
d:rec-8
leave 9
d:rec-9
after rec
d:t-cv
d:mid-cv
caught
d:t-arg
d:t-extra
d:mid-arg
d:mid-extra
after catch
got
gen resumed
d:gen-cv
d:gen-extra
d:gen-arg
after gen
in __call undefined
d:call-a
d:call-b
d:call-cv
end
"#;

#[test]
fn return_releases_in_zend_leave_helper_order() {
    // `zend_leave_helper`: a returning frame releases its compiled
    // variables in order (`i_free_compiled_variables`), then the extra
    // named and the extra positional arguments, then `$this` or the
    // closure; a frame left by an exception, a frame dropped beyond the
    // pool (`rec`) and a generator's frame release in the same order.
    assert_eq!(
        run(r#"<?php
class D { function __construct(public $n) {} function __destruct() { echo "d:{$this->n}\n"; } }
function f($a) { $b = new D('cv-b'); $c = new D('cv-c'); echo "in f\n"; }
f(new D('arg-a'), new D('extra1'), new D('extra2'));
echo "after f\n";
class M { function m($a) { $b = new D('m-cv-b'); echo "in m\n"; } function __destruct() { echo "M gone\n"; } }
(new M)->m(new D('m-arg'), new D('m-extra'));
echo "after m\n";
function g($a) { $b = new D('g-cv'); return $a; }
$r = g(new D('g-arg'), new D('g-extra'));
echo "after g\n";
$r = null;
$x = new D('use-x');
$cl = function ($p) use ($x) { $y = new D('cl-cv'); echo "in closure\n"; };
$x = null;
$cl(new D('cl-arg'), new D('cl-extra'));
echo "after closure call\n";
$cl = null;
function v($a, ...$rest) { $b = new D('v-cv'); echo "in v\n"; }
v(new D('v-a'), new D('v-r1'), new D('v-r2'));
echo "after v\n";
function n($a, $b = null, $c = null) { $z = new D('n-cv'); echo "in n\n"; }
n(c: new D('n-c'), a: new D('n-a'));
echo "after n\n";
function rec($n) { $o = new D("rec-$n"); if ($n > 0) rec($n - 1); echo "leave $n\n"; }
rec(9);
echo "after rec\n";
function thrower($a) { $b = new D('t-cv'); throw new Exception('x'); }
function mid($a) { $m = new D('mid-cv'); thrower(new D('t-arg'), new D('t-extra')); }
try { mid(new D('mid-arg'), new D('mid-extra')); } catch (Exception $e) { echo "caught\n"; }
$e = null;
echo "after catch\n";
function gen($a) { $b = new D('gen-cv'); yield $a; echo "gen resumed\n"; }
$g = gen(new D('gen-arg'), new D('gen-extra'));
foreach ($g as $v) { echo "got\n"; }
$v = null;
$g = null;
echo "after gen\n";
class C { function __call($name, $args) { $l = new D('call-cv'); echo "in __call $name\n"; } }
(new C)->undefined(new D('call-a'), new D('call-b'));
echo "end\n";
"#),
        LEAVE_ORDER_EXPECTED
    );
}

#[test]
fn method_cache_respects_class_scope_and_visibility() {
    assert_eq!(
        run(r#"<?php
class A {
    private function f() { return "A::f"; }
    protected function p() { return "A::p"; }
    public function callF(A $o) { return $o->f(); }
    public function callP(A $o) { return $o->p(); }
    public function __call($n, $a) { return "A::__call($n)"; }
}
class B extends A {
    public function f() { return "B::f"; }
    public function F2() { return "B::F2"; }
}
class C { public function f() { return "C::f"; } private function priv() {} 
          public static function st() { return isset($this) ? "this" : "static"; } }
$a = new A; $b = new B; $c = new C;
for ($i = 0; $i < 3; $i++) {
    echo $a->callF($b), " ", $b->f(), " ", $b->callF($b), " ", $a->callF($a), " ", $c->f(), "\n";
    echo $a->f(), " ", $a->p(), " ", $b->callP($b), " ", $b->f2(), " ", $b->F2(), " ", $b->F2(), " ", $c->st(), "\n";
    try { $c->priv(); } catch (Error $e) { echo $e->getMessage(), "\n"; }
}
abstract class Base { abstract function m(); function run() { return $this->m(); } }
class X1 extends Base { function m() { return "X1"; } }
class X2 extends Base { function m() { return "X2"; } }
foreach ([new X1, new X2, new X1, new X2] as $o) echo $o->run(); echo "\n";
$cl = function () { return $this->f(); };
echo Closure::bind($cl, $b, A::class)(), " ", Closure::bind($cl, $b, B::class)(), " ", Closure::bind($cl, $b, A::class)(), "\n";
$m = "F"; echo $b->$m(), $b->{"f"}(), "\n";
class LongNames { function thisIsAVeryLongMethodNameThatExceedsTheCacheLimitForSure() { return "long"; } }
$l = new LongNames; echo $l->thisIsAVeryLongMethodNameThatExceedsTheCacheLimitForSure(), $l->THISISAVERYLONGMETHODNAMETHATEXCEEDSTHECACHELIMITFORSURE(), "\n";
"#),
        r"A::f B::f A::f A::f C::f
A::__call(f) A::__call(p) A::p B::F2 B::F2 B::F2 static
Call to private method C::priv() from global scope
A::f B::f A::f A::f C::f
A::__call(f) A::__call(p) A::p B::F2 B::F2 B::F2 static
Call to private method C::priv() from global scope
A::f B::f A::f A::f C::f
A::__call(f) A::__call(p) A::p B::F2 B::F2 B::F2 static
Call to private method C::priv() from global scope
X1X2X1X2
A::f B::f A::f
B::fB::f
longlong
"
    );
}

#[test]
fn named_argument_gaps_stop_recv_skipping() {
    // A gap left by named arguments stops the skipping: the default of a
    // later parameter is still evaluated, and a missing one is reported.
    assert_eq!(
        run(r#"<?php
function d() { echo "[d]"; return 7; }
function f($a, $b = D, $c = 3) { return "$a/$b/$c"; }
const D = 5;
echo f(1), " ", f(1, 2), " ", f(1, c: 9), " ", f(c: 0, a: 4), "\n";
function g($a, $b) { return $a . $b; }
try { echo g(b: 1); } catch (ArgumentCountError $e) { echo $e->getMessage(), "\n"; }
"#),
        "1/5/3 1/2/3 1/5/9 4/5/0\ng(): Argument #1 ($a) not passed\n"
    );
}

#[test]
fn method_cache_evictions_keep_lookups_exact() {
    // More (class, scope, name) triples than the cache holds: every lookup
    // must still resolve as `zend_std_get_method` would, including private
    // methods reached from their own scope and refused from another one.
    let classes = (0..100).fold(String::new(), |mut out, i| {
        writeln!(
            out,
            "class K{i} {{ function m() {{ return {i}; }} private function p() {{ return -{i}; }} function q($o) {{ return $o->p(); }} }}"
        )
        .expect("String write");
        out
    });
    let src = format!(
        r#"<?php
{classes}$objs = [];
for ($i = 0; $i < 100; $i++) {{ $c = "K$i"; $objs[] = new $c; }}
$sum = 0;
for ($r = 0; $r < 3; $r++) {{ foreach ($objs as $o) {{ $sum += $o->m() + $o->M() + $o->q($o); }} }}
echo $sum, "\n";
try {{ $objs[0]->p(); }} catch (Error $e) {{ echo $e->getMessage(), "\n"; }}
try {{ $objs[1]->q($objs[2]); }} catch (Error $e) {{ echo $e->getMessage(), "\n"; }}
"#
    );
    assert_eq!(
        run(&src),
        "14850\nCall to private method K0::p() from global scope\nCall to private method K2::p() from scope K1\n"
    );
}
