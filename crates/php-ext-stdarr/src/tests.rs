//! End-to-end tests: PHP snippets run on a VM with this extension.

use super::StdArray;
use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Runs `code` (without the `<?php` tag) and returns its output.
fn run(code: &str) -> String {
    let sink = Sink::default();
    let ext = StdArray;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    let src = format!("<?php\n{code}");
    vm.run_script(src.as_bytes(), "test.php");
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn sorts_through_the_reference() {
    assert_eq!(
        run("$a = [3, 1, 2]; sort($a); echo implode(',', $a);"),
        "1,2,3"
    );
    assert_eq!(
        run("$a = ['b' => 1, 'a' => 2]; ksort($a); echo implode(',', array_keys($a));"),
        "a,b"
    );
}

#[test]
fn user_sort_and_callback_errors() {
    assert_eq!(
        run("$a = [1, 3, 2]; usort($a, fn($x, $y) => $y <=> $x); echo implode(',', $a);"),
        "3,2,1"
    );
    assert_eq!(
        run("$a = [1]; try { usort($a, 'nope'); } catch (TypeError $e) { echo $e->getMessage(); }"),
        "usort(): Argument #2 ($callback) must be a valid callback, function \"nope\" not found or invalid function name"
    );
    assert_eq!(
        run("try { array_map('nope', []); } catch (TypeError $e) { echo $e->getMessage(); }"),
        "array_map(): Argument #1 ($callback) must be a valid callback or null, function \"nope\" not found or invalid function name"
    );
}

#[test]
fn u_diff_variants_validate_trailing_callbacks() {
    assert_eq!(
        run("try { array_udiff([1]); } catch (ArgumentCountError $e) { echo $e->getMessage(); }"),
        "array_udiff() expects at least 2 arguments, 1 given"
    );
    assert_eq!(
        run("try { array_udiff_uassoc([1], [2], 'strcmp', 'x'); } catch (TypeError $e) { echo $e->getMessage(); }"),
        "array_udiff_uassoc(): Argument #4 must be a valid callback, function \"x\" not found or invalid function name"
    );
    assert_eq!(
        run("echo implode(',', array_udiff_uassoc([1, 5], [1], fn($a, $b) => $a <=> $b, fn($a, $b) => $a <=> $b));"),
        "5"
    );
    assert_eq!(
        run("try { array_diff([1], 2); } catch (TypeError $e) { echo $e->getMessage(); }"),
        "array_diff(): Argument #2 must be of type array, int given"
    );
}

#[test]
fn internal_pointer_mutates_the_variable() {
    assert_eq!(
        run("$a = [1, 2, 3]; next($a); next($a); echo current($a), key($a); reset($a); echo current($a);"),
        "321"
    );
    assert_eq!(run("$a = [1, 2, 3]; echo end($a), key($a);"), "32");
}

#[test]
fn extract_and_compact_use_the_callers_scope() {
    assert_eq!(
        run("function f() { $src = ['x' => 1, 'y' => 2]; $n = extract($src); return [$n, $x + $y]; } echo implode(',', f());"),
        "2,3"
    );
    assert_eq!(
        run("function g() { $a = 1; $b = 'z'; return compact('a', ['b']); } echo implode(',', g());"),
        "1,z"
    );
    assert_eq!(
        run("$a = 1; $b = 2; echo implode(',', compact('a', 'b'));"),
        "1,2"
    );
    assert_eq!(
        run("$arr = ['v' => 1]; extract($arr, EXTR_REFS); $v = 5; echo $arr['v'];"),
        "5"
    );
    assert_eq!(
        run("try { call_user_func('extract', ['a' => 1]); } catch (Error $e) { echo $e->getMessage(); }"),
        "Cannot call extract() dynamically"
    );
}

#[test]
fn rng_shares_mt_srand_state() {
    let a = run("mt_srand(42); $x = range(1, 10); shuffle($x); echo implode(',', $x);");
    let b = run("mt_srand(42); $x = range(1, 10); shuffle($x); echo implode(',', $x);");
    assert_eq!(a, b);
    assert_eq!(run("mt_srand(1); echo strlen(str_shuffle('abcdef'));"), "6");
}

#[test]
fn constants_and_misc() {
    assert_eq!(
        run("echo SORT_STRING, COUNT_RECURSIVE, EXTR_REFS, ARRAY_FILTER_USE_KEY;"),
        "212562"
    );
    assert_eq!(run("echo count([1, [2, 3]], COUNT_RECURSIVE);"), "4");
    assert_eq!(run("echo max(1, 5, 3), min([4, 2]);"), "52");
    assert_eq!(
        run("try { array_key_exists([], []); } catch (TypeError $e) { echo $e->getMessage(); }"),
        "Cannot access offset of type array on array"
    );
    assert_eq!(run("echo implode(',', array_filter([1, 0, 2]));"), "1,2");
}
