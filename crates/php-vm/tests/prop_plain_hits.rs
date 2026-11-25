// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The plainest property cache hits of the tape's handlers
//! (`exec/spec/props.rs`: `read_plain`, `assign_plain`, the `++`/`--`
//! handler) and every case they hand to the full hit or the generic op:
//! each value type written over each, a temporary, a literal and the
//! assignment's value, typed coercion and TypeError, readonly, an
//! uninitialized typed slot, a reference in the slot and in the base, a
//! method writing `$this->p`, destructor timing of a replaced object,
//! `++`/`--` of numbers, null, strings and a typed int at its maximum,
//! lazy ghosts and dynamic properties. Each runs three times in a function
//! so that the cache fill, the hit and the declines after it all run.
//! Expected output from `/usr/bin/php8.5 -n`.

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

const SCRIPT: &str = r##"<?php
class D { function __construct(public $n) {} function __destruct() { echo "dtor {$this->n}\n"; } }
class C {
    public $p = 1;
    public int $i = 0;
    public ?float $f = null;
    public readonly int $ro;
    public int $late;
    public $arr = ['k' => 1];
    function __construct() { $this->ro = 1; }
    function own($v) { $this->p = $v; $this->p++; return $this->p; }
}
function writes($o, $round) {
    $vals = [5, 2.5, "s$round", [1, 2], null, true];
    foreach ($vals as $v) {
        $o->p = $v;
        $r = $o->p;
        echo gettype($r), ":", json_encode($r), " ";
    }
    $o->p = $round + 10;
    $x = ($o->p = $round * 3);
    echo "res=$x ";
    $o->p = 'lit';
    $o->p = $round . "-tmp";
    echo $o->p, "\n";
    $o->i = 7;
    $o->i = "8";
    var_dump($o->i);
    try { $o->i = "x"; } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    $o->f = 3;
    var_dump($o->f);
    try { $o->ro = 2; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    try { echo $o->late; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    $o->late = $round;
    echo $o->late, "\n";
    $ref = &$o->p;
    $o->p = 'via-ref';
    echo $ref, "\n";
    $ref = 'changed';
    echo $o->p, "\n";
    unset($ref);
    $alias = &$o;
    $alias->p = 'alias';
    echo $alias->p, "\n";
    unset($alias);
    echo "own=", $o->own($round), "\n";
    $o->p = new D($round);
    echo "set\n";
    $o->p = new D($round + 100);
    echo "replaced\n";
    $o->p = 0;
    echo "cleared\n";
}
$o = new C;
for ($k = 0; $k < 3; $k++) writes($o, $k);
function incs($o) {
    $o->p = 1;
    for ($j = 0; $j < 4; $j++) { $o->p++; ++$o->p; $o->p--; }
    echo $o->p, " ", $o->p++, " ", ++$o->p, " ", $o->p--, " ", --$o->p, "\n";
    $o->p = 1.5; $o->p++; var_dump($o->p);
    $o->p = PHP_INT_MAX; $o->p++; var_dump($o->p);
    $o->p = PHP_INT_MIN; $o->p--; var_dump($o->p);
    $o->p = null; $o->p++; var_dump($o->p);
    $o->p = null; $o->p--; var_dump($o->p);
    $o->p = "a"; $o->p++; var_dump($o->p);
    $o->p = "9"; $o->p++; var_dump($o->p);
    $o->i = PHP_INT_MAX;
    try { $o->i++; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    var_dump($o->i);
    $o->i = 5; $o->i++; var_dump($o->i);
    $r = &$o->p; $o->p = 1; $o->p++; echo $r, "\n"; unset($r);
}
for ($k = 0; $k < 3; $k++) incs($o);
$g = (new ReflectionClass(C::class))->newLazyGhost(function ($c) { echo "init\n"; $c->p = 42; });
for ($k = 0; $k < 3; $k++) { $g->p = $k; echo $g->p, "\n"; }
$h = (new ReflectionClass(C::class))->newLazyGhost(function ($c) { echo "init2\n"; });
for ($k = 0; $k < 3; $k++) { echo $h->p, "\n"; $h->p++; }
$dyn = new stdClass;
for ($k = 0; $k < 3; $k++) { $dyn->q = $k; $dyn->q++; echo $dyn->q, "\n"; }
echo "end\n";
"##;

const EXPECTED: &str = r##"integer:5 double:2.5 string:"s0" array:[1,2] NULL:null boolean:true res=0 0-tmp
int(8)
Cannot assign string to property C::$i of type int
float(3)
Cannot modify readonly property C::$ro
Typed property C::$late must not be accessed before initialization
0
via-ref
changed
alias
own=1
set
dtor 0
replaced
dtor 100
cleared
integer:5 double:2.5 string:"s1" array:[1,2] NULL:null boolean:true res=3 1-tmp
int(8)
Cannot assign string to property C::$i of type int
float(3)
Cannot modify readonly property C::$ro
01
via-ref
changed
alias
own=2
set
dtor 1
replaced
dtor 101
cleared
integer:5 double:2.5 string:"s2" array:[1,2] NULL:null boolean:true res=6 2-tmp
int(8)
Cannot assign string to property C::$i of type int
float(3)
Cannot modify readonly property C::$ro
12
via-ref
changed
alias
own=3
set
dtor 2
replaced
dtor 102
cleared
5 5 7 7 5
float(2.5)
float(9.223372036854776E+18)
float(-9.223372036854776E+18)
int(1)

Warning: Decrement on type null has no effect, this will change in the next major version of PHP in /tmp/claude-1000/t62/t.php on line 64
NULL

Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead in /tmp/claude-1000/t62/t.php on line 65
string(1) "b"
int(10)
Cannot increment property C::$i of type int past its maximal value
int(9223372036854775807)
int(6)
2
5 5 7 7 5
float(2.5)
float(9.223372036854776E+18)
float(-9.223372036854776E+18)
int(1)

Warning: Decrement on type null has no effect, this will change in the next major version of PHP in /tmp/claude-1000/t62/t.php on line 64
NULL

Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead in /tmp/claude-1000/t62/t.php on line 65
string(1) "b"
int(10)
Cannot increment property C::$i of type int past its maximal value
int(9223372036854775807)
int(6)
2
5 5 7 7 5
float(2.5)
float(9.223372036854776E+18)
float(-9.223372036854776E+18)
int(1)

Warning: Decrement on type null has no effect, this will change in the next major version of PHP in /tmp/claude-1000/t62/t.php on line 64
NULL

Deprecated: Increment on non-numeric string is deprecated, use str_increment() instead in /tmp/claude-1000/t62/t.php on line 65
string(1) "b"
int(10)
Cannot increment property C::$i of type int past its maximal value
int(9223372036854775807)
int(6)
2
init
0
1
2
init2
1
2
3
1
2
3
end
"##;

#[test]
fn plain_property_hits_match_php() {
    assert_eq!(run(SCRIPT), EXPECTED);
}
