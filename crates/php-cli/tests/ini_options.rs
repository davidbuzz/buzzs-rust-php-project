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
    let output = php(&["-d", "precision=17", "-d", "auto_prepend_file=", "--ini=diff"]);
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
    assert!(
        names
            .windows(2)
            .all(|w| w[0].to_ascii_lowercase() <= w[1].to_ascii_lowercase())
    );
}

#[test]
fn ini_diff_unknown_argument_is_rejected() {
    let output = php(&["--ini=bogus"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"Unknown argument for --ini\n");
}
