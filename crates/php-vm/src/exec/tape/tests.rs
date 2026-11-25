// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Tests of the tape: what [`lower`] decodes, and scripts whose loops run on
//! it. Expected output from `/usr/bin/php8.5 -n`, paths normalised to
//! `t.php`.
#![allow(clippy::needless_raw_string_hashes)]

use super::{lower, Inst, TOp};
use crate::compiler::{compile, CompileOptions};
use crate::exec::hot;
use crate::opcode::{Op, OpArray};
use crate::{Vm, VmOptions};
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

fn run(src: &str) -> String {
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    vm.run_script(src.as_bytes(), "t.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

/// The op arrays of `src`: its main code and every function it declares.
fn op_arrays(src: &str) -> Vec<Rc<OpArray>> {
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    // As the VM compiles: the builtins are registered, so the special
    // functions compile to their opcodes.
    let opts = CompileOptions {
        filename: "t.php".into(),
        strlen_builtin: true,
        special_builtins: u32::MAX,
        ..CompileOptions::default()
    };
    let unit = compile(&program, Rc::from(src.as_bytes()), opts).expect("compile");
    let mut all = vec![unit.main.code.clone()];
    all.extend(unit.decls.functions.borrow().iter().map(|f| f.code.clone()));
    all
}

/// The op index instruction `i` of `tape` jumps to.
fn target(tape: &[Inst], i: usize) -> usize {
    #[allow(clippy::cast_possible_wrap)]
    let delta = i64::from(tape[i].c as i32) / std::mem::size_of::<Inst>() as i64;
    usize::try_from(i64::try_from(i).expect("index") + delta).expect("target before the tape")
}

/// What every tape must satisfy whatever it specialises: one instruction
/// per op and the sentinel, `Exit` exactly for the ops that are not hot
/// (they run in `Vm::run`), and every jump going where its op goes.
fn check(code: &OpArray) -> Box<[Inst]> {
    let tape = lower(code);
    assert_eq!(tape.len(), code.ops.len() + 1);
    assert_eq!(tape[code.ops.len()].op, TOp::Exit);
    for (i, op) in code.ops.iter().enumerate() {
        let inst = &tape[i];
        // The call instructions also take ops `Vm::run` runs itself
        // (`InitMethodCall`, `InitUserCall` and its `SendUser`s and
        // `SendArray`), and so do the instructions of the ops the
        // tape runs with the VM at hand (a frameless call, a read through
        // the global scope).
        // An array literal's ops are `Vm::run`'s too, and its table and
        // elements instructions.
        if !matches!(
            inst.op,
            TOp::CallInit
                | TOp::CallSend
                | TOp::VmOp
                | TOp::FramelessQ
                | TOp::IncDecStatic
                | TOp::InitArrayT
                | TOp::ArrayAddT
        ) {
            assert_eq!(inst.op == TOp::Exit, !hot::is_hot(op), "op {i}: {op:?}");
        }
        // An unspecialised hot op is handed to the old fast path that
        // runs it: `hot::step`'s own arms, or `hot::step_ext`.
        if matches!(inst.op, TOp::Step | TOp::Ext) {
            assert_eq!(inst.op == TOp::Ext, !hot::is_core(op), "op {i}: {op:?}");
        }
        if matches!(inst.op, TOp::Exit | TOp::Step | TOp::Ext) {
            continue;
        }
        let label = match *op {
            // A jump on a literal that is never taken is a `Nop`.
            Op::JmpZ(crate::opcode::Operand::Lit(_), _)
            | Op::JmpNz(crate::opcode::Operand::Lit(_), _)
                if inst.op == TOp::Nop =>
            {
                None
            }
            Op::Jmp(t) | Op::JmpZ(_, t) | Op::JmpNz(_, t) => Some(t),
            Op::CmpJmp { target, .. } => Some(target),
            Op::IncDecCvCmpJmp { .. } => match code.ops.get(i + 1) {
                Some(&Op::CmpJmp { target, .. }) => Some(target),
                other => panic!("op {i}: fused with {other:?}"),
            },
            // A property or element read into its folded `AssignCv`'s
            // variable: past that op.
            Op::FetchObjProp { to: Some(_), .. } if inst.op == TOp::FetchObjPropTo => {
                Some(u32::try_from(i + 2).expect("op index"))
            }
            Op::FetchDimCv { to: Some(_), .. } => Some(u32::try_from(i + 2).expect("op index")),
            // The smart branches of `strs`: the jump of the op after them.
            Op::TypeCheck { .. } | Op::Isset { .. } | Op::Empty { .. }
                if matches!(inst.op, TOp::TypeCheckJmpC | TOp::IssetJmpC) =>
            {
                match code.ops.get(i + 1) {
                    Some(&(Op::JmpZ(_, t) | Op::JmpNz(_, t))) => Some(t),
                    other => panic!("op {i}: fused with {other:?}"),
                }
            }
            _ => None,
        };
        if let Some(label) = label {
            assert_eq!(target(&tape, i), label as usize, "op {i}: {op:?}");
            assert!(label as usize <= code.ops.len());
        }
    }
    tape
}

/// The loop of the module's opening example is one instruction that jumps
/// to itself, and the op arrays around it keep their shape.
#[test]
fn a_counting_loop_is_one_instruction() {
    let arrays = op_arrays("<?php function run($n) { for ($i = 0; $i < $n; $i++) { } }");
    let code = &arrays[1];
    let tape = check(code);
    let at = tape
        .iter()
        .position(|i| i.op == TOp::IncLtC)
        .expect("the fused step and condition");
    assert_eq!(target(&tape, at), at, "an empty body: the step is the loop");
    assert!(matches!(code.ops[at], Op::IncDecCvCmpJmp { .. }));
    // The condition also stands alone (the loop is entered through it).
    assert_eq!(tape[at + 1].op, TOp::CmpLtCC);
    assert!(tape.iter().any(|i| i.op == TOp::AssignI), "$i = 0");
}

/// Operand kinds choose the opcode: variables, temporaries, small and
/// large integer literals and floats.
#[test]
fn opcodes_name_their_operand_kinds() {
    let src = "<?php function f($a, $b) {
        $c = $a + $b; $d = $a - 7; $e = 3 * $b; $g = $a + 9000000000; $h = $a * 2.5;
        $i = ($a + $b) * $c;
        $c += $a; $d -= 2; $e *= 1.5;
        ++$c; $d--; $j = $c++; $k = --$d;
        $m = $a; $n = 9000000000; $o = null; $p = 1.5; $q = 12;
        if ($a < $b) { echo 1; }
        if ($a >= 10) { echo 2; }
        if ($a + 1 == $b) { echo 3; }
        while ($a != 0.5) { $a--; }
        return $m;
    }";
    let arrays = op_arrays(src);
    let tape = check(&arrays[1]);
    let has = |op: TOp| tape.iter().any(|i| i.op == op);
    // The compiler puts the literal of a commutative operator on the right
    // (`3 * $b` is `$b * 3`); a smart branch is named after the comparison
    // that takes its jump, here the one that skips the `if` body, with the
    // variable first (`$a >= 10` is compiled as `10 <= $a`: skipped when
    // `$a < 10`).
    for op in [
        TOp::AddCCV,
        TOp::SubCIV,
        TOp::MulCIV,
        TOp::AddCLV,
        TOp::MulCLV,
        TOp::AddCC,
        TOp::MulCTV,
        TOp::AddEqC,
        TOp::SubEqI,
        TOp::MulEqL,
        TOp::IncC,
        TOp::DecC,
        TOp::PostIncT,
        TOp::PreDecT,
        TOp::AssignC,
        TOp::AssignT,
        TOp::AssignL,
        TOp::AssignI,
        TOp::CmpGeCC,
        TOp::CmpLtCI,
        TOp::CmpNeCL,
    ] {
        assert!(
            has(op),
            "{op:?} missing in {:?}",
            tape.iter().map(|i| i.op).collect::<Vec<_>>()
        );
    }
    assert!(has(TOp::CmpNeTC) || has(TOp::CmpNeCT), "$a + 1 == $b");
}

/// A loop counting down to a literal, and one bounded by a variable on the
/// left: the fused step compares the variable first, with the mirrored
/// comparison.
#[test]
fn fused_steps_with_the_variable_on_the_right() {
    let src = "<?php function f($n) {
        for ($i = 10; $i > 0; $i--) { }
        for ($j = 0; $n > $j; $j++) { }
        for ($k = 0; $k <= $k; $k++) { break; }
    }";
    let arrays = op_arrays(src);
    let tape = check(&arrays[1]);
    let has = |op: TOp| tape.iter().any(|i| i.op == op);
    assert!(has(TOp::DecGtI), "$i > 0 after $i--");
    assert!(has(TOp::IncLtC), "$n > $j after $j++");
    // Both sides the stepped variable: not fused.
    let fused = arrays[1]
        .ops
        .iter()
        .filter(|o| matches!(o, Op::IncDecCvCmpJmp { .. }))
        .count();
    let lowered = tape
        .iter()
        .filter(|i| matches!(i.op, TOp::DecGtI | TOp::IncLtC | TOp::IncLeC))
        .count();
    assert_eq!((fused, lowered), (3, 2));
}

