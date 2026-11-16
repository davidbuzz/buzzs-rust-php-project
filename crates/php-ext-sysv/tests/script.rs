// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP scripts run on a VM with the four System V IPC
//! extensions, on private (`IPC_PRIVATE`) IPC objects.  Expected outputs
//! come from `/usr/bin/php`.

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
    let (a, b, c, d) = (
        php_ext_sysv::Sysvmsg,
        php_ext_sysv::Sysvsem,
        php_ext_sysv::Sysvshm,
        php_ext_sysv::Shmop,
    );
    let exts: [&dyn Extension; 4] = [&a, &b, &c, &d];
    let opts = VmOptions { ini: vec![] };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn message_queue_round_trip() {
    assert_eq!(
        run(r"$q = msg_get_queue(0);
            var_dump(msg_send($q, 2, ['a' => 1.5, 'b' => [true]]));
            var_dump(msg_send($q, 3, 42, false));
            var_dump(msg_receive($q, 2, $t, 1024, $m), $t, $m);
            var_dump(msg_receive($q, 0, $t, 1024, $m, false), $t, $m);
            var_dump(msg_receive($q, 0, $t, 1024, $m, true, MSG_IPC_NOWAIT, $err), $t, $m, $err === MSG_ENOMSG);
            var_dump(msg_stat_queue($q)['msg_qnum']);
            var_dump(msg_remove_queue($q));"),
        "bool(true)\nbool(true)\nbool(true)\nint(2)\narray(2) {\n  [\"a\"]=>\n  float(1.5)\n  [\"b\"]=>\n  array(1) {\n    [0]=>\n    bool(true)\n  }\n}\n\
         bool(true)\nint(3)\nstring(2) \"42\"\n\
         bool(false)\nint(0)\nbool(false)\nbool(true)\n\
         int(0)\nbool(true)\n"
    );
}

#[test]
fn message_queue_errors() {
    assert_eq!(
        run(r#"$q = msg_get_queue(0);
            try { msg_send($q, 1, null, false); } catch (TypeError $e) { echo $e->getMessage(), "\n"; }
            try { msg_receive($q, 0, $t, 0, $m); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
            try { new SysvMessageQueue; } catch (Error $e) { echo $e->getMessage(), "\n"; }
            msg_send($q, 1, 'garbage', false);
            var_dump(msg_receive($q, 0, $t, 100, $m), $m);
            msg_remove_queue($q);"#),
        "msg_send(): Argument #3 ($message) must be of type string|int|float|bool, null given\n\
         msg_receive(): Argument #4 ($max_message_size) must be greater than 0\n\
         Cannot directly construct SysvMessageQueue, use msg_get_queue() instead\n\
         \nWarning: msg_receive(): Message corrupted in test.php on line 6\nbool(false)\nbool(false)\n"
    );
}

#[test]
fn shared_memory_variables() {
    assert_eq!(
        run(r#"$s = shm_attach(0, 1024);
            var_dump(shm_put_var($s, 1, ['x' => 'y']), shm_has_var($s, 1), shm_get_var($s, 1));
            var_dump(shm_remove_var($s, 1), shm_has_var($s, 1));
            var_dump(@shm_get_var($s, 1));
            shm_remove($s);
            var_dump(shm_detach($s));
            try { shm_has_var($s, 1); } catch (Error $e) { echo $e->getMessage(), "\n"; }"#),
        "bool(true)\nbool(true)\narray(1) {\n  [\"x\"]=>\n  string(1) \"y\"\n}\n\
         bool(true)\nbool(false)\nbool(false)\nbool(true)\n\
         Shared memory block has already been destroyed\n"
    );
}

#[test]
fn semaphore_acquire_release() {
    assert_eq!(
        run(r"$s = sem_get(0);
            var_dump(sem_acquire($s), sem_acquire($s, true), sem_release($s));
            var_dump(sem_release($s));
            var_dump(sem_remove($s));"),
        "bool(true)\nbool(false)\nbool(true)\n\
         \nWarning: sem_release(): SysV semaphore for key 0x0 is not currently acquired in test.php on line 3\nbool(false)\nbool(true)\n"
    );
}

#[test]
fn shmop_read_write() {
    assert_eq!(
        run(r#"$s = shmop_open(0, 'c', 0644, 16);
            var_dump(shmop_size($s), shmop_write($s, 'hello', 3), shmop_read($s, 3, 5));
            try { shmop_read($s, 20, 1); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
            var_dump(shmop_delete($s));"#),
        "int(16)\nint(5)\nstring(5) \"hello\"\n\
         shmop_read(): Argument #2 ($offset) must be between 0 and the segment size\n\
         bool(true)\n"
    );
}
