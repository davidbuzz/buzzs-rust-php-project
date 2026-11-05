//! Lifetime of compiled units (`include`/`eval` op arrays).
//!
//! php-src destroys the op array of an included file or `eval()`'d string
//! once it has run (`zend_include_or_eval`, `zend_eval_stringl`); only the
//! functions and classes it declared live on. The VM keeps a unit registered
//! in [`Vm::units`] only when it declares something.

use php_vm::{Vm, VmOptions};

/// Units registered after running `src`, minus those an empty script
/// leaves (anything the VM itself compiles from PHP source at startup).
fn retained_units(src: &str) -> usize {
    let count = |src: &str| {
        let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
        let status = vm.run_script(src.as_bytes(), "unit_lifetime.php");
        assert_eq!(status, 0, "script failed: {src}");
        vm.units.len()
    };
    count(src) - count("<?php\n")
}

#[test]
fn eval_without_declarations_is_not_retained() {
    let retained = retained_units(
        "<?php\n\
         $x = 0;\n\
         for ($i = 0; $i < 50; $i++) { $x = eval('return $i + 1;'); }\n\
         if ($x !== 50) { exit(1); }\n",
    );
    assert_eq!(retained, 0);
}

#[test]
fn eval_with_declarations_is_retained() {
    let retained = retained_units(
        "<?php\n\
         for ($i = 0; $i < 10; $i++) { eval('return 1;'); }\n\
         eval('function unit_lifetime_f() { return 2; }');\n\
         $g = eval('return fn() => 3;');\n\
         eval('class UnitLifetimeC {}');\n\
         if (unit_lifetime_f() + $g() !== 5 || !class_exists('UnitLifetimeC')) { exit(1); }\n",
    );
    assert_eq!(retained, 3);
}
