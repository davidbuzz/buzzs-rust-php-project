// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The interpreter's property and object hit paths (`exec/tape/props.rs`,
//! `exec/spec/props.rs`, `new` in `exec/tape/calls.rs`, `instanceof` and
//! `??`), each run at least three times in a function so that the cache
//! fill, the hit and the declines after it all run. Expected output from
//! `/usr/bin/php8.5 -n`.

mod common;

use php_vm::VmOptions;
use std::cell::RefCell;
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
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// `$obj->p = v` (`ZEND_ASSIGN_OBJ`): untyped, typed with coercion and a
/// TypeError, nullable, readonly, private from outside and inside, an
/// uninitialized typed slot, a property a reference points into, `__set`
/// for an inaccessible and an undeclared name, dynamic properties, an
/// unset declared slot, and the assignment's own value.
#[test]
fn property_writes_match_php() {
    let out = run(r#"<?php
class P {
    public $u = 1;
    public int $i = 0;
    public ?string $s = null;
    public readonly int $ro;
    private $priv = 0;
    public int $late;
    public $byref = 0;
    function __construct() { $this->ro = 7; }
    function setPriv($v) { $this->priv = $v; return $this->priv; }
}
class M {
    public $declared = 0;
    private $hidden = 0;
    function __set($n, $v) { echo "__set($n)\n"; }
}
#[AllowDynamicProperties] class D {}
function w(P $o, M $m, D $d, $k) {
    $o->u = $k;
    $o->u = "s$k";
    $o->u = [$k];
    $o->i = $k;
    try { $o->i = "x"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    $o->i = "4$k";
    $o->i = 2.0;
    $o->s = $k;
    try { $o->ro = $k; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    try { $o->priv = $k; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    echo $o->setPriv($k), "\n";
    $o->late = $k;
    $r = &$o->byref;
    $o->byref = $k * 10;
    echo $r, "\n";
    unset($r);
    $m->declared = $k;
    $m->hidden = $k;
    $m->nope = $k;
    $d->dyn = $k;
    $d->dyn = $k + 1;
    $x = $o->u = $k + 100;
    echo $x, "\n";
}
$o = new P; $m = new M; $d = new D;
for ($k = 0; $k < 3; $k++) { w($o, $m, $d, $k); }
var_dump($o, $m, $d);
$u = new P;
unset($u->u);
for ($k = 0; $k < 3; $k++) { $u->u = $k; }
var_dump($u->u);
"#);
    assert_eq!(
        out,
        r#"Cannot assign string to property P::$i of type int
Cannot modify readonly property P::$ro
Cannot access private property P::$priv
0
0
__set(hidden)
__set(nope)
100
Cannot assign string to property P::$i of type int
Cannot modify readonly property P::$ro
Cannot access private property P::$priv
1
10
__set(hidden)
__set(nope)
101
Cannot assign string to property P::$i of type int
Cannot modify readonly property P::$ro
Cannot access private property P::$priv
2
20
__set(hidden)
__set(nope)
102
object(P)#1 (7) {
  ["u"]=>
  int(102)
  ["i"]=>
  int(2)
  ["s"]=>
  string(1) "2"
  ["ro"]=>
  int(7)
  ["priv":"P":private]=>
  int(2)
  ["late"]=>
  int(2)
  ["byref"]=>
  int(20)
}
object(M)#2 (2) {
  ["declared"]=>
  int(2)
  ["hidden":"M":private]=>
  int(0)
}
object(D)#3 (1) {
  ["dyn"]=>
  int(3)
}
int(2)
"#
    );
}

/// `new C(...)` of a constructor that only stores its parameters, with
/// some or all of them left to their defaults: scalar literals (folded
/// without the call), a typed one whose default would coerce (`float $f =
/// 1`), a string and an array default (the call), promoted parameters, a
/// required parameter left out (`ArgumentCountError`) and a TypeError,
/// and `$r = new K(...)` over `$r`'s last object (the handle reused).
#[test]
fn constructor_defaults_match_php() {
    let out = run(r#"<?php
class K { public $a; public $b; public function __construct($a, $b = 2) { $this->a = $a; $this->b = $b; } }
class T { public int $i; public ?string $s = "d"; public float $f;
    public function __construct(int $i = 5, ?string $s = null, float $f = 1) { $this->i = $i; $this->s = $s; $this->f = $f; } }
class Pr { public function __construct(public int $x = 3, public ?array $y = null, public bool $z = false) {} }
class S { public $s; public function __construct($s = "str") { $this->s = $s; } }
class A { public $a; public function __construct($a = []) { $this->a = $a; } }
class R { public $a; public function __construct($a, $b) { $this->a = $a; } }
class F { public float $f; public function __construct(float $f = 1.5) { $this->f = $f; } }
function make($x) {
    $out = [];
    for ($k = 0; $k < 3; $k++) {
        $r = new K($x + $k);
        $out[] = [$r->a, $r->b, spl_object_id($r)];
        $q = new K($k, 9);
        $out[] = [$q->a, $q->b];
        $t = new T;
        $out[] = [$t->i, $t->s, $t->f];
        $t = new T($k);
        $out[] = [$t->i, $t->s, $t->f];
        $t = new T($k, "x");
        $out[] = [$t->i, $t->s, $t->f];
        $p = new Pr;
        $out[] = [$p->x, $p->y, $p->z];
        $p = new Pr($k);
        $out[] = [$p->x, $p->y, $p->z];
        $s = new S;
        $out[] = $s->s;
        $a = new A;
        $out[] = $a->a;
        $f = new F;
        $out[] = $f->f;
        try { new R($k); } catch (ArgumentCountError $e) { $out[] = strstr($e->getMessage(), " passed in", true); }
        try { new T("no"); } catch (TypeError $e) { $out[] = strstr($e->getMessage(), ", called in", true); }
    }
    return $out;
}
for ($j = 0; $j < 3; $j++) { echo json_encode(make($j * 10)), "\n"; }
$r = new K(1);
for ($k = 0; $k < 3; $k++) { $r = new K($k); echo spl_object_id($r), " ", $r->a, " ", $r->b, "\n"; }
$st = gc_status(); echo $st['runs'], " ", $st['roots'], "\n";
var_dump(new T(1), new Pr(2, [1]));
"#);
    assert_eq!(
        out,
        r#"[[0,2,1],[0,9],[5,null,1],[0,null,1],[0,"x",1],[3,null,false],[0,null,false],"str",[],1.5,"Too few arguments to function R::__construct(), 1","T::__construct(): Argument #1 ($i) must be of type int, string given",[1,2,9],[1,9],[5,null,1],[1,null,1],[1,"x",1],[3,null,false],[1,null,false],"str",[],1.5,"Too few arguments to function R::__construct(), 1","T::__construct(): Argument #1 ($i) must be of type int, string given",[2,2,8],[2,9],[5,null,1],[2,null,1],[2,"x",1],[3,null,false],[2,null,false],"str",[],1.5,"Too few arguments to function R::__construct(), 1","T::__construct(): Argument #1 ($i) must be of type int, string given"]
[[10,2,6],[0,9],[5,null,1],[0,null,1],[0,"x",1],[3,null,false],[0,null,false],"str",[],1.5,"Too few arguments to function R::__construct(), 1","T::__construct(): Argument #1 ($i) must be of type int, string given",[11,2,10],[1,9],[5,null,1],[1,null,1],[1,"x",1],[3,null,false],[1,null,false],"str",[],1.5,"Too few arguments to function R::__construct(), 1","T::__construct(): Argument #1 ($i) must be of type int, string given",[12,2,8],[2,9],[5,null,1],[2,null,1],[2,"x",1],[3,null,false],[2,null,false],"str",[],1.5,"Too few arguments to function R::__construct(), 1","T::__construct(): Argument #1 ($i) must be of type int, string given"]
[[20,2,1],[0,9],[5,null,1],[0,null,1],[0,"x",1],[3,null,false],[0,null,false],"str",[],1.5,"Too few arguments to function R::__construct(), 1","T::__construct(): Argument #1 ($i) must be of type int, string given",[21,2,7],[1,9],[5,null,1],[1,null,1],[1,"x",1],[3,null,false],[1,null,false],"str",[],1.5,"Too few arguments to function R::__construct(), 1","T::__construct(): Argument #1 ($i) must be of type int, string given",[22,2,8],[2,9],[5,null,1],[2,null,1],[2,"x",1],[3,null,false],[2,null,false],"str",[],1.5,"Too few arguments to function R::__construct(), 1","T::__construct(): Argument #1 ($i) must be of type int, string given"]
4 0 2
6 1 2
4 2 2
0 1
object(T)#6 (3) {
  ["i"]=>
  int(1)
  ["s"]=>
  NULL
  ["f"]=>
  float(1)
}
object(Pr)#3 (3) {
  ["x"]=>
  int(2)
  ["y"]=>
  array(1) {
    [0]=>
    int(1)
  }
  ["z"]=>
  bool(false)
}
"#
    );
}

