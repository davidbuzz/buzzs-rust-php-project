//! Opcode array disassembler in `zend_dump.c` style.
//!
//! Used for `opcache.opt_debug_level` (0x10000 / 0x20000) and debugging.

use crate::opcode::{ClassSpec, Op, OpArray, Operand};
use php_runtime::Value;
use std::fmt::Write;

/// Formats `op_array` in `zend_dump.c` style.
#[must_use]
pub fn dump_op_array(op_array: &OpArray, title: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "; (lines={}, vars={}, tmps={})",
        op_array.ops.len(),
        op_array.cv_names.len(),
        op_array.num_tmps
    );
    if !title.is_empty() {
        let _ = writeln!(out, "; ({title})");
    }
    for (i, op) in op_array.ops.iter().enumerate() {
        let _ = write!(out, "{i:04} ");
        format_op(&mut out, op, op_array);
        let _ = writeln!(out);
    }
    out
}

fn format_operand(out: &mut String, op: &Operand, op_array: &OpArray) {
    match op {
        Operand::Cv(idx) => {
            if let Some(name) = op_array.cv_names.get(*idx as usize) {
                let _ = write!(
                    out,
                    "CV{idx}(${})",
                    String::from_utf8_lossy(name.as_bytes())
                );
            } else {
                let _ = write!(out, "CV{idx}");
            }
        }
        Operand::Tmp(t) => {
            let _ = write!(out, "T{t}");
        }
        Operand::TmpKeep(t) => {
            let _ = write!(out, "T{t}[keep]");
        }
        Operand::Lit(l) => {
            if let Some(val) = op_array.literals.get(*l as usize) {
                match val {
                    Value::Null => out.push_str("null"),
                    Value::Bool(b) => {
                        let _ = write!(out, "bool({b})");
                    }
                    Value::Int(i) => {
                        let _ = write!(out, "int({i})");
                    }
                    Value::Float(f) => {
                        let _ = write!(out, "float({f})");
                    }
                    Value::Str(s) => {
                        let _ =
                            write!(out, "string(\"{}\")", String::from_utf8_lossy(s.as_bytes()));
                    }
                    Value::Array(_) => out.push_str("array(...)"),
                    _ => {
                        let _ = write!(out, "lit({l})");
                    }
                }
            } else {
                let _ = write!(out, "lit({l})");
            }
        }
        Operand::Unused => out.push_str("unused"),
    }
}

#[allow(dead_code)]
fn format_class_spec(out: &mut String, class: &ClassSpec, op_array: &OpArray) {
    match class {
        ClassSpec::Named(l) => {
            if let Some(Value::Str(s)) = op_array.literals.get(*l as usize) {
                let _ = write!(out, "{}", String::from_utf8_lossy(s.as_bytes()));
            } else {
                let _ = write!(out, "class_lit({l})");
            }
        }
        ClassSpec::SelfClass => out.push_str("self"),
        ClassSpec::Parent => out.push_str("parent"),
        ClassSpec::Static => out.push_str("static"),
        ClassSpec::Dyn(op) => format_operand(out, op, op_array),
    }
}

fn format_op(out: &mut String, op: &Op, op_array: &OpArray) {
    match op {
        Op::Nop => out.push_str("NOP"),
        Op::Move { src, dst } => {
            let _ = write!(out, "T{dst} = QM_ASSIGN ");
            format_operand(out, src, op_array);
        }
        Op::Bin { op, a, b, dst } => {
            let _ = write!(out, "T{dst} = {:?} ", op);
            format_operand(out, a, op_array);
            out.push(' ');
            format_operand(out, b, op_array);
        }
        Op::Unary { op, a, dst } => {
            let _ = write!(out, "T{dst} = {:?} ", op);
            format_operand(out, a, op_array);
        }
        Op::Cast { kind, a, dst } => {
            let _ = write!(out, "T{dst} = CAST({:?}) ", kind);
            format_operand(out, a, op_array);
        }
        Op::Jmp(t) => {
            let _ = write!(out, "JMP {t:04}");
        }
        Op::JmpZ(cond, t) => {
            out.push_str("JMPZ ");
            format_operand(out, cond, op_array);
            let _ = write!(out, " {t:04}");
        }
        Op::JmpNz(cond, t) => {
            out.push_str("JMPNZ ");
            format_operand(out, cond, op_array);
            let _ = write!(out, " {t:04}");
        }
        Op::JmpZEx { a, dst, target } => {
            let _ = write!(out, "T{dst} = JMPZ_EX ");
            format_operand(out, a, op_array);
            let _ = write!(out, " {target:04}");
        }
        Op::JmpNzEx { a, dst, target } => {
            let _ = write!(out, "T{dst} = JMPNZ_EX ");
            format_operand(out, a, op_array);
            let _ = write!(out, " {target:04}");
        }
        Op::JmpSet { a, dst, target } => {
            let _ = write!(out, "T{dst} = JMP_SET ");
            format_operand(out, a, op_array);
            let _ = write!(out, " {target:04}");
        }
        Op::Coalesce { a, dst, target } => {
            let _ = write!(out, "T{dst} = COALESCE ");
            format_operand(out, a, op_array);
            let _ = write!(out, " {target:04}");
        }
        Op::JmpNull { a, dst, target } => {
            let _ = write!(out, "T{dst} = JMP_NULL ");
            format_operand(out, a, op_array);
            let _ = write!(out, " {target:04}");
        }
        Op::AssignCv { cv, val, dst } => {
            if let Some(d) = dst {
                let _ = write!(out, "T{d} = ");
            }
            if let Some(name) = op_array.cv_names.get(*cv as usize) {
                let _ = write!(
                    out,
                    "ASSIGN CV{cv}(${}) ",
                    String::from_utf8_lossy(name.as_bytes())
                );
            } else {
                let _ = write!(out, "ASSIGN CV{cv} ");
            }
            format_operand(out, val, op_array);
        }
        Op::Assign { place, val, dst } => {
            if let Some(d) = dst {
                let _ = write!(out, "T{d} = ");
            }
            let _ = write!(out, "ASSIGN place({place}) ");
            format_operand(out, val, op_array);
        }
        Op::Echo(op) => {
            out.push_str("ECHO ");
            format_operand(out, op, op_array);
        }
        Op::Return { val } => {
            out.push_str("RETURN ");
            format_operand(out, val, op_array);
        }
        Op::Free(op) => {
            out.push_str("FREE ");
            format_operand(out, op, op_array);
        }
        Op::Strlen { a, dst } => {
            if let Some(d) = dst {
                let _ = write!(out, "T{d} = ");
            }
            out.push_str("STRLEN ");
            format_operand(out, a, op_array);
        }
        Op::TypeCheck { a, mask, dst } => {
            if let Some(d) = dst {
                let _ = write!(out, "T{d} = ");
            }
            let _ = write!(out, "TYPE_CHECK(0x{mask:02x}) ");
            format_operand(out, a, op_array);
        }
        Op::CmpJmp {
            op,
            a,
            b,
            dst,
            target,
            jmp_if,
            ..
        } => {
            let _ = write!(out, "T{dst} = CMP_JMP({:?}, if={jmp_if}, {target:04}) ", op);
            format_operand(out, a, op_array);
            out.push(' ');
            format_operand(out, b, op_array);
        }
        Op::BinAssignCv { op, a, b, dst, cv } => {
            let _ = write!(out, "T{dst} = BIN_ASSIGN_CV(CV{cv}, {:?}) ", op);
            format_operand(out, a, op_array);
            out.push(' ');
            format_operand(out, b, op_array);
        }
        other => {
            let _ = write!(out, "{other:?}");
        }
    }
}
