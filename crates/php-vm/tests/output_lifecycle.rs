use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(source: &str) -> (i32, Vec<u8>) {
    let sink = Sink::default();
    // As run-tests.php runs the .phpt tests these port.
    let options = VmOptions {
        ini: vec![("fatal_error_backtraces".to_string(), "0".to_string())],
    };
    let mut vm = Vm::new(&options, Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "test.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    (status, output)
}

#[test]
fn gh20837_shutdown_start_reports_failure_after_handler_bailout() {
    let (status, output) = run(r#"<?php
register_shutdown_function(function () {
    ob_start(function ($input) { echo "bar"; return strtoupper($input); });
});
ob_start(function () { ob_start(); }, 1);
echo "foo";
"#);
    assert_eq!(status, 255);
    assert_eq!(
        output,
        b"\nFatal error: ob_start(): Cannot use output buffering in output buffering display handlers in test.php on line 5\n\nNotice: ob_start(): Failed to create buffer in test.php on line 3\n"
    );
}

#[test]
fn ob_015_handler_exception_is_caught_at_echo() {
    let (status, output) = run(r#"<?php
ob_start("str_rot13", 1);
try {
    echo "foo\n";
} catch (TypeError $e) {
    echo $e::class, ': ', $e->getMessage(), "\n";
}
ob_end_flush();
"#);
    assert_eq!(status, 0);
    assert_eq!(
        output,
        b"foo\nArgumentCountError: str_rot13() expects exactly 1 argument, 2 given\n"
    );
}

#[test]
fn uncaught_exception_preserves_non_utf8_message_bytes() {
    for html in [0, 1] {
        let (status, output) = run(&format!(
            "<?php ini_set('html_errors', {html}); ini_set('default_charset', 'Windows-1251'); throw new Exception(\"\\xF2\\xE5\\xF1\\xF2\");"
        ));
        assert_eq!(status, 255);
        assert!(output.windows(4).any(|bytes| bytes == b"\xf2\xe5\xf1\xf2"));
        assert!(!output.windows(3).any(|bytes| bytes == b"\xef\xbf\xbd"));
    }
}

#[test]
fn html_fatal_uses_charset_and_substitutes_invalid_utf8() {
    let (status, output) =
        run(r#"<?php ini_set('html_errors', 1); throw new Exception("\xff<&\"");"#);
    assert_eq!(status, 255);
    assert!(output
        .windows(b"\xef\xbf\xbd&lt;&amp;&quot;".len())
        .any(|bytes| bytes == b"\xef\xbf\xbd&lt;&amp;&quot;"));
}

#[test]
fn destructors_run_in_a_new_vm_after_an_earlier_request_on_the_thread() {
    // Request shutdown (and any fatal error) disables destructors for the
    // rest of that request only.
    run("<?php undefined_function();");
    let (status, output) =
        run("<?php class A { function __destruct() { echo 'd'; } } new A; echo '|';");
    assert_eq!(status, 0);
    assert_eq!(output, b"d|");
}
