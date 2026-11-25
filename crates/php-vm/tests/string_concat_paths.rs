// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Concatenation, interpolation and `.=` (php-src's `ZEND_CONCAT`,
//! `ZEND_FAST_CONCAT`, `ZEND_ROPE_*` and `ZEND_ASSIGN_OP` with `.`) over
//! every operand kind on both sides (literal, variable, temporary, empty
//! strings, ints, floats under two `precision`s, null and bools, objects
//! with `__toString()` and the order of their calls, undefined variables,
//! references, arrays), a string shared with another variable or an array
//! element (never written in place), chains of ten, ropes of every part
//! kind and `.=` loops across the allocator's size classes. The tape's
//! lean paths grow a temporary or a variable in place; none of that may
//! show. The expected output is what `/usr/bin/php8.5 -n` prints.

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
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

const SCRIPT: &str = r###"<?php
class S { public $n; function __construct($n) { $this->n = $n; } function __toString(): string { echo "[ts{$this->n}]"; return "S{$this->n}"; } }
function kinds($x, $y, $e, $i, $f, $o) {
    $out = [];
    $k = 'lit';
    $out[] = $x . $y;            // cv . cv
    $out[] = $x . 'L';           // cv . lit
    $out[] = 'L' . $x;           // lit . cv
    $out[] = $x . $i;            // cv . int
    $out[] = $i . $x;            // int . cv
    $out[] = $i . $i;            // int . int
    $out[] = $e . $x;            // empty . cv
    $out[] = $x . $e;            // cv . empty
    $out[] = '' . $x . '';       // empty literals
    $out[] = strtoupper($x) . $y;    // tmp . cv
    $out[] = $y . strtoupper($x);    // cv . tmp
    $out[] = strtoupper($x) . strtolower($y);  // tmp . tmp
    $out[] = strtoupper($e) . $x;    // empty tmp . cv
    $out[] = strtoupper($x) . $e;    // tmp . empty cv
    $out[] = strtoupper($x) . '';    // tmp . empty lit
    $out[] = strtoupper($x) . $i;    // tmp . int
    $out[] = strtoupper($x) . -7;    // tmp . negative int lit
    $out[] = $x . $f;                // float
    $out[] = $f . $x;
    $out[] = strtoupper($x) . $f;
    $out[] = $x . null . true . false;
    $out[] = $x . $o;                // __toString
    $out[] = $o . $x;
    $out[] = strtoupper($x) . $o;
    $out[] = $o . $o;
    $out[] = @($x . $undef);
    $out[] = @($undef . $x);
    $r = 'ref'; $rr = &$r;
    $out[] = $r . $x;
    $out[] = $x . $rr;
    $out[] = @($x . [1]);
    return implode('|', $out);
}
function chain($a, $n) {
    $s = 'a' . $a . 'b' . $n . 'c' . $a . 'd' . $n . 'e' . $a;
    $t = $a . $a . $a . $a . $a . $a . $a . $a . $a . $a;
    $u = $n . $a . $n . $a . $n;
    return "$s,$t,$u," . strlen($t);
}
function shared() {
    $out = [];
    $a = 'x' . strlen('a');   // unique
    $b = $a;
    $a .= 'y';
    $out[] = "$a/$b";
    $c = $b . 'z';
    $out[] = "$c/$b";
    $d = 'lit';
    $d .= '!';
    $out[] = $d . 'lit';
    $arr = ['k' => 'v1'];
    $e = $arr['k'];
    $e .= 'w';
    $out[] = $e . $arr['k'];
    $g = 'r'; $h = &$g; $h .= 's'; $g .= 't';
    $out[] = "$g$h";
    $s = 'self'; $s .= $s; $out[] = $s;
    $n = 5; $s .= $n; $s .= -12; $s .= 0; $out[] = $s;
    $s .= 1.25; $out[] = $s;
    $s .= strtoupper('up'); $out[] = $s;
    $o = new S(3); $s .= $o; $out[] = $s;
    $s .= true; $s .= null; $out[] = $s;
    $v = $s .= '='; $out[] = $v;
    return implode(',', $out);
}
function loop() {
    $s = '';
    $t = 'p';
    for ($i = 0; $i < 600; $i++) {
        $s .= 'abcdefg';
        $s .= $i;
        $t .= $t[0];
        if ($i % 150 == 0) { $copy = $s; $copy .= '#'; }
    }
    return strlen($s) . ':' . substr($s, 0, 20) . '~' . substr($s, -20) . '~' . substr_count($s, '7') . ':' . strlen($t) . ':' . strlen($copy) . ':' . substr($copy, -9);
}
function ropes($x, $i, $f, $o) {
    $k = 'K';
    $arr = ['a' => 'A'];
    $out = [];
    $out[] = "a{$x}b{$i}c";
    $out[] = "$x$x$x$x$x$x$x$x$x$x$x";
    $out[] = "{$i}{$i}-{$i}";
    $out[] = "f=$f";
    $out[] = "o=$o!";
    $out[] = "t=" . "{$arr['a']}" . "u{$k}" . strtoupper("v$x");
    $out[] = @"n=$undef;";
    $out[] = "{$x}" . "";
    $out[] = "" . "$i";
    $big = str_repeat('m', 3030);
    $s = "$big$i";
    $out[] = strlen($s) . substr($s, -4);
    $s2 = $s . 'tail';
    $out[] = strlen($s2) . substr($s2, -6);
    return implode('|', $out);
}
function main() {
    echo kinds('ab', 'CD', '', 42, 1.5, new S(1)), "\n";
    ini_set('precision', 5);
    echo kinds('x', 'Y', '', PHP_INT_MIN, 1/3, new S(2)), "\n";
    ini_set('precision', 14);
    for ($i = 0; $i < 3; $i++) echo chain(str_repeat('q', $i * 7), $i - 1), "\n";
    $w = chain(str_repeat('w', 1500), 12345); echo strlen($w), substr($w, 1495, 14), substr($w, -12), "\n";
    echo shared(), "\n";
    echo loop(), "\n";
    echo ropes('xy', -3, 2.5, new S(4)), "\n";
    echo ropes('', 0, -0.0, new S(5)), "\n";
}
main();
"###;

