//! Declarations: functions, closures, classes and their members, parameters,
//! types and attributes.

use super::expr::prec;
use super::{is_identifier_token, is_name_token, PResult, Parser, Tok};
use crate::ast::{
    ArgList, ArrowFn, Attribute, AttributeGroup, Attributes, ClassConstGroup, ClassDecl, ClassKind,
    ClassMember, ClassMemberKind, Closure, ClosureUse, ConstElem, EnumCase, Expr, ExprKind,
    FunctionDecl, HookBody, Ident, MethodDecl, Modifiers, Name, NameKind, Param, PropertyGroup,
    PropertyHook, PropertyItem, Span, Stmt, TraitAdaptation, TraitMethodRef, TraitUse, Type,
    TypeKind,
};
use php_lexer::TokenKind as T;

/// What a modifier list applies to (`zend_modifier_target`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModifierTarget {
    Property,
    Method,
    Constant,
    Cpp,
    PropertyHook,
}

fn is_member_modifier(k: T) -> bool {
    matches!(
        k,
        T::Public
            | T::Protected
            | T::Private
            | T::PublicSet
            | T::ProtectedSet
            | T::PrivateSet
            | T::Static
            | T::Abstract
            | T::Final
            | T::Readonly
    )
}

fn modifier_name(k: T) -> &'static str {
    match k {
        T::Public => "public",
        T::Protected => "protected",
        T::Private => "private",
        T::Static => "static",
        T::Final => "final",
        T::Readonly => "readonly",
        T::Abstract => "abstract",
        T::PublicSet => "public(set)",
        T::ProtectedSet => "protected(set)",
        _ => "private(set)",
    }
}

fn is_ampersand(k: T) -> bool {
    matches!(
        k,
        T::AmpersandFollowedByVarOrVararg | T::AmpersandNotFollowedByVarOrVararg
    )
}

/// `zend_modifier_token_to_flag`
fn modifier_token_to_flag(target: ModifierTarget, k: T) -> Result<Modifiers, String> {
    use ModifierTarget as M;
    let flag = match k {
        T::Public if target != M::PropertyHook => Some(Modifiers::PUBLIC),
        T::Protected if target != M::PropertyHook => Some(Modifiers::PROTECTED),
        T::Private if target != M::PropertyHook => Some(Modifiers::PRIVATE),
        T::Readonly if matches!(target, M::Property | M::Cpp) => Some(Modifiers::READONLY),
        T::Abstract if matches!(target, M::Method | M::Property) => Some(Modifiers::ABSTRACT),
        T::Final => Some(Modifiers::FINAL),
        T::Static if matches!(target, M::Property | M::Method) => Some(Modifiers::STATIC),
        T::PublicSet if matches!(target, M::Property | M::Cpp) => Some(Modifiers::PUBLIC_SET),
        T::ProtectedSet if matches!(target, M::Property | M::Cpp) => Some(Modifiers::PROTECTED_SET),
        T::PrivateSet if matches!(target, M::Property | M::Cpp) => Some(Modifiers::PRIVATE_SET),
        _ => None,
    };
    flag.ok_or_else(|| {
        let member = match target {
            M::Property => "property",
            M::Method => "method",
            M::Constant => "class constant",
            M::Cpp => "parameter",
            M::PropertyHook => "property hook",
        };
        format!("Cannot use the {} modifier on a {member}", modifier_name(k))
    })
}

/// `zend_add_member_modifier`
fn add_member_modifier(
    flags: Modifiers,
    new: Modifiers,
    target: ModifierTarget,
) -> Result<Modifiers, String> {
    let new_flags = flags | new;
    if flags.intersects(Modifiers::VISIBILITY) && new.intersects(Modifiers::VISIBILITY) {
        return Err("Multiple access type modifiers are not allowed".into());
    }
    if new_flags.contains(Modifiers::ABSTRACT) && new_flags.contains(Modifiers::FINAL) {
        if target == ModifierTarget::Method {
            return Err("Cannot use the final modifier on an abstract method".into());
        }
        if target == ModifierTarget::Property {
            return Err("Cannot use the final modifier on an abstract property".into());
        }
    }
    if matches!(target, ModifierTarget::Property | ModifierTarget::Cpp)
        && flags.intersects(Modifiers::SET_VISIBILITY)
        && new.intersects(Modifiers::SET_VISIBILITY)
    {
        return Err("Multiple access type modifiers are not allowed".into());
    }
    Ok(new_flags)
}

/// `zend_modifier_list_to_flags`
fn modifier_list_to_flags(target: ModifierTarget, list: &[T]) -> Result<Modifiers, String> {
    let mut flags = Modifiers::NONE;
    for &k in list {
        let new = modifier_token_to_flag(target, k)?;
        let duplicate = flags.intersects(new);
        flags = add_member_modifier(flags, new, target)?;
        if duplicate {
            return Err(format!(
                "Multiple {} modifiers are not allowed",
                modifier_name(k)
            ));
        }
    }
    Ok(flags)
}