/// `$v instanceof C` (`ZEND_INSTANCEOF`): an object of the class itself
/// (the tape's exact-class hit), of a subclass, of another class, an
/// interface, a variable holding a reference, null, a string, an enum
/// case, an unknown class, a temporary, a parameter, a closure and an
/// undefined variable (silenced, then warned).
#[test]
fn instanceof_matches_php() {
    let out = run(r#"<?php
interface I {}
class C implements I {}
class D extends C {}
class E {}
enum Suit { case Hearts; }
function t($k) {
    $c = new C; $d = new D; $e = new E; $n = null; $s = "C"; $suit = Suit::Hearts;
    $r = &$c;
    $out = [];
    $out[] = $c instanceof C;
    $out[] = $d instanceof C;
    $out[] = $d instanceof D;
    $out[] = $c instanceof D;
    $out[] = $e instanceof C;
    $out[] = $c instanceof I;
    $out[] = $r instanceof C;
    $out[] = $n instanceof C;
    $out[] = $s instanceof C;
    $out[] = $suit instanceof Suit;
    $out[] = $suit instanceof UnitEnum;
    $out[] = $c instanceof Missing;
    $out[] = (new D) instanceof C;
    $out[] = $k instanceof C;
    $f = function () {};
    $out[] = $f instanceof Closure;
    $out[] = @$undef instanceof C;
    return implode(",", array_map(fn($b) => $b ? "1" : "0", $out));
}
for ($k = 0; $k < 3; $k++) { echo t($k ? new C : 0), "\n"; }
echo $undef2 instanceof C ? "y" : "n", "\n";
"#);
    assert_eq!(
        out,
        r#"1,1,1,0,0,1,1,0,0,1,1,0,1,0,1,0
1,1,1,0,0,1,1,0,0,1,1,0,1,1,1,0
1,1,1,0,0,1,1,0,0,1,1,0,1,1,1,0

Warning: Undefined variable $undef2 in t.php on line 31
n
"#
    );
}

/// `new` sites the constructor fold refuses for what depends on the class
/// and the instruction alone (a constructor with a body, one inherited,
/// arguments to a class without one, a string default, more arguments
/// than parameters): the site keeps the refusal, and every `new` there
/// still runs the constructor, evaluates its arguments and makes the
/// object.
#[test]
fn refused_constructor_folds_match_php() {
    let out = run(r#"<?php
class Body { public $a; function __construct($a) { $this->a = $a; echo "B$a "; } }
class NoCtor { public $p = 1; }
class Str { public $s; function __construct($s = "dflt") { $this->s = $s; } }
class Two { public $a; function __construct($a) { $this->a = $a; } }
class Sub extends Body {}
function side() { echo "side "; return 7; }
function run($k) {
    $out = [];
    $o = new Body($k); $out[] = $o->a;
    $o = new Sub($k + 1); $out[] = $o->a;
    $n = new NoCtor($k, side()); $out[] = $n->p;
    $s = new Str; $out[] = $s->s;
    $s = new Str($k); $out[] = $s->s;
    $t = new Two($k, $k + 1); $out[] = $t->a;
    echo "\n";
    return implode(",", $out);
}
for ($k = 0; $k < 4; $k++) { echo run($k), "\n"; }
"#);
    assert_eq!(
        out,
        r#"B0 B1 side 
0,1,1,dflt,0,0
B1 B2 side 
1,2,1,dflt,1,1
B2 B3 side 
2,3,1,dflt,2,2
B3 B4 side 
3,4,1,dflt,3,3
"#
    );
}

/// A property site in a base class's method that sees objects of many
/// subclasses keeps an entry for each of the last four classes
/// (`PropCacheEntry::alt`, most recently used first): a redeclared
/// property, a subclass with `__get`/`__isset`, one with a property hook,
/// the base's private property, reads, writes and `isset`. Expected
/// output: php-src's (PHP 8.5).
#[test]
fn property_sites_keep_entries_for_several_subclasses() {
    assert_eq!(
        run(r#"<?php
class B { protected $types = []; protected $x = 1; private $priv = 'bp'; public $pub = 'p';
  function t() { return ($this->types ? 1 : $this->x) . $this->priv . $this->pub . (isset($this->zz) ? 'z' : '-'); }
  function w($v) { $this->x = $v; $this->pub .= '!'; return $this->x; } }
class C1 extends B {} class C2 extends B { protected $x = 2; } class C3 extends B { public $pub = 'c3'; }
class C4 extends B { function __get($n) { return "magic:$n"; } function __isset($n) { return $n === 'zz'; } }
class C5 extends B { public $pub { get => 'hooked'; } }
class C6 extends B { protected $types = [1]; }
$os = [new C1, new C2, new C3, new C4, new C5, new C6, new B];
for ($r = 0; $r < 3; $r++) { foreach ($os as $o) echo $o->t(), " "; echo "\n"; }
foreach ($os as $i => $o) { try { echo $o->w($i * 10), " "; } catch (Error $e) { echo get_class($e), " "; } } echo "\n";
for ($r = 0; $r < 2; $r++) { foreach ($os as $o) echo $o->t(), " "; echo "\n"; }
function rd($o) { return $o->pub; } for ($r = 0; $r < 2; $r++) { foreach ($os as $o) echo rd($o), ","; echo "\n"; }
"#),
        r#"1bpp- 2bpp- 1bpc3- 1bppz 1bphooked- 1bpp- 1bpp- 
1bpp- 2bpp- 1bpc3- 1bppz 1bphooked- 1bpp- 1bpp- 
1bpp- 2bpp- 1bpc3- 1bppz 1bphooked- 1bpp- 1bpp- 
0 10 20 30 40 50 60 
0bpp!- 10bpp!- 20bpc3!- 30bpp!z 40bphooked- 1bpp!- 60bpp!- 
0bpp!- 10bpp!- 20bpc3!- 30bpp!z 40bphooked- 1bpp!- 60bpp!- 
p!,p!,c3!,p!,hooked,p!,p!,
p!,p!,c3!,p!,hooked,p!,p!,
"#
    );
}
