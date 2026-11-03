//! Expressions: precedence climbing over bison's precedence table, plus the
//! `variable` / `callable_variable` / `fully_dereferenceable` distinctions of
//! the grammar that decide which postfix operations are allowed.

use super::{is_identifier_token, is_name_token, PResult, Parser};
use crate::ast::{
    Arg, ArgKind, ArgList, ArrayItem, ArraySyntax, BinaryOp, Callee, CastKind, ClassRef, Expr,
    ExprKind, Ident, IncDecOp, IncludeKind, InterpPart, InterpSyntax, MagicConst, MatchArm,
    MemberName, Name, NameKind, NewClass, Span, UnaryOp, VarName,
};
use php_lexer::{TokenKind as T, TokenValue};

/// Precedence levels from the `%left`/`%right`/`%precedence` declarations
/// of the grammar (higher binds tighter).
pub(crate) mod prec {
    pub const THROW: u8 = 1;
    pub const ARROW_FN: u8 = 2;
    pub const INCLUDE: u8 = 3;
    pub const LOGICAL_OR: u8 = 4;
    pub const LOGICAL_XOR: u8 = 5;
    pub const LOGICAL_AND: u8 = 6;
    pub const PRINT: u8 = 7;
    pub const YIELD: u8 = 8;
    pub const DOUBLE_ARROW: u8 = 9;
    pub const YIELD_FROM: u8 = 10;
    pub const ASSIGN: u8 = 11;
    pub const TERNARY: u8 = 12;
    pub const COALESCE: u8 = 13;
    pub const BOOL_OR: u8 = 14;
    pub const BOOL_AND: u8 = 15;
    pub const BIT_OR: u8 = 16;
    pub const BIT_XOR: u8 = 17;
    pub const BIT_AND: u8 = 18;
    pub const EQUALITY: u8 = 19;
    pub const COMPARISON: u8 = 20;
    pub const PIPE: u8 = 21;
    pub const CONCAT: u8 = 22;
    pub const SHIFT: u8 = 23;
    pub const ADDITIVE: u8 = 24;
    pub const MULTIPLICATIVE: u8 = 25;
    pub const NOT: u8 = 26;
    pub const INSTANCEOF: u8 = 27;
    pub const UNARY: u8 = 28;
    pub const POW: u8 = 29;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Assoc {
    Left,
    Right,
    Non,
}

/// Which grammar category a parsed operand belongs to; decides what may
/// follow it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cat {
    /// `callable_variable`: `$a`, `$a[0]`, `f()`, `$a->b()`, `A::b()`.
    Var,
    /// A `variable` that is not a `callable_variable`: `$a->b`, `A::$b`.
    Prop,
    /// `callable_expr` that is not a variable: `(expr)`, dereferenceable
    /// scalars, `new A()`.
    Callable,
    /// `class_constant`: fully dereferenceable but not callable.
    ClassConst,
    /// `constant`: a bare name.
    Const,
    /// A magic constant: dereferenceable with `[`/`->` only.
    Magic,
    /// `'(' expr ')'`: like `Callable`; which grammar rule reduced the
    /// parentheses (and so which "parenthesized" flag is set) depends on
    /// what follows.
    Paren,
    /// Anything else.
    Other,
}

impl Cat {
    fn is_variable(self) -> bool {
        matches!(self, Cat::Var | Cat::Prop)
    }
    /// `array_object_dereferenceable`
    fn is_aod(self) -> bool {
        matches!(
            self,
            Cat::Var | Cat::Prop | Cat::Callable | Cat::ClassConst | Cat::Const | Cat::Magic
        )
    }
    /// `fully_dereferenceable` (usable as `variable_class_name`).
    fn is_fully_deref(self) -> bool {
        matches!(self, Cat::Var | Cat::Prop | Cat::Callable | Cat::ClassConst)
    }
}

fn binary_info(k: T) -> Option<(u8, Assoc, BinaryOp)> {
    use prec::{
        ADDITIVE, BIT_AND, BIT_OR, BIT_XOR, BOOL_AND, BOOL_OR, COALESCE, COMPARISON, CONCAT,
        EQUALITY, LOGICAL_AND, LOGICAL_OR, LOGICAL_XOR, MULTIPLICATIVE, POW, SHIFT,
    };
    Some(match k {
        T::LogicalOr => (LOGICAL_OR, Assoc::Left, BinaryOp::BoolOr),
        T::LogicalXor => (LOGICAL_XOR, Assoc::Left, BinaryOp::BoolXor),
        T::LogicalAnd => (LOGICAL_AND, Assoc::Left, BinaryOp::BoolAnd),
        T::Coalesce => (COALESCE, Assoc::Right, BinaryOp::Coalesce),
        T::BooleanOr => (BOOL_OR, Assoc::Left, BinaryOp::BoolOr),
        T::BooleanAnd => (BOOL_AND, Assoc::Left, BinaryOp::BoolAnd),
        T::Pipe => (BIT_OR, Assoc::Left, BinaryOp::BitOr),
        T::Caret => (BIT_XOR, Assoc::Left, BinaryOp::BitXor),
        T::AmpersandFollowedByVarOrVararg | T::AmpersandNotFollowedByVarOrVararg => {
            (BIT_AND, Assoc::Left, BinaryOp::BitAnd)
        }
        T::IsEqual => (EQUALITY, Assoc::Non, BinaryOp::Equal),
        T::IsNotEqual => (EQUALITY, Assoc::Non, BinaryOp::NotEqual),
        T::IsIdentical => (EQUALITY, Assoc::Non, BinaryOp::Identical),
        T::IsNotIdentical => (EQUALITY, Assoc::Non, BinaryOp::NotIdentical),
        T::Spaceship => (EQUALITY, Assoc::Non, BinaryOp::Spaceship),
        T::Lt => (COMPARISON, Assoc::Non, BinaryOp::Less),
        T::IsSmallerOrEqual => (COMPARISON, Assoc::Non, BinaryOp::LessEqual),
        T::Gt => (COMPARISON, Assoc::Non, BinaryOp::Greater),
        T::IsGreaterOrEqual => (COMPARISON, Assoc::Non, BinaryOp::GreaterEqual),
        T::Dot => (CONCAT, Assoc::Left, BinaryOp::Concat),
        T::Sl => (SHIFT, Assoc::Left, BinaryOp::ShiftLeft),
        T::Sr => (SHIFT, Assoc::Left, BinaryOp::ShiftRight),
        T::Plus => (ADDITIVE, Assoc::Left, BinaryOp::Add),
        T::Minus => (ADDITIVE, Assoc::Left, BinaryOp::Sub),
        T::Star => (MULTIPLICATIVE, Assoc::Left, BinaryOp::Mul),
        T::Slash => (MULTIPLICATIVE, Assoc::Left, BinaryOp::Div),
        T::Percent => (MULTIPLICATIVE, Assoc::Left, BinaryOp::Mod),
        T::Pow => (POW, Assoc::Right, BinaryOp::Pow),
        _ => return None,
    })
}

/// An assignment operator: `=` (`None`) or a compound operator.
fn assign_op(k: T) -> Option<AssignOp> {
    Some(match k {
        T::Equals => None,
        T::PlusEqual => Some(BinaryOp::Add),
        T::MinusEqual => Some(BinaryOp::Sub),
        T::MulEqual => Some(BinaryOp::Mul),
        T::PowEqual => Some(BinaryOp::Pow),
        T::DivEqual => Some(BinaryOp::Div),
        T::ConcatEqual => Some(BinaryOp::Concat),
        T::ModEqual => Some(BinaryOp::Mod),
        T::AndEqual => Some(BinaryOp::BitAnd),
        T::OrEqual => Some(BinaryOp::BitOr),
        T::XorEqual => Some(BinaryOp::BitXor),
        T::SlEqual => Some(BinaryOp::ShiftLeft),
        T::SrEqual => Some(BinaryOp::ShiftRight),
        T::CoalesceEqual => Some(BinaryOp::Coalesce),
        _ => return None,
    })
}