fn class_modifier_flag(k: T) -> Modifiers {
    match k {
        T::Abstract => Modifiers::ABSTRACT,
        T::Final => Modifiers::FINAL,
        _ => Modifiers::READONLY,
    }
}

/// `zend_add_class_modifier`
fn add_class_modifier(flags: Modifiers, new: Modifiers) -> Result<Modifiers, &'static str> {
    let new_flags = flags | new;
    if flags.contains(Modifiers::ABSTRACT) && new.contains(Modifiers::ABSTRACT) {
        return Err("Multiple abstract modifiers are not allowed");
    }
    if flags.contains(Modifiers::FINAL) && new.contains(Modifiers::FINAL) {
        return Err("Multiple final modifiers are not allowed");
    }
    if flags.contains(Modifiers::READONLY) && new.contains(Modifiers::READONLY) {
        return Err("Multiple readonly modifiers are not allowed");
    }
    if new_flags.contains(Modifiers::ABSTRACT) && new_flags.contains(Modifiers::FINAL) {
        return Err("Cannot use the final modifier on an abstract class");
    }
    Ok(new_flags)
}

/// `zend_add_anonymous_class_modifier`
fn add_anonymous_class_modifier(
    flags: Modifiers,
    new: Modifiers,
) -> Result<Modifiers, &'static str> {
    if new.contains(Modifiers::ABSTRACT) {
        return Err("Cannot use the abstract modifier on an anonymous class");
    }
    if new.contains(Modifiers::FINAL) {
        return Err("Cannot use the final modifier on an anonymous class");
    }
    if flags.contains(Modifiers::READONLY) && new.contains(Modifiers::READONLY) {
        return Err("Multiple readonly modifiers are not allowed");
    }
    Ok(flags | new)
}

/// Tokens that may start a parameter.
fn starts_parameter(k: T) -> bool {
    is_member_modifier(k)
        || is_name_token(k)
        || matches!(
            k,
            T::Attribute
                | T::Array
                | T::Callable
                | T::Question
                | T::LParen
                | T::AmpersandFollowedByVarOrVararg
                | T::Ellipsis
                | T::Variable
        )
}

