// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
use std::process::{Command, Output};

fn php(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_php"))
        .args(["-n", "-d", "memory_limit=1G"])
        .args(args)
        .output()
        .expect("spawn php")
}

#[test]
fn ri_main_prints_core_configuration() {
    let output = php(&[
        "-d",
        "precision=17",
        "-d",
        "display_errors=stderr",
        "--ri",
        "main",
    ]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let s = String::from_utf8(output.stdout).unwrap();
    assert!(s.starts_with("\nDirective => Local Value => Master Value\n"));
    assert!(s.contains("precision => 17 => 17\n"));
    assert!(s.contains("display_errors => STDERR => STDERR\n"));
    assert!(s.contains("html_errors => Off => Off\n"));
    assert!(s.contains("memory_limit => 1G => 1G\n"));
    assert!(!s.contains("default_socket_timeout"));
    assert!(!s.contains("date.timezone"));
}

#[test]
fn rextinfo_main_alias_and_case_sensitive_special_name() {
    let short = php(&["--ri", "main"]);
    let long = php(&["--rextinfo", "main"]);
    assert!(short.status.success());
    assert!(long.status.success());
    assert_eq!(short.stdout, long.stdout);
    let missing = php(&["--ri", "MAIN"]);
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(missing.stdout, b"Extension 'MAIN' not present.\n");
}

#[test]
fn ini_diff_reports_cli_defines_against_builtin_defaults() {
    let output = php(&[
        "-d",
        "precision=17",
        "-d",
        "auto_prepend_file=",
        "--ini=diff",
    ]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let s = String::from_utf8(output.stdout).unwrap();
    assert!(s.starts_with("Non-default INI settings:\n"));
    assert!(s.contains("precision: \"14\" -> \"17\"\n"));
    assert!(s.contains("auto_prepend_file: (none) -> \"\"\n"));
    assert!(s.contains("html_errors: \"1\" -> \"0\"\n"));
    assert!(s.contains("implicit_flush: \"0\" -> \"1\"\n"));
    assert!(s.contains("max_execution_time: \"30\" -> \"0\"\n"));
    assert!(s.contains("memory_limit: \"128M\" -> \"1G\"\n"));
    assert!(!s.contains("serialize_precision:"));
    let names: Vec<&str> = s
        .lines()
        .skip(1)
        .map(|line| line.split(':').next().unwrap())
        .collect();
    assert!(names
        .windows(2)
        .all(|w| w[0].to_ascii_lowercase() <= w[1].to_ascii_lowercase()));
}

#[test]
fn ini_diff_unknown_argument_is_rejected() {
    let output = php(&["--ini=bogus"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"Unknown argument for --ini\n");
}

#[test]
fn ini_get_all_core_lists_every_extensions_directives() {
    // Core is module number 0, which PHP_FUNCTION(ini_get_all) does not
    // filter on (php8.5 -n: 'core' holds pcre.*, session.*, date.*).
    let output = php(&[
        "-r",
        "$c = ini_get_all('core', false); $a = ini_get_all(null, false); \
         var_dump(isset($c['pcre.jit'], $c['session.name'], $c['precision']), \
         count($c) === count($a), isset(ini_get_all('pcre')['precision']));",
    ]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "bool(true)\nbool(true)\nbool(false)\n"
    );
}

#[test]
fn ini_directives_belong_to_their_extensions() {
    // php8.5 -n: assert.* are ext/standard's, date.timezone date's,
    // mbstring.language mbstring's; arg_separator and highlight.bg do not
    // exist (highlight.bg is a removed directive).
    let output = php(&[
        "-r",
        "$core = (new ReflectionExtension('Core'))->getINIEntries(); \
         var_dump(array_key_exists('assert.active', $core), \
         array_key_exists('date.timezone', $core), \
         array_key_exists('mbstring.language', $core), \
         ini_get('arg_separator'), ini_get('highlight.bg')); \
         echo implode(',', array_keys(ini_get_all('standard', false))), \"\\n\"; \
         var_dump(array_key_exists('date.timezone', ini_get_all('date')), \
         array_key_exists('mbstring.language', ini_get_all('mbstring')));",
    ]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "bool(false)\nbool(false)\nbool(false)\nbool(false)\nbool(false)\n\
         assert.active,assert.bail,assert.callback,assert.exception,assert.warning,\
         auto_detect_line_endings,default_socket_timeout,from,session.trans_sid_hosts,\
         session.trans_sid_tags,unserialize_max_depth,url_rewriter.hosts,\
         url_rewriter.tags,user_agent\nbool(true)\nbool(true)\n"
    );
}

#[test]
fn ri_standard_lists_directives_in_registration_order() {
    let output = php(&["--ri", "standard"]);
    assert!(output.status.success());
    let s = String::from_utf8(output.stdout).unwrap();
    let names: Vec<&str> = s
        .lines()
        .skip_while(|l| !l.starts_with("Directive => "))
        .skip(1)
        .filter_map(|l| l.split(" => ").next())
        .filter(|n| !n.is_empty())
        .collect();
    assert_eq!(
        names,
        [
            "unserialize_max_depth",
            "user_agent",
            "from",
            "default_socket_timeout",
            "auto_detect_line_endings",
            "assert.active",
            "assert.bail",
            "assert.warning",
            "assert.callback",
            "assert.exception",
            "session.trans_sid_tags",
            "session.trans_sid_hosts",
            "url_rewriter.tags",
            "url_rewriter.hosts",
        ]
    );
}
