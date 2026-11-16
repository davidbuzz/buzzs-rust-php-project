// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
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

/// Literals of `eval()`'d units that are released go with them: the intern
/// table does not keep strings alive, so a loop over unique code does not
/// grow it (php-src without opcache keeps request-interned strings until
/// the request ends).
#[test]
fn literals_of_released_eval_units_leave_the_intern_table() {
    let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
    let before = php_runtime::ZStr::live_interned_count();
    let status = vm.run_script(
        b"<?php\n\
          for ($i = 0; $i < 2000; $i++) {\n\
              eval('$x = \"unique literal number ' . $i . ' of a released unit\";');\n\
          }\n\
          if ($x !== 'unique literal number 1999 of a released unit') { exit(1); }\n",
        "literals.php",
    );
    assert_eq!(status, 0);
    let grown = php_runtime::ZStr::live_interned_count().saturating_sub(before);
    // The main script's own literals and the one `$x` still holds remain.
    assert!(grown < 50, "{grown} interned strings kept");
}

/// A VM that runs after another on the same thread (`php --repeat`) finds
/// none of the earlier request's literals in the intern table.
#[test]
fn a_finished_request_leaves_no_interned_literals_behind() {
    let run = |src: &[u8]| {
        let mut vm = Vm::new(&VmOptions::default(), Box::new(std::io::sink()));
        assert_eq!(vm.run_script(src, "request.php"), 0);
        drop(vm);
        php_runtime::ZStr::live_interned_count()
    };
    // Twice, so that anything a first VM initialises once per thread is in.
    run(b"<?php\n");
    let baseline = run(b"<?php\n");
    let after = run(b"<?php\n\
          $a = ['first request literal one' => 'first request literal two'];\n\
          echo 'first request literal three', \"\\n\";\n\
          $b = eval('return \"first request literal four\";');\n");
    assert_eq!(after, baseline);
}