/// The operator of an assignment token: `None` for plain `=`.
type AssignOp = Option<BinaryOp>;

fn magic_const(k: T) -> Option<MagicConst> {
    Some(match k {
        T::Line => MagicConst::Line,
        T::File => MagicConst::File,
        T::Dir => MagicConst::Dir,
        T::TraitC => MagicConst::Trait,
        T::MethodC => MagicConst::Method,
        T::FuncC => MagicConst::Function,
        T::PropertyC => MagicConst::Property,
        T::NsC => MagicConst::Namespace,
        T::ClassC => MagicConst::Class,
        _ => return None,
    })
}

fn cast_kind(k: T) -> Option<CastKind> {
    Some(match k {
        T::IntCast => CastKind::Int,
        T::DoubleCast => CastKind::Float,
        T::StringCast => CastKind::String,
        T::ArrayCast => CastKind::Array,
        T::ObjectCast => CastKind::Object,
        T::BoolCast => CastKind::Bool,
        T::UnsetCast => CastKind::Unset,
        _ => return None,
    })
}

/// Whether a token can start an `expr`.
pub(crate) fn starts_expr(k: T) -> bool {
    matches!(
        k,
        T::Variable
            | T::Dollar
            | T::String
            | T::NameQualified
            | T::NameFullyQualified
            | T::NameRelative
            | T::Static
            | T::Readonly
            | T::LNumber
            | T::DNumber
            | T::ConstantEncapsedString
            | T::DoubleQuote
            | T::Backtick
            | T::StartHeredoc
            | T::LBracket
            | T::Array
            | T::List
            | T::LParen
            | T::New
            | T::Clone
            | T::Exit
            | T::Print
            | T::Yield
            | T::YieldFrom
            | T::Throw
            | T::Include
            | T::IncludeOnce
            | T::Require
            | T::RequireOnce
            | T::Eval
            | T::Isset
            | T::Empty
            | T::Function
            | T::Fn
            | T::Attribute
            | T::Match
            | T::Plus
            | T::Minus
            | T::Bang
            | T::Tilde
            | T::At
            | T::Inc
            | T::Dec
            | T::IntCast
            | T::DoubleCast
            | T::StringCast
            | T::ArrayCast
            | T::ObjectCast
            | T::BoolCast
            | T::UnsetCast
    ) || magic_const(k).is_some()
}

/// Tokens that can start a `variable` (fully dereferenceable expressions
/// included).
pub(crate) fn starts_variable(k: T) -> bool {
    matches!(
        k,
        T::Variable
            | T::Dollar
            | T::String
            | T::NameQualified
            | T::NameFullyQualified
            | T::NameRelative
            | T::Static
            | T::Readonly
            | T::ConstantEncapsedString
            | T::DoubleQuote
            | T::LBracket
            | T::Array
            | T::LParen
            | T::New
    ) || magic_const(k).is_some()
}

const AOD_FOLLOW: &[T] = &[T::ObjectOperator, T::NullsafeObjectOperator, T::LBracket];
const PROPERTY_NAME_START: &[T] = &[T::String, T::Variable, T::LBrace, T::Dollar];

fn bx(e: Expr) -> Box<Expr> {
    Box::new(e)
}

