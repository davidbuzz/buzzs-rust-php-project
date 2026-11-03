//! Access to the source ASTs of declarations.
//!
//! The VM compiles constant expressions (property defaults, class constant
//! values) to op arrays without keeping the AST. php-src prints such
//! not-yet-evaluated expressions with `zend_ast_export`, so reflection
//! re-parses the declaring file (cached per file) and exports the
//! expression from there.

use php_parser::ast::{self, ClassDecl, ClassMemberKind, Expr, Stmt, StmtKind};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

thread_local! {
    static CACHE: RefCell<HashMap<Vec<u8>, Option<Rc<ast::Program>>>> = RefCell::new(HashMap::new());
}

fn program(file: &[u8]) -> Option<Rc<ast::Program>> {
    if let Some(p) = CACHE.with(|c| c.borrow().get(file).cloned()) {
        return p;
    }
    let path = String::from_utf8_lossy(file).into_owned();
    let p = std::fs::read(&path).ok().and_then(|src| {
        let opts = php_parser::ParseOptions { skip_shebang: true, ..Default::default() };
        php_parser::parse(&src, &opts).ok().map(Rc::new)
    });
    CACHE.with(|c| c.borrow_mut().insert(file.to_vec(), p.clone()));
    p
}

fn find_in<'a>(stmts: &'a [Stmt], line: u32, ns: &mut Vec<u8>, out: &mut Option<(&'a ClassDecl, Vec<u8>)>) {
    for s in stmts {
        if out.is_some() {
            return;
        }
        match &s.kind {
            StmtKind::Class(c) => {
                if c.span.line == line || c.end_line == line {
                    *out = Some((c, ns.clone()));
                    return;
                }
            }
            StmtKind::Namespace { name, body } => {
                let n = name.as_ref().map(|n| n.name.clone()).unwrap_or_default();
                match body {
                    Some(b) => {
                        let mut inner = n;
                        find_in(b, line, &mut inner, out);
                    }
                    None => *ns = n,
                }
            }
            StmtKind::Block(b) => find_in(b, line, ns, out),
            StmtKind::If { branches, else_branch } => {
                for br in branches {
                    find_in(std::slice::from_ref(br.body.as_ref()), line, ns, out);
                }
                if let Some(e) = else_branch {
                    find_in(std::slice::from_ref(e.as_ref()), line, ns, out);
                }
            }
            StmtKind::Function(f) => find_in(&f.body, line, ns, out),
            _ => {}
        }
    }
}

/// Finds a class declaration (by file and start line) and the namespace
/// it is declared in.
pub(crate) fn class_ast(file: &[u8], line: u32) -> Option<(ClassDecl, Vec<u8>)> {
    let p = program(file)?;
    let mut ns = Vec::new();
    let mut out = None;
    find_in(&p.statements, line, &mut ns, &mut out);
    out.map(|(c, n)| (c.clone(), n))
}

/// The default value expression of property `name` declared in the class at
/// `file:line`.
pub(crate) fn prop_default(file: &[u8], line: u32, name: &[u8]) -> Option<(Expr, Vec<u8>)> {
    let (c, ns) = class_ast(file, line)?;
    for m in &c.members {
        if let ClassMemberKind::Property(g) = &m.kind {
            for p in &g.props {
                if p.name.name == name {
                    return p.default.clone().map(|e| (e, ns.clone()));
                }
            }
        }
    }
    None
}

/// The value expression of class constant `name` declared in the class at
/// `file:line`.
pub(crate) fn const_value(file: &[u8], line: u32, name: &[u8]) -> Option<(Expr, Vec<u8>)> {
    let (c, ns) = class_ast(file, line)?;
    for m in &c.members {
        match &m.kind {
            ClassMemberKind::Const(g) => {
                for k in &g.consts {
                    if k.name.name == name {
                        return Some((k.value.clone(), ns.clone()));
                    }
                }
            }
            ClassMemberKind::EnumCase(ec) if ec.name.name == name => {
                return ec.value.clone().map(|e| (e, ns.clone()));
            }
            _ => {}
        }
    }
    None
}

/// The doc comment of enum case `name` declared in the class at `file:line`.
pub(crate) fn case_doc_comment(file: &[u8], line: u32, name: &[u8]) -> Option<Vec<u8>> {
    let (c, _) = class_ast(file, line)?;
    c.members.iter().find_map(|m| match &m.kind {
        ClassMemberKind::EnumCase(ec) if ec.name.name == name => ec.doc_comment.clone(),
        _ => None,
    })
}