/// A jump on a literal is decided when the tape is built: a plain jump
/// (`while (1)`, whose test stands at the loop's end) or nothing
/// (`do { } while (0)`); `$a = $b` with a used result and a read into a
/// temporary are instructions of their own.
#[test]
fn literal_jumps_and_used_assignments_are_instructions() {
    const SRC: &str = r#"<?php
function f($n, $a) {
    $i = 0; $j = 1; $t = 0;
    while (1) { if (++$i > $n) { break; } $j += ($k = $j); $t += $a[$i & 3] + $k; }
    do { $t++; } while (0);
    do { $t += 2; } while (false);
    while (true) { $t += 3; break; }
    return [$i, $j, $t];
}
var_dump(f(5, [1, 2, 3, 4]));
"#;
    let arrays = op_arrays(SRC);
    let code = &arrays[1];
    let tape = check(code);
    let mut taken = 0;
    let mut never = 0;
    for (i, op) in code.ops.iter().enumerate() {
        match op {
            Op::JmpZ(crate::opcode::Operand::Lit(_), _)
            | Op::JmpNz(crate::opcode::Operand::Lit(_), _) => match tape[i].op {
                TOp::Jmp => taken += 1,
                TOp::Nop => never += 1,
                other => panic!("op {i}: {op:?} is {other:?}"),
            },
            Op::AssignCv { dst: Some(_), .. } => assert_eq!(tape[i].op, TOp::AssignR, "op {i}"),
            Op::FetchDimCv { to: None, .. } => assert_eq!(tape[i].op, TOp::FetchDimIT, "op {i}"),
            _ => {}
        }
    }
    assert_eq!((taken, never), (2, 2), "{:?}", code.ops);
    assert_eq!(
        run(SRC),
        "array(3) {
  [0]=>
  int(6)
  [1]=>
  int(32)
  [2]=>
  int(49)
}
"
    );
}

const ARRAY_LITERALS: &str = r#"<?php
function f($a, $b) {
    $list = [$a, $b, $a . $b, 7];
    $map = ['x' => $a, 'y' => $b, 5 => $a, '6' => $b, 'x' => $b];
    $k = 'dyn';
    $mixed = [$a, $k => $b, false => $b, true => $a];
    return [$list, $map, $mixed, [[$a], ['n' => [$b]]]];
}
function g($a) {
    $r = &$a;
    $u = [$a, $r, $undef];
    try { $o = [PHP_INT_MAX => 1, $a]; } catch (Error $e) { echo $e->getMessage(), "\n"; }
    return $u;
}
for ($i = 0; $i < 3; $i++) { $r = f($i, "s$i"); }
var_dump($r, g(4));
"#;

const ARRAY_LITERALS_OUT: &str = r#"
Warning: Undefined variable $undef in t.php on line 11
Cannot add element to the array as the next element is already occupied
array(4) {
  [0]=>
  array(4) {
    [0]=>
    int(2)
    [1]=>
    string(2) "s2"
    [2]=>
    string(3) "2s2"
    [3]=>
    int(7)
  }
  [1]=>
  array(4) {
    ["x"]=>
    string(2) "s2"
    ["y"]=>
    string(2) "s2"
    [5]=>
    int(2)
    [6]=>
    string(2) "s2"
  }
  [2]=>
  array(3) {
    [0]=>
    string(2) "s2"
    ["dyn"]=>
    string(2) "s2"
    [1]=>
    int(2)
  }
  [3]=>
  array(2) {
    [0]=>
    array(1) {
      [0]=>
      int(2)
    }
    [1]=>
    array(1) {
      ["n"]=>
      array(1) {
        [0]=>
        string(2) "s2"
      }
    }
  }
}
array(3) {
  [0]=>
  int(4)
  [1]=>
  int(4)
  [2]=>
  NULL
}
"#;

/// An array literal with an element that is no constant: its table and
/// each element with no key or a string/int literal key are instructions;
/// an element with any other key stays the generic op. The results are the
/// generic ops' (keys given twice, an integer in digits, references, an
/// undefined variable, the next index after `PHP_INT_MAX`).
#[test]
fn array_literals_are_instructions() {
    let src = ARRAY_LITERALS;
    let arrays = op_arrays(src);
    let code = &arrays[1];
    let tape = check(code);
    let (mut inits, mut adds, mut generic) = (0, 0, 0);
    for (i, op) in code.ops.iter().enumerate() {
        match op {
            Op::InitArray { .. } => {
                assert_eq!(tape[i].op, TOp::InitArrayT, "op {i}: {op:?}");
                inits += 1;
            }
            Op::ArrayAdd {
                key: None | Some(crate::opcode::Operand::Lit(_)),
                ..
            } if tape[i].op == TOp::ArrayAddT => adds += 1,
            Op::ArrayAdd { key, .. } => {
                assert!(key.is_some(), "op {i}: {op:?}");
                assert_ne!(tape[i].op, TOp::ArrayAddT, "op {i}: {op:?}");
                generic += 1;
            }
            _ => {}
        }
    }
    assert!(inits >= 7, "{inits} literals");
    assert!(adds >= 14, "{adds} elements as instructions");
    // `$k => $b` at least: the generic op converts the key.
    assert!(generic >= 1, "{generic} generic elements");
    assert_eq!(run(src), ARRAY_LITERALS_OUT);
}

const LEAN_FOREACH: &str = r#"<?php
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "d{$this->n} "; } }
function kv($n) {
    $list = []; $hash = [];
    for ($i = 0; $i < $n; $i++) { $list[] = $i * 2; $hash["k$i"] = $i; $hash[$i + 100] = "s$i"; }
    $s = 0; $ks = '';
    foreach ($list as $i => $v) { $s += $i * $v; }
    foreach ($hash as $k => $v) { $ks = $k; if (is_int($v)) { $s += $v; } else { $s += strlen($v); } }
    foreach ($list as $v) { $s += $v; }
    unset($hash['k1'], $list[1]);
    foreach ($hash as $k => $v) { $s += is_int($v) ? 1 : 2; }
    foreach ($list as $k => $v) { $s += $k; }
    foreach ([1.5, true, null, 'x', [1], new D(7)] as $k => $v) { $s += $k; }
    echo $s, ' ', $ks, ' ', $i, ' ', $k, "\n";
}
function vars($n) {
    $a = ['x' => 1, 'y' => 2, 'z' => 3];
    for ($r = 0; $r < $n; $r++) {
        $v = [1, 2]; $k = new D(100 + $r % 2);
        foreach ($a as $k => $v) { }
        $x = 5; $v = &$x; $kk = 'q'; $k = &$kk;
        foreach ($a as $k => $v) { }
        unset($v, $k);
        foreach ($a as $same => $same) { }
        $v = 'long string value ' . $r; $k = 'key ' . $r;
        foreach ($a as $k => $v) { }
    }
    echo "| $x $kk $same $k $v\n";
}
function sums($n) {
    $h = []; $w = [];
    for ($i = 0; $i < 40; $i++) { $h["foo_$i"] = 0; $h["a-key-that-is-longer-than-sixteen-bytes-$i"] = 0; $h[$i] = 0; $h["-$i"] = 0; $w["foo_$i"] = $i; }
    for ($r = 0; $r < $n; $r++) {
        foreach ($w as $k => $v) { $h[$k] += $v; $h[$k] -= 1; $h[$k] *= 1; $h[$k] |= 1; $h[$k] &= 0xffff; $h[$k] ^= 2; $h[$k] += 3; }
        $k = "a-key-that-is-longer-than-sixteen-bytes-" . ($r % 40); $one = 1;
        $h[$k] += $one;
        $d = (string)($r % 40); $h[$d] += $one;
        $m = '-' . ($r % 40); $h[$m] += $one;
        $i = $r % 40; $h[$i] += $one;
    }
    $t = 0; foreach ($h as $v) { $t += $v; }
    $b = $h; $k = 'foo_7'; $h[$k] += 1000; $big = PHP_INT_MAX; $h[$k] += $big;
    $f = ['f' => 1.5, 's' => '5']; $k2 = 'f'; $f[$k2] += $one; $k2 = 's'; $f[$k2] += $one;
    echo $t, ' ', $b['foo_7'], ' ', $h[3], ' ', count($h), ' ';
    var_dump($h['foo_7'], $f);
}
foreach ([3, 50] as $n) { kv($n); vars($n); sums($n); }
"#;