impl Parser<'_> {
    /// Parses an `expr`.
    pub(crate) fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_expr_bp(0)
    }

    /// Parses an expression, absorbing binary operators of precedence
    /// level `min` or higher.
    pub(crate) fn parse_expr_bp(&mut self, min: u8) -> PResult<Expr> {
        let lhs = self.parse_unary()?;
        self.parse_binary_loop(lhs, min)
    }

    /// Continues an expression whose first operand has been parsed.
    pub(crate) fn parse_expr_continue(&mut self, lhs: Expr) -> PResult<Expr> {
        self.parse_binary_loop(lhs, 0)
    }

    fn parse_binary_loop(&mut self, mut lhs: Expr, min: u8) -> PResult<Expr> {
        loop {
            let k = self.peek();
            if k == T::Question {
                if prec::TERNARY < min {
                    break;
                }
                lhs = self.parse_ternary(lhs)?;
                continue;
            }
            if k == T::Instanceof {
                if prec::INSTANCEOF < min {
                    break;
                }
                self.bump();
                let class = self.parse_class_name_reference()?;
                let span = self.span_from(lhs.span);
                lhs = Expr::new(
                    ExprKind::Instanceof {
                        expr: bx(lhs),
                        class: Box::new(class),
                    },
                    span,
                );
                continue;
            }
            if k == T::PipeOp {
                if prec::PIPE < min {
                    break;
                }
                self.bump();
                let rhs = self.parse_expr_bp(prec::PIPE + 1)?;
                let span = self.span_from(lhs.span);
                lhs = Expr::new(
                    ExprKind::Pipe {
                        left: bx(lhs),
                        right: bx(rhs),
                    },
                    span,
                );
                continue;
            }
            let Some((level, assoc, op)) = binary_info(k) else {
                break;
            };
            if level < min {
                break;
            }
            self.bump();
            let rhs = match assoc {
                Assoc::Left | Assoc::Non => self.parse_expr_bp(level + 1)?,
                Assoc::Right => self.parse_expr_bp(level)?,
            };
            let span = self.span_from(lhs.span);
            lhs = Expr::new(
                ExprKind::Binary {
                    op,
                    left: bx(lhs),
                    right: bx(rhs),
                },
                span,
            );
            if assoc == Assoc::Non {
                if let Some((l2, Assoc::Non, _)) = binary_info(self.peek()) {
                    if l2 == level {
                        return Err(self.unexpected(&[]));
                    }
                }
            }
        }
        Ok(lhs)
    }

    fn parse_ternary(&mut self, cond: Expr) -> PResult<Expr> {
        self.bump(); // ?
        let then = if self.eat(T::Colon).is_some() {
            None
        } else {
            let t = self.parse_expr()?;
            if self.peek() != T::Colon {
                return Err(self.unexpected(&[]));
            }
            self.bump();
            Some(bx(t))
        };
        let else_ = self.parse_expr_bp(prec::TERNARY + 1)?;
        let span = self.span_from(cond.span);
        Ok(Expr::new(
            ExprKind::Conditional {
                cond: bx(cond),
                then,
                else_: bx(else_),
                parenthesized: false,
            },
            span,
        ))
    }

    fn prefix(
        &mut self,
        start: Span,
        min: u8,
        f: impl FnOnce(Box<Expr>) -> ExprKind,
    ) -> PResult<Expr> {
        let operand = self.parse_expr_bp(min)?;
        let span = self.span_from(start);
        Ok(Expr::new(f(bx(operand)), span))
    }

    /// Parses a unary expression: prefix operators, primaries with their
    /// postfix chains, and assignments to variables.
    fn parse_unary(&mut self) -> PResult<Expr> {
        self.enter()?;
        let r = self.parse_unary_inner();
        self.depth -= 1;
        r
    }

    #[allow(clippy::too_many_lines)]
    fn parse_unary_inner(&mut self) -> PResult<Expr> {
        let k = self.peek();
        let start = self.cur_span();
        match k {
            T::Bang => {
                self.bump();
                self.prefix(start, prec::NOT + 1, |e| ExprKind::Unary {
                    op: UnaryOp::Not,
                    expr: e,
                })
            }
            T::Tilde | T::Minus | T::Plus => {
                self.bump();
                let op = match k {
                    T::Tilde => UnaryOp::BitNot,
                    T::Minus => UnaryOp::Minus,
                    _ => UnaryOp::Plus,
                };
                self.prefix(start, prec::UNARY + 1, |e| ExprKind::Unary { op, expr: e })
            }
            T::At => {
                self.bump();
                self.prefix(start, prec::UNARY + 1, ExprKind::Silence)
            }
            _ if cast_kind(k).is_some() => {
                self.bump();
                let kind = cast_kind(k).unwrap_or(CastKind::Int);
                self.prefix(start, prec::UNARY + 1, |e| ExprKind::Cast { kind, expr: e })
            }
            T::Inc | T::Dec => {
                self.bump();
                let var = self.parse_variable()?;
                let op = if k == T::Inc {
                    IncDecOp::PreInc
                } else {
                    IncDecOp::PreDec
                };
                let span = self.span_from(start);
                Ok(Expr::new(ExprKind::IncDec { op, var: bx(var) }, span))
            }
            T::Print => {
                self.bump();
                self.prefix(start, prec::PRINT + 1, ExprKind::Print)
            }
            T::Yield => {
                self.bump();
                self.mark_generator();
                if !starts_expr(self.peek()) {
                    let span = self.span_from(start);
                    return Ok(Expr::new(
                        ExprKind::Yield {
                            key: None,
                            value: None,
                        },
                        span,
                    ));
                }
                let first = self.parse_expr_bp(prec::YIELD + 1)?;
                if self.eat(T::DoubleArrow).is_some() {
                    let value = self.parse_expr_bp(prec::DOUBLE_ARROW + 1)?;
                    let span = self.span_from(start);
                    return Ok(Expr::new(
                        ExprKind::Yield {
                            key: Some(bx(first)),
                            value: Some(bx(value)),
                        },
                        span,
                    ));
                }
                let span = self.span_from(start);
                Ok(Expr::new(
                    ExprKind::Yield {
                        key: None,
                        value: Some(bx(first)),
                    },
                    span,
                ))
            }
            T::YieldFrom => {
                self.bump();
                self.mark_generator();
                self.prefix(start, prec::YIELD_FROM + 1, ExprKind::YieldFrom)
            }
            T::Throw => {
                self.bump();
                self.prefix(start, prec::THROW + 1, ExprKind::Throw)
            }
            T::Include | T::IncludeOnce | T::Require | T::RequireOnce => {
                self.bump();
                let kind = match k {
                    T::Include => IncludeKind::Include,
                    T::IncludeOnce => IncludeKind::IncludeOnce,
                    T::Require => IncludeKind::Require,
                    _ => IncludeKind::RequireOnce,
                };
                self.prefix(start, prec::INCLUDE + 1, |e| ExprKind::Include {
                    kind,
                    expr: e,
                })
            }
            T::Clone => self.parse_clone(),
            T::List => {
                self.bump();
                self.expect1(T::LParen)?;
                let items = self.parse_array_pair_list(T::RParen)?;
                self.expect1(T::RParen)?;
                let target = Expr::new(
                    ExprKind::Array {
                        items,
                        syntax: ArraySyntax::List,
                    },
                    self.span_from(start),
                );
                self.expect1(T::Equals)?;
                let value = self.parse_expr_bp(prec::ASSIGN + 1)?;
                let span = self.span_from(start);
                Ok(Expr::new(
                    ExprKind::Assign {
                        target: bx(target),
                        value: bx(value),
                    },
                    span,
                ))
            }
            T::LBracket => {
                let arr = self.parse_short_array()?;
                if self.peek() == T::Equals {
                    self.bump();
                    let value = self.parse_expr_bp(prec::ASSIGN + 1)?;
                    let span = self.span_from(start);
                    return Ok(Expr::new(
                        ExprKind::Assign {
                            target: bx(arr),
                            value: bx(value),
                        },
                        span,
                    ));
                }
                let (e, cat) = self.parse_postfix(arr, Cat::Callable)?;
                self.finish_variable(e, cat)
            }
            T::Function | T::Fn => self.parse_closure(Vec::new(), false, start),
            T::Static if matches!(self.peek_nth(1), T::Function | T::Fn) => {
                self.bump();
                self.parse_closure(Vec::new(), true, start)
            }
            T::Attribute => {
                let attrs = self.parse_attributes()?;
                match self.peek() {
                    T::Function | T::Fn => self.parse_closure(attrs, false, start),
                    T::Static if matches!(self.peek_nth(1), T::Function | T::Fn) => {
                        self.bump();
                        self.parse_closure(attrs, true, start)
                    }
                    T::Static => {
                        self.bump();
                        Err(self.unexpected(&[T::Function, T::Fn]))
                    }
                    _ => Err(self.unexpected(&[T::Function, T::Fn, T::Static, T::Attribute])),
                }
            }
            _ => {
                let (e, cat) = self.parse_primary()?;
                let (e, cat) = self.parse_postfix(e, cat)?;
                self.finish_variable(e, cat)
            }
        }
    }

    /// After a postfix chain: assignments and `++`/`--` on variables.
    fn finish_variable(&mut self, e: Expr, cat: Cat) -> PResult<Expr> {
        if !cat.is_variable() {
            return Ok(e);
        }
        let k = self.peek();
        if let Some(op) = assign_op(k) {
            self.bump();
            if op.is_none()
                && matches!(
                    self.peek(),
                    T::AmpersandFollowedByVarOrVararg | T::AmpersandNotFollowedByVarOrVararg
                )
            {
                self.bump();
                let value = self.parse_variable()?;
                let span = self.span_from(e.span);
                return Ok(Expr::new(
                    ExprKind::AssignRef {
                        target: bx(e),
                        value: bx(value),
                    },
                    span,
                ));
            }
            let value = self.parse_expr_bp(prec::ASSIGN + 1)?;
            let span = self.span_from(e.span);
            let kind = match op {
                None => ExprKind::Assign {
                    target: bx(e),
                    value: bx(value),
                },
                Some(op) => ExprKind::AssignOp {
                    op,
                    target: bx(e),
                    value: bx(value),
                },
            };
            return Ok(Expr::new(kind, span));
        }
        if k == T::Inc || k == T::Dec {
            self.bump();
            let op = if k == T::Inc {
                IncDecOp::PostInc
            } else {
                IncDecOp::PostDec
            };
            let span = self.span_from(e.span);
            return Ok(Expr::new(ExprKind::IncDec { op, var: bx(e) }, span));
        }
        Ok(e)
    }

    /// Parses a `variable` (an expression that can be written to).
    pub(crate) fn parse_variable(&mut self) -> PResult<Expr> {
        if !starts_variable(self.peek()) {
            return Err(self.unexpected(&[]));
        }
        let (e, cat) = self.parse_primary()?;
        let (e, cat) = self.parse_postfix(e, cat)?;
        if cat.is_variable() {
            Ok(e)
        } else if cat.is_aod() {
            Err(self.unexpected(AOD_FOLLOW))
        } else if cat == Cat::Other && matches!(e.kind, ExprKind::New { .. }) {
            Err(self.unexpected(&[T::LParen]))
        } else {
            Err(self.unexpected(&[]))
        }
    }

    /// Parses a `variable` that must be followed by one of `follow`; any
    /// other token is reported after bison's reduction to
    /// `array_object_dereferenceable`.
    pub(crate) fn parse_variable_followed_by(&mut self, follow: &[T]) -> PResult<Expr> {
        let v = self.parse_variable()?;
        if follow.contains(&self.peek()) {
            Ok(v)
        } else {
            Err(self.unexpected(AOD_FOLLOW))
        }
    }

    /// Parses a `simple_variable`: `$a`, `$$a`, `${expr}`.
    pub(crate) fn parse_simple_variable(&mut self) -> PResult<Expr> {
        let start = self.cur_span();
        match self.peek() {
            T::Variable => {
                let t = self.bump();
                let text = self.text(&t);
                Ok(Expr::new(
                    ExprKind::Var(VarName::Name(Ident::new(text[1..].to_vec(), t.span))),
                    t.span,
                ))
            }
            T::Dollar => {
                self.bump();
                match self.peek() {
                    T::LBrace => {
                        self.bump();
                        let e = self.parse_expr()?;
                        self.expect_close_brace()?;
                        let span = self.span_from(start);
                        Ok(Expr::new(ExprKind::Var(VarName::Expr(bx(e))), span))
                    }
                    T::Variable | T::Dollar => {
                        let inner = self.parse_simple_variable()?;
                        let span = self.span_from(start);
                        Ok(Expr::new(ExprKind::Var(VarName::Expr(bx(inner))), span))
                    }
                    _ => Err(self.unexpected(&[T::Variable, T::LBrace, T::Dollar])),
                }
            }
            _ => Err(self.unexpected(&[T::Variable, T::Dollar])),
        }
    }

    /// `'}'` closing an embedded expression (generic error: operators could
    /// continue the expression).
    fn expect_close_brace(&mut self) -> PResult<()> {
        if self.eat(T::RBrace).is_some() {
            Ok(())
        } else {
            Err(self.unexpected(&[]))
        }
    }

    /// The name part of a variable (`VarName`) from a simple variable
    /// expression.
    fn var_name_of(e: Expr) -> VarName {
        match e.kind {
            ExprKind::Var(VarName::Name(id)) => VarName::Name(id),
            ExprKind::Var(VarName::Expr(inner)) => VarName::Expr(inner),
            _ => VarName::Expr(bx(e)),
        }
    }

    /// Parses a primary expression, returning its grammar category.
    #[allow(clippy::too_many_lines)]
    fn parse_primary(&mut self) -> PResult<(Expr, Cat)> {
        let k = self.peek();
        let start = self.cur_span();
        if let Some(m) = magic_const(k) {
            let t = self.bump();
            return Ok((Expr::new(ExprKind::MagicConst(m), t.span), Cat::Magic));
        }
        match k {
            T::Variable | T::Dollar => Ok((self.parse_simple_variable()?, Cat::Var)),
            T::String | T::NameQualified | T::NameFullyQualified | T::NameRelative => {
                let t = self.bump();
                let name = self.name_of(&t);
                match self.peek() {
                    T::LParen => {
                        let args = self.parse_argument_list()?;
                        let span = self.span_from(start);
                        Ok((
                            Expr::new(
                                ExprKind::Call {
                                    callee: Box::new(Callee::Name(name)),
                                    args,
                                },
                                span,
                            ),
                            Cat::Var,
                        ))
                    }
                    T::PaamayimNekudotayim => self.parse_static_access(ClassRef::Name(name), start),
                    _ => Ok((Expr::new(ExprKind::Const(name), t.span), Cat::Const)),
                }
            }
            T::Static => {
                let t = self.bump();
                let name = Name {
                    kind: NameKind::Unqualified,
                    name: b"static".to_vec(),
                    span: t.span,
                };
                if self.peek() != T::PaamayimNekudotayim {
                    return Err(self.unexpected(&[T::PaamayimNekudotayim]));
                }
                self.parse_static_access(ClassRef::Name(name), start)
            }
            T::Readonly => {
                let t = self.bump();
                if self.peek() != T::LParen {
                    return Err(self.unexpected(&[T::LParen]));
                }
                let name = Name {
                    kind: NameKind::Unqualified,
                    name: self.text(&t).to_vec(),
                    span: t.span,
                };
                let args = self.parse_argument_list()?;
                let span = self.span_from(start);
                Ok((
                    Expr::new(
                        ExprKind::Call {
                            callee: Box::new(Callee::Name(name)),
                            args,
                        },
                        span,
                    ),
                    Cat::Var,
                ))
            }
            T::LNumber | T::DNumber => {
                let t = self.bump();
                let kind = match t.value {
                    Some(TokenValue::Int(i)) => ExprKind::Int(i),
                    Some(TokenValue::Float(f)) => ExprKind::Float(f),
                    _ => ExprKind::Int(0),
                };
                Ok((Expr::new(kind, t.span), Cat::Other))
            }
            T::ConstantEncapsedString => {
                let t = self.bump();
                let s = match t.value {
                    Some(TokenValue::Str(s)) => s,
                    _ => Vec::new(),
                };
                Ok((Expr::new(ExprKind::String(s), t.span), Cat::Callable))
            }
            T::DoubleQuote => {
                self.bump();
                let parts = self.parse_encaps_list(T::DoubleQuote)?;
                self.expect1(T::DoubleQuote)?;
                let span = self.span_from(start);
                Ok((
                    Expr::new(ExprKind::Interpolated(parts), span),
                    Cat::Callable,
                ))
            }
            T::Backtick => {
                self.bump();
                let parts = if self.peek() == T::Backtick {
                    Vec::new()
                } else {
                    self.parse_encaps_list(T::Backtick)?
                };
                self.expect1(T::Backtick)?;
                let span = self.span_from(start);
                Ok((Expr::new(ExprKind::ShellExec(parts), span), Cat::Other))
            }
            T::StartHeredoc => {
                self.bump();
                let kind = match self.peek() {
                    T::EndHeredoc => ExprKind::String(Vec::new()),
                    T::EncapsedAndWhitespace if self.peek_nth(1) == T::EndHeredoc => {
                        let t = self.bump();
                        match t.value {
                            Some(TokenValue::Str(s)) => ExprKind::String(s),
                            _ => ExprKind::String(Vec::new()),
                        }
                    }
                    _ => ExprKind::Interpolated(self.parse_encaps_list(T::EndHeredoc)?),
                };
                self.expect1(T::EndHeredoc)?;
                let span = self.span_from(start);
                Ok((Expr::new(kind, span), Cat::Other))
            }
            T::LBracket => Ok((self.parse_short_array()?, Cat::Callable)),
            T::Array => {
                self.bump();
                self.expect1(T::LParen)?;
                let items = self.parse_array_pair_list(T::RParen)?;
                self.expect1(T::RParen)?;
                let span = self.span_from(start);
                Ok((
                    Expr::new(
                        ExprKind::Array {
                            items,
                            syntax: ArraySyntax::Long,
                        },
                        span,
                    ),
                    Cat::Callable,
                ))
            }
            T::LParen => {
                self.bump();
                let e = self.parse_expr()?;
                if self.peek() != T::RParen {
                    return Err(self.unexpected(&[]));
                }
                self.bump();
                Ok((e, Cat::Paren))
            }
            T::New => self.parse_new(),
            T::Exit => {
                let t = self.bump();
                let args = if self.peek() == T::LParen {
                    self.parse_argument_list()?
                } else {
                    ArgList {
                        args: Vec::new(),
                        span: Span::new(t.span.end, t.span.end, t.end_line),
                    }
                };
                let name = Name {
                    kind: NameKind::FullyQualified,
                    name: b"exit".to_vec(),
                    span: t.span,
                };
                let span = self.span_from(start);
                Ok((
                    Expr::new(
                        ExprKind::Call {
                            callee: Box::new(Callee::Name(name)),
                            args,
                        },
                        span,
                    ),
                    Cat::Other,
                ))
            }
            T::Isset => {
                self.bump();
                self.expect1(T::LParen)?;
                let mut vars = vec![self.parse_expr()?];
                loop {
                    match self.peek() {
                        T::Comma => {
                            self.bump();
                            if self.peek() == T::RParen {
                                break;
                            }
                            if !starts_expr(self.peek()) {
                                return Err(self.unexpected(&[T::RParen]));
                            }
                            vars.push(self.parse_expr()?);
                        }
                        T::RParen => break,
                        _ => return Err(self.unexpected(&[T::RParen])),
                    }
                }
                self.bump();
                let span = self.span_from(start);
                Ok((Expr::new(ExprKind::Isset(vars), span), Cat::Other))
            }
            T::Empty | T::Eval => {
                self.bump();
                self.expect1(T::LParen)?;
                let e = self.parse_expr()?;
                if self.peek() != T::RParen {
                    return Err(self.unexpected(&[]));
                }
                self.bump();
                let span = self.span_from(start);
                let kind = if k == T::Empty {
                    ExprKind::Empty(bx(e))
                } else {
                    ExprKind::Include {
                        kind: IncludeKind::Eval,
                        expr: bx(e),
                    }
                };
                Ok((Expr::new(kind, span), Cat::Other))
            }
            T::Match => Ok((self.parse_match()?, Cat::Other)),
            _ => Err(self.unexpected(&[])),
        }
    }

    /// Parses the postfix chain (`[...]`, `->`, `::`, calls) after a
    /// primary.
    #[allow(clippy::too_many_lines)]
    fn parse_postfix(&mut self, mut e: Expr, mut cat: Cat) -> PResult<(Expr, Cat)> {
        if cat == Cat::Paren {
            match self.peek() {
                // `fully_dereferenceable: '(' expr ')'`
                T::LBracket
                | T::ObjectOperator
                | T::NullsafeObjectOperator
                | T::PaamayimNekudotayim => {
                    if let ExprKind::StaticProp { parenthesized, .. } = &mut e.kind {
                        *parenthesized = true;
                    }
                }
                // `callable_expr: '(' expr ')'`
                T::LParen => {}
                // `expr: '(' expr ')'`
                _ => match &mut e.kind {
                    ExprKind::Conditional { parenthesized, .. } => *parenthesized = true,
                    ExprKind::ArrowFn(f) => f.parenthesized = true,
                    _ => {}
                },
            }
            cat = Cat::Callable;
        }
        loop {
            match self.peek() {
                T::LBracket if cat.is_aod() => {
                    self.bump();
                    let index = if self.peek() == T::RBracket {
                        None
                    } else if !starts_expr(self.peek()) {
                        return Err(self.unexpected(&[T::RBracket]));
                    } else {
                        let idx = self.parse_expr()?;
                        Some(bx(idx))
                    };
                    if self.peek() != T::RBracket {
                        return Err(self.unexpected(&[T::RBracket]));
                    }
                    self.bump();
                    let span = self.span_from(e.span);
                    e = Expr::new(ExprKind::Dim { base: bx(e), index }, span);
                    cat = Cat::Var;
                }
                T::ObjectOperator | T::NullsafeObjectOperator if cat.is_aod() => {
                    let nullsafe = self.bump().kind == T::NullsafeObjectOperator;
                    let name = self.parse_property_name()?;
                    if self.peek() == T::LParen {
                        let args = self.parse_argument_list()?;
                        let span = self.span_from(e.span);
                        e = Expr::new(
                            ExprKind::MethodCall {
                                object: bx(e),
                                name,
                                args,
                                nullsafe,
                            },
                            span,
                        );
                        cat = Cat::Var;
                    } else {
                        let span = self.span_from(e.span);
                        e = Expr::new(
                            ExprKind::Prop {
                                object: bx(e),
                                name,
                                nullsafe,
                            },
                            span,
                        );
                        cat = Cat::Prop;
                    }
                }
                T::PaamayimNekudotayim if cat.is_fully_deref() || cat == Cat::Const => {
                    let span = e.span;
                    let class = match e.kind {
                        ExprKind::Const(name) => ClassRef::Name(name),
                        kind => ClassRef::Expr(Expr::new(kind, span)),
                    };
                    let start = match &class {
                        ClassRef::Name(n) => n.span,
                        ClassRef::Expr(x) => x.span,
                    };
                    let (ne, ncat) = self.parse_static_access(class, start)?;
                    e = ne;
                    cat = ncat;
                }
                T::LParen if matches!(cat, Cat::Var | Cat::Callable) => {
                    let paren_line = self.cur().end_line;
                    let args = self.parse_argument_list()?;
                    let mut span = self.span_from(e.span);
                    span.line = paren_line;
                    e = Expr::new(
                        ExprKind::Call {
                            callee: Box::new(Callee::Expr(e)),
                            args,
                        },
                        span,
                    );
                    cat = Cat::Var;
                }
                _ => return Ok((e, cat)),
            }
        }
    }

    /// After `class ::`: static property, static call, class constant or
    /// `::class`.
    #[allow(clippy::too_many_lines)]
    fn parse_static_access(&mut self, class: ClassRef, start: Span) -> PResult<(Expr, Cat)> {
        self.bump(); // ::
        let k = self.peek();
        match k {
            T::Variable | T::Dollar => {
                let var = self.parse_simple_variable()?;
                if self.peek() == T::LParen {
                    let args = self.parse_argument_list()?;
                    let span = self.span_from(start);
                    return Ok((
                        Expr::new(
                            ExprKind::StaticCall {
                                class: Box::new(class),
                                name: MemberName::Expr(bx(var)),
                                args,
                            },
                            span,
                        ),
                        Cat::Var,
                    ));
                }
                let span = self.span_from(start);
                Ok((
                    Expr::new(
                        ExprKind::StaticProp {
                            class: Box::new(class),
                            name: Self::var_name_of(var),
                            parenthesized: false,
                        },
                        span,
                    ),
                    Cat::Prop,
                ))
            }
            T::LBrace => {
                self.bump();
                let name_expr = self.parse_expr()?;
                self.expect_close_brace()?;
                if self.peek() == T::LParen {
                    let args = self.parse_argument_list()?;
                    let span = self.span_from(start);
                    return Ok((
                        Expr::new(
                            ExprKind::StaticCall {
                                class: Box::new(class),
                                name: MemberName::Expr(bx(name_expr)),
                                args,
                            },
                            span,
                        ),
                        Cat::Var,
                    ));
                }
                let span = self.span_from(start);
                Ok((
                    Expr::new(
                        ExprKind::ClassConst {
                            class: Box::new(class),
                            name: MemberName::Expr(bx(name_expr)),
                        },
                        span,
                    ),
                    Cat::ClassConst,
                ))
            }
            _ if is_identifier_token(k) => {
                let t = self.bump();
                let ident = self.ident_checked(&t)?;
                if self.peek() == T::LParen {
                    let args = self.parse_argument_list()?;
                    let span = self.span_from(start);
                    return Ok((
                        Expr::new(
                            ExprKind::StaticCall {
                                class: Box::new(class),
                                name: MemberName::Ident(ident),
                                args,
                            },
                            span,
                        ),
                        Cat::Var,
                    ));
                }
                let span = self.span_from(start);
                if ident.eq_ignore_case("class") {
                    Ok((
                        Expr::new(ExprKind::ClassName(Box::new(class)), span),
                        Cat::ClassConst,
                    ))
                } else {
                    Ok((
                        Expr::new(
                            ExprKind::ClassConst {
                                class: Box::new(class),
                                name: MemberName::Ident(ident),
                            },
                            span,
                        ),
                        Cat::ClassConst,
                    ))
                }
            }
            _ => Err(self.unexpected(&[])),
        }
    }

    /// `property_name`: identifier, `{expr}` or simple variable.
    fn parse_property_name(&mut self) -> PResult<MemberName> {
        match self.peek() {
            T::String => {
                let t = self.bump();
                Ok(MemberName::Ident(self.ident_checked(&t)?))
            }
            T::LBrace => {
                self.bump();
                let e = self.parse_expr()?;
                self.expect_close_brace()?;
                Ok(MemberName::Expr(bx(e)))
            }
            T::Variable | T::Dollar => Ok(MemberName::Expr(bx(self.parse_simple_variable()?))),
            _ => Err(self.unexpected(PROPERTY_NAME_START)),
        }
    }

    /// `class_name_reference`: after `new` and `instanceof`.
    fn parse_class_name_reference(&mut self) -> PResult<ClassRef> {
        let start = self.cur_span();
        match self.peek() {
            T::Static => {
                let t = self.bump();
                let name = Name {
                    kind: NameKind::Unqualified,
                    name: b"static".to_vec(),
                    span: t.span,
                };
                if self.peek() == T::PaamayimNekudotayim {
                    let e = self.parse_new_variable_static(ClassRef::Name(name), start)?;
                    let e = self.parse_new_variable_tail(e)?;
                    return Ok(ClassRef::Expr(e));
                }
                Ok(ClassRef::Name(name))
            }
            k if is_name_token(k) => {
                let t = self.bump();
                let name = self.name_of(&t);
                if self.peek() == T::PaamayimNekudotayim {
                    let e = self.parse_new_variable_static(ClassRef::Name(name), start)?;
                    let e = self.parse_new_variable_tail(e)?;
                    return Ok(ClassRef::Expr(e));
                }
                Ok(ClassRef::Name(name))
            }
            T::Variable | T::Dollar => {
                let v = self.parse_simple_variable()?;
                Ok(ClassRef::Expr(self.parse_new_variable_tail(v)?))
            }
            T::LParen => {
                self.bump();
                let e = self.parse_expr()?;
                if self.peek() != T::RParen {
                    return Err(self.unexpected(&[]));
                }
                self.bump();
                Ok(ClassRef::Expr(e))
            }
            _ => Err(self.unexpected(&[])),
        }
    }

    fn parse_new_variable_static(&mut self, class: ClassRef, start: Span) -> PResult<Expr> {
        self.bump(); // ::
        let var = self.parse_simple_variable()?;
        let span = self.span_from(start);
        Ok(Expr::new(
            ExprKind::StaticProp {
                class: Box::new(class),
                name: Self::var_name_of(var),
                parenthesized: false,
            },
            span,
        ))
    }

    /// `new_variable` continuation: `[...]`, `->prop`, `?->prop`, `::$prop`.
    fn parse_new_variable_tail(&mut self, mut e: Expr) -> PResult<Expr> {
        loop {
            match self.peek() {
                T::LBracket => {
                    self.bump();
                    let index = if self.peek() == T::RBracket {
                        None
                    } else {
                        Some(bx(self.parse_expr()?))
                    };
                    if self.peek() != T::RBracket {
                        return Err(self.unexpected(&[T::RBracket]));
                    }
                    self.bump();
                    let span = self.span_from(e.span);
                    e = Expr::new(ExprKind::Dim { base: bx(e), index }, span);
                }
                T::ObjectOperator | T::NullsafeObjectOperator => {
                    let nullsafe = self.bump().kind == T::NullsafeObjectOperator;
                    let name = self.parse_property_name()?;
                    let span = self.span_from(e.span);
                    e = Expr::new(
                        ExprKind::Prop {
                            object: bx(e),
                            name,
                            nullsafe,
                        },
                        span,
                    );
                }
                T::PaamayimNekudotayim => {
                    let start = e.span;
                    e = self.parse_new_variable_static(ClassRef::Expr(e), start)?;
                }
                _ => return Ok(e),
            }
        }
    }

    fn parse_new(&mut self) -> PResult<(Expr, Cat)> {
        let start = self.bump().span; // new
        let attrs = if self.peek() == T::Attribute {
            self.parse_attributes()?
        } else {
            Vec::new()
        };
        if !attrs.is_empty()
            || matches!(self.peek(), T::Class | T::Abstract | T::Final | T::Readonly)
        {
            let (decl, args) = self.parse_anonymous_class(attrs)?;
            let span = self.span_from(start);
            return Ok((
                Expr::new(
                    ExprKind::New {
                        class: Box::new(NewClass::Anonymous(decl)),
                        args,
                    },
                    span,
                ),
                Cat::Callable,
            ));
        }
        if !matches!(
            self.peek(),
            T::Static
                | T::String
                | T::NameQualified
                | T::NameFullyQualified
                | T::NameRelative
                | T::Variable
                | T::Dollar
                | T::LParen
        ) {
            return Err(self.unexpected(&[T::Class]));
        }
        let class = match self.parse_class_name_reference()? {
            ClassRef::Name(n) => NewClass::Name(n),
            ClassRef::Expr(e) => NewClass::Expr(e),
        };
        if self.peek() == T::LParen {
            let args = self.parse_argument_list()?;
            let span = self.span_from(start);
            return Ok((
                Expr::new(
                    ExprKind::New {
                        class: Box::new(class),
                        args,
                    },
                    span,
                ),
                Cat::Callable,
            ));
        }
        let end = self.prev_span;
        let span = self.span_from(start);
        Ok((
            Expr::new(
                ExprKind::New {
                    class: Box::new(class),
                    args: ArgList {
                        args: Vec::new(),
                        span: Span::new(end.end, end.end, self.prev_end_line),
                    },
                },
                span,
            ),
            Cat::Other,
        ))
    }

    fn parse_clone(&mut self) -> PResult<Expr> {
        let t = self.bump();
        let start = t.span;
        let name = Name {
            kind: NameKind::FullyQualified,
            name: b"clone".to_vec(),
            span: t.span,
        };
        let make = |p: &Self, args: ArgList| {
            Expr::new(
                ExprKind::Call {
                    callee: Box::new(Callee::Name(name.clone())),
                    args,
                },
                p.span_from(start),
            )
        };
        if self.peek() == T::LParen {
            let lp = self.cur_span();
            // `clone()`, `clone(...)`, `clone(name: ...)` and
            // `clone($x, ...)` are argument lists; `clone($x)` is the
            // construct applied to a parenthesized expression.
            let k1 = self.peek_nth(1);
            let is_arglist = k1 == T::RParen
                || k1 == T::Ellipsis
                || k1 == T::Question
                || (is_identifier_token(k1) && self.peek_nth(2) == T::Colon);
            if is_arglist {
                let args = self.parse_argument_list()?;
                return Ok(make(self, args));
            }
            self.bump(); // (
            let first = self.parse_expr()?;
            if self.peek() == T::Comma {
                self.bump();
                let mut args = vec![Arg {
                    span: first.span,
                    kind: ArgKind::Positional(first),
                }];
                if self.peek() != T::RParen {
                    self.parse_args_into(&mut args)?;
                }
                if self.peek() != T::RParen {
                    return Err(self.unexpected(&[T::RParen]));
                }
                self.bump();
                let args = ArgList {
                    args,
                    span: self.span_from(lp),
                };
                return Ok(make(self, args));
            }
            if self.peek() != T::RParen {
                return Err(self.unexpected(&[]));
            }
            self.bump();
            let (e, _) = self.parse_postfix(first, Cat::Paren)?;
            let span = e.span;
            let args = ArgList {
                args: vec![Arg {
                    span,
                    kind: ArgKind::Positional(e),
                }],
                span: self.span_from(lp),
            };
            return Ok(make(self, args));
        }
        let operand = self.parse_expr_bp(prec::POW + 1)?;
        let span = operand.span;
        let args = ArgList {
            args: vec![Arg {
                span,
                kind: ArgKind::Positional(operand),
            }],
            span,
        };
        Ok(make(self, args))
    }

    /// `argument_list`: `(` args `)`.
    pub(crate) fn parse_argument_list(&mut self) -> PResult<ArgList> {
        let start = self.expect1(T::LParen)?.span;
        let mut args = Vec::new();
        if self.peek() != T::RParen {
            if !(starts_expr(self.peek())
                || matches!(self.peek(), T::Ellipsis | T::Question)
                || is_identifier_token(self.peek()))
            {
                return Err(self.unexpected(&[]));
            }
            self.parse_args_into(&mut args)?;
        }
        if self.peek() != T::RParen {
            return Err(self.unexpected(&[T::RParen]));
        }
        self.bump();
        Ok(ArgList {
            args,
            span: self.span_from(start),
        })
    }

    /// Parses `argument (',' argument)* ','?` stopping before `)`.
    fn parse_args_into(&mut self, args: &mut Vec<Arg>) -> PResult<()> {
        loop {
            args.push(self.parse_argument()?);
            if self.peek() == T::Comma {
                self.bump();
                if self.peek() == T::RParen {
                    return Ok(());
                }
                if !(starts_expr(self.peek())
                    || matches!(self.peek(), T::Ellipsis | T::Question)
                    || is_identifier_token(self.peek()))
                {
                    return Err(self.unexpected(&[T::RParen]));
                }
                continue;
            }
            return Ok(());
        }
    }

    fn parse_argument(&mut self) -> PResult<Arg> {
        let k = self.peek();
        // A keyword at the start of an argument may also be the name of a
        // named argument: if the keyword's own rule fails on the very next
        // token, bison has reduced it to `identifier` and expects `:`.
        let saved = self.colon_hint;
        if matches!(
            k,
            T::New
                | T::Clone
                | T::Print
                | T::Include
                | T::IncludeOnce
                | T::Require
                | T::RequireOnce
                | T::Eval
                | T::Isset
                | T::Empty
                | T::Array
                | T::List
                | T::Throw
                | T::Match
                | T::Static
                | T::Readonly
        ) {
            self.colon_hint = Some(self.cur_span().start);
        }
        let r = self.parse_argument_inner();
        self.colon_hint = saved;
        r
    }

    fn parse_argument_inner(&mut self) -> PResult<Arg> {
        let start = self.cur_span();
        let k = self.peek();
        if is_identifier_token(k) && self.peek_nth(1) == T::Colon {
            let t = self.bump();
            let name = self.ident_checked(&t)?;
            self.bump(); // :
            if self.peek() == T::Question && matches!(self.peek_nth(1), T::Comma | T::RParen) {
                self.bump();
                return Ok(Arg {
                    kind: ArgKind::NamedPlaceholder(name),
                    span: self.span_from(start),
                });
            }
            let value = self.parse_expr()?;
            return Ok(Arg {
                kind: ArgKind::Named { name, value },
                span: self.span_from(start),
            });
        }
        match k {
            T::Ellipsis => {
                self.bump();
                if starts_expr(self.peek()) {
                    let e = self.parse_expr()?;
                    return Ok(Arg {
                        kind: ArgKind::Unpack(e),
                        span: self.span_from(start),
                    });
                }
                Ok(Arg {
                    kind: ArgKind::VariadicPlaceholder,
                    span: self.span_from(start),
                })
            }
            T::Question => {
                self.bump();
                Ok(Arg {
                    kind: ArgKind::Placeholder,
                    span: self.span_from(start),
                })
            }
            _ => {
                let e = self.parse_expr()?;
                Ok(Arg {
                    kind: ArgKind::Positional(e),
                    span: self.span_from(start),
                })
            }
        }
    }

    /// `'[' array_pair_list ']'`
    fn parse_short_array(&mut self) -> PResult<Expr> {
        let start = self.bump().span; // [
        let items = self.parse_array_pair_list(T::RBracket)?;
        self.expect1(T::RBracket)?;
        Ok(Expr::new(
            ExprKind::Array {
                items,
                syntax: ArraySyntax::Short,
            },
            self.span_from(start),
        ))
    }

    /// `array_pair_list` up to (not including) `close`.
    pub(crate) fn parse_array_pair_list(&mut self, close: T) -> PResult<Vec<Option<ArrayItem>>> {
        let mut items = Vec::new();
        loop {
            let k = self.peek();
            if k == T::Comma {
                items.push(None);
                self.bump();
                continue;
            }
            if k == close {
                break;
            }
            if !(starts_expr(k)
                || matches!(
                    k,
                    T::AmpersandFollowedByVarOrVararg
                        | T::AmpersandNotFollowedByVarOrVararg
                        | T::Ellipsis
                ))
            {
                return Err(self.unexpected(&[close]));
            }
            items.push(Some(self.parse_array_pair()?));
            match self.peek() {
                T::Comma => {
                    self.bump();
                    if self.peek() == close {
                        // trailing comma: list was rtrimmed by one
                        return Ok(items);
                    }
                }
                k if k == close => break,
                _ => return Err(self.unexpected(&[close])),
            }
        }
        // `zend_ast_list_rtrim`: drop one trailing empty element.
        if matches!(items.last(), Some(None)) {
            items.pop();
        }
        Ok(items)
    }

    fn parse_array_pair(&mut self) -> PResult<ArrayItem> {
        let start = self.cur_span();
        match self.peek() {
            T::AmpersandFollowedByVarOrVararg | T::AmpersandNotFollowedByVarOrVararg => {
                self.bump();
                let value = self.parse_variable_followed_by(&[T::Comma, T::RBracket, T::RParen])?;
                return Ok(ArrayItem {
                    key: None,
                    value,
                    by_ref: true,
                    unpack: false,
                    span: self.span_from(start),
                });
            }
            T::Ellipsis => {
                self.bump();
                let value = self.parse_expr()?;
                return Ok(ArrayItem {
                    key: None,
                    value,
                    by_ref: false,
                    unpack: true,
                    span: self.span_from(start),
                });
            }
            T::List => {
                let value = self.parse_list_pattern()?;
                let value = self.list_maybe_assign(value)?;
                return Ok(ArrayItem {
                    key: None,
                    value,
                    by_ref: false,
                    unpack: false,
                    span: self.span_from(start),
                });
            }
            _ => {}
        }
        let first = self.parse_expr()?;
        if self.eat(T::DoubleArrow).is_none() {
            return Ok(ArrayItem {
                key: None,
                value: first,
                by_ref: false,
                unpack: false,
                span: self.span_from(start),
            });
        }
        let (value, by_ref) = match self.peek() {
            T::AmpersandFollowedByVarOrVararg | T::AmpersandNotFollowedByVarOrVararg => {
                self.bump();
                (
                    self.parse_variable_followed_by(&[T::Comma, T::RBracket, T::RParen])?,
                    true,
                )
            }
            T::List => {
                let l = self.parse_list_pattern()?;
                (self.list_maybe_assign(l)?, false)
            }
            _ => (self.parse_expr()?, false),
        };
        Ok(ArrayItem {
            key: Some(first),
            value,
            by_ref,
            unpack: false,
            span: self.span_from(start),
        })
    }

    /// A `list()` inside an array may also be the start of a destructuring
    /// assignment expression.
    fn list_maybe_assign(&mut self, target: Expr) -> PResult<Expr> {
        if self.peek() != T::Equals {
            return Ok(target);
        }
        self.bump();
        let value = self.parse_expr_bp(prec::ASSIGN + 1)?;
        let span = self.span_from(target.span);
        let e = Expr::new(
            ExprKind::Assign {
                target: bx(target),
                value: bx(value),
            },
            span,
        );
        self.parse_expr_continue(e)
    }

    /// `T_LIST '(' array_pair_list ')'` (without assignment).
    pub(crate) fn parse_list_pattern(&mut self) -> PResult<Expr> {
        let start = self.bump().span; // list
        self.expect1(T::LParen)?;
        let items = self.parse_array_pair_list(T::RParen)?;
        self.expect1(T::RParen)?;
        Ok(Expr::new(
            ExprKind::Array {
                items,
                syntax: ArraySyntax::List,
            },
            self.span_from(start),
        ))
    }

    /// `encaps_list` up to `close`.
    fn parse_encaps_list(&mut self, close: T) -> PResult<Vec<InterpPart>> {
        let mut parts = Vec::new();
        loop {
            let k = self.peek();
            if k == close {
                break;
            }
            match k {
                T::EncapsedAndWhitespace => {
                    let t = self.bump();
                    let s = match t.value {
                        Some(TokenValue::Str(s)) => s,
                        _ => self.text(&t).to_vec(),
                    };
                    parts.push(InterpPart::Literal(s, t.span));
                }
                T::Variable | T::DollarOpenCurlyBraces | T::CurlyOpen => {
                    parts.push(self.parse_encaps_var()?);
                }
                _ => {
                    let only_literal =
                        parts.len() == 1 && matches!(parts[0], InterpPart::Literal(..));
                    let expected: &[T] = match (close, parts.is_empty(), only_literal) {
                        (T::DoubleQuote, true, _) => &[
                            T::EncapsedAndWhitespace,
                            T::Variable,
                            T::DollarOpenCurlyBraces,
                            T::CurlyOpen,
                        ],
                        (T::DoubleQuote, _, true) => {
                            &[T::Variable, T::DollarOpenCurlyBraces, T::CurlyOpen]
                        }
                        (T::EndHeredoc, _, true) => &[
                            T::Variable,
                            T::EndHeredoc,
                            T::DollarOpenCurlyBraces,
                            T::CurlyOpen,
                        ],
                        (T::Backtick, true, _) | (T::Backtick, _, true) => &[T::Backtick],
                        _ => &[],
                    };
                    return Err(self.unexpected(expected));
                }
            }
        }
        Ok(parts)
    }

    fn parse_encaps_var(&mut self) -> PResult<InterpPart> {
        let start = self.cur_span();
        match self.peek() {
            T::Variable => {
                let var = self.parse_simple_variable()?;
                match self.peek() {
                    T::LBracket => {
                        self.bump();
                        let index = self.parse_encaps_var_offset()?;
                        self.expect1(T::RBracket)?;
                        let span = self.span_from(start);
                        Ok(InterpPart::Expr {
                            expr: Expr::new(
                                ExprKind::Dim {
                                    base: bx(var),
                                    index: Some(bx(index)),
                                },
                                span,
                            ),
                            syntax: InterpSyntax::Simple,
                        })
                    }
                    T::ObjectOperator | T::NullsafeObjectOperator => {
                        let nullsafe = self.bump().kind == T::NullsafeObjectOperator;
                        let t = self.expect1(T::String)?;
                        let name = MemberName::Ident(self.ident_checked(&t)?);
                        let span = self.span_from(start);
                        Ok(InterpPart::Expr {
                            expr: Expr::new(
                                ExprKind::Prop {
                                    object: bx(var),
                                    name,
                                    nullsafe,
                                },
                                span,
                            ),
                            syntax: InterpSyntax::Simple,
                        })
                    }
                    _ => Ok(InterpPart::Expr {
                        expr: var,
                        syntax: InterpSyntax::Simple,
                    }),
                }
            }
            T::DollarOpenCurlyBraces => {
                self.bump();
                if self.peek() == T::StringVarname {
                    let k1 = self.peek_nth(1);
                    if k1 == T::RBrace || k1 == T::LBracket {
                        let t = self.bump();
                        let name = Ident::new(self.text(&t).to_vec(), t.span);
                        let var = Expr::new(ExprKind::Var(VarName::Name(name)), t.span);
                        if self.eat(T::LBracket).is_some() {
                            let idx = self.parse_expr()?;
                            if self.peek() != T::RBracket {
                                return Err(self.unexpected(&[]));
                            }
                            self.bump();
                            self.expect1(T::RBrace)?;
                            let span = self.span_from(start);
                            return Ok(InterpPart::Expr {
                                expr: Expr::new(
                                    ExprKind::Dim {
                                        base: bx(var),
                                        index: Some(bx(idx)),
                                    },
                                    span,
                                ),
                                syntax: InterpSyntax::DollarCurly,
                            });
                        }
                        self.bump(); // }
                        let span = self.span_from(start);
                        return Ok(InterpPart::Expr {
                            expr: Expr::new(var.kind, span),
                            syntax: InterpSyntax::DollarCurly,
                        });
                    }
                }
                let e = self.parse_expr()?;
                self.expect_close_brace()?;
                let span = self.span_from(start);
                Ok(InterpPart::Expr {
                    expr: Expr::new(ExprKind::Var(VarName::Expr(bx(e))), span),
                    syntax: InterpSyntax::DollarCurlyExpr,
                })
            }
            _ => {
                // T_CURLY_OPEN variable '}'
                self.bump();
                let v = self.parse_variable_followed_by(&[T::RBrace])?;
                self.bump();
                Ok(InterpPart::Expr {
                    expr: v,
                    syntax: InterpSyntax::Braced,
                })
            }
        }
    }

    fn parse_encaps_var_offset(&mut self) -> PResult<Expr> {
        let t = match self.peek() {
            T::String => {
                let t = self.bump();
                let s = self.text(&t).to_vec();
                return Ok(Expr::new(ExprKind::String(s), t.span));
            }
            T::NumString => self.bump(),
            T::Minus => {
                let m = self.bump();
                let t = self.expect1(T::NumString)?;
                let span = Span::new(m.span.start, t.span.end, m.span.line);
                let kind = match t.value {
                    Some(TokenValue::Int(0)) => ExprKind::String(b"-0".to_vec()),
                    Some(TokenValue::Int(i)) => ExprKind::Int(-i),
                    Some(TokenValue::Str(s)) => {
                        let mut v = b"-".to_vec();
                        v.extend_from_slice(&s);
                        ExprKind::String(v)
                    }
                    _ => ExprKind::Int(0),
                };
                return Ok(Expr::new(kind, span));
            }
            T::Variable => return self.parse_simple_variable(),
            _ => return Err(self.unexpected(&[T::Minus, T::String, T::Variable, T::NumString])),
        };
        let kind = match t.value {
            Some(TokenValue::Int(i)) => ExprKind::Int(i),
            Some(TokenValue::Str(s)) => ExprKind::String(s),
            _ => ExprKind::String(self.text(&t).to_vec()),
        };
        Ok(Expr::new(kind, t.span))
    }

    fn parse_match(&mut self) -> PResult<Expr> {
        let start = self.bump().span; // match
        self.expect1(T::LParen)?;
        let subject = self.parse_expr()?;
        if self.peek() != T::RParen {
            return Err(self.unexpected(&[]));
        }
        self.bump();
        self.expect1(T::LBrace)?;
        let mut arms = Vec::new();
        loop {
            let k = self.peek();
            if k == T::RBrace {
                break;
            }
            if !(starts_expr(k) || k == T::Default) {
                return Err(self.unexpected(&[T::RBrace]));
            }
            let arm_start = self.cur_span();
            let conds = if self.eat(T::Default).is_some() {
                self.eat(T::Comma);
                None
            } else {
                let mut conds = vec![self.parse_expr()?];
                while self.peek() == T::Comma {
                    self.bump();
                    if self.peek() == T::DoubleArrow {
                        break;
                    }
                    if !starts_expr(self.peek()) {
                        return Err(self.unexpected(&[T::DoubleArrow]));
                    }
                    conds.push(self.parse_expr()?);
                }
                Some(conds)
            };
            self.expect1(T::DoubleArrow)?;
            let body = self.parse_expr()?;
            arms.push(MatchArm {
                conds,
                body,
                span: self.span_from(arm_start),
            });
            match self.peek() {
                T::Comma => {
                    self.bump();
                }
                T::RBrace => break,
                _ => return Err(self.unexpected(&[T::RBrace])),
            }
        }
        self.bump();
        Ok(Expr::new(
            ExprKind::Match {
                subject: bx(subject),
                arms,
            },
            self.span_from(start),
        ))
    }
}