impl Parser<'_> {
    /// A compile error raised by a parser action; PHP reports the line of
    /// the lookahead token.
    pub(crate) fn action_error(&mut self, msg: impl Into<String>) -> crate::ParseError {
        let line = self.cur().end_line;
        crate::ParseError::compile(msg, line)
    }

    // ---- attributes ------------------------------------------------------

    /// Parses one or more `#[...]` groups.
    pub(crate) fn parse_attributes(&mut self) -> PResult<Attributes> {
        let mut groups = Vec::new();
        while self.peek() == T::Attribute {
            let start = self.bump().span;
            self.peek();
            let saved_doc = self.take_doc_comment();
            let mut attributes = Vec::new();
            loop {
                let astart = self.cur_span();
                let name = self.parse_class_name()?;
                let args = if self.peek() == T::LParen {
                    Some(self.parse_argument_list()?)
                } else {
                    None
                };
                attributes.push(Attribute {
                    name,
                    args,
                    span: self.span_from(astart),
                });
                match self.peek() {
                    T::Comma => {
                        self.bump();
                        if self.peek() == T::RBracket {
                            break;
                        }
                        if !(is_name_token(self.peek()) || self.peek() == T::Static) {
                            return Err(self.unexpected(&[T::RBracket]));
                        }
                    }
                    T::RBracket => break,
                    _ => return Err(self.unexpected(&[T::RBracket])),
                }
            }
            self.bump(); // ]
            self.doc_comment = saved_doc;
            groups.push(AttributeGroup {
                attributes,
                span: self.span_from(start),
            });
        }
        Ok(groups)
    }

    /// `class_name`: `static` or a name.
    pub(crate) fn parse_class_name(&mut self) -> PResult<Name> {
        match self.peek() {
            T::Static => {
                let t = self.bump();
                Ok(Name {
                    kind: NameKind::Unqualified,
                    name: b"static".to_vec(),
                    span: t.span,
                })
            }
            k if is_name_token(k) => {
                let t = self.bump();
                Ok(self.name_of(&t))
            }
            _ => Err(self.unexpected(&[])),
        }
    }

    /// `class_name_list`
    fn parse_class_name_list(&mut self) -> PResult<Vec<Name>> {
        let mut names = vec![self.parse_class_name()?];
        while self.peek() == T::Comma {
            self.bump();
            names.push(self.parse_class_name()?);
        }
        Ok(names)
    }

    // ---- types -----------------------------------------------------------

    fn parse_single_type(&mut self, allow_static: bool) -> PResult<Type> {
        let k = self.peek();
        let t = match k {
            T::Array => {
                let t = self.bump();
                Type {
                    kind: TypeKind::Array,
                    span: t.span,
                }
            }
            T::Callable => {
                let t = self.bump();
                Type {
                    kind: TypeKind::Callable,
                    span: t.span,
                }
            }
            T::Static if allow_static => {
                let t = self.bump();
                Type {
                    kind: TypeKind::Static,
                    span: t.span,
                }
            }
            _ if is_name_token(k) => {
                let t = self.bump();
                Type {
                    kind: TypeKind::Named(self.name_of(&t)),
                    span: t.span,
                }
            }
            _ => return Err(self.unexpected(&[])),
        };
        Ok(t)
    }

    /// Whether a token can start a type.
    pub(crate) fn starts_type(k: T, allow_static: bool) -> bool {
        is_name_token(k)
            || matches!(k, T::Array | T::Callable | T::Question | T::LParen)
            || (allow_static && k == T::Static)
    }

    /// Parses `type_expr` (or `type_expr_without_static`).
    pub(crate) fn parse_type(&mut self, allow_static: bool) -> PResult<Type> {
        let start = self.cur_span();
        if self.eat(T::Question).is_some() {
            let inner = self.parse_single_type(allow_static)?;
            return Ok(Type {
                kind: TypeKind::Nullable(Box::new(inner)),
                span: self.span_from(start),
            });
        }
        let first_paren = self.peek() == T::LParen;
        let first = if first_paren {
            self.parse_paren_intersection(allow_static)?
        } else {
            self.parse_single_type(allow_static)?
        };
        if !first_paren && self.peek() == T::AmpersandNotFollowedByVarOrVararg {
            let mut types = vec![first];
            while self.eat(T::AmpersandNotFollowedByVarOrVararg).is_some() {
                types.push(self.parse_single_type(allow_static)?);
            }
            return Ok(Type {
                kind: TypeKind::Intersection(types),
                span: self.span_from(start),
            });
        }
        if self.peek() == T::Pipe {
            let mut types = vec![first];
            while self.eat(T::Pipe).is_some() {
                if self.peek() == T::LParen {
                    types.push(self.parse_paren_intersection(allow_static)?);
                } else {
                    types.push(self.parse_single_type(allow_static)?);
                }
            }
            return Ok(Type {
                kind: TypeKind::Union(types),
                span: self.span_from(start),
            });
        }
        if first_paren {
            return Err(self.unexpected(&[T::Pipe]));
        }
        Ok(first)
    }

    fn parse_paren_intersection(&mut self, allow_static: bool) -> PResult<Type> {
        let start = self.bump().span; // (
        let mut types = vec![self.parse_single_type(allow_static)?];
        if self.peek() != T::AmpersandNotFollowedByVarOrVararg {
            return Err(self.unexpected(&[T::AmpersandNotFollowedByVarOrVararg]));
        }
        while self.eat(T::AmpersandNotFollowedByVarOrVararg).is_some() {
            types.push(self.parse_single_type(allow_static)?);
        }
        if self.peek() != T::RParen {
            return Err(self.unexpected(&[T::AmpersandNotFollowedByVarOrVararg, T::RParen]));
        }
        self.bump();
        Ok(Type {
            kind: TypeKind::Intersection(types),
            span: self.span_from(start),
        })
    }

    /// `return_type`: `: type_expr`, optional.
    fn parse_return_type(&mut self) -> PResult<Option<Type>> {
        if self.eat(T::Colon).is_some() {
            Ok(Some(self.parse_type(true)?))
        } else {
            Ok(None)
        }
    }

    // ---- parameters ------------------------------------------------------

    /// Parses `'(' parameter_list ')'`.
    pub(crate) fn parse_parameter_list(&mut self) -> PResult<Vec<Param>> {
        self.expect1(T::LParen)?;
        let mut params = Vec::new();
        if self.peek() != T::RParen {
            loop {
                if !starts_parameter(self.peek()) {
                    return Err(self.unexpected(&[T::Variable]));
                }
                params.push(self.parse_parameter()?);
                match self.peek() {
                    T::Comma => {
                        self.bump();
                        if self.peek() == T::RParen {
                            break;
                        }
                    }
                    T::RParen => break,
                    _ => return Err(self.unexpected(&[T::RParen])),
                }
            }
        }
        self.bump(); // )
        Ok(params)
    }

    fn parse_member_modifier_tokens(&mut self) -> Vec<T> {
        let mut mods = Vec::new();
        while is_member_modifier(self.peek()) {
            mods.push(self.bump().kind);
        }
        mods
    }

    fn parse_parameter(&mut self) -> PResult<Param> {
        let start = self.cur_span();
        let attributes = self.parse_attributes()?;
        let mods = self.parse_member_modifier_tokens();
        let modifiers = if mods.is_empty() {
            Modifiers::NONE
        } else {
            modifier_list_to_flags(ModifierTarget::Cpp, &mods).map_err(|m| self.action_error(m))?
        };
        let ty = if Self::starts_type(self.peek(), false) {
            Some(self.parse_type(false)?)
        } else {
            None
        };
        let by_ref = self.eat(T::AmpersandFollowedByVarOrVararg).is_some();
        let variadic = self.eat(T::Ellipsis).is_some();
        let var = self.expect1(T::Variable)?;
        let name = Ident::new(self.text(&var)[1..].to_vec(), var.span);
        let default = if self.eat(T::Equals).is_some() {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.peek();
        let doc_comment = self.take_doc_comment();
        let hooks = if self.peek() == T::LBrace {
            Some(self.parse_hook_list()?)
        } else {
            None
        };
        Ok(Param {
            attributes,
            modifiers,
            ty,
            by_ref,
            variadic,
            name,
            default,
            doc_comment,
            hooks,
            span: self.span_from(start),
        })
    }

    // ---- functions -------------------------------------------------------

    fn parse_body_block(&mut self) -> PResult<(Vec<Stmt>, bool)> {
        self.generator_stack.push(false);
        let r = self.parse_brace_block();
        let gen = self.generator_stack.pop().unwrap_or(false);
        Ok((r?, gen))
    }

    /// `'{' inner_statement_list '}'` (the `{` must be current).
    pub(crate) fn parse_brace_block(&mut self) -> PResult<Vec<Stmt>> {
        self.bump(); // {
        let stmts = self.parse_inner_list(&[T::RBrace], &[])?;
        self.bump(); // }
        Ok(stmts)
    }

    /// `function_declaration_statement`, current token is `function`.
    pub(crate) fn parse_function_decl(&mut self, attributes: Attributes) -> PResult<Stmt> {
        let kw = self.bump();
        let start = kw.span;
        let by_ref = if is_ampersand(self.peek()) {
            self.bump();
            true
        } else {
            false
        };
        let nt = self.bump(); // T_STRING or T_READONLY
        let name = self.ident_checked(&nt)?;
        self.peek();
        let doc_comment = self.take_doc_comment();
        if self.peek() != T::LParen {
            return Err(self.unexpected(&[T::LParen]));
        }
        let params = self.parse_parameter_list()?;
        let return_type = self.parse_return_type()?;
        if self.peek() != T::LBrace {
            return Err(self.unexpected(&[T::LBrace]));
        }
        let (body, is_generator) = self.parse_body_block()?;
        let span = Span::new(start.start, self.prev_span.end, kw.end_line);
        Ok(Stmt {
            kind: crate::ast::StmtKind::Function(Box::new(FunctionDecl {
                attributes,
                name,
                by_ref,
                params,
                return_type,
                body,
                doc_comment,
                is_generator,
                span,
                end_line: self.prev_end_line,
            })),
            span,
        })
    }

    /// A closure or arrow function; the current token is `function` or
    /// `fn` (after `static`, which has been consumed if `is_static`).
    pub(crate) fn parse_closure(
        &mut self,
        attributes: Attributes,
        is_static: bool,
        start: Span,
    ) -> PResult<Expr> {
        let kw = self.bump();
        let is_fn = kw.kind == T::Fn;
        let by_ref = if is_ampersand(self.peek()) {
            self.bump();
            true
        } else {
            false
        };
        self.peek();
        let doc_comment = self.take_doc_comment();
        if self.peek() != T::LParen {
            return Err(self.unexpected(&[T::LParen]));
        }
        let params = self.parse_parameter_list()?;
        if is_fn {
            let return_type = self.parse_return_type()?;
            if self.peek() != T::DoubleArrow {
                return Err(self.unexpected(&[T::DoubleArrow]));
            }
            self.bump();
            self.generator_stack.push(false);
            let body = self.parse_expr_bp(prec::ARROW_FN + 1);
            let is_generator = self.generator_stack.pop().unwrap_or(false);
            let body = body?;
            let span = Span::new(start.start, self.prev_span.end, kw.end_line);
            return Ok(Expr::new(
                ExprKind::ArrowFn(Box::new(ArrowFn {
                    attributes,
                    is_static,
                    by_ref,
                    params,
                    return_type,
                    body: Box::new(body),
                    doc_comment,
                    is_generator,
                    parenthesized: false,
                    span,
                    end_line: self.prev_end_line,
                })),
                span,
            ));
        }
        let mut uses = Vec::new();
        if self.eat(T::Use).is_some() {
            self.expect1(T::LParen)?;
            loop {
                let ustart = self.cur_span();
                let by_ref = if is_ampersand(self.peek()) {
                    self.bump();
                    true
                } else {
                    false
                };
                if self.peek() != T::Variable {
                    return Err(self.unexpected(if by_ref {
                        &[T::Variable]
                    } else {
                        &[
                            T::Variable,
                            T::AmpersandFollowedByVarOrVararg,
                            T::AmpersandNotFollowedByVarOrVararg,
                        ]
                    }));
                }
                let v = self.bump();
                uses.push(ClosureUse {
                    name: Ident::new(self.text(&v)[1..].to_vec(), v.span),
                    by_ref,
                    span: self.span_from(ustart),
                });
                match self.peek() {
                    T::Comma => {
                        self.bump();
                        if self.peek() == T::RParen {
                            break;
                        }
                    }
                    T::RParen => break,
                    _ => return Err(self.unexpected(&[T::RParen])),
                }
            }
            self.bump();
        }
        let return_type = self.parse_return_type()?;
        if self.peek() != T::LBrace {
            return Err(self.unexpected(&[T::LBrace]));
        }
        let (body, is_generator) = self.parse_body_block()?;
        let span = Span::new(start.start, self.prev_span.end, kw.end_line);
        Ok(Expr::new(
            ExprKind::Closure(Box::new(Closure {
                attributes,
                is_static,
                by_ref,
                params,
                uses,
                return_type,
                body,
                doc_comment,
                is_generator,
                span,
                end_line: self.prev_end_line,
            })),
            span,
        ))
    }

    // ---- classes ---------------------------------------------------------

    /// Class modifiers (`abstract`, `final`, `readonly`) followed by `class`.
    pub(crate) fn parse_class_with_modifiers(&mut self, attributes: Attributes) -> PResult<Stmt> {
        let start = self.cur_span();
        let mut flags = Modifiers::NONE;
        while matches!(self.peek(), T::Abstract | T::Final | T::Readonly) {
            let k = self.bump().kind;
            if flags.is_empty() {
                flags = class_modifier_flag(k);
            } else {
                flags = add_class_modifier(flags, class_modifier_flag(k))
                    .map_err(|m| self.action_error(m))?;
            }
        }
        if self.peek() != T::Class {
            return Err(self.unexpected(&[T::Abstract, T::Final, T::Readonly, T::Class]));
        }
        self.parse_class_decl(attributes, flags, start)
    }

    /// A named class-like declaration starting at `class`, `interface`,
    /// `trait` or `enum`.
    pub(crate) fn parse_class_decl(
        &mut self,
        attributes: Attributes,
        modifiers: Modifiers,
        start: Span,
    ) -> PResult<Stmt> {
        let kw = self.bump();
        let kind = match kw.kind {
            T::Interface => ClassKind::Interface,
            T::Trait => ClassKind::Trait,
            T::Enum => ClassKind::Enum,
            _ => ClassKind::Class,
        };
        let line = kw.end_line;
        let name_tok = self.expect1(T::String)?;
        let name = self.ident_checked(&name_tok)?;
        let mut extends = None;
        let mut implements = Vec::new();
        let mut backing_type = None;
        match kind {
            ClassKind::Class => {
                if self.eat(T::Extends).is_some() {
                    extends = Some(self.parse_class_name()?);
                }
                if self.eat(T::Implements).is_some() {
                    implements = self.parse_class_name_list()?;
                }
            }
            ClassKind::Interface => {
                if self.eat(T::Extends).is_some() {
                    implements = self.parse_class_name_list()?;
                }
            }
            ClassKind::Enum => {
                if self.eat(T::Colon).is_some() {
                    backing_type = Some(self.parse_type(true)?);
                }
                if self.eat(T::Implements).is_some() {
                    implements = self.parse_class_name_list()?;
                }
            }
            ClassKind::Trait => {}
        }
        self.peek();
        let doc_comment = self.take_doc_comment();
        if self.peek() != T::LBrace {
            return Err(self.unexpected(&[T::LBrace]));
        }
        let members = self.parse_class_body()?;
        let span = Span::new(start.start, self.prev_span.end, line);
        Ok(Stmt {
            kind: crate::ast::StmtKind::Class(Box::new(ClassDecl {
                attributes,
                kind,
                modifiers,
                name: Some(name),
                extends,
                implements,
                backing_type,
                members,
                doc_comment,
                span,
                end_line: self.prev_end_line,
            })),
            span,
        })
    }

    /// `anonymous_class` after `new` (and its attributes).
    pub(crate) fn parse_anonymous_class(
        &mut self,
        attributes: Attributes,
    ) -> PResult<(ClassDecl, ArgList)> {
        let start = self.cur_span();
        let mut flags = Modifiers::NONE;
        while matches!(self.peek(), T::Abstract | T::Final | T::Readonly) {
            let k = self.bump().kind;
            flags = add_anonymous_class_modifier(flags, class_modifier_flag(k))
                .map_err(|m| self.action_error(m))?;
        }
        if self.peek() != T::Class {
            return Err(self.unexpected(&[T::Abstract, T::Final, T::Readonly, T::Class]));
        }
        let kw = self.bump();
        let args = if self.peek() == T::LParen {
            self.parse_argument_list()?
        } else {
            ArgList {
                args: Vec::new(),
                span: Span::new(kw.span.end, kw.span.end, kw.end_line),
            }
        };
        let extends = if self.eat(T::Extends).is_some() {
            Some(self.parse_class_name()?)
        } else {
            None
        };
        let implements = if self.eat(T::Implements).is_some() {
            self.parse_class_name_list()?
        } else {
            Vec::new()
        };
        self.peek();
        let doc_comment = self.take_doc_comment();
        if self.peek() != T::LBrace {
            return Err(self.unexpected(&[T::LBrace]));
        }
        let members = self.parse_class_body()?;
        let span = Span::new(start.start, self.prev_span.end, kw.end_line);
        Ok((
            ClassDecl {
                attributes,
                kind: ClassKind::Class,
                modifiers: flags,
                name: None,
                extends,
                implements,
                backing_type: None,
                members,
                doc_comment,
                span,
                end_line: self.prev_end_line,
            },
            args,
        ))
    }

    /// `'{' class_statement_list '}'`
    fn parse_class_body(&mut self) -> PResult<Vec<ClassMember>> {
        self.bump(); // {
        let mut members = Vec::new();
        while self.peek() != T::RBrace {
            members.push(self.parse_class_member()?);
        }
        self.bump();
        Ok(members)
    }

    #[allow(clippy::too_many_lines)]
    fn parse_class_member(&mut self) -> PResult<ClassMember> {
        let start = self.cur_span();
        let has_attrs = self.peek() == T::Attribute;
        let attributes = self.parse_attributes()?;
        let k = self.peek();
        if k == T::Use && !has_attrs {
            self.bump();
            let traits = self.parse_class_name_list()?;
            let adaptations = match self.peek() {
                T::Semicolon => {
                    self.bump();
                    Vec::new()
                }
                T::LBrace => {
                    self.bump();
                    let mut list = Vec::new();
                    while self.peek() != T::RBrace {
                        list.push(self.parse_trait_adaptation()?);
                    }
                    self.bump();
                    list
                }
                _ => return Err(self.unexpected(&[T::Comma, T::Semicolon, T::LBrace])),
            };
            return Ok(ClassMember {
                kind: ClassMemberKind::TraitUse(TraitUse {
                    traits,
                    adaptations,
                }),
                span: self.span_from(start),
            });
        }
        if k == T::Case {
            self.bump();
            self.peek();
            let doc_comment = self.take_doc_comment();
            if !is_identifier_token(self.peek()) {
                return Err(self.unexpected(&[]));
            }
            let t = self.bump();
            let name = self.ident_checked(&t)?;
            let value = if self.eat(T::Equals).is_some() {
                Some(self.parse_expr()?)
            } else {
                None
            };
            if self.peek() != T::Semicolon {
                return Err(self.unexpected(if value.is_some() {
                    &[]
                } else {
                    &[T::Equals, T::Semicolon]
                }));
            }
            self.bump();
            return Ok(ClassMember {
                kind: ClassMemberKind::EnumCase(EnumCase {
                    attributes,
                    name,
                    value,
                    doc_comment,
                }),
                span: self.span_from(start),
            });
        }
        if k == T::Var {
            self.bump();
            let group = self.parse_property_rest(attributes, Modifiers::VAR | Modifiers::PUBLIC)?;
            return Ok(ClassMember {
                kind: ClassMemberKind::Property(group),
                span: self.span_from(start),
            });
        }
        let mods = self.parse_member_modifier_tokens();
        match self.peek() {
            T::Const => {
                let modifiers = if mods.is_empty() {
                    Modifiers::NONE
                } else {
                    modifier_list_to_flags(ModifierTarget::Constant, &mods)
                        .map_err(|m| self.action_error(m))?
                };
                self.bump();
                let ty = if is_identifier_token(self.peek()) && self.peek_nth(1) == T::Equals {
                    None
                } else {
                    if !Self::starts_type(self.peek(), true) && !is_identifier_token(self.peek()) {
                        return Err(self.unexpected(&[]));
                    }
                    Some(self.parse_type(true)?)
                };
                let consts = self.parse_class_const_list()?;
                Ok(ClassMember {
                    kind: ClassMemberKind::Const(ClassConstGroup {
                        attributes,
                        modifiers,
                        ty,
                        consts,
                    }),
                    span: self.span_from(start),
                })
            }
            T::Function => {
                let modifiers = if mods.is_empty() {
                    Modifiers::NONE
                } else {
                    modifier_list_to_flags(ModifierTarget::Method, &mods)
                        .map_err(|m| self.action_error(m))?
                };
                let method = self.parse_method(attributes, modifiers, start)?;
                Ok(ClassMember {
                    kind: ClassMemberKind::Method(method),
                    span: self.span_from(start),
                })
            }
            _ if mods.is_empty() => Err(self.unexpected(&[T::Function])),
            _ => {
                let modifiers = modifier_list_to_flags(ModifierTarget::Property, &mods)
                    .map_err(|m| self.action_error(m))?;
                let group = self.parse_property_rest(attributes, modifiers)?;
                Ok(ClassMember {
                    kind: ClassMemberKind::Property(group),
                    span: self.span_from(start),
                })
            }
        }
    }

    fn parse_class_const_list(&mut self) -> PResult<Vec<ConstElem>> {
        let mut consts = Vec::new();
        loop {
            if !is_identifier_token(self.peek()) {
                return Err(self.unexpected(&[]));
            }
            let t = self.bump();
            let name = self.ident_checked(&t)?;
            self.expect1(T::Equals)?;
            let value = self.parse_expr()?;
            self.peek();
            let doc_comment = self.take_doc_comment();
            consts.push(ConstElem {
                span: Span::new(t.span.start, self.prev_span.end, t.span.line),
                name,
                value,
                doc_comment,
            });
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

    /// After property modifiers: optional type, then the property list or a
    /// single hooked property.
    fn parse_property_rest(
        &mut self,
        attributes: Attributes,
        modifiers: Modifiers,
    ) -> PResult<PropertyGroup> {
        let ty = if Self::starts_type(self.peek(), false) {
            Some(self.parse_type(false)?)
        } else {
            None
        };
        let mut props = Vec::new();
        loop {
            let v = self.expect1(T::Variable)?;
            let name = Ident::new(self.text(&v)[1..].to_vec(), v.span);
            let default = if self.eat(T::Equals).is_some() {
                Some(self.parse_expr()?)
            } else {
                None
            };
            self.peek();
            let doc_comment = self.take_doc_comment();
            if props.is_empty() && self.peek() == T::LBrace {
                let hooks = self.parse_hook_list()?;
                props.push(PropertyItem {
                    name,
                    default,
                    doc_comment,
                    hooks: Some(hooks),
                    span: self.span_from(v.span),
                });
                return Ok(PropertyGroup {
                    attributes,
                    modifiers,
                    ty,
                    props,
                });
            }
            props.push(PropertyItem {
                name,
                default,
                doc_comment,
                hooks: None,
                span: self.span_from(v.span),
            });
            match self.peek() {
                T::Comma => {
                    self.bump();
                }
                T::Semicolon => {
                    self.bump();
                    return Ok(PropertyGroup {
                        attributes,
                        modifiers,
                        ty,
                        props,
                    });
                }
                _ => return Err(self.unexpected(&[T::Comma, T::Semicolon])),
            }
        }
    }

    /// `'{' property_hook_list '}'`
    fn parse_hook_list(&mut self) -> PResult<Vec<PropertyHook>> {
        self.bump(); // {
        let mut hooks = Vec::new();
        while self.peek() != T::RBrace {
            hooks.push(self.parse_property_hook()?);
        }
        self.bump();
        Ok(hooks)
    }

    fn parse_property_hook(&mut self) -> PResult<PropertyHook> {
        let start = self.cur_span();
        let attributes = self.parse_attributes()?;
        let mods = self.parse_member_modifier_tokens();
        let modifiers = if mods.is_empty() {
            Modifiers::NONE
        } else {
            modifier_list_to_flags(ModifierTarget::PropertyHook, &mods)
                .map_err(|m| self.action_error(m))?
        };
        let by_ref = if is_ampersand(self.peek()) {
            self.bump();
            true
        } else {
            false
        };
        let nt = self.expect1(T::String)?;
        let name = self.ident_checked(&nt)?;
        self.peek();
        let doc_comment = self.take_doc_comment();
        let line = self.prev_end_line;
        let params = if self.peek() == T::LParen {
            Some(self.parse_parameter_list()?)
        } else {
            None
        };
        self.generator_stack.push(false);
        let body = self.parse_hook_body();
        let is_generator = self.generator_stack.pop().unwrap_or(false);
        let body = body?;
        let mut span = self.span_from(start);
        span.line = line;
        Ok(PropertyHook {
            attributes,
            modifiers,
            by_ref,
            name,
            params,
            body,
            doc_comment,
            is_generator,
            span,
            end_line: self.prev_end_line,
        })
    }

    fn parse_hook_body(&mut self) -> PResult<HookBody> {
        match self.peek() {
            T::Semicolon => {
                self.bump();
                Ok(HookBody::None)
            }
            T::LBrace => Ok(HookBody::Block(self.parse_brace_block()?)),
            T::DoubleArrow => {
                self.bump();
                let e = self.parse_expr()?;
                if self.peek() != T::Semicolon {
                    return Err(self.unexpected(&[]));
                }
                self.bump();
                Ok(HookBody::Expr(Box::new(e)))
            }
            _ => Err(self.unexpected(&[T::DoubleArrow, T::Semicolon, T::LBrace])),
        }
    }

    fn parse_method(
        &mut self,
        attributes: Attributes,
        modifiers: Modifiers,
        start: Span,
    ) -> PResult<MethodDecl> {
        let kw = self.bump(); // function
        let by_ref = if is_ampersand(self.peek()) {
            self.bump();
            true
        } else {
            false
        };
        if !is_identifier_token(self.peek()) {
            return Err(self.unexpected(&[]));
        }
        let nt: Tok = self.bump();
        let name = self.ident_checked(&nt)?;
        self.peek();
        let doc_comment = self.take_doc_comment();
        if self.peek() != T::LParen {
            return Err(self.unexpected(&[T::LParen]));
        }
        let params = self.parse_parameter_list()?;
        let return_type = self.parse_return_type()?;
        let (body, is_generator) = match self.peek() {
            T::Semicolon => {
                self.bump();
                (None, false)
            }
            T::LBrace => {
                let (b, g) = self.parse_body_block()?;
                (Some(b), g)
            }
            _ => return Err(self.unexpected(&[T::Semicolon, T::LBrace])),
        };
        let span = Span::new(start.start, self.prev_span.end, kw.end_line);
        Ok(MethodDecl {
            attributes,
            modifiers,
            by_ref,
            name,
            params,
            return_type,
            body,
            doc_comment,
            is_generator,
            span,
            end_line: self.prev_end_line,
        })
    }

    fn parse_trait_method_ref(&mut self) -> PResult<TraitMethodRef> {
        let start = self.cur_span();
        let k = self.peek();
        if (is_name_token(k) || k == T::Static) && self.peek_nth(1) == T::PaamayimNekudotayim {
            let trait_name = self.parse_class_name()?;
            self.bump(); // ::
            if !is_identifier_token(self.peek()) {
                return Err(self.unexpected(&[]));
            }
            let t = self.bump();
            return Ok(TraitMethodRef {
                trait_name: Some(trait_name),
                method: self.ident_checked(&t)?,
                span: self.span_from(start),
            });
        }
        if !is_identifier_token(k) {
            return Err(self.unexpected(&[]));
        }
        let t = self.bump();
        Ok(TraitMethodRef {
            trait_name: None,
            method: self.ident_checked(&t)?,
            span: self.span_from(start),
        })
    }

    fn parse_trait_adaptation(&mut self) -> PResult<TraitAdaptation> {
        let start = self.cur_span();
        let method = self.parse_trait_method_ref()?;
        let absolute = method.trait_name.is_some();
        let adaptation = match self.peek() {
            T::Insteadof if absolute => {
                self.bump();
                let insteadof = self.parse_class_name_list()?;
                TraitAdaptation::Precedence {
                    method,
                    insteadof,
                    span: self.span_from(start),
                }
            }
            T::As => {
                self.bump();
                let k = self.peek();
                if is_member_modifier(k) {
                    let mk = self.bump().kind;
                    let modifiers = modifier_token_to_flag(ModifierTarget::Method, mk)
                        .map_err(|m| self.action_error(m))?;
                    let alias = if is_identifier_token(self.peek()) {
                        let t = self.bump();
                        Some(self.ident_checked(&t)?)
                    } else {
                        None
                    };
                    TraitAdaptation::Alias {
                        method,
                        modifiers,
                        alias,
                        span: self.span_from(start),
                    }
                } else if k == T::String || k.is_reserved_non_modifier() {
                    let t = self.bump();
                    TraitAdaptation::Alias {
                        method,
                        modifiers: Modifiers::NONE,
                        alias: Some(self.ident_checked(&t)?),
                        span: self.span_from(start),
                    }
                } else {
                    return Err(self.unexpected(&[]));
                }
            }
            _ => {
                return Err(self.unexpected(if absolute {
                    &[T::As, T::Insteadof]
                } else {
                    &[T::As]
                }))
            }
        };
        if self.peek() != T::Semicolon {
            return Err(self.unexpected(&[T::Semicolon]));
        }
        self.bump();
        Ok(adaptation)
    }
}
