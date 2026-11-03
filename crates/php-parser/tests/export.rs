//! `zend_ast_export` compatibility, checked against the `assert()` messages
//! expected by php-src tests.

use php_parser::ast::{ExprKind, StmtKind};
use php_parser::export::assert_message;
use std::path::Path;

fn phpt_sections(path: &Path) -> Option<(String, String)> {
    let data = std::fs::read_to_string(path).ok()?;
    let file_start = data.find("--FILE--\n")? + 9;
    let exp_start = data.find("--EXPECT--\n")?;
    Some((
        data[file_start..exp_start].to_owned(),
        data[exp_start + 11..].to_owned(),
    ))
}

/// Collects the first argument of every top-level `assert(...)` call that
/// is the first statement of a `try` block or a top-level statement.
fn assert_args(src: &str) -> Vec<Vec<u8>> {
    let program = php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default())
        .expect("test file parses");
    let mut out = Vec::new();
    let mut stmts: Vec<&php_parser::ast::Stmt> = program.statements.iter().collect();
    while let Some(s) = stmts.first().copied() {
        stmts.remove(0);
        match &s.kind {
            StmtKind::Try { body, .. } => stmts.splice(0..0, body.iter()).for_each(drop),
            StmtKind::Expr(e) => {
                if let ExprKind::Call { callee, args } = &e.kind {
                    if let php_parser::ast::Callee::Name(n) = callee.as_ref() {
                        if n.name == b"assert" {
                            if let Some(a) = args.args.first() {
                                out.push(assert_message(a));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn check(test: &str, prefix: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../modules/php-src/Zend/tests/assert")
        .join(test);
    let Some((file, expect)) = phpt_sections(&path) else {
        eprintln!("{test} not available; skipping");
        return;
    };
    let msgs = assert_args(&file);
    let mut expected: Vec<String> = expect.split(prefix).skip(1).map(|m| m.to_owned()).collect();
    // Each message runs up to the next prefix; drop the trailing newline.
    for m in &mut expected {
        while m.ends_with('\n') {
            m.pop();
        }
    }
    assert_eq!(msgs.len(), expected.len(), "{test}: number of asserts");
    for (got, want) in msgs.iter().zip(&expected) {
        assert_eq!(String::from_utf8_lossy(got), *want, "{test}");
    }
}

#[test]
fn expect_015() {
    check("expect_015.phpt", "AssertionError: ");
}

struct AssertFinder(Vec<Vec<u8>>);

impl php_parser::visit::Visitor for AssertFinder {
    fn visit_expr(&mut self, e: &php_parser::ast::Expr) {
        if let ExprKind::Call { callee, args } = &e.kind {
            if let php_parser::ast::Callee::Name(n) = callee.as_ref() {
                if n.name.eq_ignore_ascii_case(b"assert") {
                    if let Some(a) = args.args.first() {
                        self.0.push(assert_message(a));
                    }
                }
            }
        }
        php_parser::visit::walk_expr(self, e);
    }
}

/// Every phpt whose expected output shows `assert(...)` messages: each
/// expected message must be produced by one of the file's assert calls.
#[test]
fn assert_messages_in_corpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../modules/php-src");
    if !root.exists() {
        return;
    }
    let mut stack = vec![root.join("Zend/tests"), root.join("tests")];
    let mut checked = 0;
    let mut failures = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().is_none_or(|x| x != "phpt") {
                continue;
            }
            let Some((file, expect)) = phpt_sections(&p) else {
                continue;
            };
            if !expect.contains("assert(") || file.contains("zend.assertions=0") {
                continue;
            }
            let Ok(program) =
                php_parser::parse(file.as_bytes(), &php_parser::ParseOptions::default())
            else {
                continue;
            };
            let mut f = AssertFinder(Vec::new());
            php_parser::visit::walk_stmts(&mut f, &program.statements);
            let found =
                f.0.iter()
                    .filter(|m| expect.contains(&*String::from_utf8_lossy(m)))
                    .count();
            // Expected messages: lines containing `assert(` that start a
            // message (`AssertionError: assert(` or `assert(): assert(`).
            let expected_msgs = expect.matches("AssertionError: assert(").count();
            checked += 1;
            if found < expected_msgs.min(f.0.len()) {
                failures.push(p.strip_prefix(&root).unwrap_or(&p).display().to_string());
            }
        }
    }
    eprintln!("assert export: checked {checked} files");
    assert!(
        failures.is_empty(),
        "no matching assert message in: {failures:#?}"
    );
}
