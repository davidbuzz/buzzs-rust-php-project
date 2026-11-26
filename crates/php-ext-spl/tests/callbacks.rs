// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! SPL callables (`iterator_apply()`, the callback filter iterators,
//! `ArrayObject::uasort()`): resolved once, in the calling scope, with the
//! callable deprecations raised there (`Z_PARAM_FUNC`), and called through
//! that resolution. Expected output from php-src (`/usr/bin/php8.5`).

use php_vm::{Vm, VmOptions};
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

/// Runs `code` as one request (`E_ALL`, so the deprecations show) on a
/// thread of its own, like `tests/script.rs`.
fn run(code: &str) -> String {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn_scoped(s, || {
                let sink = Sink::default();
                let options = VmOptions {
                    ini: vec![("error_reporting".into(), "32767".into())],
                };
                let mut vm = Vm::with_extensions(
                    &options,
                    Box::new(sink.clone()),
                    &[
                        &php_ext_spl::Spl,
                        &php_ext_stdarr::StdArray,
                        &php_ext_stdstr::StdStr,
                    ],
                );
                assert_eq!(vm.run_script(code.as_bytes(), "test.php"), 0);
                drop(vm);
                let out = sink.0.borrow().clone();
                String::from_utf8(out).expect("UTF-8 output")
            })
            .expect("spawn script thread")
            .join()
            .expect("script thread")
    })
}