const LEAN_FOREACH_OUT: &str = r#"50 102 2 5
d7 d100 d101 d100 | 3 z z z 3
68108 24 0 160 float(9.223372036854776E+18)
array(2) {
  ["f"]=>
  float(2.5)
  ["s"]=>
  int(6)
}
86053 149 49 5
d7 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 d100 d101 | 3 z z z 3
107736 400 4 160 float(9.223372036854778E+18)
array(2) {
  ["f"]=>
  float(2.5)
  ["s"]=>
  int(6)
}
"#;

/// The lean paths of the `foreach` step and of `$a[$key] op= $int`: what
/// they take and what they leave to the general code gives the generic
/// ops' results (lists and hashes, holes, string and other values, loop
/// variables that hold arrays, objects and references, keys that are
/// integers in digits, shared tables, overflow, missing keys).
#[test]
fn lean_foreach_steps_and_keyed_compound_writes() {
    let src = LEAN_FOREACH;
    let out = run(src);
    assert_eq!(out, LEAN_FOREACH_OUT);
    let arrays = op_arrays(src);
    let steps = arrays
        .iter()
        .flat_map(|code| check(code).into_vec())
        .filter(|inst| matches!(inst.op, TOp::FeFetchV | TOp::FeFetchKV))
        .count();
    assert!(steps >= 8, "{steps} fused foreach steps");
}

/// Ops with no specialised form keep their place: hot ones are `Step`,
/// the others `Exit`, and a string or array literal operand is not a
/// number (`/` and `%` of a number literal are specialised, and `.` of a
/// string literal is the concat instruction).
#[test]
fn unspecialised_ops_keep_their_place() {
    let src = "<?php function f($a) { $s = $a . 'x'; $t = $a + 'y'; $u = [1] + $a; $v = $a / 2; \
               $w = $a % 2; $x = $a / 'z'; return strlen($s); }";
    let arrays = op_arrays(src);
    let code = &arrays[1];
    let tape = check(code);
    for (i, op) in code.ops.iter().enumerate() {
        let (binop, b) = match *op {
            Op::Bin { op, b, .. } | Op::BinAssignCv { op, b, .. } => (Some(op), b),
            _ => (None, crate::opcode::Operand::Unused),
        };
        let Some(binop) = binop else {
            if let Op::Return { .. } = op {
                assert_eq!(tape[i].op, TOp::Exit);
            }
            continue;
        };
        // `$a / 2`, `$a % 2`: the plain-number instruction of `scalar`.
        let by_number = matches!(
            binop,
            crate::opcode::BinKind::Arith(php_runtime::BinaryOp::Div | php_runtime::BinaryOp::Mod)
        ) && matches!(b, crate::opcode::Operand::Lit(l)
            if matches!(code.literals[l as usize], php_runtime::Value::Int(_)));
        let concat = binop == crate::opcode::BinKind::Arith(php_runtime::BinaryOp::Concat);
        if by_number {
            assert!(
                matches!(tape[i].op, TOp::NumBinT | TOp::NumBinC),
                "op {i}: {op:?}"
            );
        } else if concat {
            assert_eq!(tape[i].op, TOp::ConcatT, "op {i}: {op:?}");
        } else {
            assert_eq!(tape[i].op, TOp::Step, "op {i}: {op:?}");
        }
    }
}

const NUMBERS_IN_EVERY_SPECIALISED_FORM: &str = r#"<?php
function loops($n, $f) {
    $s = 0;
    for ($i = 0; $i < $n; $i++) { $s = $s + $i; }
    echo $s, "\n";
    for ($i = $n; $i > 0; $i--) { $s = $s - 1; }
    echo $s, "\n";
    for ($i = 0; $i <= 10; ++$i) { $s += 2; }
    echo $s, " ", $i, "\n";
    for ($i = 10; $i >= 1; --$i) { $s -= 1; }
    echo $s, " ", $i, "\n";
    for ($i = 0; $i != 7; $i++) { $s = $s * 2; }
    echo $s, "\n";
    for ($i = 0; $i == 0; $i++) { $s = $i + 5; }
    echo $s, " ", $i, "\n";
    $x = 1.5;
    for ($j = 0.0; $j < $f; $j++) { $x = $x * 1.25; $x = $x - 0.5; $x = $x + $j; }
    var_dump($x, $j);
    $k = 0;
    while ($k < 5) { $k++; $t = $k * 3; $u = $t - $k; $v = $u + 100; $w = 7 - $k; $y = 3 * $k; echo $t, ":", $u, ":", $v, ":", $w, ":", $y, " "; }
    echo "\n";
    $a = 3; $b = 4;
    $c = $a + $b; $d = $a - $b; $e = $a * $b;
    $c += $a; $d -= $b; $e *= $c;
    $g = 9000000000 + $a; $h = $a + 9000000000; $m = 2.5 * $a; $o = $a * 2.5;
    var_dump($c, $d, $e, $g, $h, $m, $o);
    $p = $a; $q = 1.25; $r = null; $z = true; $big = 9000000000;
    var_dump($p, $q, $r, $z, $big);
    $i = 5; $pre = ++$i; $post = $i++; $pd = --$i; $po = $i--;
    var_dump($pre, $post, $pd, $po, $i);
    $fl = 1.5; $fa = ++$fl; $fb = $fl--; $fl++; $fl--; --$fl;
    var_dump($fa, $fb, $fl);
    $cnt = 0;
    do { $cnt += 3; } while ($cnt < 20);
    var_dump($cnt);
    $l = 0; $hits = 0;
    while (true) { $l = $l + 1; if ($l > 6) { break; } if ($l == 3) { continue; } $hits += $l; }
    var_dump($l, $hits);
    return $s + ($x > 3 ? 1 : 2);
}
var_dump(loops(10, 4.0));
"#;

/// Loops and arithmetic on plain ints and floats inside a function (its
/// compiled variables are not mirrored in a symbol table): every family of
/// specialised instruction with variable, temporary and literal operands.
#[test]
fn numbers_in_every_specialised_form() {
    for code in op_arrays(NUMBERS_IN_EVERY_SPECIALISED_FORM) {
        check(&code);
    }
    assert_eq!(
        run(NUMBERS_IN_EVERY_SPECIALISED_FORM),
        r#"45
35
57 11
47 0
6016
5 1
float(7.841796875)
float(4)
3:2:102:6:3 6:4:104:5:6 9:6:106:4:9 12:8:108:3:12 15:10:110:2:15 
int(10)
int(-5)
int(120)
int(9000000003)
int(9000000003)
float(7.5)
float(7.5)
int(3)
float(1.25)
NULL
bool(true)
int(9000000000)
int(6)
int(6)
int(6)
int(6)
int(5)
float(2.5)
float(2.5)
float(0.5)
int(21)
int(7)
int(18)
int(6)
"#
    );
}

