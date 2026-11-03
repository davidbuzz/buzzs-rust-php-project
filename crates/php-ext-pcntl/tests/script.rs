//! End-to-end tests: PHP scripts run on a VM with the `pcntl` extension
//! (and `posix` for `posix_kill()`).  Expected outputs come from
//! `/usr/bin/php`.

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

/// Runs `code` (without `<?php`) and returns its output.
fn run(code: &str) -> String {
    let sink = Sink::default();
    let pcntl = php_ext_pcntl::PcntlExt;
    let posix = php_ext_posix::PosixExt;
    let exts: [&dyn Extension; 2] = [&pcntl, &posix];
    let opts = VmOptions { ini: vec![] };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

fn status_macros() {
    assert_eq!(
        run("var_dump(pcntl_wifexited(256), pcntl_wexitstatus(256), pcntl_wifsignaled(9), pcntl_wtermsig(9), pcntl_wifstopped(0x137f), pcntl_wstopsig(0x137f));"),
        "bool(true)\nint(1)\nbool(true)\nint(9)\nbool(true)\nint(19)\n"
    );
}

fn handlers_are_dispatched_with_siginfo() {
    assert_eq!(
        run(
            r#"pcntl_signal(SIGUSR1, function ($signo, $info) { echo "got $signo ", $info['signo'], "\n"; });
            posix_kill(posix_getpid(), SIGUSR1);
            echo "before\n";
            var_dump(pcntl_signal_dispatch());
            var_dump(pcntl_signal_get_handler(SIGUSR1) instanceof Closure, pcntl_signal_get_handler(SIGUSR2));
            pcntl_signal(SIGUSR1, SIG_DFL);"#
        ),
        "before\ngot 10 10\nbool(true)\nbool(true)\nint(0)\n"
    );
}

fn argument_errors() {
    assert_eq!(
        run(r#"foreach ([fn () => pcntl_signal(0, SIG_DFL), fn () => pcntl_signal(SIGUSR1, 5),
                fn () => pcntl_signal(SIGUSR1, 'nope'), fn () => pcntl_sigprocmask(9, [])] as $f) {
                try { $f(); } catch (Throwable $e) { echo $e::class, ': ', $e->getMessage(), "\n"; }
            }"#),
        "ValueError: pcntl_signal(): Argument #1 ($signal) must be greater than or equal to 1\n\
         ValueError: pcntl_signal(): Argument #2 ($handler) must be either SIG_DFL or SIG_IGN when an integer value is given\n\
         TypeError: pcntl_signal(): Argument #2 ($handler) must be of type callable|int, string given\n\
         ValueError: pcntl_sigprocmask(): Argument #1 ($mode) must be one of SIG_BLOCK, SIG_UNBLOCK, or SIG_SETMASK\n"
    );
}

fn sigprocmask_and_sigtimedwait() {
    assert_eq!(
        run(r"pcntl_sigprocmask(SIG_BLOCK, [SIGUSR2], $old);
            posix_kill(posix_getpid(), SIGUSR2);
            var_dump(pcntl_sigtimedwait([SIGUSR2], $info, 1), $info['signo']);
            pcntl_sigprocmask(SIG_SETMASK, $old);
            var_dump(pcntl_sigtimedwait([SIGUSR2], $info, 0, 1000));"),
        "int(12)\nint(12)\nbool(false)\n"
    );
}

/// Signals are process-wide and a process-directed signal reaches any
/// thread not blocking it, so the scenarios run sequentially on one test
/// thread (the harness's main thread is isolated by the extension).
#[test]
fn scenarios() {
    status_macros();
    handlers_are_dispatched_with_siginfo();
    argument_errors();
    sigprocmask_and_sigtimedwait();
}
