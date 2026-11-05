//! Compile-time validation of constant expressions
//! (`zend_compile_const_expr`, `zend_is_allowed_in_const_expr`).

use super::{CResult, Compiler};
use crate::ast::{self, ArgKind, ArgList, Callee, ClassRef, Expr, ExprKind, MemberName, NewClass};

const INVALID: &str = "Constant expression contains invalid operations";

fn is_static_name(n: &ast::Name) -> bool {
    n.kind != ast::NameKind::FullyQualified && n.name.eq_ignore_ascii_case(b"static")
}

impl Compiler {
    /// The class operand `(expr)::NAME` of a class constant in a constant
    /// expression. `zend_eval_const_expr` folds it first: with a literal
    /// constant name, a folded non-string class is "Illegal class name"
    /// (`zend_resolve_class_name_ast`); then
    /// `zend_compile_const_expr_class_const` rejects an unfolded class and,
    /// with `zend_throw_error` during compilation (a plain fatal error), a
    /// folded non-string one. A string literal class is allowed.
    #[cold]
    fn check_const_expr_class_operand(
        &self,
        class: &Expr,
        name: &MemberName,
        line: u32,
    ) -> CResult<()> {
        let Some(v) = self.try_fold(class) else {
            return self.err(
                "Dynamic class names are not allowed in compile-time class constant references",
                line,
            );
        };
        if matches!(v, php_runtime::Value::Str(_)) {
            return Ok(());
        }
        let literal_name = match name {
            MemberName::Ident(_) => true,
            MemberName::Expr(x) => matches!(self.try_fold(x), Some(php_runtime::Value::Str(_))),
        };
        if literal_name {
            return self.err("Illegal class name", line);
        }
        self.err("Class name must be a valid object or a string", line)
    }

    /// Rejects what php-src rejects in a constant expression;
    /// `allow_dynamic` permits `new` and object casts (attribute
    /// arguments, parameter defaults, global constants).
    pub(crate) fn check_const_expr(&self, e: &Expr, allow_dynamic: bool) -> CResult<()> {
        let line = e.span.line;
        let rec = |x: &Expr| self.check_const_expr(x, allow_dynamic);
        match &e.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::String(_)
            | ExprKind::Const(_)
            | ExprKind::MagicConst(_) => Ok(()),
            ExprKind::Array { items, .. } => {
                for it in items.iter().flatten() {
                    if let Some(k) = &it.key {
                        rec(k)?;
                    }
                    rec(&it.value)?;
                }
                Ok(())
            }
            ExprKind::ClassConst { class, name } => {
                match &**class {
                    ClassRef::Expr(x) => self.check_const_expr_class_operand(x, name, line)?,
                    ClassRef::Name(n) if is_static_name(n) => {
                        return self.err(
                            "\"static::\" is not allowed in compile-time constants",
                            line,
                        )
                    }
                    ClassRef::Name(_) => {}
                }
                if let MemberName::Expr(x) = name {
                    rec(x)?;
                }
                Ok(())
            }
            ExprKind::ClassName(class) => match &**class {
                ClassRef::Expr(_) => self.err(
                    "(expression)::class cannot be used in constant expressions",
                    line,
                ),
                ClassRef::Name(n) if is_static_name(n) => self.err(
                    "static::class cannot be used for compile-time class name resolution",
                    line,
                ),
                ClassRef::Name(_) => Ok(()),
            },
            ExprKind::Dim { base, index } => {
                rec(base)?;
                match index {
                    Some(i) => rec(i),
                    None => self.err("Cannot use [] for reading", line),
                }
            }
            ExprKind::Prop { object, name, .. } => {
                rec(object)?;
                if let MemberName::Expr(x) = name {
                    rec(x)?;
                }
                Ok(())
            }
            ExprKind::Binary { left, right, .. } => {
                rec(left)?;
                rec(right)
            }
            ExprKind::Unary { expr, .. } => rec(expr),
            ExprKind::Cast { kind, expr } => {
                if *kind == ast::CastKind::Object && !allow_dynamic {
                    return self.err("Object casts are not supported in this context", line);
                }
                rec(expr)
            }
            ExprKind::Conditional {
                cond, then, else_, ..
            } => {
                rec(cond)?;
                if let Some(t) = then {
                    rec(t)?;
                }
                rec(else_)
            }
            ExprKind::New { class, args } => {
                if !allow_dynamic {
                    return self.err("New expressions are not supported in this context", line);
                }
                match &**class {
                    NewClass::Anonymous(_) => {
                        return self.err("Cannot use anonymous class in constant expression", line)
                    }
                    NewClass::Expr(_) => {
                        return self
                            .err("Cannot use dynamic class name in constant expression", line)
                    }
                    NewClass::Name(n) if is_static_name(n) => {
                        return self
                            .err("\"static\" is not allowed in compile-time constants", line)
                    }
                    NewClass::Name(_) => {}
                }
                if args.is_first_class_callable() {
                    return self.err("Cannot create Closure for new expression", line);
                }
                self.check_const_args(args, allow_dynamic, line)
            }
            ExprKind::Closure(c) => {
                if !c.is_static {
                    return self.err("Closures in constant expressions must be static", line);
                }
                if !c.uses.is_empty() {
                    return self.err("Cannot use(...) variables in constant expression", line);
                }
                Ok(())
            }
            ExprKind::Call { callee, args } => {
                if !args.is_partial() {
                    return self.err(INVALID, line);
                }
                if let Callee::Expr(x) = &**callee {
                    // zend_compile_const_expr_fcc(): a literal callee that is
                    // not a string (`(0)(...)`).
                    if matches!(x.kind, ExprKind::Int(_) | ExprKind::Float(_)) {
                        return self.err("Illegal function name", line);
                    }
                    return self.err(
                        "Cannot use dynamic function name in constant expression",
                        line,
                    );
                }
                self.check_const_args(args, allow_dynamic, line)
            }
            ExprKind::StaticCall { class, name, args } => {
                if !args.is_partial() {
                    return self.err(INVALID, line);
                }
                match &**class {
                    ClassRef::Expr(_) => {
                        return self
                            .err("Cannot use dynamic class name in constant expression", line)
                    }
                    ClassRef::Name(n) if is_static_name(n) => {
                        return self
                            .err("\"static\" is not allowed in compile-time constants", line)
                    }
                    ClassRef::Name(_) => {}
                }
                if let MemberName::Expr(_) = name {
                    return self.err(
                        "Cannot use dynamic method name in constant expression",
                        line,
                    );
                }
                self.check_const_args(args, allow_dynamic, line)
            }
            _ => self.err(INVALID, line),
        }
    }

    fn check_const_args(&self, args: &ArgList, allow_dynamic: bool, line: u32) -> CResult<()> {
        for a in &args.args {
            match &a.kind {
                ArgKind::Unpack(_) => {
                    return self.err(
                        "Argument unpacking in constant expressions is not supported",
                        line,
                    )
                }
                ArgKind::Positional(x) | ArgKind::Named { value: x, .. } => {
                    self.check_const_expr(x, allow_dynamic)?
                }
                _ => {}
            }
        }
        Ok(())
    }
}
