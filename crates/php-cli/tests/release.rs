// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The request VM's release: discarded on the exit path (`Vm::discard`,
//! php-src's fast shutdown), dropped when another request follows
//! (`--repeat`, `-l` over several files). Everything a script left behind
//! must still reach its destination.

use std::path::PathBuf;
use std::process::{Command, Output};

fn php(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_php"))
        .args(["-n", "-d", "memory_limit=1G"])
        .args(args)
        .output()
        .expect("spawn php")
}

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("php-cli-release-{}-{name}", std::process::id()))
}

#[test]
fn exit_status_and_output_survive_the_discard() {
    let out = php(&["-r", "echo 'x'; exit(3);"]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(out.stdout, b"x");
}

#[test]
fn streams_left_open_by_statics_globals_and_closures_are_complete_at_exit() {
    let files: Vec<PathBuf> = ["class", "static", "global"]
        .iter()
        .map(|n| temp(&format!("{n}.txt")))
        .collect();
    let script = temp("streams.php");
    let (class, stat, global) = (files[0].display(), files[1].display(), files[2].display());
    std::fs::write(
        &script,
        format!(
            "<?php
class Keep {{ public static $f; }}
Keep::$f = fopen('{class}', 'w'); fwrite(Keep::$f, \"class\\n\");
function holder() {{ static $f; $f = fopen('{stat}', 'w'); fwrite($f, \"static\\n\"); }}
holder();
$g = fopen('{global}', 'w'); fwrite($g, \"global\\n\");
register_shutdown_function(function () use ($g) {{ fwrite($g, \"shutdown\\n\"); }});
echo 'done';
"
        ),
    )
    .unwrap();
    let out = php(&[script.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        out.stdout,
        b"done",
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let contents: Vec<String> = files
        .iter()
        .map(|f| std::fs::read_to_string(f).unwrap())
        .collect();
    assert_eq!(contents, ["class\n", "static\n", "global\nshutdown\n"]);
    for f in files.iter().chain(std::iter::once(&script)) {
        let _ = std::fs::remove_file(f);
    }
}

#[test]
fn repeat_runs_the_request_again_in_a_fresh_vm() {
    let out = php(&[
        "--repeat",
        "2",
        "-r",
        "echo defined('X') ? 'again' : 'first'; define('X', 1);",
    ]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Executing for the first time...\nfirstFinished execution, repeating...\nfirst"
    );
}

/// Each repeat is a request of its own, whose objects are numbered from 1
/// as in php-src (`php8.5 --repeat 3` prints 2 every time): the objects the
/// previous run's function and method statics held are freed with its VM,
/// not handed out again to the next run's `new` in the order the drop freed
/// them (2, then 1, then 3 before this was fixed).
#[test]
fn repeat_numbers_each_requests_objects_from_one() {
    let out = php(&[
        "--repeat",
        "3",
        "-r",
        "function f($v = null) { static $x = null; if ($v !== null) { $x = $v; } return $x; } \
         class K { function m($v = null) { static $y = null; if ($v !== null) { $y = $v; } return $y; } } \
         f(new stdClass); (new K)->m(new stdClass); echo spl_object_id(new stdClass), \"\\n\";",
    ]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Executing for the first time...\n2\nFinished execution, repeating...\n2\n\
         Finished execution, repeating...\n2\n"
    );
}

/// The request's stream wrapper and filter registrations end with it
/// (`php_shutdown_stream_hashes()`): the second run registers `test://` and
/// the filter again and finds `data://` back, as `php8.5 --repeat 2` does.
/// Before, the second run got "Protocol test:// is already defined", "Unable
/// to unregister protocol data://" and `false` three times.
#[test]
fn repeat_starts_from_the_startup_stream_wrappers_and_filters() {
    let out = php(&[
        "--repeat",
        "2",
        "-r",
        "class W { public $context; function stream_open($p, $m, $o, &$op) { return true; } \
         function stream_eof() { return true; } } \
         var_dump(stream_wrapper_register(\"test\", \"W\")); \
         var_dump(stream_wrapper_unregister(\"data\")); \
         echo in_array(\"data\", stream_get_wrappers()) ? \"data\" : \"no data\", \"\\n\"; \
         class F extends php_user_filter {} \
         var_dump(stream_filter_register(\"myfilter\", \"F\"));",
    ]);
    assert!(out.status.success());
    let run = "bool(true)\nbool(true)\nno data\nbool(true)\n";
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("Executing for the first time...\n{run}Finished execution, repeating...\n{run}")
    );
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}

/// `PHP_RSHUTDOWN(basic)` puts back the value each `putenv()` name had
/// before the request (`BG(putenv_ht)`, `php_putenv_destructor`), and
/// `php_stream_error_state_cleanup()` drops the stored stream errors: the
/// second run sees what the first saw. php8.5 prints `bool(false)` and
/// `string(4) "orig"` in both runs; the stored errors are php-src master's
/// (main/streams/stream_errors.c).
#[test]
fn repeat_starts_from_the_startup_environment_and_no_stored_stream_errors() {
    let out = Command::new(env!("CARGO_BIN_EXE_php"))
        .args(["-n", "-d", "memory_limit=1G", "--repeat", "2", "-r"])
        .arg(
            "var_dump(getenv('PHP_CLI_RELEASE_UNSET')); \
             putenv('PHP_CLI_RELEASE_UNSET=value'); putenv('PHP_CLI_RELEASE_UNSET='); \
             var_dump(getenv('PHP_CLI_RELEASE_PRESET')); \
             putenv('PHP_CLI_RELEASE_PRESET=a'); putenv('PHP_CLI_RELEASE_PRESET=b'); \
             var_dump(count(stream_last_errors())); \
             $c = stream_context_create(['stream' => ['error_mode' => StreamErrorMode::Silent, \
             'error_store' => StreamErrorStore::All]]); \
             fopen('php://nonexistent', 'r', false, $c); \
             var_dump(count(stream_last_errors()));",
        )
        .env_remove("PHP_CLI_RELEASE_UNSET")
        .env("PHP_CLI_RELEASE_PRESET", "orig")
        .output()
        .expect("spawn php");
    assert!(out.status.success());
    let run = "bool(false)\nstring(4) \"orig\"\nint(0)\nint(1)\n";
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("Executing for the first time...\n{run}Finished execution, repeating...\n{run}")
    );
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}

/// php-src starts its modules before `do_cli()` reads its arguments: what
/// a module reports at startup comes first, once, whatever the run then
/// does. Output of php8.5 for each command line (its version banner aside).
#[test]
fn module_startup_warnings_come_before_everything_a_run_prints() {
    let warning = "\nWarning: PHP Startup: session.gc_probability must be greater than or equal \
                   to 0 in Unknown on line 0\n";
    let bad = ["-d", "session.gc_probability=-1"];
    // A script that cannot be opened.
    let out = php(&[bad[0], bad[1], "/nonexistent/php-cli-release.php"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), warning);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Could not open input file: /nonexistent/php-cli-release.php\n"
    );
    assert_eq!(out.status.code(), Some(1));
    // `-v`.
    let out = php(&[bad[0], bad[1], "-v"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(text.starts_with(&format!("{warning}PHP ")), "{text}");
    // `--repeat`: ahead of the first line, and not again for the second run.
    let out = php(&[bad[0], bad[1], "--repeat", "2", "-r", "echo \"run\\n\";"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "{warning}Executing for the first time...\nrun\nFinished execution, repeating...\nrun\n"
        )
    );
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}

#[test]
fn lint_checks_every_file_given() {
    let good = temp("good.php");
    let bad = temp("bad.php");
    std::fs::write(&good, "<?php echo 1;").unwrap();
    std::fs::write(&bad, "<?php echo ;").unwrap();
    let out = php(&["-l", good.to_str().unwrap(), bad.to_str().unwrap()]);
    let s = String::from_utf8_lossy(&out.stdout);
    let good_line = format!("No syntax errors detected in {}\n", good.display());
    let bad_line = format!("Errors parsing {}\n", bad.display());
    assert!(s.contains(&good_line), "{s}");
    assert!(s.contains(&bad_line), "{s}");
    assert_eq!(out.status.code(), Some(255));
    let _ = std::fs::remove_file(&good);
    let _ = std::fs::remove_file(&bad);
}

/// `php -l` is `php_lint_script`: the whole compile, with the diagnostics
/// reported by the engine. A compile error the compiler (not the parser)
/// raises fails the lint, with the fatal error backtrace
/// `fatal_error_backtraces=On` gives a frameless fatal error (`#0 {main}`),
/// and `display_errors=stderr` takes php_error_cb's stderr format.
#[test]
fn lint_reports_compile_errors_through_the_engine() {
    let file = temp("isset.php");
    std::fs::write(&file, "<?php function f() { return isset(f()); }").unwrap();
    let path = file.to_str().unwrap();
    let message = format!(
        "Fatal error: Cannot use isset() on the result of an expression (you can use \"null !== expression\" instead) in {path} on line 1\nStack trace:\n#0 {{main}}\n"
    );
    let out = php(&["-l", path]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("\n{message}Errors parsing {path}\n")
    );
    assert_eq!(out.status.code(), Some(255));
    let out = php(&["-d", "display_errors=stderr", "-l", path]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("Errors parsing {path}\n")
    );
    assert_eq!(String::from_utf8_lossy(&out.stderr), message);
    assert_eq!(out.status.code(), Some(255));
    let _ = std::fs::remove_file(&file);
}
