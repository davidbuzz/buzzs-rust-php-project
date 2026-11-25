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