const OPERANDS_THAT_DECLINE: &str = r#"<?php
function ovf() {
    $i = PHP_INT_MAX - 2;
    for ($k = 0; $k < 4; $k++) { $i++; }
    var_dump($i);
    $i = PHP_INT_MIN + 2;
    for ($k = 0; $k < 4; $k++) { $i--; }
    var_dump($i);
    $a = PHP_INT_MAX; $b = 1;
    $c = $a + $b; var_dump($c);
    $c = $a * 2; var_dump($c);
    $c = PHP_INT_MIN; $c = $c - $b; var_dump($c);
    $d = PHP_INT_MAX; $d += 1; var_dump($d);
    $d = PHP_INT_MAX; $d *= 3; var_dump($d);
    $d = PHP_INT_MIN; $d -= 1; var_dump($d);
    $n = PHP_INT_MAX - 1;
    for ($j = PHP_INT_MAX - 3; $j < $n; $j++) { echo "j "; }
    var_dump($j);
    for ($j = PHP_INT_MAX - 1; $j <= PHP_INT_MAX; $j++) { if ($j > PHP_INT_MAX - 1 && $j + 1 == $j) { break; } echo "x "; }
    var_dump($j);
    $e = PHP_INT_MAX; $pre = ++$e; var_dump($pre, $e);
    $e = PHP_INT_MAX; $post = $e++; var_dump($post, $e);
    $e = PHP_INT_MIN; $post = $e--; var_dump($post, $e);
}
ovf();
function mixed() {
    $s = "5"; $n = null; $t = true; $f = 1.5; $i = 2;
    $a = $s + $i; $b = $i + $n; $c = $t * $i; $d = $f + $i; $e = $i - $f; $g = $i * $f;
    var_dump($a, $b, $c, $d, $e, $g);
    $i += $f; var_dump($i);
    $f -= 1; var_dump($f);
    $s += 1; var_dump($s);
    $n++; var_dump($n);
    $q = "9"; $q++; var_dump($q);
    $x = null; $x--; var_dump($x);
    $arr = [1]; $arr2 = $arr + [5, 6]; var_dump($arr2);
    for ($k = "0"; $k < 3; $k++) { echo "k=", $k, " "; }
    var_dump($k);
    for ($k = 0; $k < "3"; $k++) { echo "k=", $k, " "; }
    var_dump($k);
    for ($k = 0; $k < 2.5; $k++) { echo "k=", $k, " "; }
    var_dump($k);
    for ($k = 0.5; $k < 3; $k++) { echo "k=", $k, " "; }
    var_dump($k);
    $nan = NAN; $one = 1;
    var_dump($nan < $one, $nan > $one, $nan == $nan, $nan != $nan, $one <= $nan, $one >= $nan);
    $if = 1; $ff = 1.0;
    var_dump($if == $ff, $if != $ff, $if < $ff, $if <= $ff, $ff >= $if, $ff > $if);
    if ($if == $ff) { echo "eq\n"; } else { echo "ne\n"; }
    if ($nan == $nan) { echo "nan eq\n"; } else { echo "nan ne\n"; }
    if ($nan != $nan) { echo "nan ne\n"; } else { echo "nan eq\n"; }
    if ($one < $nan) { echo "lt\n"; } else { echo "not lt\n"; }
    if (!($one < $nan)) { echo "not lt\n"; }
    while ($one <= $nan) { echo "never\n"; }
}
mixed();
function undef() {
    $s = 0;
    for ($i = 0; $i < 2; $i++) {
        $s = $s + $u;
        $w = $v * 2;
        $s += $zz;
    }
    var_dump($s, $w);
    $c++;
    var_dump($c);
    $d--;
    var_dump($d);
    $e += 5;
    var_dump($e);
    if ($nope < 3) { echo "lt\n"; }
    $r = $nope2;
    var_dump($r);
}
undef();
"#;

/// What the specialised instructions hand back, having changed nothing:
/// int overflows (`++`, `--`, `+`, `-`, `*`, the compound assignments and
/// the fused loop step), numeric strings, null, bools, mixed int/float,
/// NAN comparisons and undefined variables (warnings on their own lines).
#[test]
fn operands_that_decline() {
    for code in op_arrays(OPERANDS_THAT_DECLINE) {
        check(&code);
    }
    assert_eq!(
        run(OPERANDS_THAT_DECLINE),
        r#"float(9.223372036854776E+18)
float(-9.223372036854776E+18)
float(9.223372036854776E+18)
float(1.8446744073709552E+19)
float(-9.223372036854776E+18)
float(9.223372036854776E+18)
float(2.7670116110564327E+19)
float(-9.223372036854776E+18)
j j int(9223372036854775806)
x int(9223372036854775807)
float(9.223372036854776E+18)
float(9.223372036854776E+18)
int(9223372036854775807)
float(9.223372036854776E+18)
int(-9223372036854775808)
float(-9.223372036854776E+18)
int(7)
int(2)
int(2)
float(3.5)
float(0.5)
float(3)
float(3.5)
float(0.5)
int(6)
int(1)
int(10)

Warning: Decrement on type null has no effect, this will change in the next major version of PHP in t.php on line 35
NULL
array(2) {
  [0]=>
  int(1)
  [1]=>
  int(6)
}
k=0 k=1 k=2 int(3)
k=0 k=1 k=2 int(3)
k=0 k=1 k=2 int(3)
k=0.5 k=1.5 k=2.5 float(3.5)
bool(false)
bool(false)
bool(false)
bool(true)
bool(false)
bool(false)
bool(true)
bool(false)
bool(false)
bool(true)
bool(true)
bool(false)
eq
nan ne
nan ne
not lt
not lt

Warning: Undefined variable $u in t.php on line 60

Warning: Undefined variable $v in t.php on line 61

Warning: Undefined variable $zz in t.php on line 62

Warning: Undefined variable $u in t.php on line 60

Warning: Undefined variable $v in t.php on line 61

Warning: Undefined variable $zz in t.php on line 62
int(0)
int(0)

Warning: Undefined variable $c in t.php on line 65
int(1)

Warning: Undefined variable $d in t.php on line 67

Warning: Decrement on type null has no effect, this will change in the next major version of PHP in t.php on line 67
NULL

Warning: Undefined variable $e in t.php on line 69
int(5)

Warning: Undefined variable $nope in t.php on line 71
lt

Warning: Undefined variable $nope2 in t.php on line 72
NULL
"#
    );
}

const REFERENCES_SYMBOL_TABLES_DESTRUCTORS_AND_FRAMES: &str = r#"<?php
function refs() {
    $i = 0; $r = &$i;
    for (; $i < 3; $i++) { $r = $r + 10; echo $i, " "; }
    var_dump($i, $r);
    $a = 1; $b = &$a; $c = $a + $b; $a += $b; $b++; $d = $b * 2; $e = $b;
    var_dump($a, $b, $c, $d, $e);
    $x = 5; $y = &$x; $x = 7; var_dump($y); $x = $x + 1; var_dump($y);
    $z = 0; $q = &$z;
    while ($z < 3) { $z++; }
    var_dump($q);
}
refs();
function symtab($n) {
    $s = 0;
    for ($i = 0; $i < $n; $i++) {
        $s = $s + $i;
        if ($i == 2) { $name = 's'; $$name = 100; }
        if ($i == 4) { extract(['s' => 1000]); }
        $s += 1;
    }
    var_dump($s, compact('s', 'i'));
    $vars = get_defined_vars();
    var_dump(count($vars), $vars['s'], $vars['name'], isset($vars['vars']));
    for ($j = 0; $j < 3; $j++) { $s = $s + 1; $t = $s * 2; }
    var_dump($s, $t, $j);
}
symtab(6);
$g = 0;
for ($i = 0; $i < 5; $i++) { $g = $g + $i; $h = $g * 2; $g += 1; }
var_dump($g, $h, $i, $GLOBALS['g']);
function globs() {
    global $g;
    static $st = 0;
    for ($i = 0; $i < 3; $i++) { $g = $g + 1; $st++; $g += 2; $st = $st + 1; }
    return $st;
}
var_dump(globs(), globs(), $g);
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "d", $this->n, " "; } }
function dtor() {
    for ($i = 0; $i < 3; $i++) {
        $o = new D($i);
        $o = $i;
        echo "a", $i, " ";
        $p = new D($i + 10);
        $p = $i * 2;
        echo "b", $i, " ";
        $q = new D($i + 20);
        $q = $i + $i;
        echo "c", $i, "\n";
    }
    $t = new D(99);
    $t = 1 + 2;
    echo "end\n";
    $str = "abc"; $str = 5; $str2 = "x"; $str2 = $str + 1; var_dump($str, $str2);
    $arr = [1, 2]; $arr = 3 * 3; var_dump($arr);
}
dtor();
function gen($n) { for ($i = 0; $i < $n; $i++) { $v = $i * 2; yield $v; $v = $v + 1; } return $v; }
$gen = gen(3);
foreach ($gen as $k => $v) { echo $k, "=>", $v, " "; }
echo $gen->getReturn(), "\n";
function thrower($n) {
    $s = 0;
    for ($i = 0; $i < $n; $i++) {
        try {
            $s = $s + $i;
            if ($i == 2) { throw new Exception("at " . $i); }
            $s += 1;
        } catch (Exception $e) {
            echo $e->getMessage(), " line ", $e->getLine(), "\n";
            $s = $s * 10;
        } finally {
            $s++;
        }
    }
    return $s;
}
var_dump(thrower(4));
function rec($n) { if ($n <= 1) { return 1; } return $n * rec($n - 1); }
var_dump(rec(10), rec(20), rec(21));
function fib($n) { return $n < 2 ? $n : fib($n - 1) + fib($n - 2); }
var_dump(fib(20));
function nested() {
    $t = 0;
    for ($i = 0; $i < 4; $i++) { for ($j = $i; $j < 4; $j++) { if ($j == $i) { continue; } $t = $t + $i * $j; } }
    return $t;
}
var_dump(nested());
function selfcmp() {
    $c = 0;
    for ($i = 0; $i <= $i; $i++) { if ($i >= 3) { break; } $c++; }
    for ($k = 5; $k > $k - 1; $k--) { if ($k < 3) { break; } $c++; }
    return $c;
}
var_dump(selfcmp());
"#;

