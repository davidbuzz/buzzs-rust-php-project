// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Frame-local fast paths of string building: [`Op::Rope`] (php-src's
//! `ZEND_ROPE_INIT` / `ZEND_ROPE_ADD` / `ZEND_ROPE_END`) and `ZEND_CONCAT`
//! ([`Op::Bin`] / [`Op::BinAssignCv`] with `.`) whose operands are strings
//! or integers held directly by a literal, a defined compiled variable or
//! a temporary (ref: Zend/zend_vm_def.h `ZEND_ROPE_END`, `ZEND_CONCAT`,
//! `ZEND_FAST_CONCAT`), and `(string)` ([`Op::Cast`], `ZEND_CAST`) of
//! such a value.
//!
//! Strings and integers convert to strings without diagnostics or user
//! code, so these complete the op exactly as `Vm::op_rope` /
//! `Vm::bin_slow` would: every other operand (an undefined or referenced
//! variable, a float, whose form depends on `precision`, arrays, objects,
//! null, bools) declines, having changed nothing. The parts are looked at
//! in place (no refcount traffic for variables and literals), the result is
//! built in one exactly sized allocation, and consumed temporaries are
//! released afterwards (a string's release has no side effect).
//!
//! [`Op::Rope`]: crate::opcode::Op::Rope
//! [`Op::Bin`]: crate::opcode::Op::Bin
//! [`Op::BinAssignCv`]: crate::opcode::Op::BinAssignCv
//! [`Op::Cast`]: crate::opcode::Op::Cast

use crate::opcode::Operand;
use crate::vm::{CvSlot, Frame};
use php_runtime::convert::long_to_zstr;
use php_runtime::string::{class_capacity, long_str_len, ZStrBuf};
use php_runtime::{Value, ZStr};

/// The value operand `op` of frame `f` holds when it is a string or an
/// integer read without diagnostics (a literal, a defined compiled
/// variable not bound by reference, a temporary); `None` otherwise.
#[inline(always)]
#[allow(clippy::inline_always)] // one tag test per rope part
fn peek_str_int<'a>(f: &'a Frame, lits: &'a [Value], op: Operand) -> Option<&'a Value> {
    let v = match op {
        Operand::Lit(i) => lits.get(i as usize)?,
        Operand::Cv(cv) => f.cvs.get(cv as usize).and_then(CvSlot::get)?,
        Operand::Tmp(t) | Operand::TmpKeep(t) => f.tmps.get(t as usize)?,
        Operand::Unused => return None,
    };
    matches!(v, Value::Str(_) | Value::Int(_)).then_some(v)
}

/// The length of the string form of a string or integer part.
#[inline(always)]
#[allow(clippy::inline_always)]
fn part_len(v: &Value) -> usize {
    match v {
        Value::Str(s) => s.len(),
        Value::Int(i) => long_str_len(*i),
        _ => 0,
    }
}

/// Appends the string form of a string or integer part.
#[inline(always)]
#[allow(clippy::inline_always)]
fn push_part(out: &mut ZStrBuf, v: &Value) {
    match v {
        Value::Str(s) => out.push_str(s),
        Value::Int(i) => out.push_long(*i),
        _ => {}
    }
}

/// Releases the value of temporary operand `op` (`Vm::read` consumes it).
#[inline(always)]
#[allow(clippy::inline_always)]
fn consume_tmp(f: &mut Frame, op: Operand) {
    if let Operand::Tmp(t) = op {
        if let Some(slot) = f.tmps.get_mut(t as usize) {
            if !matches!(slot, Value::Int(_)) {
                drop(std::mem::take(slot));
            }
        }
    }
}

/// [`Op::Rope`](crate::opcode::Op::Rope) when every part is a string or an
/// integer ([`peek_str_int`]): the parts are measured, copied into one
/// exactly sized string, and the temporaries among them released. `None`,
/// nothing changed, otherwise.
#[inline]
pub(in crate::exec) fn rope_in(f: &mut Frame, lits: &[Value], parts: &[Operand]) -> Option<ZStr> {
    let mut len = 0usize;
    for &p in parts {
        len = len.saturating_add(part_len(peek_str_int(f, lits, p)?));
    }
    let mut out = ZStrBuf::with_capacity(len);
    for &p in parts {
        if let Some(v) = peek_str_int(f, lits, p) {
            push_part(&mut out, v);
        }
    }
    for &p in parts {
        consume_tmp(f, p);
    }
    Some(out.finish())
}

