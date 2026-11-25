// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Builtin calls through the tape's internal call (`exec/tape/calls.rs`,
//! `call_native`): results, by-reference arguments, the builtin's frame in
//! an exception's trace, and steady memory across identical iterations.
//! Expected output from `/usr/bin/php8.5 -n`, except the empty separator
//! message of explode(), which follows php-src master
//! (ext/standard/string.c: "use str_split() ...").

#![allow(clippy::needless_raw_string_hashes)]

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
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn native_calls_keep_results_traces_and_memory() {
    let out = run(r#"<?php
function f($s, $a) {
    $out = [];
    for ($i = 0; $i < 3; $i++) {
        $p = explode(',', $s);
        $out[] = implode('|', $p) . ':' . count($p);
        $out[] = implode(',', array_slice($a, 1, 2));
        $out[] = str_replace('a', 'b', $s);
        $out[] = array_key_last(array_merge($a, ['k' => 1]));
        $out[] = preg_match('/(b)/', $s, $m) . $m[1];
        $out[] = strlen(json_encode($m));
    }
    try {
        explode('', $s);
    } catch (ValueError $e) {
        $out[] = $e->getMessage();
        $t = $e->getTrace();
        $out[] = $t[0]['function'] . '(' . implode(',', $t[0]['args']) . ')';
    }
    try {
        str_repeat($s, -1);
    } catch (ValueError $e) {
        $out[] = $e->getTraceAsString();
    }
    $out[] = count(func_get_args());
    $before = memory_get_usage();
    for ($j = 0; $j < 2; $j++) {
        for ($i = 0; $i < 50; $i++) {
            $r = explode(',', $s);
            $r = str_replace('a', 'b', $s);
        }
        $now = memory_get_usage();
        $out[] = $j === 0 ? "first" : $now - $before;
        $before = $now;
    }
    return $out;
}
foreach (f('a,b,c', [1, 2, 3, 4]) as $line) {
    echo $line, "\n";
}
"#);
    assert_eq!(out, r#"a|b|c:3
2,3
b,b,c
k
1b
9
a|b|c:3
2,3
b,b,c
k
1b
9
a|b|c:3
2,3
b,b,c
k
1b
9
explode(): Argument #1 ($separator) must not be empty, use str_split() to split a string into characters
explode(,a,b,c)
#0 t.php(21): str_repeat('a,b,c', -1)
#1 t.php(38): f('a,b,c', Array)
#2 {main}
2
first
0
"#);
}

/// Lists the array builtins build to size (array_keys, array_slice,
/// array_reverse, array_fill from 0, array_pad of a list, array_values)
/// next to the cases that keep or make other keys: keys, values and the
/// next index (an append after each) as php8.5 gives them.
#[test]
fn array_builders_give_the_lists_and_keys_php_gives() {
    let out = run(r#"<?php
function show($a) { $a[] = 'n'; $o = []; foreach ($a as $k => $v) { $o[] = "$k=" . (is_array($v) ? 'A' : var_export($v, true)); } return implode(' ', $o); }
function t() {
    $h = [5 => 'a', 'x' => 'b', 9 => 'c'];
    $l = ['a', 'b', 'c', 'd'];
    unset($l[1]);
    echo show(array_keys($h)), "\n";
    echo show(array_keys($l)), "\n";
    echo show(array_keys([])), "\n";
    echo show(array_slice($l, 1)), "\n";
    echo show(array_slice($l, 0, 2, true)), "\n";
    echo show(array_slice(['a', 'b', 'c'], 0, 2, true)), "\n";
    echo show(array_slice(['a', 'b', 'c'], 1, 5, true)), "\n";
    echo show(array_reverse($l)), "\n";
    echo show(array_reverse($l, true)), "\n";
    echo show(array_reverse($h)), "\n";
    echo show(array_reverse([])), "\n";
    echo show(array_fill(0, 3, 'v')), "\n";
    echo show(array_fill(2, 2, 'v')), "\n";
    echo show(array_fill(-3, 2, 'v')), "\n";
    echo show(array_pad($l, 5, 0)), "\n";
    echo show(array_pad($l, -5, 0)), "\n";
    echo show(array_pad($h, 4, 0)), "\n";
    echo show(array_values($l)), "\n";
    echo show(array_values($h)), "\n";
}
t();
"#);
    assert_eq!(out, r#"0=5 1='x' 2=9 3='n'
0=0 1=2 2=3 3='n'
0='n'
0='c' 1='d' 2='n'
0='a' 2='c' 3='n'
0='a' 1='b' 2='n'
1='b' 2='c' 3='n'
0='d' 1='c' 2='a' 3='n'
3='d' 2='c' 0='a' 4='n'
0='c' x='b' 1='a' 2='n'
0='n'
0='v' 1='v' 2='v' 3='n'
2='v' 3='v' 4='n'
-3='v' -2='v' -1='n'
0='a' 1='c' 2='d' 3=0 4=0 5='n'
0=0 1=0 2='a' 3='c' 4='d' 5='n'
0='a' x='b' 1='c' 2=0 3='n'
0='a' 1='c' 2='d' 3='n'
0='a' 1='b' 2='c' 3='n'
"#);
}

/// array_merge() and the other variadic array builtins called with their
/// arguments as they are: string keys overwritten, lists renumbered and
/// filled to size, a shared reference kept, the type error of a
/// non-array. Expected output from php8.5, except the named variadic
/// error, which follows php-src master (Zend/zend_API.c).
#[test]
fn array_merge_forms_give_what_php_gives() {
    let out = run(r#"<?php
function show($a) { $a[] = 'n'; $o = []; foreach ($a as $k => $v) { $o[] = "$k=" . (is_array($v) ? '[' . implode(',', $v) . ']' : var_export($v, true)); } return implode(' ', $o); }
function t() {
    $x = ['k' => 1, 'j' => 2]; $y = ['z' => 3, 'k' => 4];
    $l = [1, 2, 3]; unset($l[1]); $m = [5 => 'a', 9 => 'b'];
    echo show(array_merge($x, $y)), "\n";
    echo show(array_merge($l, [7, 8])), "\n";
    echo show(array_merge($l)), "\n";
    echo show(array_merge($m, $x, $l)), "\n";
    echo show(array_merge([], $m)), "\n";
    echo show(array_merge([], [])), "\n";
    echo show(array_merge()), "\n";
    $r = 1; $withref = [&$r, 2];
    $mm = array_merge($withref, [3]); $r = 9;
    echo show($mm), "\n";
    echo show(array_merge_recursive([1, [2]], [3], ['a' => [4]], ['a' => 5])), "\n";
    echo show(array_merge_recursive($l, [7])), "\n";
    try { array_merge([1], 2); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
    try { array_merge(...['a' => [1]]); } catch (Error $e) { echo get_class($e), ': ', $e->getMessage(), "\n"; }
    echo show(array_intersect([1, 2, 3], [2, 3])), "\n";
    echo show(array_diff_key(['a' => 1, 'b' => 2], ['a' => 0])), "\n";
}
t();
"#);
    assert_eq!(out, r#"k=4 j=2 z=3 0='n'
0=1 1=3 2=7 3=8 4='n'
0=1 1=3 2='n'
0='a' 1='b' k=1 j=2 2=1 3=3 4='n'
0='a' 1='b' 2='n'
0='n'
0='n'
0=9 1=2 2=3 3='n'
0=1 1=[2] 2=3 a=[4,5] 3='n'
0=1 1=3 2=7 3='n'
array_merge(): Argument #2 must be of type array, int given
ArgumentCountError: Internal function array_merge() does not accept named variadic arguments
1=2 2=3 3='n'
b=2 0='n'
"#);
}