/// Slots the tape must not write through: references, a symbol table
/// attached in mid-loop (`$$name`, `extract()`, `get_defined_vars()`),
/// global code, `global` and `static` variables; values whose release
/// runs a destructor; and loops across generator resumes, exceptions with
/// `finally`, and recursive calls and returns made inside the loop.
#[test]
fn references_symbol_tables_destructors_and_frames() {
    for code in op_arrays(REFERENCES_SYMBOL_TABLES_DESTRUCTORS_AND_FRAMES) {
        check(&code);
    }
    assert_eq!(
        run(REFERENCES_SYMBOL_TABLES_DESTRUCTORS_AND_FRAMES),
        r#"10 int(11)
int(11)
int(3)
int(3)
int(2)
int(6)
int(3)
int(7)
int(8)
int(3)
int(1007)
array(2) {
  ["s"]=>
  int(1007)
  ["i"]=>
  int(6)
}
int(4)
int(1007)
string(1) "s"
bool(false)
int(1010)
int(2020)
int(3)
int(15)
int(28)
int(5)
int(15)
int(6)
int(12)
int(33)
d0 a0 d10 b0 d20 c0
d1 a1 d11 b1 d21 c1
d2 a2 d12 b2 d22 c2
d99 end
int(5)
int(6)
int(9)
0=>0 1=>2 2=>4 5
at 2 line 68
int(76)
int(3628800)
int(2432902008176640000)
float(5.109094217170944E+19)
int(6765)
int(11)
int(6)
"#
    );
}

const PROPS_IN_EVERY_SPECIALISED_FORM: &str = r#"<?php
class P { public int $x = 10; public string $s = "ab"; public float $f = 1.5; public function __construct() {} }
class Q { public int $y = 3; }
function props($o) {
    echo $o->x, " "; $o->x = 20; $o->x += 5; $o->x++; $p = $o->x++;
    echo $o->x, " ", $p, "\n";
    if (isset($o->x)) { echo "set "; }
    if (empty($o->s)) { echo "empty "; }
    $v = $o->x; var_dump($v);
    $m = $o->x; echo $m, "\n";
    $o->x = $o->x = 9; echo $o->x, "\n";
    $n = new P(); var_dump($n->x);
    $q = new Q(); var_dump($q->y);
    return $o->x;
}
$o = new P(); var_dump(props($o));
"#;

/// Object properties and `new` in every specialised form: reads, writes,
/// compound assignments, `++`/`--`, `isset`/`empty`, chained assignment,
/// and `new` on both the ctor path (fall-through) and the no-ctor path
/// (jump over the ctor call). Every props family instruction appears, the
/// tape shape is checked, and the output matches `/usr/bin/php8.5`.
#[test]
fn props_in_every_specialised_form() {
    let arrays = op_arrays(PROPS_IN_EVERY_SPECIALISED_FORM);
    for code in &arrays {
        check(code);
    }
    let tape = check(&arrays[1]);
    let has = |op: TOp| tape.iter().any(|i| i.op == op);
    for op in [
        TOp::FetchObjProp,
        // `$v = $o->x;`: the read into the folded `AssignCv`'s variable.
        TOp::FetchObjPropTo,
        TOp::AssignObjProp,
        TOp::AssignOpObjProp,
        TOp::IncDecObjProp,
        TOp::IssetObjProp,
        // `new`: a call instruction now (`calls::new_object`).
        TOp::CallInit,
    ] {
        assert!(
            has(op),
            "{op:?} missing in {:?}",
            tape.iter().map(|i| i.op).collect::<Vec<_>>()
        );
    }
    assert_eq!(
        run(PROPS_IN_EVERY_SPECIALISED_FORM),
        "10 27 26\nset int(27)\n27\n9\nint(10)\nint(3)\nint(9)\n"
    );
}

const DIMS_IN_EVERY_SPECIALISED_FORM: &str = r#"<?php
function dims($n) {
    $a = [1, 2, 3, 4, 5];
    $s = 0;
    for ($i = 0; $i < $n; $i++) {
        $s = $s + $a[$i];
        $a[$i] = $a[$i] + 1;
        $a[$i] += 2;
        if (isset($a[$i])) { echo "set ", $a[$i], " "; }
        if (empty($a[$i])) { echo "empty "; }
    }
    var_dump($s, $a);
    $d = [[1,2],[3,4]];
    $d[0][1] = 9; $d[1][0] += 5;
    var_dump($d);
    $v = $d[0][1];
    var_dump($v);
    $x = 1; $x .= 'a'; $x .= 'b';
    var_dump($x);
    $c = [10, 20];
    $c[0]++; $c[1]--; $p = $c[0]++; $q = --$c[1];
    var_dump($c, $p, $q);
    $str = "hello";
    echo $str[1], $str[4], "\n";
    $big = 0;
    for ($i = 0; $i < 3; $i++) { $big += $a[$i] * 2; }
    var_dump($big);
    $k = ['x' => 1, 'y' => 2];
    echo $k['x'], $k['y'], "\n";
    $k['z'] = 3;
    var_dump($k);
    return $a[0];
}
var_dump(dims(5));
"#;

/// Array dimensions of a variable in every specialised form: reads, writes,
/// compound assignments, `isset`/`empty`, `++`/`--`, chained dimensions,
/// string offsets, string keys, and `.=` on a variable. Every dims family
/// instruction appears, the tape shape is checked, and the output matches
/// `/usr/bin/php8.5`.
#[test]
fn dims_in_every_specialised_form() {
    let arrays = op_arrays(DIMS_IN_EVERY_SPECIALISED_FORM);
    for code in &arrays {
        check(code);
    }
    let tape = check(&arrays[1]);
    let has = |op: TOp| tape.iter().any(|i| i.op == op);
    // A read or write of one offset is one of its instructions (the
    // general one or a fast path's, from its operands).
    assert!(tape
        .iter()
        .any(|i| matches!(i.op, TOp::FetchDimC | TOp::FetchDimIC | TOp::FetchDimSC)));
    assert!(tape
        .iter()
        .any(|i| matches!(i.op, TOp::AssignDimC | TOp::AssignDimIC)));
    for op in [
        TOp::AssignOpDimC,
        TOp::IssetDimC,
        TOp::FetchDimsC,
        TOp::AssignDimsC,
        TOp::AssignOpDimsC,
        TOp::IncDecDimC,
        TOp::ConcatCv,
    ] {
        assert!(
            has(op),
            "{op:?} missing in {:?}",
            tape.iter().map(|i| i.op).collect::<Vec<_>>()
        );
    }
    assert_eq!(
        run(DIMS_IN_EVERY_SPECIALISED_FORM),
        r#"set 4 set 5 set 6 set 7 set 8 int(15)
array(5) {
  [0]=>
  int(4)
  [1]=>
  int(5)
  [2]=>
  int(6)
  [3]=>
  int(7)
  [4]=>
  int(8)
}
array(2) {
  [0]=>
  array(2) {
    [0]=>
    int(1)
    [1]=>
    int(9)
  }
  [1]=>
  array(2) {
    [0]=>
    int(8)
    [1]=>
    int(4)
  }
}
int(9)
string(3) "1ab"
array(2) {
  [0]=>
  int(12)
  [1]=>
  int(18)
}
int(11)
int(18)
eo
int(30)
12
array(3) {
  ["x"]=>
  int(1)
  ["y"]=>
  int(2)
  ["z"]=>
  int(3)
}
int(4)
"#
    );
}

