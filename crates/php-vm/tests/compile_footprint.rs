// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Peak memory of compiling a large file with the real request allocator
//! installed (one test: the counters are process-wide).
//!
//! `compile_program` consumes the AST and drops each top-level statement
//! once it is compiled, so the AST and the op array it becomes are not
//! alive in full together
//! (`Zend/tests/runtime_compile_time_binary_operands.phpt` includes 11 MB of
//! generated code under `memory_limit=256M`).

use php_parser::ParseOptions;
use php_runtime::ZStr;
use php_vm::compiler::{compile, compile_program, CompileOptions};
use std::io::Write;
use std::rc::Rc;

php_memlimit::install_global_allocator!();

/// Lines shaped like the generated file of
/// `runtime_compile_time_binary_operands.phpt`.
fn source(lines: usize) -> Vec<u8> {
    let mut s = b"<?php\n$c = 0; $f = 0;\n".to_vec();
    for i in 0..lines {
        writeln!(
            s,
            "$c++; if (@(({i}) == ({i})) !== (true)) {{ echo '({i}) == ({i})', \"\\n\"; $f++; }}"
        )
        .expect("writing to a Vec cannot fail");
    }
    s
}

/// `(AST bytes, peak bytes while compiling, unit bytes)`, all above the
/// usage before parsing.
fn measure(src: &[u8], consume: bool) -> (usize, usize, usize) {
    let base = php_memlimit::usage();
    let program = php_parser::parse(src, &ParseOptions::default()).expect("source parses");
    let ast = php_memlimit::usage().saturating_sub(base);
    php_memlimit::reset_peak();
    let opts = CompileOptions {
        filename: ZStr::from("t.php"),
        ..CompileOptions::default()
    };
    let unit = if consume {
        compile_program(program, Rc::from(src), opts)
    } else {
        let unit = compile(&program, Rc::from(src), opts);
        drop(program);
        unit
    }
    .expect("source compiles");
    let peak = php_memlimit::peak().saturating_sub(base);
    let unit_bytes = php_memlimit::usage().saturating_sub(base);
    drop(unit);
    (ast, peak, unit_bytes)
}

#[test]
fn consuming_compile_releases_the_ast_while_compiling() {
    let src = source(5000);
    // Warm up one-time allocations (interned strings, thread-locals).
    let _ = measure(&src, false);
    let (ast, peak_borrowed, _) = measure(&src, false);
    let (_, peak_consumed, unit_c) = measure(&src, true);
    assert!(ast > 0 && unit_c > 0);
    // Borrowed: the whole AST lives until the unit is complete.
    assert!(
        peak_borrowed >= ast + unit_c / 2,
        "borrowed peak {peak_borrowed}, AST {ast}, unit {unit_c}"
    );
    // Consumed: the AST shrinks while the unit grows; besides the copy of
    // the source only the top-level statement vector survives until the
    // end.
    let bound = ast.max(unit_c) + ast / 4 + src.len();
    assert!(
        peak_consumed <= bound,
        "consumed peak {peak_consumed} > {bound} (AST {ast}, unit {unit_c})"
    );
    assert!(peak_consumed < peak_borrowed);
}

/// A compiled op array keeps no more than 64 spare bytes per table
/// (`pass_two` reallocates the opcodes to their count): a function of a
/// loaded code base would otherwise hold up to twice its code.
#[test]
fn compiled_tables_give_back_their_spare_capacity() {
    let mut src = b"<?php\n".to_vec();
    for i in 0..300 {
        writeln!(src, "$v{i} = 'literal {i}' . $v{i};").expect("writing to a Vec cannot fail");
    }
    let program = php_parser::parse(&src, &ParseOptions::default()).expect("source parses");
    let opts = CompileOptions {
        filename: ZStr::from("t.php"),
        ..CompileOptions::default()
    };
    let unit = compile(&program, Rc::from(&src[..]), opts).expect("source compiles");
    let code = &unit.main.code;
    let spare = |cap: usize, len: usize, size: usize| (cap - len) * size;
    assert!(
        spare(
            code.ops.capacity(),
            code.ops.len(),
            size_of::<php_vm::opcode::Op>()
        ) < 64
    );
    assert!(spare(code.lines.capacity(), code.lines.len(), 4) < 64);
    assert!(spare(code.literals.capacity(), code.literals.len(), 16) < 64);
    assert!(spare(code.cv_names.capacity(), code.cv_names.len(), 8) < 64);
}