/// Collects the parameter lists of every function-like declaration.
struct ParamCollector {
    line: u32,
    end: u32,
    found: Option<Vec<ast::Param>>,
}

impl ParamCollector {
    fn check(&mut self, start: u32, end: u32, params: &[ast::Param]) {
        if self.found.is_none() && start == self.line && (end == self.end || self.end == 0) {
            self.found = Some(params.to_vec());
        }
    }
}

impl php_parser::visit::Visitor for ParamCollector {
    fn visit_stmt(&mut self, s: &Stmt) {
        if let StmtKind::Function(f) = &s.kind {
            self.check(f.span.line, f.end_line, &f.params);
        }
        php_parser::visit::walk_stmt(self, s);
    }
    fn visit_expr(&mut self, e: &Expr) {
        match &e.kind {
            ast::ExprKind::Closure(c) => self.check(c.span.line, c.end_line, &c.params),
            ast::ExprKind::ArrowFn(c) => self.check(c.span.line, 0, &c.params),
            _ => {}
        }
        php_parser::visit::walk_expr(self, e);
    }
    fn visit_class(&mut self, c: &ClassDecl) {
        for m in &c.members {
            if let ClassMemberKind::Method(md) = &m.kind {
                self.check(md.span.line, md.end_line, &md.params);
            }
        }
        php_parser::visit::walk_class(self, c);
    }
}

/// The doc comment of parameter `idx` of the function declared at
/// `file:line-end`.
pub(crate) fn param_doc_comment(file: &[u8], line: u32, end: u32, idx: usize) -> Option<Vec<u8>> {
    let p = program(file)?;
    let mut v = ParamCollector { line, end, found: None };
    php_parser::visit::walk_stmts(&mut v, &p.statements);
    v.found?.get(idx)?.doc_comment.clone()
}

/// Reads the source line `line` (1-based) and everything after it.
fn text_from_line(file: &[u8], line: u32) -> Option<Vec<u8>> {
    let src = std::fs::read(String::from_utf8_lossy(file).into_owned()).ok()?;
    let mut cur = 1u32;
    let mut start = 0usize;
    for (i, &b) in src.iter().enumerate() {
        if cur == line {
            start = i;
            break;
        }
        if b == b'\n' {
            cur += 1;
            if cur == line {
                start = i + 1;
                break;
            }
        }
    }
    if cur != line {
        return None;
    }
    Some(src[start..].to_vec())
}

/// Extracts the attribute group starting at the first `#[` of `text`.
fn attribute_group(text: &[u8]) -> Option<Vec<u8>> {
    let start = text.windows(2).position(|w| w == b"#[")?;
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut i = start + 1;
    while i < text.len() {
        let c = text[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else {
            match c {
                b'\'' | b'"' => quote = Some(c),
                b'[' | b'(' => depth += 1,
                b']' | b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(text[start..=i].to_vec());
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// The argument expressions of the attribute `name` written at `file:line`
/// (the `n`-th attribute of that name in the group).
pub(crate) fn attribute_args(file: &[u8], line: u32, name: &[u8]) -> Option<Vec<ast::Arg>> {
    let text = text_from_line(file, line)?;
    let mut rest: &[u8] = &text;
    // An attribute may be the 2nd+ group on the line: try each group.
    for _ in 0..8 {
        let g = attribute_group(rest)?;
        let mut src = b"<?php ".to_vec();
        src.extend_from_slice(&g);
        src.extend_from_slice(b" function __reflection_attribute_probe() {}");
        if let Ok(p) = php_parser::parse(&src, &php_parser::ParseOptions::default()) {
            if let Some(StmtKind::Function(f)) = p.statements.first().map(|s| &s.kind) {
                for grp in &f.attributes {
                    for at in &grp.attributes {
                        let short = at.name.name.rsplit(|&c| c == b'\\').next().unwrap_or(&at.name.name);
                        let want = name.rsplit(|&c| c == b'\\').next().unwrap_or(name);
                        if short.eq_ignore_ascii_case(want) {
                            return Some(at.args.as_ref().map(|a| a.args.clone()).unwrap_or_default());
                        }
                    }
                }
            }
        }
        let pos = rest.windows(2).position(|w| w == b"#[")?;
        rest = &rest[pos + g.len()..];
    }
    None
}
