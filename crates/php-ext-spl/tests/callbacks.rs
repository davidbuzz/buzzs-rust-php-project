// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! SPL callables (`iterator_apply()`, the callback filter iterators,
//! `ArrayObject::uasort()`): resolved once, in the calling scope, with the
//! callable deprecations raised there (`Z_PARAM_FUNC`), and called through
//! that resolution. Expected output from php-src (`/usr/bin/php`).

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
