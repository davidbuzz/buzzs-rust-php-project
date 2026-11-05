//! End-to-end tests: PHP scripts run on a VM with the `session` extension.

#![allow(clippy::needless_raw_string_hashes)]

use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;

use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};

/// A `Write` sink shared with the test.
#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A fresh, empty directory for session files.
fn save_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "php-ext-session-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Runs `code` (a whole script, including `<?php`) with extra INI values.
fn run_with(code: &str, ini: &[(&str, &str)]) -> String {
    let sink = Sink::default();
    let ext = php_ext_session::Session;
    let exts: [&dyn Extension; 1] = [&ext];
    let opts = VmOptions {
        ini: ini
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.run_script(code.as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn files_handler_round_trip_and_superglobal_binding() {
    let dir = save_dir("files");
    let path = dir.to_string_lossy().into_owned();
    let ini = [
        ("session.save_path", path.as_str()),
        ("session.use_strict_mode", "0"),
    ];
    let out = run_with(
        r#"<?php ob_start();
        session_id("abc123");
        var_dump(session_start(), session_status() === PHP_SESSION_ACTIVE);
        function put() { $_SESSION['n'] = 41; }
        put();
        $_SESSION['n']++;
        echo session_encode(), "\n";"#,
        &ini,
    );
    assert_eq!(out, "bool(true)\nbool(true)\nn|i:42;\n");
    let stored = std::fs::read(dir.join("sess_abc123")).expect("session file");
    assert_eq!(stored, b"n|i:42;");
    let out = run_with(
        r#"<?php ob_start(); session_id("abc123"); session_start(); var_dump($_SESSION);"#,
        &ini,
    );
    assert_eq!(out, "array(1) {\n  [\"n\"]=>\n  int(42)\n}\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ini_values_are_shared_with_the_vm() {
    let out = run_with(
        r#"<?php
        ob_start();
        var_dump(session_name("other"), ini_get("session.name"));
        var_dump(ini_set("session.name", "third"), session_name());
        var_dump(ini_set("session.sid_length", "1"));
        ini_restore("session.name");
        var_dump(session_name());"#,
        &[("session.cookie_secure", "On")],
    );
    let expected = "string(9) \"PHPSESSID\"\nstring(5) \"other\"\nstring(5) \"other\"\nstring(5) \"third\"\n\
         \nDeprecated: ini_set(): session.sid_length INI setting is deprecated in test.php on line 5\n\
         \nWarning: ini_set(): session.configuration \"session.sid_length\" must be between 22 and 256 in test.php on line 5\n\
         bool(false)\nstring(9) \"PHPSESSID\"\n";
    assert_eq!(out, expected);
    let out = run_with(
        r#"<?php var_dump(ini_get("session.cookie_secure"));"#,
        &[("session.cookie_secure", "1")],
    );
    assert_eq!(out, "string(1) \"1\"\n");
}

#[test]
fn user_handler_object_reenters_the_module() {
    let out = run_with(
        r#"<?php ob_start();
        class H extends SessionHandler {
            public $log = [];
            function open($p, $n): bool { $this->log[] = "open:" . session_status(); return true; }
            function close(): bool { $this->log[] = "close"; return true; }
            function read($id): string|false { $this->log[] = "read:" . session_id(); return "a|i:1;"; }
            function write($id, $d): bool { $this->log[] = "write:$d"; return true; }
            function destroy($id): bool { return true; }
            function gc($m): int|false { return 0; }
            function create_sid(): string { return "fixedid"; }
        }
        $h = new H;
        session_set_save_handler($h, false);
        session_start();
        $_SESSION['b'] = 2;
        session_write_close();
        echo implode(",", $h->log), "\n";"#,
        &[],
    );
    assert_eq!(out, "open:2,read:fixedid,write:a|i:1;b|i:2;,close\n");
}

#[test]
fn shutdown_writes_through_the_user_handler() {
    let out = run_with(
        r#"<?php ob_start();
        class H implements SessionHandlerInterface, SessionIdInterface, SessionUpdateTimestampHandlerInterface {
            function open($p, $n): bool { return true; }
            function close(): bool { echo "close\n"; return true; }
            function read($id): string|false { return ""; }
            function write($id, $d): bool { echo "write $d\n"; return true; }
            function destroy($id): bool { return true; }
            function gc($m): int|false { return 0; }
            function create_sid(): string { return "sid1"; }
            function validateId($id): bool { return true; }
            function updateTimestamp($id, $d): bool { return true; }
        }
        session_set_save_handler(new H);
        session_start();
        $_SESSION['x'] = 'y';
        echo "end\n";"#,
        &[],
    );
    assert_eq!(out, "end\nwrite x|s:1:\"y\";\nclose\n");
}

#[test]
fn trans_sid_rewrites_links_and_forms() {
    let out = run_with(
        "<?php session_id('s1'); session_start(); ?>\n<a href=\"x.php?a=1\">\n<form action=\"/f\">\n",
        &[
            ("session.use_cookies", "0"),
            ("session.use_only_cookies", "0"),
            ("session.use_trans_sid", "1"),
            ("session.use_strict_mode", "0"),
            ("session.save_handler", "files"),
            ("session.save_path", std::env::temp_dir().to_str().unwrap_or("/tmp")),
        ],
    );
    assert!(
        out.ends_with(
            "<a href=\"x.php?a=1&PHPSESSID=s1\">\n<form action=\"/f\">\
             <input type=\"hidden\" name=\"PHPSESSID\" value=\"s1\" />\n"
        ),
        "{out}"
    );
    let _ = std::fs::remove_file(std::env::temp_dir().join("sess_s1"));
}

#[test]
fn output_add_rewrite_var_uses_url_rewriter_tags() {
    let out = run_with(
        "<?php ini_set('url_rewriter.tags', 'a=href,form='); output_add_rewrite_var('k', 'v w'); ?>\n<a href=\"p\">\n",
        &[],
    );
    assert_eq!(out, "<a href=\"p?k=v%20w\">\n");
}

#[test]
fn rewrite_handler_refreshes_environment_when_output_is_processed() {
    let out = run_with(
        r#"<?php ob_start();
        ini_set('url_rewriter.tags', 'a=href,form=');
        output_add_rewrite_var('k', 'v');
        echo "<a href=\"p?q=1\">\n<form action=\"http://new.test/\">x\n";
        ini_set('arg_separator.output', '&amp;');
        $_SERVER['HTTP_HOST'] = 'new.test';
        ob_end_flush();
        ob_end_flush();"#,
        &[],
    );
    assert_eq!(
        out,
        "<a href=\"p?q=1&amp;k=v\">\n<form action=\"http://new.test/\">\
         <input type=\"hidden\" name=\"k\" value=\"v\" />x\n"
    );
}

#[test]
fn rewrite_variables_reset_duplicates_and_encode_links_and_forms() {
    let out = run_with(
        r#"<?php ini_set('url_rewriter.tags', 'a=href,form=');
        output_add_rewrite_var('old', 'discarded');
        echo "<a href=\"p#f\">\n<form>x\n";
        output_reset_rewrite_vars();
        output_add_rewrite_var('k&', '<v>');
        output_add_rewrite_var('k&', 'a b');
        ob_end_flush();"#,
        &[],
    );
    assert_eq!(
        out,
        "<a href=\"p?k%26=%3Cv%3E&k%26=a%20b#f\">\n<form>\
         <input type=\"hidden\" name=\"k&amp;\" value=\"&lt;v&gt;\" />\
         <input type=\"hidden\" name=\"k&amp;\" value=\"a b\" />x\n"
    );
}

#[test]
fn rewrite_vars_do_not_restart_a_removed_handler() {
    let out = run_with(
        r#"<?php ini_set('url_rewriter.tags', 'a=href');
        output_add_rewrite_var('a', 'b');
        ob_end_clean();
        output_reset_rewrite_vars();
        var_dump(output_add_rewrite_var('c', 'd'), ob_get_level());
        echo "<a href=\"p\">\n";"#,
        &[],
    );
    assert_eq!(out, "bool(true)\nint(0)\n<a href=\"p\">\n");
}

#[test]
fn rewrite_handler_name_is_not_a_userland_alias() {
    let out = run_with(
        r#"<?php output_add_rewrite_var('k', 'v');
        $r = @ob_start('URL-Rewriter');
        $level = ob_get_level();
        ob_end_clean();
        var_dump($r, $level);"#,
        &[],
    );
    assert_eq!(out, "bool(false)\nint(1)\n");
}

#[test]
fn rewrite_var_start_inside_a_handler_bails_out() {
    let out = run_with(
        r#"<?php ob_start(function($s) {
        output_add_rewrite_var('k', 'v');
        echo "unreachable";
        return $s;
        });
        echo "discarded";
        ob_end_flush();"#,
        // As run-tests.php runs the .phpt tests: no fatal error backtrace.
        &[("fatal_error_backtraces", "0")],
    );
    assert_eq!(
        out,
        "\nFatal error: output_add_rewrite_var(): Cannot use output buffering in output buffering display handlers in test.php on line 2\n"
    );
}

#[test]
fn upload_progress_is_stored_before_request_startup() {
    use php_runtime::{Value, ZArray};
    use php_vm::ext::Rfc1867Event;

    let dir = save_dir("upload-progress");
    let path = dir.to_string_lossy().into_owned();
    let ini: Vec<(String, String)> = [
        ("session.save_path", path.as_str()),
        ("session.use_strict_mode", "0"),
        ("session.upload_progress.cleanup", "0"),
        ("session.upload_progress.freq", "0"),
        ("session.upload_progress.min_freq", "0"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect();
    let sink = Sink::default();
    let ext = php_ext_session::Session;
    let exts: [&dyn Extension; 1] = [&ext];
    let opts = VmOptions { ini };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    // The SAPI has parsed the cookies (php_session_rfc1867_early_find_sid).
    {
        let mut cookie = ZArray::new();
        cookie
            .make_mut()
            .insert("PHPSESSID", Value::string("upload1"));
        let mut g = vm.globals.borrow_mut();
        let i = g.find_or_add(b"_COOKIE") as usize;
        g.slots[i] = Some(Value::Array(cookie));
    }
    let cb = vm
        .rfc1867_callback
        .expect("the session module observes uploads");
    let events = [
        Rfc1867Event::Start {
            content_length: 100,
        },
        Rfc1867Event::FormData {
            name: b"PHP_SESSION_UPLOAD_PROGRESS",
            value: b"t",
        },
        Rfc1867Event::FileStart {
            name: b"file",
            filename: b"a.txt",
        },
        Rfc1867Event::FileData {
            offset: 0,
            length: 3,
        },
        Rfc1867Event::FileEnd {
            temp_filename: Some(&b"/tmp/phpUpload"[..]),
            cancel_upload: 0,
        },
        Rfc1867Event::End,
    ];
    for event in events {
        assert!(cb(&mut vm, event, 100));
    }
    vm.run_script(
        br#"<?php ob_start(); session_id("upload1"); session_start();
        $p = $_SESSION["upload_progress_t"];
        var_dump($p["done"], $p["bytes_processed"], $p["files"][0]["name"], $p["files"][0]["tmp_name"]);"#,
        "test.php",
    );
    drop(vm);
    let out = String::from_utf8_lossy(&sink.0.borrow()).into_owned();
    assert_eq!(
        out,
        "bool(true)\nint(100)\nstring(5) \"a.txt\"\nstring(14) \"/tmp/phpUpload\"\n"
    );
}

#[test]
fn handler_interface_warns_about_missing_php9_methods() {
    let out = run_with(
        r#"<?php
interface Combined extends SessionIdInterface, SessionUpdateTimestampHandlerInterface {}
abstract class Fine implements SessionHandlerInterface, Combined {}
abstract class Sub extends SessionHandler {}
abstract class MissingBoth implements SessionHandlerInterface {}
abstract class MissingValidateId implements SessionHandlerInterface, SessionIdInterface {}
echo "Done\n";"#,
        &[],
    );
    let expected = "\nWarning: Class MissingBoth implementing SessionHandlerInterface is missing the create_sid() method which will be required in PHP 9.0 in test.php on line 5\n\
         \nWarning: Class MissingBoth implementing SessionHandlerInterface is missing the validateId() method which will be required in PHP 9.0 in test.php on line 5\n\
         \nWarning: Class MissingValidateId implementing SessionHandlerInterface is missing the validateId() method which will be required in PHP 9.0 in test.php on line 6\n\
         Done\n";
    assert_eq!(out, expected);
}