/// `'parent::m'` and a private method: deprecations once, at the call that
/// takes the callable; a `CallbackFilterIterator` iterated outside the
/// class still calls what its constructor resolved.
#[test]
fn callables_resolve_once_in_the_calling_scope() {
    let out = run(r#"<?php
class foo {
    public function ia() { echo __METHOD__, "\n"; return true; }
    public function af($v) { return $v > 1; }
    public function cmp($a, $b) { return $a <=> $b; }
}
class bar extends foo {
    private function mine($v) { return $v != 2; }
    function apply() { return iterator_apply(new ArrayIterator([1, 2]), 'parent::ia'); }
    function filter() { return new CallbackFilterIterator(new ArrayIterator([1, 2, 3]), 'parent::af'); }
    function priv() { return new CallbackFilterIterator(new ArrayIterator([1, 2, 3]), [$this, 'mine']); }
    function sort() { $a = new ArrayObject([3, 1, 2]); $a->uasort('parent::cmp'); return $a->getArrayCopy(); }
}
$b = new bar;
var_dump($b->apply());
var_dump(iterator_to_array($b->filter()));
var_dump(iterator_to_array($b->priv()));
var_dump($b->sort());
try { new CallbackFilterIterator(new ArrayIterator([]), 'parent::af'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
try { iterator_apply(new ArrayIterator([1]), 'parent::ia'); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
"#);
    assert_eq!(
        out,
        "\nDeprecated: Use of \"parent\" in callables is deprecated in test.php on line 9\n\
         foo::ia\nfoo::ia\nint(2)\n\
         \nDeprecated: Use of \"parent\" in callables is deprecated in test.php on line 10\n\
         array(2) {\n  [1]=>\n  int(2)\n  [2]=>\n  int(3)\n}\n\
         array(2) {\n  [0]=>\n  int(1)\n  [2]=>\n  int(3)\n}\n\
         \nDeprecated: Use of \"parent\" in callables is deprecated in test.php on line 12\n\
         array(3) {\n  [1]=>\n  int(1)\n  [2]=>\n  int(2)\n  [0]=>\n  int(3)\n}\n\
         CallbackFilterIterator::__construct(): Argument #2 ($callback) must be a valid callback, cannot access \"parent\" when no class scope is active\n\
         iterator_apply(): Argument #2 ($callback) must be a valid callback, cannot access \"parent\" when no class scope is active\n"
    );
}

/// `spl_autoload_register()` resolves (and deprecates) once: the
/// binding's own resolution, not `php-spl`'s check of the rewritten
/// callable. `[$this, 'parent::m']` keeps its form there.
#[test]
fn autoloader_registration_deprecates_once() {
    let out = run(r#"<?php
class foo { public function al($c) { echo "al: $c\n"; } }
class bar extends foo {
    function reg() { spl_autoload_register([$this, 'parent::al']); }
    function unreg() { var_dump(spl_autoload_unregister([$this, 'parent::al'])); }
}
$b = new bar;
$b->reg();
var_dump(class_exists('NopeClass'));
$b->unreg();
"#);
    assert_eq!(
        out,
        "\nDeprecated: Callables of the form [\"bar\", \"parent::al\"] are deprecated in test.php on line 4\n\
         al: NopeClass\nbool(false)\n\
         \nDeprecated: Callables of the form [\"bar\", \"parent::al\"] are deprecated in test.php on line 5\n\
         bool(true)\n"
    );
}

/// `class_uses()`, `class_implements()`, `class_parents()` and
/// `spl_object_hash()` answered without the generic SPL dispatch for an
/// object or a loaded class's name; a name in another case or with a
/// leading backslash, a class to autoload, a missing one (with and without
/// autoloading) and a type error take it. Expected output: php-src's (PHP
/// 8.4 CLI).
#[test]
fn class_lists_and_object_hashes() {
    let src = r#"<?php
namespace App;
interface I1 {} trait T1 {} trait T2 { use T1; }
class P implements I1 { use T2; } class C extends P { use T1; }
spl_autoload_register(function ($c) { echo "[autoload $c]"; if ($c === 'App\Late') { eval('namespace App; class Late extends P {}'); } });
$o = new C;
foreach ([$o, new P, C::class, 'app\c', '\App\C', 'App\Late', 'App\Missing'] as $x) {
    $label = is_object($x) ? get_class($x) : $x;
    $f = fn($a) => $a === false ? 'false' : implode(',', array_keys($a)) . '=' . implode(',', $a);
    echo $label, ' uses=', $f(class_uses($x)), ' impl=', $f(class_implements($x)), ' parents=', $f(class_parents($x)), "\n";
}
var_dump(class_uses('App\Missing2', false), class_parents('App\C', false));
try { class_uses(42); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
$h = @spl_object_hash($o); echo strlen($h), ' ', $h === @spl_object_hash($o) ? 'same' : 'diff', ' ', $h !== @spl_object_hash(new \stdClass) ? 'distinct' : 'clash', "\n";
try { @spl_object_hash('x'); } catch (\TypeError $e) { echo $e->getMessage(), "\n"; }
"#;
    let expected = r#"App\C uses=App\T1=App\T1 impl=App\I1=App\I1 parents=App\P=App\P
App\P uses=App\T2=App\T2 impl=App\I1=App\I1 parents==
App\C uses=App\T1=App\T1 impl=App\I1=App\I1 parents=App\P=App\P
app\c uses=App\T1=App\T1 impl=App\I1=App\I1 parents=App\P=App\P
\App\C uses=App\T1=App\T1 impl=App\I1=App\I1 parents=App\P=App\P
App\Late uses=[autoload App\Late]= impl=App\I1=App\I1 parents=App\P=App\P
App\Missing uses=[autoload App\Missing]
Warning: class_uses(): Class App\Missing does not exist and could not be loaded in test.php on line 10
false impl=[autoload App\Missing]
Warning: class_implements(): Class App\Missing does not exist and could not be loaded in test.php on line 10
false parents=[autoload App\Missing]
Warning: class_parents(): Class App\Missing does not exist and could not be loaded in test.php on line 10
false

Warning: class_uses(): Class App\Missing2 does not exist in test.php on line 12
bool(false)
array(1) {
  ["App\P"]=>
  string(5) "App\P"
}
class_uses(): Argument #1 ($object_or_class) must be of type object|string, int given
32 same distinct
spl_object_hash(): Argument #1 ($object) must be of type object, string given
"#;
    assert_eq!(run(src), expected);
}

/// An SPL iterator or array object asks whether its class overrides a
/// method with a user one (answered at once for the internal class
/// itself): `ArrayIterator` and `ArrayObject` and user subclasses
/// overriding `current()`, `key()`, `offsetGet()`. Expected output:
/// php-src's (PHP 8.4 CLI).
#[test]
fn spl_user_overrides() {
    let src = r#"<?php
class MyIt extends ArrayIterator { function current(): mixed { return 'c' . parent::current(); } function key(): string|int|null { return 'k' . parent::key(); } }
class MyAO extends ArrayObject { function offsetGet($k): mixed { return "got $k"; } }
foreach ([new ArrayIterator([1, 2]), new MyIt([1, 2]), new ArrayIterator(['a' => 1])] as $it) { var_dump(iterator_to_array($it), iterator_to_array($it, false), iterator_count($it)); }
$ao = new MyAO(['x' => 1]); var_dump($ao['x'], isset($ao['x']), count($ao));
$ao2 = new ArrayObject(['y' => 2]); var_dump($ao2['y'], iterator_to_array($ao2->getIterator()));
"#;
    let expected = r#"array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(2)
}
int(2)
array(2) {
  ["k0"]=>
  string(2) "c1"
  ["k1"]=>
  string(2) "c2"
}
array(2) {
  [0]=>
  string(2) "c1"
  [1]=>
  string(2) "c2"
}
int(2)
array(1) {
  ["a"]=>
  int(1)
}
array(1) {
  [0]=>
  int(1)
}
int(1)
string(5) "got x"
bool(true)
int(1)
int(2)
array(1) {
  ["y"]=>
  int(2)
}
"#;
    assert_eq!(run(src), expected);
}
