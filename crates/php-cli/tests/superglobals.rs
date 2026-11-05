//! The CLI symbol table (`php_hash_environment`, `php_startup_auto_globals`
//! in main/php_variables.c; `do_cli` in sapi/cli/php_cli.c): php-src master
//! registers `$argv`/`$argc` first, then the non-JIT auto-globals, then
//! `$_SERVER`; a JIT `$_ENV`/`$_REQUEST` appears once a script names it.

use std::process::Command;

fn keys(ini: &[&str], code: &str) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_php"));
    command.args(["-n", "-d", "memory_limit=1G"]);
    for setting in ini {
        command.args(["-d", setting]);
    }
    let out = command.args(["-r", code]).output().expect("spawn php");
    String::from_utf8(out.stdout).expect("UTF-8 output")
}

const KEYS: &str = r#"echo implode(",", array_keys($GLOBALS)), "\n";"#;

#[test]
fn default_cli_globals_follow_master_order_without_jit_arrays() {
    assert_eq!(
        keys(&[], KEYS),
        "argv,argc,_GET,_POST,_COOKIE,_FILES,_SERVER\n"
    );
    let dynamic = format!(r#"{KEYS} $x = "_ENV"; var_dump(isset($$x));"#);
    assert_eq!(
        keys(&[], &dynamic),
        "argv,argc,_GET,_POST,_COOKIE,_FILES,_SERVER\nbool(false)\n"
    );
}

#[test]
fn naming_a_jit_auto_global_creates_it_at_compile_time() {
    let code = format!("{KEYS} function f() {{ return $_REQUEST; }} $_ENV;");
    assert_eq!(
        keys(&[], &code),
        "argv,argc,_GET,_POST,_COOKIE,_FILES,_SERVER,_REQUEST,_ENV\n"
    );
    let env = "var_dump(is_array($_ENV), $_REQUEST);";
    assert_eq!(keys(&[], env), "bool(true)\narray(0) {\n}\n");
}

#[test]
fn variables_order_without_e_gives_an_empty_env() {
    let code = "var_dump($_ENV, count($_SERVER));";
    assert_eq!(
        keys(&["variables_order=GPC"], code),
        "array(0) {\n}\nint(0)\n"
    );
}

#[test]
fn auto_globals_jit_off_creates_every_array_in_registration_order() {
    assert_eq!(
        keys(&["auto_globals_jit=0"], KEYS),
        "argv,argc,_GET,_POST,_COOKIE,_SERVER,_ENV,_REQUEST,_FILES\n"
    );
    assert_eq!(
        keys(&["register_argc_argv=1"], KEYS),
        "argv,argc,_GET,_POST,_COOKIE,_SERVER,_FILES\n"
    );
}

#[test]
fn constant_var_var_auto_global_is_global_in_functions() {
    let code = r#"function f() { return is_array(${"_SERVER"}); } var_dump(f());"#;
    assert_eq!(keys(&[], code), "bool(true)\n");
}
