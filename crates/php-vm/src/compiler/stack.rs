//! Compile-time stack limit: `zend_check_stack_limit()` and
//! `zend_stack_limit_error()` (Zend/zend_compile.c).
//!
//! php-src checks `zend_call_stack_overflowed(EG(stack_limit))` on entry to
//! `zend_compile_expr()`, `zend_compile_var()`, `zend_delayed_compile_var()`
//! and `zend_eval_const_expr()`, so a deeply nested expression fails to
//! compile once php-src's C stack passes `zend.max_allowed_stack_size -
//! zend.reserved_stack_size`. This compiler recurses on a far larger native
//! stack with frames of another size, so the check models php-src's C
//! stack instead: a compilation starts from the bytes already in use (see
//! [`CompileStackLimit`]) and each recursive step adds what its php-src
//! counterpart takes (measured on PHP 8.3 x86-64 builds, rounded down so
//! the model never rejects code php-src accepts).

use super::{CResult, Compiler};
use crate::ast::ExprKind;
use std::cell::Cell;

/// The C stack a compilation starts from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompileStackLimit {
    /// C stack bytes in use when compilation starts (the runtime nesting
    /// plus php-src's frames down to `zend_compile_top_stmt()`).
    pub used: usize,
    /// The usable stack, `EG(stack_base) - EG(stack_limit)`.
    pub size: usize,
}

/// One `zend_compile_expr()` level through a call (a method call takes
/// ~750 bytes, a function call ~510).
pub(crate) const CALL_FRAME_BYTES: usize = 512;
/// One `zend_compile_expr()` level through a unary operator or a cast
/// (~455 bytes).
pub(crate) const UNARY_FRAME_BYTES: usize = 448;
/// One `zend_compile_expr()` level through any other expression (binary
/// operators ~160 bytes, fetches ~195-275, ternaries ~210).
pub(crate) const EXPR_FRAME_BYTES: usize = 160;
/// One `zend_delayed_compile_var()` level of a write fetch (~190-275 bytes).
pub(crate) const VAR_FRAME_BYTES: usize = 160;
/// One `zend_eval_const_expr()` level (~113 bytes).
pub(crate) const FOLD_FRAME_BYTES: usize = 112;

/// The C stack one `zend_compile_expr()` level for `kind` takes.
pub(crate) fn expr_frame_bytes(kind: &ExprKind) -> usize {
    match kind {
        ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::StaticCall { .. } => {
            CALL_FRAME_BYTES
        }
        ExprKind::Unary { .. } | ExprKind::Cast { .. } => UNARY_FRAME_BYTES,
        _ => EXPR_FRAME_BYTES,
    }
}

/// The model's running state within one compilation.
#[derive(Debug)]
pub(crate) struct StackModel {
    used: Cell<usize>,
    size: usize,
    /// A check failed where no `CResult` can carry the error (constant
    /// folding); the compiler reports it at its next expression.
    overflowed: Cell<bool>,
}

impl StackModel {
    /// The model for a compilation; `None` never overflows
    /// (`zend.max_allowed_stack_size=-1`, or compilations outside a
    /// request such as `zend_test` helpers).
    pub(crate) fn new(limit: Option<CompileStackLimit>) -> Self {
        let (used, size) = limit.map_or((0, usize::MAX), |l| (l.used, l.size));
        StackModel {
            used: Cell::new(used),
            size,
            overflowed: Cell::new(false),
        }
    }

    /// `zend_check_stack_limit()` on entering a level that takes `bytes`:
    /// false (and the level is not entered) when php-src's stack would
    /// have overflowed.
    pub(crate) fn enter(&self, bytes: usize) -> bool {
        let used = self.used.get().saturating_add(bytes);
        if used > self.size {
            self.overflowed.set(true);
            return false;
        }
        self.used.set(used);
        true
    }

    /// Leaves a level [`enter`](Self::enter) entered.
    pub(crate) fn leave(&self, bytes: usize) {
        self.used.set(self.used.get().saturating_sub(bytes));
    }

    /// Whether a check has failed.
    pub(crate) fn overflowed(&self) -> bool {
        self.overflowed.get()
    }
}

impl Compiler {
    /// `zend_stack_limit_error()`.
    #[cold]
    #[inline(never)]
    pub(crate) fn stack_limit_error<T>(&self, line: u32) -> CResult<T> {
        self.err(
            format!(
                "Maximum call stack size of {} bytes (zend.max_allowed_stack_size - zend.reserved_stack_size) reached during compilation. Try splitting expression",
                self.stack.size
            ),
            line,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{compile, CompileOptions};
    use std::rc::Rc;

    /// Parses and compiles `src` on a thread with the CLI's large stack
    /// (debug-build compiler frames are big).
    fn compile_with(src: &str, limit: Option<CompileStackLimit>) -> Result<(), String> {
        let src = src.to_owned();
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(move || {
                let program =
                    php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default())
                        .map_err(|e| e.message)?;
                let opts = CompileOptions {
                    filename: "t.php".into(),
                    stack_limit: limit,
                    ..CompileOptions::default()
                };
                compile(&program, Rc::from(src.as_bytes()), opts)
                    .map(|_| ())
                    .map_err(|e| e.message)
            })
            .expect("spawn compiler thread")
            .join()
            .expect("compiler thread")
    }

    /// `zend.max_allowed_stack_size=128K` with the default reservation.
    const LIMIT_128K: CompileStackLimit = CompileStackLimit {
        used: 24 << 10,
        size: 80 << 10,
    };

    #[test]
    fn model_enter_and_leave() {
        let m = StackModel::new(Some(CompileStackLimit {
            used: 100,
            size: 300,
        }));
        assert!(m.enter(150));
        assert!(!m.enter(100));
        assert!(m.overflowed());
        m.leave(150);
        assert!(m.enter(200));
        let unlimited = StackModel::new(None);
        assert!(unlimited.enter(usize::MAX / 2));
        assert!(!unlimited.overflowed());
    }

    #[test]
    fn deep_method_chain_overflows() {
        let src = format!("<?php $test{};", "->f()".repeat(500));
        let err = compile_with(&src, Some(LIMIT_128K)).expect_err("overflow");
        assert_eq!(
            err,
            "Maximum call stack size of 81920 bytes (zend.max_allowed_stack_size - zend.reserved_stack_size) reached during compilation. Try splitting expression"
        );
        assert!(compile_with(&src, None).is_ok());
    }

    #[test]
    fn shallow_code_compiles() {
        let src = format!("<?php $test{}; $x = 1 + 2 . 'a';", "->f()".repeat(50));
        assert!(compile_with(&src, Some(LIMIT_128K)).is_ok());
    }

    #[test]
    fn exhausted_budget_fails_at_first_expression() {
        let tiny = CompileStackLimit {
            used: 24 << 10,
            size: 16 << 10,
        };
        assert!(compile_with("<?php $a->p;", Some(tiny)).is_err());
        assert!(compile_with("<?php function f() {}", Some(tiny)).is_ok());
    }

    #[test]
    fn deep_constant_folding_overflows() {
        let src = format!("<?php $x = [{}1];", "~".repeat(1000));
        assert!(compile_with(&src, Some(LIMIT_128K)).is_err());
        assert!(compile_with(&src, None).is_ok());
    }
}
