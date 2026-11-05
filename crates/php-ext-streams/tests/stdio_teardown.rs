//! The CLI's `STDIN`/`STDOUT`/`STDERR` resources close their descriptors
//! only on an explicit `fclose()` (GH-8827); dropping a VM (teardown,
//! embeddings such as `php -S` workers, in-process tests) leaves the
//! process's standard descriptors open.

use std::cell::RefCell;
use std::io::Write;
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

fn run(code: &str) -> String {
    let sink = Sink::default();
    let streams = php_ext_streams::Streams;
    let exts: [&dyn Extension; 1] = [&streams];
    let opts = VmOptions { ini: vec![] };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

fn fd_open(fd: i32) -> bool {
    // SAFETY: F_GETFD only queries the descriptor table.
    unsafe { libc::fcntl(fd, libc::F_GETFD) != -1 }
}

#[test]
fn dropping_the_vm_keeps_std_descriptors_but_fclose_closes_them() {
    // Plain teardown: STDIN/STDOUT/STDERR resources are dropped.
    let out = run("echo is_resource(STDOUT) ? 'ok' : 'no';");
    assert_eq!(out, "ok");
    assert!(fd_open(0) && fd_open(1) && fd_open(2));
    assert!(writeln!(std::io::stdout(), "stdout still writable").is_ok());
    assert!(std::io::stdout().flush().is_ok());

    // An explicit fclose(STDIN) really closes descriptor 0. It runs on a
    // fresh thread: the streams layer's request globals are thread-local
    // and only the first `php://stdin` of a thread uses descriptor 0
    // itself (later ones are dups, like the CLI's `cli_in`).
    // SAFETY: duplicating a descriptor has no memory effects.
    let saved = unsafe { libc::dup(0) };
    assert!(saved >= 0);
    let (out, closed) = std::thread::spawn(|| {
        let out = run("var_dump(fclose(STDIN));");
        (out, !fd_open(0))
    })
    .join()
    .expect("thread");
    // SAFETY: `saved` is our duplicate; it is moved back onto 0 and closed.
    unsafe {
        libc::dup2(saved, 0);
        libc::close(saved);
    }
    assert_eq!(out, "bool(true)\n");
    assert!(closed, "fclose(STDIN) must close descriptor 0");
}

/// `user_filter_factory_create` (`ext/standard/user_filters.c`): on a class
/// that forbids dynamic properties, the `$params` failure is thrown with
/// the `$filtername` one as its previous exception.
#[test]
fn user_filter_property_errors_are_chained() {
    let out = run(r#"var_dump(stream_filter_register("f", "SensitiveParameter"));
$s = fopen("php://memory", "w+");
try { stream_filter_append($s, "f", STREAM_FILTER_READ); } catch (Error $e) { echo $e->getMessage(), "|", $e->getPrevious()?->getMessage(), "\n"; }
fclose($s);
echo "end\n";"#);
    assert_eq!(
        out,
        "bool(true)\nCannot create dynamic property SensitiveParameter::$params|Cannot create dynamic property SensitiveParameter::$filtername\nend\n"
    );
}

/// GH-15836: a stream released when a function returns is closed right
/// after that opcode (not at shutdown), and its user filters see
/// `$this->stream === null` during the close flush.
#[test]
fn released_filtered_stream_closes_before_next_statement() {
    let out = run(r#"class F extends php_user_filter {
    function filter($in, $out, &$consumed, $closing): int {
        if ($closing) { echo "closing "; var_dump($this->stream); }
        return PSFS_PASS_ON;
    }
}
stream_filter_register("f", "F");
function run() { $s = fopen("php://memory", "w+"); stream_filter_append($s, "f", STREAM_FILTER_WRITE); }
run();
echo "after\n";"#);
    assert_eq!(out, "closing NULL\nafter\n");
}

/// `php_verror()`: once creating an include's filter threw, its warnings
/// name the active function (`main`, `f`) rather than `include`.
#[test]
fn include_warnings_after_a_throw_name_the_active_function() {
    let out = run(r#"class SampleFilter extends php_user_filter { private $data = \FOO; }
stream_filter_register('sample.filter', SampleFilter::class);
function f() { include 'php://filter/read=sample.filter/resource=php://memory'; }
try { include 'php://filter/read=sample.filter/resource=php://memory'; } catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), "\n"; }
try { f(); } catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), "\n"; }"#);
    assert_eq!(
        out,
        "\nWarning: main(): Unable to create or locate filter \"sample.filter\" in test.php on line 4\n\
         \nWarning: main(): Unable to create filter (sample.filter) in test.php on line 4\n\
         Error: Undefined constant \"FOO\"\n\
         \nWarning: f(): Unable to create or locate filter \"sample.filter\" in test.php on line 3\n\
         \nWarning: f(): Unable to create filter (sample.filter) in test.php on line 3\n\
         Error: Undefined constant \"FOO\"\n"
    );
}
