//! Statements and top-level declarations.

use super::expr::{starts_expr, starts_variable};
use super::{is_identifier_token, PResult, Parser};
use crate::ast::{
    Catch, ConstElem, Expr, ExprKind, Ident, IfBranch, Name, NameKind, Span, StaticVar, Stmt,
    StmtKind, SwitchCase, UseItem, UseKind,
};
use php_lexer::TokenKind as T;

/// Tokens that can start a statement in any statement list (besides
/// expression starts).
fn starts_statement_only(k: T) -> bool {
    matches!(
        k,
        T::LBrace
            | T::If
            | T::While
            | T::Do
            | T::For
            | T::Foreach
            | T::Switch
            | T::Break
            | T::Continue
            | T::Return
            | T::Global
            | T::Echo
            | T::InlineHtml
            | T::Unset
            | T::Declare
            | T::Semicolon
            | T::Try
            | T::Goto
            | T::VoidCast
            | T::Class
            | T::Abstract
            | T::Final
            | T::Trait
            | T::Interface
            | T::Enum
            | T::HaltCompiler
    )
}

fn starts_inner_statement(k: T) -> bool {
    starts_statement_only(k) || starts_expr(k)
}

fn starts_top_statement(k: T) -> bool {
    starts_inner_statement(k) || matches!(k, T::Const | T::Namespace | T::Use)
}