const DIM_FAST_PATHS: &str = r#"<?php
function f($a, $i, $h) {
    $v = $a[$i];
    $x = $a[$i + 1];
    $y = $a[2];
    $z = $h["beta"];
    $u = $h["7"];
    $w = $a[1.5];
    echo $a[$i], "\n";
    $a[$i] = $v . $x . $y . $z . $u;
    $a[] = 5;
    $a["k"] = 1;
    $a[$i] = [1];
    $a[0] = $i + 1;
    var_dump($a, $w);
}
f([10, 20, 30], 1, ["beta" => 2, 7 => "seven"]);
"#;

/// The fast path an array read or write tries first is chosen from what
/// its operands can hold when the tape is built: an int offset (a variable,
/// a temporary, an int literal) of a read folded with its `AssignCv` or
/// into its temporary, a literal string key that is no integer key, an
/// append; none for a float or integer-string literal offset, a string
/// literal key of a write or an array literal value.
#[test]
fn dim_fast_paths_follow_the_operands() {
    use TOp::{AssignDimC, AssignDimIC, FetchDimC, FetchDimIC, FetchDimIT, FetchDimSC};
    let arrays = op_arrays(DIM_FAST_PATHS);
    let tape = check(&arrays[1]);
    let ops = |pick: fn(TOp) -> bool| {
        tape.iter()
            .map(|i| i.op)
            .filter(|&op| pick(op))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ops(|op| matches!(op, FetchDimC | FetchDimIC | FetchDimIT | FetchDimSC)),
        [FetchDimIC, FetchDimIC, FetchDimIC, FetchDimSC, FetchDimC, FetchDimC, FetchDimIT]
    );
    assert_eq!(
        ops(|op| matches!(op, AssignDimC | AssignDimIC)),
        [
            AssignDimIC,
            AssignDimIC,
            AssignDimC,
            AssignDimC,
            AssignDimIC
        ]
    );
    assert_eq!(
        run(DIM_FAST_PATHS),
        r#"
Deprecated: Implicit conversion from float 1.5 to int loses precision in t.php on line 8
20
array(5) {
  [0]=>
  int(2)
  [1]=>
  array(1) {
    [0]=>
    int(1)
  }
  [2]=>
  int(30)
  [3]=>
  int(5)
  ["k"]=>
  int(1)
}
int(20)
"#
    );
}

const STRS_IN_EVERY_SPECIALISED_FORM: &str = r#"<?php
function f($s, $a, $n) {
    $r = 0;
    for ($i = 0; $i < 3; $i++) {
        $r += strlen($s) + count($a);
        strlen($s);
        count($a);
        $b = is_int($n);
        if (is_string($s)) { $r++; }
        if (!is_array($s)) { $r++; }
        $c = isset($s);
        $d = empty($a);
        if (isset($n)) { $r++; }
        if (empty($u)) { $r++; }
    }
    var_dump($r, $b, $c, $d);
}
f("abc", [1, 2], 5);
f("", [], "5");
"#;

/// `strlen()`, `count()`, the type tests and `isset()`/`empty()` of a
/// variable, as values and as smart branches.
#[test]
fn strs_in_every_specialised_form() {
    let arrays = op_arrays(STRS_IN_EVERY_SPECIALISED_FORM);
    for code in &arrays {
        check(code);
    }
    let tape = check(&arrays[1]);
    let count = |op: TOp| tape.iter().filter(|i| i.op == op).count();
    assert_eq!(count(TOp::StrlenC), 2, "a result and an unused one");
    assert_eq!(count(TOp::CountC), 2, "a result and an unused one");
    // `!is_array()`: the `BOOL_NOT` and the `JMPZ` after the test are the
    // opposite jump (`specialise::thread_short_circuits`), which the test
    // is fused with.
    assert_eq!(count(TOp::TypeCheckC), 1, "$b = is_int($n)");
    assert_eq!(
        count(TOp::TypeCheckJmpC),
        2,
        "if (is_string()), if (!is_array())"
    );
    assert_eq!(count(TOp::IssetC), 2, "$c = isset(), $d = empty()");
    assert_eq!(count(TOp::IssetJmpC), 2, "if (isset()) / if (empty())");
    assert_eq!(
        run(STRS_IN_EVERY_SPECIALISED_FORM),
        "int(27)\nbool(true)\nbool(true)\nbool(false)\nint(12)\nbool(false)\nbool(true)\nbool(true)\n"
    );
}

const BUILTIN_CALLS: &str = r#"<?php
function f($s, $n) {
    $r = [];
    for ($i = 0; $i < 3; $i++) {
        $r[] = str_repeat($s, $n);
        $r[] = ucfirst(strrev($s));
        strrev($s);
        try { str_repeat($s, -1); } catch (ValueError $e) { $r[] = $e->getLine(); }
        $r[] = str_repeat($s, "2");
    }
    return implode("|", $r);
}
echo f("ab", 2), "\n";
"#;

/// Builtin calls by name start as call instructions; their results, an
/// exception a builtin throws (caught in the loop, with its line), an
/// argument the generic call coerces and nested calls come out as in
/// php-src.
#[test]
fn builtin_calls() {
    let arrays = op_arrays(BUILTIN_CALLS);
    for code in &arrays {
        check(code);
    }
    let tape = check(&arrays[1]);
    assert!(tape.iter().filter(|i| i.op == TOp::CallInit).count() >= 4);
    assert_eq!(
        run(BUILTIN_CALLS),
        "abab|Ba|8|abab|abab|Ba|8|abab|abab|Ba|8|abab\n"
    );
}

const USER_CALLS: &str = r#"<?php
function z() { return 'z'; }
function add($a, $b = 10) { return $a + $b; }
class K { function m($x) { return "m$x"; } static function s($x) { return "s$x"; } function __invoke($x) { return "i$x"; } }
function f($cb, $args, $o, $up) {
    $r = [];
    $k = 'k';
    $cl = function ($x) use ($k) { return "$k$x"; };
    for ($i = 0; $i < 3; $i++) {
        $r[] = call_user_func($cb, $i);
        $r[] = call_user_func_array($cb, $args);
        $r[] = call_user_func_array($cb, [$i, $i]);
        $r[] = call_user_func_array('z', []);
        $r[] = call_user_func([$o, 'm'], $i);
        $r[] = call_user_func([$o, 's'], $i + 1);
        $r[] = call_user_func($cl, $i);
        $r[] = call_user_func($o, $i);
        $r[] = call_user_func($up, "a$i");
        $r[] = call_user_func_array($up, ["b$i"]);
    }
    return implode("|", $r);
}
try { echo f('add', [1, 2], new K, 'strtoupper'), "\n"; } catch (ArgumentCountError $e) { echo $e->getMessage(), "\n"; }
function g($cb, $o) {
    $r = [];
    for ($i = 0; $i < 3; $i++) {
        $r[] = call_user_func($cb, $i, 1, 2);
        $r[] = call_user_func_array($cb, ['b' => $i, 'a' => 1]);
        $r[] = call_user_func($cb, call_user_func($cb, $i));
        $r[] = call_user_func([$o, 'm'], $i);
    }
    return implode("|", $r);
}
echo g('add', new K), "\n";
function w() { return call_user_func_array('add', array_slice(func_get_args(), 1)); }
echo w(0, 1), w(0, 1, 2), "\n";
"#;

/// The call instructions of `code`'s `InitUserCall`s, `SendUser`s and
/// `SendArray`s, in op order.
fn user_call_insts(code: &OpArray, tape: &[Inst]) -> Vec<(&'static str, TOp)> {
    code.ops
        .iter()
        .zip(tape)
        .filter_map(|(op, inst)| {
            let name = match op {
                Op::InitUserCall { .. } => "init",
                Op::SendUser { .. } => "user",
                Op::SendArray { .. } => "array",
                _ => return None,
            };
            Some((name, inst.op))
        })
        .collect()
}

