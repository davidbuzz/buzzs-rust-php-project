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