impl Parser<'_> {
    /// Parses `top_statement_list` up to the end of the file.
    pub(crate) fn parse_top_level(&mut self) -> PResult<(Vec<Stmt>, Option<usize>)> {
        let mut stmts = Vec::new();
        let mut halt = None;
        loop {
            let k = self.peek();
            if k == T::End {
                break;
            }
            if !starts_top_statement(k) {
                return Err(self.unexpected(&[T::End]));
            }
            let s = self.parse_top_statement()?;
            if let StmtKind::HaltCompiler { offset } = s.kind {
                halt = Some(offset);
                stmts.push(s);
                break;
            }
            stmts.push(s);
        }
        Ok((stmts, halt))
    }

    /// A `top_statement_list` inside `namespace X { ... }`.
    fn parse_top_list_until_brace(&mut self, open: Span) -> PResult<Vec<Stmt>> {
        let mut stmts = Vec::new();
        loop {
            let k = self.peek();
            if k == T::RBrace {
                return Ok(stmts);
            }
            if !starts_top_statement(k) {
                return Err(self.unexpected(&[]));
            }
            let s = self.parse_top_statement()?;
            let halted = matches!(s.kind, StmtKind::HaltCompiler { .. });
            stmts.push(s);
            if halted {
                // The scanner stopped inside the braces: at the end of input
                // it reports the unclosed `{`.
                let line = self.prev_end_line;
                let msg = if open.line == line {
                    "Unclosed '{'".to_owned()
                } else {
                    format!("Unclosed '{{' on line {}", open.line)
                };
                return Err(crate::ParseError::parse(msg, line));
            }
        }
    }

    /// `inner_statement_list`, stopping before any token of `terminators`;
    /// any other token that cannot start a statement is reported with
    /// `on_bad` as the expected list.
    pub(crate) fn parse_inner_list(
        &mut self,
        terminators: &[T],
        on_bad: &[T],
    ) -> PResult<Vec<Stmt>> {
        let mut stmts = Vec::new();
        loop {
            let k = self.peek();
            if terminators.contains(&k) {
                return Ok(stmts);
            }
            if !starts_inner_statement(k) {
                return Err(self.unexpected(on_bad));
            }
            stmts.push(self.parse_inner_statement()?);
        }
    }

    fn parse_top_statement(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        match self.peek() {
            T::HaltCompiler => {
                self.bump();
                self.expect1(T::LParen)?;
                self.expect1(T::RParen)?;
                let semi = self.expect1(T::Semicolon)?;
                let offset = semi.span.end;
                self.stop_lexing();
                Ok(Stmt {
                    kind: StmtKind::HaltCompiler { offset },
                    span: self.span_from(start),
                })
            }
            T::Namespace => self.parse_namespace(),
            T::Use => self.parse_use(),
            T::Const => {
                let consts = self.parse_const_list()?;
                Ok(Stmt {
                    kind: StmtKind::Const {
                        consts,
                        attributes: Vec::new(),
                    },
                    span: self.span_from(start),
                })
            }
            T::Attribute => {
                let attrs = self.parse_attributes()?;
                if self.peek() == T::Const {
                    let consts = self.parse_const_list()?;
                    return Ok(Stmt {
                        kind: StmtKind::Const {
                            consts,
                            attributes: attrs,
                        },
                        span: self.span_from(start),
                    });
                }
                self.parse_attributed_statement(attrs, start, true)
            }
            _ => self.parse_statement(),
        }
    }

    fn parse_inner_statement(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        match self.peek() {
            T::HaltCompiler => {
                self.bump();
                self.expect1(T::LParen)?;
                self.expect1(T::RParen)?;
                self.expect1(T::Semicolon)?;
                Err(self
                    .compile_error("__HALT_COMPILER() can only be used from the outermost scope"))
            }
            T::Attribute => {
                let attrs = self.parse_attributes()?;
                self.parse_attributed_statement(attrs, start, false)
            }
            _ => self.parse_statement(),
        }
    }

    /// After attributes at statement level: a declaration or a closure
    /// expression statement.
    fn parse_attributed_statement(
        &mut self,
        attrs: crate::ast::Attributes,
        start: Span,
        top: bool,
    ) -> PResult<Stmt> {
        match self.peek() {
            T::Function if self.is_function_decl() => self.parse_function_decl(attrs),
            T::Function | T::Fn => {
                let e = self.parse_closure(attrs, false, start)?;
                self.finish_expr_statement(e)
            }
            T::Static if matches!(self.peek_nth(1), T::Function | T::Fn) => {
                self.bump();
                let e = self.parse_closure(attrs, true, start)?;
                self.finish_expr_statement(e)
            }
            T::Abstract | T::Final | T::Readonly => self.parse_class_with_modifiers(attrs),
            T::Class | T::Trait | T::Interface | T::Enum => {
                self.parse_class_decl(attrs, crate::ast::Modifiers::NONE, start)
            }
            _ => {
                let _ = top;
                Err(self.unexpected(&[]))
            }
        }
    }

    /// Continues an expression statement whose first operand is `e`.
    fn finish_expr_statement(&mut self, e: Expr) -> PResult<Stmt> {
        let e = self.continue_expr(e)?;
        let span = e.span;
        self.expect_semicolon_after_expr()?;
        Ok(Stmt {
            kind: StmtKind::Expr(e),
            span: Span::new(span.start, self.prev_span.end, span.line),
        })
    }

    fn expect_semicolon_after_expr(&mut self) -> PResult<()> {
        if self.eat(T::Semicolon).is_some() {
            Ok(())
        } else {
            Err(self.unexpected(&[]))
        }
    }

    /// Whether `function` at the current position starts a named function
    /// declaration (rather than a closure).
    fn is_function_decl(&mut self) -> bool {
        let mut n = 1;
        if matches!(
            self.peek_nth(1),
            T::AmpersandFollowedByVarOrVararg | T::AmpersandNotFollowedByVarOrVararg
        ) {
            n = 2;
        }
        matches!(self.peek_nth(n), T::String | T::Readonly)
    }

    /// Parses a `statement`.
    pub(crate) fn parse_statement(&mut self) -> PResult<Stmt> {
        self.enter()?;
        let r = self.parse_statement_inner();
        self.depth -= 1;
        r
    }

    #[allow(clippy::too_many_lines)]
    fn parse_statement_inner(&mut self) -> PResult<Stmt> {
        let start = self.cur_span();
        let k = self.peek();
        let kind = match k {
            T::LBrace => {
                let stmts = self.parse_brace_block()?;
                StmtKind::Block(stmts)
            }
            T::If => return self.parse_if(),
            T::While => {
                self.bump();
                self.expect1(T::LParen)?;
                let cond = self.parse_paren_tail_expr()?;
                let body = self.parse_loop_body(T::EndWhile)?;
                StmtKind::While {
                    cond,
                    body: Box::new(body),
                }
            }
            T::Do => {
                self.bump();
                let body = self.parse_statement()?;
                self.expect1(T::While)?;
                self.expect1(T::LParen)?;
                let cond = self.parse_paren_tail_expr()?;
                self.expect1(T::Semicolon)?;
                StmtKind::DoWhile {
                    body: Box::new(body),
                    cond,
                }
            }
            T::For => {
                self.bump();
                self.expect1(T::LParen)?;
                let init = self.parse_for_exprs(T::Semicolon, false)?;
                self.expect1(T::Semicolon)?;
                let cond = self.parse_for_exprs(T::Semicolon, true)?;
                self.expect1(T::Semicolon)?;
                let step = self.parse_for_exprs(T::RParen, false)?;
                self.expect1(T::RParen)?;
                let body = self.parse_loop_body(T::EndFor)?;
                StmtKind::For {
                    init,
                    cond,
                    step,
                    body: Box::new(body),
                }
            }
            T::Foreach => return self.parse_foreach(),
            T::Switch => return self.parse_switch(),
            T::Break | T::Continue | T::Return => {
                self.bump();
                let e = if starts_expr(self.peek()) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                if self.peek() != T::Semicolon {
                    return Err(self.unexpected(&[T::Semicolon]));
                }
                self.bump();
                match k {
                    T::Break => StmtKind::Break(e),
                    T::Continue => StmtKind::Continue(e),
                    _ => StmtKind::Return(e),
                }
            }
            T::Global => {
                self.bump();
                let mut vars = Vec::new();
                loop {
                    vars.push(self.parse_simple_variable()?);
                    match self.peek() {
                        T::Comma => {
                            self.bump();
                        }
                        T::Semicolon => break,
                        _ => return Err(self.unexpected(&[T::Comma, T::Semicolon])),
                    }
                }
                self.bump();
                StmtKind::Global(vars)
            }
            T::Static if self.peek_nth(1) == T::Variable => {
                self.bump();
                let mut vars = Vec::new();
                loop {
                    let v = self.expect1(T::Variable)?;
                    let name = Ident::new(self.text(&v)[1..].to_vec(), v.span);
                    let default = if self.eat(T::Equals).is_some() {
                        Some(self.parse_expr()?)
                    } else {
                        None
                    };
                    vars.push(StaticVar {
                        name,
                        default,
                        span: self.span_from(v.span),
                    });
                    match self.peek() {
                        T::Comma => {
                            self.bump();
                        }
                        T::Semicolon => break,
                        _ => return Err(self.unexpected(&[T::Comma, T::Semicolon])),
                    }
                }
                self.bump();
                StmtKind::Static(vars)
            }
            T::Echo => {
                self.bump();
                let mut exprs = Vec::new();
                loop {
                    exprs.push(self.parse_expr()?);
                    match self.peek() {
                        T::Comma => {
                            self.bump();
                        }
                        T::Semicolon => break,
                        _ => return Err(self.unexpected(&[T::Comma, T::Semicolon])),
                    }
                }
                self.bump();
                StmtKind::Echo(exprs)
            }
            T::InlineHtml => {
                let t = self.bump();
                StmtKind::InlineHtml(self.text(&t).to_vec())
            }
            T::Unset => {
                self.bump();
                self.expect1(T::LParen)?;
                let mut vars = Vec::new();
                loop {
                    vars.push(self.parse_variable_followed_by(&[T::Comma, T::RParen])?);
                    match self.peek() {
                        T::Comma => {
                            self.bump();
                            if self.peek() == T::RParen {
                                break;
                            }
                            if !starts_variable(self.peek()) {
                                return Err(self.unexpected(&[T::RParen]));
                            }
                        }
                        T::RParen => break,
                        _ => return Err(self.unexpected(&[T::RParen])),
                    }
                }
                self.bump();
                self.expect1(T::Semicolon)?;
                StmtKind::Unset(vars)
            }
            T::Declare => return self.parse_declare(),
            T::Semicolon => {
                self.bump();
                StmtKind::Nop
            }
            T::Try => return self.parse_try(),
            T::Goto => {
                self.bump();
                let t = self.expect1(T::String)?;
                let label = self.ident_checked(&t)?;
                self.expect1(T::Semicolon)?;
                StmtKind::Goto(label)
            }
            T::String if self.peek_nth(1) == T::Colon => {
                let t = self.bump();
                self.bump();
                StmtKind::Label(self.ident_checked(&t)?)
            }
            T::VoidCast => {
                self.bump();
                let e = self.parse_expr()?;
                let span = self.span_from(start);
                self.expect_semicolon_after_expr()?;
                StmtKind::Expr(Expr::new(ExprKind::VoidCast(Box::new(e)), span))
            }
            T::Function if self.is_function_decl() => {
                return self.parse_function_decl(Vec::new());
            }
            T::Abstract | T::Final => return self.parse_class_with_modifiers(Vec::new()),
            T::Readonly if self.peek_nth(1) != T::LParen => {
                return self.parse_class_with_modifiers(Vec::new())
            }
            T::Class | T::Trait | T::Interface | T::Enum => {
                return self.parse_class_decl(Vec::new(), crate::ast::Modifiers::NONE, start)
            }
            _ => {
                if !starts_expr(k) {
                    return Err(self.unexpected(&[]));
                }
                let e = self.parse_expr()?;
                self.expect_semicolon_after_expr()?;
                let line = e.span.line;
                return Ok(Stmt {
                    kind: StmtKind::Expr(e),
                    span: Span::new(start.start, self.prev_span.end, line),
                });
            }
        };
        Ok(Stmt {
            kind,
            span: self.span_from(start),
        })
    }

    /// Continues parsing binary operators after an already parsed operand
    /// (used when a statement starts with a closure).
    fn continue_expr(&mut self, e: Expr) -> PResult<Expr> {
        self.parse_expr_continue(e)
    }

    /// `expr ')'` where the `(` has been consumed; the error after the
    /// expression is generic (operators could continue it).
    fn parse_paren_tail_expr(&mut self) -> PResult<Expr> {
        let e = self.parse_expr()?;
        if self.peek() != T::RParen {
            return Err(self.unexpected(&[]));
        }
        self.bump();
        Ok(e)
    }

    /// `statement` or `':' inner_statement_list END ';'`.
    fn parse_loop_body(&mut self, end: T) -> PResult<Stmt> {
        if self.peek() == T::Colon {
            let start = self.bump().span;
            let stmts = self.parse_inner_list(&[end], &[])?;
            self.bump();
            self.expect1(T::Semicolon)?;
            return Ok(Stmt {
                kind: StmtKind::Block(stmts),
                span: self.span_from(start),
            });
        }
        if !(starts_inner_statement(self.peek()) || self.peek() == T::Attribute) {
            return Err(self.unexpected(&[]));
        }
        self.parse_statement()
    }

    /// `for_exprs` / `for_cond_exprs` up to `close`.
    fn parse_for_exprs(&mut self, close: T, is_cond: bool) -> PResult<Vec<Expr>> {
        let mut exprs = Vec::new();
        if self.peek() == close {
            return Ok(exprs);
        }
        loop {
            let start = self.cur_span();
            if self.eat(T::VoidCast).is_some() {
                let e = self.parse_expr()?;
                let span = self.span_from(start);
                exprs.push(Expr::new(ExprKind::VoidCast(Box::new(e)), span));
                // `for_cond_exprs` cannot end with a `(void)` cast.
                if is_cond && self.peek() != T::Comma {
                    return Err(self.unexpected(&[T::Comma]));
                }
            } else {
                exprs.push(self.parse_expr()?);
            }
            if self.peek() == T::Comma {
                self.bump();
                continue;
            }
            if self.peek() != close {
                return Err(self.unexpected(&[close]));
            }
            return Ok(exprs);
        }
    }

    fn parse_if(&mut self) -> PResult<Stmt> {
        let start = self.bump().span; // if
        self.expect1(T::LParen)?;
        let cond = self.parse_paren_tail_expr()?;
        if self.peek() == T::Colon {
            // alternative syntax
            self.bump();
            let alt_end = [T::ElseIf, T::Else, T::EndIf];
            let body_start = self.prev_span;
            let stmts = self.parse_inner_list(&alt_end, &alt_end)?;
            let mut branches = vec![IfBranch {
                span: cond.span,
                cond,
                body: Box::new(Stmt {
                    kind: StmtKind::Block(stmts),
                    span: self.span_from(body_start),
                }),
            }];
            let mut else_branch = None;
            loop {
                match self.peek() {
                    T::ElseIf => {
                        self.bump();
                        self.expect1(T::LParen)?;
                        let c = self.parse_paren_tail_expr()?;
                        self.expect1(T::Colon)?;
                        let bs = self.prev_span;
                        let stmts = self.parse_inner_list(&alt_end, &alt_end)?;
                        branches.push(IfBranch {
                            span: c.span,
                            cond: c,
                            body: Box::new(Stmt {
                                kind: StmtKind::Block(stmts),
                                span: self.span_from(bs),
                            }),
                        });
                    }
                    T::Else => {
                        self.bump();
                        self.expect1(T::Colon)?;
                        let bs = self.prev_span;
                        let stmts = self.parse_inner_list(&[T::EndIf], &[])?;
                        else_branch = Some(Box::new(Stmt {
                            kind: StmtKind::Block(stmts),
                            span: self.span_from(bs),
                        }));
                        break;
                    }
                    _ => break,
                }
            }
            self.expect1(T::EndIf)?;
            self.expect1(T::Semicolon)?;
            return Ok(Stmt {
                kind: StmtKind::If {
                    branches,
                    else_branch,
                },
                span: self.span_from(start),
            });
        }
        let body = self.parse_if_body()?;
        let mut branches = vec![IfBranch {
            span: cond.span,
            cond,
            body: Box::new(body),
        }];
        let mut else_branch = None;
        loop {
            match self.peek() {
                T::ElseIf => {
                    self.bump();
                    self.expect1(T::LParen)?;
                    let c = self.parse_paren_tail_expr()?;
                    let body = self.parse_if_body()?;
                    branches.push(IfBranch {
                        span: c.span,
                        cond: c,
                        body: Box::new(body),
                    });
                }
                T::Else => {
                    self.bump();
                    let body = self.parse_if_body()?;
                    else_branch = Some(Box::new(body));
                    break;
                }
                _ => break,
            }
        }
        Ok(Stmt {
            kind: StmtKind::If {
                branches,
                else_branch,
            },
            span: self.span_from(start),
        })
    }

    fn parse_if_body(&mut self) -> PResult<Stmt> {
        if !(starts_inner_statement(self.peek()) || self.peek() == T::Attribute) {
            return Err(self.unexpected(&[]));
        }
        self.parse_statement()
    }

    fn parse_foreach(&mut self) -> PResult<Stmt> {
        let start = self.bump().span; // foreach
        self.expect1(T::LParen)?;
        let subject = self.parse_expr()?;
        if self.peek() != T::As {
            return Err(self.unexpected(&[]));
        }
        self.bump();
        let (first, first_ref) = self.parse_foreach_variable()?;
        let (key, value, by_ref) = if self.eat(T::DoubleArrow).is_some() {
            if first_ref {
                // Rejected by the compiler (see `validate`), not the grammar.
                self.foreach_ref_keys.push(start.start);
            }
            let (v, r) = self.parse_foreach_variable()?;
            if self.peek() != T::RParen {
                return Err(self.unexpected(&[T::RParen]));
            }
            (Some(first), v, r)
        } else {
            if self.peek() != T::RParen {
                return Err(self.unexpected(&[T::DoubleArrow, T::RParen]));
            }
            (None, first, first_ref)
        };
        self.bump(); // )
        let body = self.parse_loop_body(T::EndForeach)?;
        Ok(Stmt {
            kind: StmtKind::Foreach {
                subject,
                key,
                value,
                by_ref,
                body: Box::new(body),
            },
            span: self.span_from(start),
        })
    }

    fn parse_foreach_variable(&mut self) -> PResult<(Expr, bool)> {
        match self.peek() {
            T::AmpersandFollowedByVarOrVararg | T::AmpersandNotFollowedByVarOrVararg => {
                self.bump();
                Ok((
                    self.parse_variable_followed_by(&[T::DoubleArrow, T::RParen])?,
                    true,
                ))
            }
            T::List => Ok((self.parse_list_pattern()?, false)),
            T::LBracket => {
                let start = self.bump().span;
                let items = self.parse_array_pair_list(T::RBracket)?;
                self.expect1(T::RBracket)?;
                Ok((
                    Expr::new(
                        ExprKind::Array {
                            items,
                            syntax: crate::ast::ArraySyntax::Short,
                        },
                        self.span_from(start),
                    ),
                    false,
                ))
            }
            _ => Ok((
                self.parse_variable_followed_by(&[T::DoubleArrow, T::RParen])?,
                false,
            )),
        }
    }

    fn parse_switch(&mut self) -> PResult<Stmt> {
        let start = self.bump().span; // switch
        self.expect1(T::LParen)?;
        let subject = self.parse_paren_tail_expr()?;
        let alt = match self.peek() {
            T::LBrace => false,
            T::Colon => true,
            _ => return Err(self.unexpected(&[T::Colon, T::LBrace])),
        };
        self.bump();
        let end = if alt { T::EndSwitch } else { T::RBrace };
        self.eat(T::Semicolon);
        let mut cases = Vec::new();
        let terminators = [T::Case, T::Default, end];
        loop {
            let k = self.peek();
            if k == end {
                break;
            }
            let cstart = self.cur_span();
            let cond = match k {
                T::Case => {
                    self.bump();
                    let e = self.parse_expr()?;
                    Some(e)
                }
                T::Default => {
                    self.bump();
                    None
                }
                _ => return Err(self.unexpected(&terminators)),
            };
            let semicolon = match self.peek() {
                T::Colon => false,
                T::Semicolon => true,
                _ => {
                    return Err(self.unexpected(if cond.is_some() {
                        &[]
                    } else {
                        &[T::Colon, T::Semicolon]
                    }))
                }
            };
            self.bump();
            let body = self.parse_inner_list(&terminators, &terminators)?;
            cases.push(SwitchCase {
                cond,
                body,
                semicolon,
                span: self.span_from(cstart),
            });
        }
        self.bump();
        if alt {
            self.expect1(T::Semicolon)?;
        }
        Ok(Stmt {
            kind: StmtKind::Switch { subject, cases },
            span: self.span_from(start),
        })
    }

    fn parse_declare(&mut self) -> PResult<Stmt> {
        let start = self.bump().span; // declare
        self.expect1(T::LParen)?;
        let mut directives = Vec::new();
        loop {
            directives.push(self.parse_const_decl()?);
            match self.peek() {
                T::Comma => {
                    self.bump();
                }
                T::RParen => break,
                _ => return Err(self.unexpected(&[T::Comma, T::RParen])),
            }
        }
        self.bump(); // )
                     // `zend_handle_encoding_declaration`, run as a parser action right
                     // after the `)`.
        let line = self.prev_end_line;
        for d in &directives {
            if d.name.eq_ignore_case("encoding") {
                if !matches!(
                    d.value.kind,
                    ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::String(_)
                ) {
                    return Err(crate::ParseError::compile(
                        "Encoding must be a literal",
                        line,
                    ));
                }
                self.warnings.push(crate::ast::Diagnostic {
                    level: crate::ast::DiagnosticLevel::CompileWarning,
                    message: "declare(encoding=...) ignored because Zend multibyte feature is turned off by settings".to_owned(),
                    line,
                });
            }
        }
        let body = match self.peek() {
            T::Semicolon => {
                self.bump();
                None
            }
            T::Colon => {
                let bs = self.bump().span;
                let stmts = self.parse_inner_list(&[T::EndDeclare], &[])?;
                self.bump();
                self.expect1(T::Semicolon)?;
                Some(Box::new(Stmt {
                    kind: StmtKind::Block(stmts),
                    span: self.span_from(bs),
                }))
            }
            _ => {
                if !(starts_inner_statement(self.peek()) || self.peek() == T::Attribute) {
                    return Err(self.unexpected(&[]));
                }
                Some(Box::new(self.parse_statement()?))
            }
        };
        Ok(Stmt {
            kind: StmtKind::Declare { directives, body },
            span: self.span_from(start),
        })
    }

    fn parse_try(&mut self) -> PResult<Stmt> {
        let start = self.bump().span; // try
        if self.peek() != T::LBrace {
            return Err(self.unexpected(&[T::LBrace]));
        }
        let body = self.parse_brace_block()?;
        let mut catches = Vec::new();
        while self.peek() == T::Catch {
            let cstart = self.bump().span;
            self.expect1(T::LParen)?;
            let mut types = vec![self.parse_class_name()?];
            while self.eat(T::Pipe).is_some() {
                types.push(self.parse_class_name()?);
            }
            let var = if self.peek() == T::Variable {
                let v = self.bump();
                Some(Ident::new(self.text(&v)[1..].to_vec(), v.span))
            } else {
                None
            };
            if self.peek() != T::RParen {
                return Err(self.unexpected(&[T::RParen]));
            }
            self.bump();
            if self.peek() != T::LBrace {
                return Err(self.unexpected(&[T::LBrace]));
            }
            let cbody = self.parse_brace_block()?;
            catches.push(Catch {
                types,
                var,
                body: cbody,
                span: self.span_from(cstart),
            });
        }
        let finally = if self.eat(T::Finally).is_some() {
            if self.peek() != T::LBrace {
                return Err(self.unexpected(&[T::LBrace]));
            }
            Some(self.parse_brace_block()?)
        } else {
            None
        };
        Ok(Stmt {
            kind: StmtKind::Try {
                body,
                catches,
                finally,
            },
            span: self.span_from(start),
        })
    }

    /// `const_decl`: `T_STRING '=' expr`.
    fn parse_const_decl(&mut self) -> PResult<ConstElem> {
        let t = self.expect1(T::String)?;
        let name = self.ident_checked(&t)?;
        self.expect1(T::Equals)?;
        let value = self.parse_expr()?;
        self.peek();
        let doc_comment = self.take_doc_comment();
        Ok(ConstElem {
            span: Span::new(t.span.start, self.prev_span.end, t.span.line),
            name,
            value,
            doc_comment,
        })
    }

    /// `T_CONST const_list ';'`
    fn parse_const_list(&mut self) -> PResult<Vec<ConstElem>> {
        self.bump(); // const
        let mut consts = Vec::new();
        loop {
            consts.push(self.parse_const_decl()?);
            match self.peek() {
                T::Comma => {
                    self.bump();
                }
                T::Semicolon => {
                    self.bump();
                    return Ok(consts);
                }
                _ => return Err(self.unexpected(&[T::Comma, T::Semicolon])),
            }
        }
    }

    fn parse_namespace(&mut self) -> PResult<Stmt> {
        let start = self.bump().span; // namespace
        let k = self.peek();
        let name = if k == T::NameQualified || is_identifier_token(k) {
            let t = self.bump();
            if k != T::NameQualified {
                // `namespace_declaration_name: identifier` (`zend_lex_tstring`)
                self.ident_checked(&t)?;
            }
            Some(Name {
                kind: if k == T::NameQualified {
                    NameKind::Qualified
                } else {
                    NameKind::Unqualified
                },
                name: self.text(&t).to_vec(),
                span: t.span,
            })
        } else {
            None
        };
        self.doc_comment = None;
        match self.peek() {
            T::Semicolon if name.is_some() => {
                self.bump();
                Ok(Stmt {
                    kind: StmtKind::Namespace { name, body: None },
                    span: self.span_from(start),
                })
            }
            T::LBrace => {
                self.bump();
                let open = self.prev_span;
                let body = self.parse_top_list_until_brace(open)?;
                self.bump();
                Ok(Stmt {
                    kind: StmtKind::Namespace {
                        name,
                        body: Some(body),
                    },
                    span: self.span_from(start),
                })
            }
            _ => Err(self.unexpected(&[T::LBrace])),
        }
    }

    fn parse_use(&mut self) -> PResult<Stmt> {
        let start = self.bump().span; // use
        let kind = match self.peek() {
            T::Function => {
                self.bump();
                Some(UseKind::Function)
            }
            T::Const => {
                self.bump();
                Some(UseKind::Const)
            }
            _ => None,
        };
        let legacy = [T::String, T::NameFullyQualified, T::NameQualified];
        let mut uses = Vec::new();
        loop {
            let k = self.peek();
            if !legacy.contains(&k) {
                return Err(self.unexpected(if kind.is_some() || !uses.is_empty() {
                    &legacy
                } else {
                    &[]
                }));
            }
            let t = self.bump();
            let name = self.name_of(&t);
            if uses.is_empty() && self.peek() == T::NsSeparator {
                // group use
                self.bump();
                self.expect1(T::LBrace)?;
                let items = self.parse_group_use_items(kind)?;
                self.expect1(T::Semicolon)?;
                let mut prefix = name;
                if prefix.kind == NameKind::FullyQualified {
                    prefix.kind = NameKind::Qualified;
                }
                return Ok(Stmt {
                    kind: StmtKind::GroupUse {
                        kind,
                        prefix,
                        uses: items,
                    },
                    span: self.span_from(start),
                });
            }
            let alias = if self.eat(T::As).is_some() {
                let a = self.expect1(T::String)?;
                Some(self.ident_checked(&a)?)
            } else {
                None
            };
            uses.push(UseItem {
                kind: kind.unwrap_or(UseKind::Class),
                name,
                alias,
                span: self.span_from(t.span),
            });
            match self.peek() {
                T::Comma => {
                    self.bump();
                }
                T::Semicolon => {
                    self.bump();
                    break;
                }
                _ => return Err(self.unexpected(&[T::Comma, T::Semicolon])),
            }
        }
        Ok(Stmt {
            kind: StmtKind::Use {
                kind: kind.unwrap_or(UseKind::Class),
                uses,
            },
            span: self.span_from(start),
        })
    }

    fn parse_group_use_items(&mut self, kind: Option<UseKind>) -> PResult<Vec<UseItem>> {
        let mut items = Vec::new();
        loop {
            let istart = self.cur_span();
            let item_kind = match (kind, self.peek()) {
                (None, T::Function) => {
                    self.bump();
                    UseKind::Function
                }
                (None, T::Const) => {
                    self.bump();
                    UseKind::Const
                }
                (Some(k), _) => k,
                (None, _) => UseKind::Class,
            };
            let k = self.peek();
            if !matches!(k, T::String | T::NameQualified) {
                let keyword_seen = self.prev_span.start == istart.start && self.prev_span != istart;
                let expected: &[T] = if !items.is_empty() && !keyword_seen {
                    // after `,`: bison reduced `possible_comma`
                    &[T::RBrace]
                } else if kind.is_none() && item_kind == UseKind::Class {
                    &[T::String, T::NameQualified, T::Function, T::Const]
                } else {
                    &[T::String, T::NameQualified]
                };
                return Err(self.unexpected(expected));
            }
            let t = self.bump();
            let name = self.name_of(&t);
            let alias = if self.eat(T::As).is_some() {
                let a = self.expect1(T::String)?;
                Some(self.ident_checked(&a)?)
            } else {
                None
            };
            items.push(UseItem {
                kind: item_kind,
                name,
                alias,
                span: self.span_from(istart),
            });
            match self.peek() {
                T::Comma => {
                    self.bump();
                    if self.peek() == T::RBrace {
                        self.bump();
                        return Ok(items);
                    }
                }
                T::RBrace => {
                    self.bump();
                    return Ok(items);
                }
                _ => return Err(self.unexpected(&[T::RBrace])),
            }
        }
    }
}
