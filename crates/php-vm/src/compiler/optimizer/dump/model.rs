// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The Zend opline model the dump works on (`zend_op` as `zend_dump_op`
//! prints it, `Zend/Optimizer/zend_dump.c`).

use php_runtime::Value;

/// An operand of a [`ZLine`].
#[derive(Clone, Debug)]
pub(crate) enum Z {
    /// `IS_UNUSED` with nothing to print.
    Unused,
    /// A compiled variable (index into the op array's `cv_names`).
    Cv(u32),
    /// A temporary (`IS_TMP_VAR` or `IS_VAR`, see [`ZLine::res_var`]).
    Tmp(u32),
    /// A literal (`IS_CONST`).
    Const(Value),
    /// An `IS_UNUSED` operand printed as a number (`ZEND_VM_OP_NUM`).
    Num(u32),
    /// A jump target: an index into the source op array, mapped to the
    /// first opline emitted for that op.
    Label(u32),
    /// A jump target given as an index into the lowered oplines.
    ZLabel(u32),
    /// An `IS_UNUSED` operand printed as text (`THIS`, `NEXT`,
    /// `(self) (exception)`, ...).
    Text(String),
}

impl Z {
    /// The temporary this operand names.
    pub(crate) fn tmp(&self) -> Option<u32> {
        match self {
            Z::Tmp(t) => Some(*t),
            _ => None,
        }
    }

    /// The compiled variable this operand names.
    pub(crate) fn cv(&self) -> Option<u32> {
        match self {
            Z::Cv(c) => Some(*c),
            _ => None,
        }
    }

    /// Whether this operand is a literal.
    pub(crate) fn is_const(&self) -> bool {
        matches!(self, Z::Const(_))
    }

    /// The integer literal this operand holds.
    pub(crate) fn const_int(&self) -> Option<i64> {
        match self {
            Z::Const(Value::Int(i)) => Some(*i),
            _ => None,
        }
    }
}

/// How `INIT_FCALL`'s used stack size (`op1.num`,
/// `zend_vm_calc_used_stack`) is computed once every op array is lowered.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StackOf {
    /// An internal function: `(ZEND_CALL_FRAME_SLOT + args) * 16`.
    Internal(u32),
    /// The user function at this index of the dumped op arrays, called
    /// with this many arguments.
    User(usize, u32),
}

/// One Zend opline.
#[derive(Clone, Debug)]
pub(crate) struct ZLine {
    /// The opcode name without `ZEND_`.
    pub op: &'static str,
    /// Printed right after the name (`(strpos)` of a frameless call).
    pub suffix: String,
    /// The result operand.
    pub res: Z,
    /// The result is an `IS_VAR` (printed `V`) rather than an `IS_TMP_VAR`.
    pub res_var: bool,
    /// The printed `extended_value` (with its leading space).
    pub ext: String,
    /// The raw `extended_value` where the passes need it (rope sizes).
    pub xv: u32,
    /// `op1`.
    pub op1: Z,
    /// `op2`.
    pub op2: Z,
    /// `op1` is an `IS_VAR` temporary (printed `V`).
    pub op1_var: bool,
    /// `op2` is an `IS_VAR` temporary.
    pub op2_var: bool,
    /// A `ZEND_VM_EXT_JMP_ADDR` target.
    pub ext_jmp: Option<Z>,
    /// The jump table of `MATCH` (`SWITCH_*`): keys with their targets,
    /// printed after `op1` and followed by ` default:` and `ext_jmp`.
    pub jt: Option<Vec<(Value, Z)>>,
    /// `ZEND_FREE_ON_RETURN`.
    pub free_on_return: bool,
    /// `INIT_FCALL`'s stack size, filled in when printing.
    pub stack: Option<StackOf>,
    /// The type bits of a call's result when the callee is known
    /// (`zend_func_info` / its declared return type).
    pub ret: Option<u32>,
    /// Temporaries `zend_compile` allocated before this line that the
    /// numbering counts here: one a pass dropped, the `finally` call
    /// variable of a `try` starting here.
    pub pre: Vec<u32>,
}

impl ZLine {
    /// An opline without operands.
    pub(crate) fn new(op: &'static str) -> Self {
        ZLine {
            op,
            suffix: String::new(),
            res: Z::Unused,
            res_var: false,
            ext: String::new(),
            xv: 0,
            op1: Z::Unused,
            op2: Z::Unused,
            op1_var: false,
            op2_var: false,
            ext_jmp: None,
            jt: None,
            free_on_return: false,
            stack: None,
            ret: None,
            pre: Vec::new(),
        }
    }

    /// Sets `op1`.
    pub(crate) fn a(mut self, z: Z) -> Self {
        self.op1 = z;
        self
    }

    /// Sets `op2`.
    pub(crate) fn b(mut self, z: Z) -> Self {
        self.op2 = z;
        self
    }

    /// Sets a TMP result.
    pub(crate) fn t(mut self, t: u32) -> Self {
        self.res = Z::Tmp(t);
        self.res_var = false;
        self
    }

    /// Sets a VAR result.
    pub(crate) fn v(mut self, t: u32) -> Self {
        self.res = Z::Tmp(t);
        self.res_var = true;
        self
    }

    /// Sets a TMP result when there is one.
    pub(crate) fn ot(self, t: Option<u32>) -> Self {
        match t {
            Some(t) => self.t(t),
            None => self,
        }
    }

    /// Sets the result operand.
    pub(crate) fn r(mut self, z: Z) -> Self {
        self.res = z;
        self
    }

    /// Sets the printed extended value.
    pub(crate) fn x(mut self, s: impl Into<String>) -> Self {
        self.ext = s.into();
        self
    }

    /// Sets the jump target printed after the operands.
    pub(crate) fn j(mut self, z: Z) -> Self {
        self.ext_jmp = Some(z);
        self
    }

    /// Whether this opline was removed.
    pub(crate) fn is_nop(&self) -> bool {
        self.op == "NOP"
    }

    /// The jump targets of this opline (`Label`/`ZLabel` operands).
    pub(crate) fn targets(&self) -> impl Iterator<Item = &Z> {
        self.jt
            .iter()
            .flatten()
            .map(|(_, z)| z)
            .chain(
                [Some(&self.op1), Some(&self.op2), self.ext_jmp.as_ref()]
                    .into_iter()
                    .flatten(),
            )
            .filter(|z| matches!(z, Z::Label(_) | Z::ZLabel(_)))
    }

    /// Every operand that may hold a jump target, mutably (the table's,
    /// `op1`, `op2`, `ext_jmp`).
    pub(crate) fn label_slots_mut(&mut self) -> impl Iterator<Item = &mut Z> {
        self.jt
            .iter_mut()
            .flatten()
            .map(|(_, z)| z)
            .chain([&mut self.op1, &mut self.op2])
            .chain(self.ext_jmp.as_mut())
    }
}