/// `ZEND_CONCAT` of two strings or integers, exactly as `Vm::bin_slow`
/// builds it (`concat_strings`): an empty non-literal string operand
/// yields the other string itself, a temporary left string is extended in
/// place when nothing else holds it, and anything else is a new string.
/// `None`, nothing changed, when an operand is not a plain string or
/// integer.
#[inline]
pub(in crate::exec) fn concat_in(
    f: &mut Frame,
    lits: &[Value],
    a: Operand,
    b: Operand,
) -> Option<ZStr> {
    if peek_str_int(f, lits, a).is_none() || peek_str_int(f, lits, b).is_none() {
        return None;
    }
    // A left temporary holding a string: moved out, so that a unique one
    // is extended in place (`zend_string_extend`).
    if let Operand::Tmp(ta) = a {
        if matches!(f.tmps.get(ta as usize), Some(Value::Str(_))) {
            let Some(Value::Str(s)) = f.tmps.get_mut(ta as usize).map(std::mem::take) else {
                return None;
            };
            let r = match peek_str_int(f, lits, b) {
                Some(Value::Str(t)) => Ok(if s.is_empty() {
                    t.clone()
                } else if t.is_empty() && !matches!(b, Operand::Lit(_)) {
                    s
                } else {
                    s.concat_owned_str_spare(t)
                }),
                Some(Value::Int(i)) => Ok(s.concat_owned_long_spare(*i)),
                // Unreachable (checked above, and `b` cannot be the
                // temporary just taken): the string goes back.
                _ => Err(s),
            };
            let r = match r {
                Ok(r) => r,
                Err(s) => {
                    if let Some(slot) = f.tmps.get_mut(ta as usize) {
                        *slot = Value::Str(s);
                    }
                    return None;
                }
            };
            consume_tmp(f, b);
            return Some(r);
        }
    }
    let x = peek_str_int(f, lits, a)?;
    let y = peek_str_int(f, lits, b)?;
    let r = match (x, y) {
        (Value::Str(s), Value::Str(t)) => {
            if s.is_empty() && !matches!(a, Operand::Lit(_)) {
                t.clone()
            } else if t.is_empty() && !matches!(b, Operand::Lit(_)) {
                s.clone()
            } else {
                ZStr::concat_str_spare(s, t)
            }
        }
        _ => {
            let len = part_len(x).saturating_add(part_len(y));
            let mut out = ZStrBuf::with_capacity(class_capacity(len));
            push_part(&mut out, x);
            push_part(&mut out, y);
            out.finish()
        }
    };
    consume_tmp(f, a);
    consume_tmp(f, b);
    Some(r)
}

/// `(string)` of a string (the string itself, `zval_get_string`'s added
/// reference) or an integer (`zend_long_to_str`), as `Vm::cast` does it.
/// `None`, nothing changed, for any other operand.
#[inline]
pub(in crate::exec) fn cast_string_in(f: &mut Frame, lits: &[Value], a: Operand) -> Option<ZStr> {
    let r = match peek_str_int(f, lits, a)? {
        Value::Str(s) => s.clone(),
        Value::Int(i) => long_to_zstr(*i),
        _ => return None,
    };
    consume_tmp(f, a);
    Some(r)
}

#[cfg(test)]
mod tests {
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

    /// Ropes and concatenations of strings and integers (the fast paths),
    /// with empty operands, negative numbers, in-place extension of a
    /// temporary, and the parts that decline (floats, undefined and
    /// referenced variables, arrays, null and bools). Expected output from
    /// `/usr/bin/php8.5 -n`.
    #[test]
    fn ropes_and_concats_match_php() {
        let out = run(r#"<?php
function f($a, $b, $n, $e) {
    $o = [];
    for ($i = -2; $i < 2; $i++) {
        $o[] = "[$a|$i|{$n}|$e]";
        $o[] = $a . $b . $i . $e;
        $o[] = $i . $n;
        $o[] = $e . $a;
        $o[] = $a . '';
        $o[] = strtoupper($a) . $i;
        $x = "p$i";
        $o[] = $x . $x;
    }
    $f = 1.5; $o[] = "f=$f" . $f;
    $o[] = "u=" . @$undef . PHP_INT_MIN;
    $arr = [1]; $o[] = @"a=$arr";
    $r = "r"; $ref = &$r; $o[] = "ref=$r" . $r;
    $o[] = "n=" . null . true . false;
    $o[] = (string)$i . (string)$a . (string)-7 . (string)(string)($i * 3) . (string)1.0;
    return implode(",", $o);
}
echo f("x", "y", 42, ""), "\n";
$big = str_repeat("z", 40);
for ($i = 0; $i < 3; $i++) { $s = "$big$i$big"; echo strlen($s), substr($s, 38, 5), "\n"; }
"#);
        assert_eq!(
            out,
            "[x|-2|42|],xy-2,-242,x,x,X-2,p-2p-2,[x|-1|42|],xy-1,-142,x,x,X-1,p-1p-1,\
             [x|0|42|],xy0,042,x,x,X0,p0p0,[x|1|42|],xy1,142,x,x,X1,p1p1,f=1.51.5,\
             u=-9223372036854775808,a=Array,ref=rr,n=1,2x-761\n81zz0zz\n81zz1zz\n81zz2zz\n"
        );
    }
}