/// One run of `main()`.
const EXPECTED: &[&str] = &[
    "[ts1][ts1][ts1][ts1][ts1]abCD|abL|Lab|ab42|42ab|4242|ab|ab|ab|ABCD|CDAB|ABcd|ab|AB|AB|AB42|AB-7|ab1.5|1.5ab|AB1.5|ab1|abS1|S1ab|ABS1|S1S1|ab|ab|refab|abref|abArray\n",
    "[ts2][ts2][ts2][ts2][ts2]xY|xL|Lx|x-9223372036854775808|-9223372036854775808x|-9223372036854775808-9223372036854775808|x|x|x|XY|YX|Xy|x|X|X|X-9223372036854775808|X-7|x0.33333|0.33333x|X0.33333|x1|xS2|S2x|XS2|S2S2|x|x|refx|xref|xArray\n",
    "ab-1cd-1e,,-1-1-1,0\n",
    "aqqqqqqqb0cqqqqqqqd0eqqqqqqq,qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq,0qqqqqqq0qqqqqqq0,70\n",
    "aqqqqqqqqqqqqqqb1cqqqqqqqqqqqqqqd1eqqqqqqqqqqqqqq,qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq,1qqqqqqqqqqqqqq1qqqqqqqqqqqqqq1,140\n",
    "22538wwwwwwb12345cww12345,15000\n",
    "[ts3]x1y/x1,x1z/x1,lit!lit,v1wv1,rstrst,selfself,selfself5-120,selfself5-1201.25,selfself5-1201.25UP,selfself5-1201.25UPS3,selfself5-1201.25UPS31,selfself5-1201.25UPS31=\n",
    "5890:abcdefg0abcdefg1abcd~abcdefg598abcdefg599~120:601:4401:cdefg450#\n",
    "[ts4]axyb-3c|xyxyxyxyxyxyxyxyxyxyxy|-3-3--3|f=2.5|o=S4!|t=AuKVXY|n=;|xy|-3|3032mm-3|3036-3tail\n",
    "[ts5]ab0c||00-0|f=-0|o=S5!|t=AuKV|n=;||0|3031mmm0|3035m0tail\n",
];

#[test]
fn concatenation_paths_match_php() {
    assert_eq!(run(SCRIPT), EXPECTED.concat());
}

/// `main()` three times: the second and third runs find the tape's rows
/// (and the in-place growth of their temporaries and variables) warm.
#[test]
fn concatenation_paths_match_php_when_warm() {
    let warm = SCRIPT.replace("\nmain();\n", "\nmain(); main(); main();\n");
    assert_ne!(warm, SCRIPT);
    assert_eq!(run(&warm), EXPECTED.concat().repeat(3));
}

/// A string block of `class_capacity(len)` inline bytes is charged what
/// the exactly sized block is (`php_memlimit::zend_size`, php-src's size
/// classes, what the accountant charges a block aligned to 8), so the
/// room left for in-place growth changes no `memory_get_usage()` figure
/// and no memory-limit decision.
#[test]
fn class_capacity_is_charged_as_the_exact_block() {
    use php_memlimit::{zend_size, MAX_SMALL_SIZE};
    use php_runtime::string::{class_capacity, STRING_HEADER_SIZE};
    for len in 0..=MAX_SMALL_SIZE + 64 {
        let exact = zend_size(STRING_HEADER_SIZE + len);
        let spare = zend_size(STRING_HEADER_SIZE + class_capacity(len));
        assert_eq!(exact, spare, "{len}");
        if STRING_HEADER_SIZE + len <= MAX_SMALL_SIZE {
            assert_eq!(STRING_HEADER_SIZE + class_capacity(len), exact, "{len}");
        }
    }
}