/// The op array among `arrays` whose compiled variables include `name`.
fn with_cv<'a>(arrays: &'a [Rc<OpArray>], name: &str) -> &'a OpArray {
    arrays
        .iter()
        .find(|c| c.cv_names.iter().any(|n| n.as_bytes() == name.as_bytes()))
        .expect("an op array with that variable")
}

/// `call_user_func[_array]()` written by name starts as a call
/// instruction, its arguments as call sends, when nothing between it and
/// its `DoCall` starts another call (`calls::user_call_stays_prepared`);
/// a callback call as an argument and the `array_slice()` form keep the
/// generic ops. The prepared calls (functions by name, methods, static
/// methods through an object, closures with bound variables, invokable
/// objects, builtins by a variable name, arrays of arguments, an empty
/// one) and the ones handed back (more arguments than parameters, named
/// arguments, nested callbacks) come out as in php-src.
#[test]
fn user_calls() {
    let arrays = op_arrays(USER_CALLS);
    for code in &arrays {
        check(code);
    }
    let f = with_cv(&arrays, "up");
    let insts = user_call_insts(f, &check(f));
    assert!(insts.len() >= 16, "{insts:?}");
    for (name, op) in &insts {
        let want = if *name == "init" {
            TOp::CallInit
        } else {
            TOp::CallSend
        };
        assert_eq!(*op, want, "{insts:?}");
    }
    // `g`: the outer call of `call_user_func($cb, call_user_func($cb, $i))`
    // is generic, the inner one prepared.
    let has = |c: &OpArray, name: &[u8]| c.cv_names.iter().any(|n| n.as_bytes() == name);
    let g = arrays
        .iter()
        .find(|c| has(c, b"o") && !has(c, b"up"))
        .expect("g");
    let insts = user_call_insts(g, &check(g));
    let generic = insts
        .iter()
        .filter(|(name, op)| *name == "init" && *op == TOp::Exit)
        .count();
    assert_eq!(generic, 1, "{insts:?}");
    // `w`: the `array_slice()` form.
    for code in &arrays {
        for (i, op) in code.ops.iter().enumerate() {
            if let Op::SendArray { len, .. } = op {
                if *len != crate::opcode::Operand::Unused {
                    assert_eq!(check(code)[i].op, TOp::Exit, "op {i}: {op:?}");
                }
            }
        }
    }
    assert_eq!(
        run(USER_CALLS),
        "10|3|0|z|m0|s1|k0|i0|A0|B0|11|3|2|z|m1|s2|k1|i1|A1|B1|12|3|4|z|m2|s3|k2|i2|A2|B2\n\
         1|1|20|m0|2|2|21|m1|3|3|22|m2\n\
         113\n"
    );
}

const OTHER_BINARY_OPERATORS: &str = r#"<?php
function f($a, $b, $x) {
    $r = [];
    for ($i = 0; $i < 3; $i++) {
        $r[] = $a & 7; $r[] = $a | $b; $r[] = $a ^ $b; $r[] = $a << 2; $r[] = $a >> 1;
        $r[] = $a % 3; $r[] = $a / 4; $r[] = $a / $b; $r[] = $x / 2;
        $r[] = $a < $b; $r[] = $a <= $x; $r[] = $a == $b; $r[] = $a != $x;
        $r[] = $a === $b; $r[] = $a !== $x; $r[] = $a <=> $b;
        $r[] = $a << 70; $r[] = -$a >> 70; $r[] = $a % -1;
        $a++;
    }
    return implode(",", array_map(fn($v) => var_export($v, true), $r));
}
echo f(10, 3, 2.5), "\n";
try { f(1, 0, 1.0); } catch (DivisionByZeroError $e) { echo get_class($e), ": ", $e->getMessage(), "\n"; }
function g($a) { return $a % 0; }
try { g(5); } catch (DivisionByZeroError $e) { echo $e->getMessage(), "\n"; }
function h($a) { return $a << -1; }
try { h(5); } catch (ArithmeticError $e) { echo $e->getMessage(), "\n"; }
"#;

/// `/`, `%`, `<<`, `>>`, `|`, `&`, `^` and the comparisons into a
/// temporary: one instruction ([`TOp::NumBinT`]) whose plain-number cases
/// match php-src, and whose errors (a zero divisor, a negative shift) and
/// wide shifts are the generic op's.
#[test]
fn other_binary_operators() {
    let arrays = op_arrays(OTHER_BINARY_OPERATORS);
    for code in &arrays {
        check(code);
    }
    // `f`'s nineteen results are temporaries appended to `$r`.
    let all: Vec<TOp> = arrays
        .iter()
        .flat_map(|c| lower(c).to_vec())
        .map(|i| i.op)
        .collect();
    assert!(
        all.iter().filter(|&&op| op == TOp::NumBinT).count() >= 16,
        "{all:?}"
    );
    assert_eq!(run(OTHER_BINARY_OPERATORS), "2,11,9,40,5,1,2.5,3.3333333333333335,1.25,false,false,false,true,false,true,1,0,-1,0,3,11,8,44,5,2,2.75,3.6666666666666665,1.25,false,false,false,true,false,true,1,0,-1,0,4,15,15,48,6,0,3,4,1.25,false,false,false,true,false,true,1,0,-1,0\nDivisionByZeroError: Division by zero\nModulo by zero\nBit shift by negative number\n");
}

const BINARY_OPERATORS_INTO_VARIABLES: &str = r#"<?php
function k($a, $b) { $x = 0; for ($i = 0; $i < 3; $i++) { $x = $a & 6; $y = $a % $b; $x ^= $b; $y <<= 1; $z = $a < $b; $a++; } return "$x $y " . var_export($z, true); }
echo k(10, 3), "\n";
function m($a) { $s = "str"; $s = $a & 1; $r = [1]; $r = $a | 2; return [$s, $r]; }
echo implode(",", m(5)), "\n";
"#;

/// The same operators assigned to a variable (`$x = $a & 6`, through the
/// `AssignCv` after the op when the variable holds a string or an array)
/// and compound-assigned (`$x ^= $b`).
#[test]
fn binary_operators_into_variables() {
    let arrays = op_arrays(BINARY_OPERATORS_INTO_VARIABLES);
    for code in &arrays {
        check(code);
    }
    let all: Vec<TOp> = arrays
        .iter()
        .flat_map(|c| lower(c).to_vec())
        .map(|i| i.op)
        .collect();
    assert!(all.contains(&TOp::NumBinC), "{all:?}");
    assert!(all.contains(&TOp::NumAssignOpC), "{all:?}");
    assert_eq!(run(BINARY_OPERATORS_INTO_VARIABLES), "7 0 false\n1,7\n");
}

const FRAMELESS_CALLS: &str = r#"<?php
function f($x, $s) {
    $r = [];
    for ($i = 1; $i <= 3; $i++) {
        $r[] = sqrt($x); $r[] = abs($i - 2); $r[] = floor(2.5); $r[] = ord($s); $r[] = chr(64 + $i);
        $r[] = strpos($s . 'xyz', 'y'); $r[] = substr('hello', $i); $r[] = max($i, 2);
        $r[] = abs(PHP_INT_MIN); $r[] = sqrt("16"); sin($x);
        try { $r[] = intdiv($x, $i - 2); } catch (DivisionByZeroError $e) { $r[] = $e->getLine(); }
        $x++;
    }
    return implode("|", $r);
}
echo f(4, "a"), "\n";
"#;

