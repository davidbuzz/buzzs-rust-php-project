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
