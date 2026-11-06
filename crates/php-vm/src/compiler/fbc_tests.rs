//! `zend_compile_args`' known-`fbc` decision: an argument for a by-value
//! parameter of a function in `CG(function_table)` is compiled for reading,
//! as are the arguments of a namespaced call's frameless branch.

use super::{
    compile, compile_with_functions, frameless_arities, CResult, CompileOptions, CompiledUnit,
    FunctionInfo,
};
use std::rc::Rc;

/// `trim(string $string, string $characters = ...)` as the VM registers it.
fn trim_info() -> FunctionInfo {
    FunctionInfo::from_flags(true, 1, [(false, false), (false, false)].into_iter())
}

/// `str_replace(..., &$count = null)`.
fn str_replace_info() -> FunctionInfo {
    FunctionInfo::from_flags(
        true,
        3,
        [
            (false, false),
            (false, false),
            (false, false),
            (true, false),
        ]
        .into_iter(),
    )
}

fn table(lc: &[u8]) -> Option<FunctionInfo> {
    match lc {
        b"trim" => Some(trim_info()),
        b"str_replace" => Some(str_replace_info()),
        _ => None,
    }
}

fn compile_src(src: &str, functions: bool) -> CResult<CompiledUnit> {
    let program =
        php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
    let opts = CompileOptions {
        filename: "t.php".into(),
        ..CompileOptions::default()
    };
    if functions {
        compile_with_functions(&program, Rc::from(src.as_bytes()), opts, &table)
    } else {
        compile(&program, Rc::from(src.as_bytes()), opts)
    }
}

fn reading_error_line(src: &str) -> Option<u32> {
    match compile_src(src, true) {
        Ok(_) => None,
        Err(e) => {
            assert_eq!(e.message, "Cannot use [] for reading", "{src}");
            Some(e.line)
        }
    }
}

#[test]
fn send_modes_follow_the_declared_parameters() {
    let info = str_replace_info();
    assert!(!info.sends_by_ref(0));
    assert!(info.sends_by_ref(3));
    // Past the declared parameters of a non-variadic function: by value.
    assert!(!info.sends_by_ref(4));
    let variadic_ref =
        FunctionInfo::from_flags(true, 1, [(false, false), (true, true)].into_iter());
    assert!(variadic_ref.variadic);
    assert_eq!(variadic_ref.num_params, 2);
    assert!(!variadic_ref.sends_by_ref(0));
    assert!(variadic_ref.sends_by_ref(1));
    assert!(variadic_ref.sends_by_ref(7));
    let none = FunctionInfo::from_flags(true, 0, std::iter::empty());
    assert!(!none.sends_by_ref(0));
}

#[test]
fn frameless_arities_come_from_the_stubs() {
    assert_eq!(frameless_arities(b"trim"), Some(&[1, 2][..]));
    assert_eq!(frameless_arities(b"str_replace"), Some(&[3][..]));
    assert_eq!(frameless_arities(b"strlen"), None);
    assert_eq!(frameless_arities(b"TRIM"), None);
}

#[test]
fn known_internal_function_reads_its_by_value_arguments() {
    assert_eq!(reading_error_line("<?php\n\ntrim($a[]);"), Some(3));
    assert_eq!(reading_error_line("<?php\ntrim($a[]->b);"), Some(2));
    assert_eq!(reading_error_line("<?php\ntrim(1, $a[][0]);"), Some(2));
    assert_eq!(
        reading_error_line("<?php\nnamespace X;\n\\trim($a[]);"),
        Some(3)
    );
    // The by-reference parameter fetches for writing.
    assert_eq!(
        reading_error_line("<?php\nstr_replace(1, 2, 3, $a[]);"),
        None
    );
    // Named arguments are not matched to parameters (run-time decision).
    assert_eq!(reading_error_line("<?php\ntrim(string: $a[]);"), None);
    // Without the function table only this file's functions are known.
    assert!(compile_src("<?php\ntrim($a[]);", false).is_ok());
    assert!(compile_src("<?php\nunknown($a[]->b);", true).is_ok());
}

#[test]
fn file_functions_are_known_once_compiled() {
    assert_eq!(
        reading_error_line("<?php\nfunction f($v) {}\nf($a[]);"),
        Some(3)
    );
    assert_eq!(
        reading_error_line("<?php\nfunction f(&$v) {}\nf($a[]->b);"),
        None
    );
    assert_eq!(
        reading_error_line("<?php\nf($a[]);\nfunction f($v) {}"),
        None
    );
    // A function's own body is compiled before it is finalized.
    assert_eq!(
        reading_error_line("<?php\nfunction f($v) { f($v[][0]); }"),
        None
    );
}

#[test]
fn namespaced_calls_read_arguments_in_their_frameless_branch() {
    assert_eq!(
        reading_error_line("<?php\nnamespace X;\ntrim($a[]);"),
        Some(3)
    );
    assert_eq!(
        reading_error_line("<?php\nnamespace X;\ntrim(1, $a[]);"),
        Some(3)
    );
    assert_eq!(
        reading_error_line("<?php\nnamespace X;\nstr_replace(1, 2, $a[]);"),
        Some(3)
    );
    // No handler for three arguments of trim(), four of str_replace().
    assert_eq!(
        reading_error_line("<?php\nnamespace X;\ntrim(1, 2, $a[]);"),
        None
    );
    assert_eq!(
        reading_error_line("<?php\nnamespace X;\nstr_replace(1, 2, 3, $a[]);"),
        None
    );
    // Unpacking or a named argument: no frameless branch.
    assert_eq!(
        reading_error_line("<?php\nnamespace X;\ntrim($a[], ...[]);"),
        None
    );
    assert_eq!(
        reading_error_line("<?php\nnamespace X;\ntrim(string: $a[]);"),
        None
    );
    // A call nested in the arguments of a frameless branch has none, but a
    // closure body is a new op array.
    assert_eq!(
        reading_error_line("<?php\nnamespace X;\ntrim(trim($a[]));"),
        None
    );
    assert_eq!(
        reading_error_line("<?php\nnamespace X;\ntrim(function () { trim($a[]); });"),
        Some(3)
    );
    // A function the name cannot fall back to has no frameless branch.
    assert_eq!(
        reading_error_line("<?php\nnamespace X;\nstrlen($a[]);"),
        None
    );
}