/// Frameless calls stay on the tape: a call whose result needs no handler
/// is one instruction ([`TOp::FramelessQ`], which leaves an argument it
/// does not cover to the handler: `sqrt("16")`), also with two or three
/// arguments and with arguments computed first; any other one runs with
/// the VM at hand (a call instruction). The instruction jumps to the op
/// after the call. An error a handler raises comes out with the call's
/// line.
#[test]
fn frameless_calls() {
    let arrays = op_arrays(FRAMELESS_CALLS);
    for code in &arrays {
        check(code);
    }
    let code = &arrays[1];
    let tape = check(code);
    let quick = |name: &str| {
        code.ops.iter().zip(tape.iter()).any(|(op, inst)| {
            matches!(op, Op::FramelessICall(fc)
                if matches!(&code.literals[fc.name as usize],
                    php_runtime::Value::Str(lc) if lc.as_bytes() == name.as_bytes()))
                && inst.op == TOp::FramelessQ
        })
    };
    assert!(quick("sqrt"), "sqrt($x)");
    assert!(quick("floor"), "floor(2.5)");
    assert!(quick("ord"), "ord($s)");
    assert!(quick("sin"), "sin($x), unused");
    // The instruction of the call of `name`, with the op its jump lands on.
    let row = |name: &str| {
        code.ops
            .iter()
            .zip(tape.iter())
            .enumerate()
            .find(|(_, (op, _))| {
                matches!(op, Op::FramelessICall(fc)
                    if matches!(&code.literals[fc.name as usize],
                        php_runtime::Value::Str(lc) if lc.as_bytes() == name.as_bytes()))
            })
            .map(|(i, (op, inst))| {
                let Op::FramelessICall(fc) = op else {
                    unreachable!()
                };
                (inst.op, target(&tape, i) - i, fc.tail() + 1)
            })
    };
    // `substr('hello', $i)` and `max($i, 2)`: two arguments, sent right
    // after the call's start: past the three `Nop`s behind the op.
    assert_eq!(row("substr"), Some((TOp::FramelessQ, 4, 4)));
    assert_eq!(row("max"), Some((TOp::FramelessQ, 4, 4)));
    assert_eq!(row("ord"), Some((TOp::FramelessQ, 3, 3)));
    // `strpos($s . 'xyz', 'y')` and `chr(64 + $i)`: an argument computed
    // first, the op at the call's `DoCall`.
    assert_eq!(row("strpos"), Some((TOp::FramelessQ, 1, 1)));
    assert_eq!(row("chr"), Some((TOp::FramelessQ, 1, 1)));
    // `intdiv($x, $i - 2)`: php-src sends `$x` before `$i - 2` is computed,
    // so the call keeps its ops.
    assert_eq!(row("intdiv"), None);
    assert_eq!(
        run(FRAMELESS_CALLS),
        "2|1|2|97|A|2|ello|2|9.2233720368548E+18|4|-4|2.2360679774998|0|2|97|B|2|llo|2|9.2233720368548E+18|4|8|2.4494897427832|1|2|97|C|2|lo|3|9.2233720368548E+18|4|6\n"
    );
}

const MOVES_AND_ASSIGNMENTS: &str = r#"<?php
function f($a, $s, $n) {
    $r = []; $f = false; $z = null;
    for ($i = 0; $i < 3; $i++) {
        $x = $f ?: $i + 1; $r[] = $x;
        $x = $s ?: 'none'; $r[] = $x;
        $x = $z ?? $a; $r[] = count($x);
        $x = $s ?? 'd'; $r[] = $x;
        $x = $i ? $a : $s; $r[] = is_array($x) ? 'arr' : $x;
        $x = $i > 1 ? 'big' : $n; $r[] = $x;
        $y = $a; $y[] = $i; $r[] = count($y);
        $t = "v$i"; $u = $t; $r[] = $u;
    }
    return implode(",", $r) . ' ' . count($a);
}
echo f([1, 2], "str", 7), "\n";
class D { public $n; function __construct($n) { $this->n = $n; } function __destruct() { echo "d", $this->n, " "; } }
function g() { for ($i = 0; $i < 3; $i++) { $o = new D($i); echo "i$i "; } echo "end "; }
g(); echo "\n";
"#;

/// A ternary's arms, `?:` and `??` are instructions of their own
/// ([`TOp::MoveT`], [`TOp::JmpSetC`], [`TOp::CoalesceC`]) for values of
/// every type, and an assignment of a string, an array or an object stays
/// on the tape: the old value is released when it is overwritten (an
/// object's destructor runs before the next statement).
#[test]
fn moves_and_assignments() {
    let arrays = op_arrays(MOVES_AND_ASSIGNMENTS);
    for code in &arrays {
        check(code);
    }
    let tape = check(&arrays[1]);
    let count = |op: TOp| tape.iter().filter(|i| i.op == op).count();
    assert!(count(TOp::MoveT) >= 6, "{}", count(TOp::MoveT));
    assert_eq!(count(TOp::JmpSetC), 2, "$f ?: ..., $s ?: ...");
    assert_eq!(count(TOp::CoalesceC), 2, "$z ?? ..., $s ?? ...");
    assert_eq!(
        run(MOVES_AND_ASSIGNMENTS),
        "1,str,2,str,str,7,3,v0,2,str,2,str,arr,7,3,v1,3,str,2,str,arr,big,3,v2 2\ni0 d0 i1 d1 i2 end d2 \n"
    );
}

const VM_OPS_AND_STATICS: &str = r#"<?php
$g = 5;
class K {
    const A = 3;
    public static $n = 0;
    function r($k) { $a = 0; for ($i = 0; $i < $k; $i++) { $x = $this::A; $a += $x + $this::A; self::$n++; $a += ++self::$n; } return $a; }
}
function f($s, $k) { $a = 0; $t = ''; for ($i = 0; $i < $k; $i++) { $x = $GLOBALS['g']; $a += $x; $y = $_GET; $a += count($y); $c = $s[$i % 3]; $t .= $c; K::$n++; } return "$a $t " . K::$n; }
echo (new K)->r(3), ' ', K::$n, "\n";
echo f("abc", 4), "\n";
for ($i = 0; $i < 3; $i++) { $v = abs(-$i) + max($i, 1); echo $v; }
echo "\n";
"#;

/// `$this::NAME` is one instruction for its two ops, `++` of a static
/// property has an instruction, reads of a global by name and of a
/// superglobal run with the VM at hand without leaving the tape, a string
/// offset is read by the element read's fast path, and builtin calls at
/// the script's top level (where no call left a pooled frame) are made.
#[test]
fn vm_ops_and_statics() {
    let arrays = op_arrays(VM_OPS_AND_STATICS);
    for code in &arrays {
        check(code);
    }
    let all: Vec<TOp> = arrays
        .iter()
        .flat_map(|c| lower(c).to_vec())
        .map(|i| i.op)
        .collect();
    let count = |op: TOp| all.iter().filter(|&&o| o == op).count();
    // (The method's rows are not among these op arrays: its `$this::A`
    // and `self::$n++` show in the output below.)
    assert_eq!(count(TOp::IncDecStatic), 1, "K::$n++ in f(): {all:?}");
    assert!(count(TOp::VmOp) >= 2, "$GLOBALS['g'], $_GET: {all:?}");
    assert_eq!(run(VM_OPS_AND_STATICS), "30 6\n20 abca 10\n124\n");
}

const NEW_WITH_ARGUMENTS: &str = r#"<?php
class P { public $x; public $y; function __construct($x, $y) { $this->x = $x; $this->y = $y; } }
function same($v) { return $v; }
function run($n) {
    for ($i = 0; $i < $n; $i++) { $p = new P($i, $i + 1); $q = new P(same($i), $i); }
    return $p->x + $p->y + $q->x;
}
var_dump(run(20));
"#;

/// `new C(arguments)` whose arguments are variables, literals and sums of
/// them is the tape's `new` instruction, jumping past the constructor's
/// `DoCall` (`calls::new_plain` then runs a constructor that only stores
/// its parameters without the call); a call among the arguments keeps the
/// call instructions.
#[test]
fn new_with_arguments_at_hand_is_one_instruction() {
    let arrays = op_arrays(NEW_WITH_ARGUMENTS);
    for code in &arrays {
        check(code);
    }
    let code = arrays
        .iter()
        .find(|c| {
            c.ops
                .iter()
                .filter(|op| matches!(op, Op::New { .. }))
                .count()
                == 2
        })
        .expect("the function with the two `new`s");
    let tape = check(code);
    let news: Vec<usize> = (0..code.ops.len())
        .filter(|&i| matches!(code.ops[i], Op::New { .. }))
        .collect();
    let first = &tape[news[0]];
    assert_eq!(
        (first.op, first.k, first.e),
        (TOp::CallInit, super::calls::CALL_NEW, 1)
    );
    let end = target(&tape, news[0]);
    assert!(matches!(code.ops[end - 1], Op::DoCall { dst: None }));
    assert!(
        code.ops[news[0] + 1..end - 1]
            .iter()
            .all(|op| matches!(op, Op::Send { .. } | Op::Bin { .. })),
        "{:?}",
        &code.ops[news[0]..end]
    );
    let second = &tape[news[1]];
    assert_eq!(second.op, TOp::CallInit);
    assert_ne!(second.k, super::calls::CALL_NEW);
    assert_eq!(run(NEW_WITH_ARGUMENTS), "int(58)\n");
}
