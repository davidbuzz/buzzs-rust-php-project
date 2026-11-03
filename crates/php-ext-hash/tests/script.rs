//! End-to-end tests: PHP scripts run on a VM with the `hash` extension.
//! Expected values come from php-src (`/usr/bin/php`).

const EXT: php_ext_hash::Hash = php_ext_hash::Hash;

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
    let ext = EXT;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn one_shot_digests() {
    assert_eq!(
        run(r"echo hash('sha256', 'abc'), ' ', hash_hmac('md5', 'data', 'key');"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad 9d5c73ef85594d34ec4438b7c97e51d8"
    );
    assert_eq!(
        run(r"var_dump(md5('x'), sha1('x'), crc32('x'), hash_equals('a', 'a'));"),
        "string(32) \"9dd4e461268c8034f5c8564e155c67a6\"\nstring(40) \"11f6ad8ec52a2984abaafd7c3b516503785c2072\"\nint(2363233923)\nbool(true)\n"
    );
}

#[test]
fn incremental_and_copy() {
    assert_eq!(
        run(
            r"$c = hash_init('sha1'); hash_update($c, 'ab'); $d = hash_copy($c); hash_update($c, 'c');
            echo hash_final($c), ' ', hash_final($d);"
        ),
        "a9993e364706816aba3e25717850c26c9cd0d89d da23614e02469a0d7c7bd1bdab5c9c474b1904dc"
    );
}

#[test]
fn errors() {
    assert_eq!(
        run(r"$c = hash_init('md5'); hash_final($c);
            try { hash_update($c, 'x'); } catch (TypeError $e) { echo $e->getMessage(), '|'; }
            try { hash('nope', ''); } catch (ValueError $e) { echo $e->getMessage(), '|'; }
            try { hash_equals(1, 'a'); } catch (TypeError $e) { echo $e->getMessage(); }"),
        "hash_update(): Argument #1 ($context) must be a valid, non-finalized HashContext|hash(): Argument #1 ($algo) must be a valid hashing algorithm|hash_equals(): Argument #1 ($known_string) must be of type string, int given"
    );
}

#[test]
fn derivation() {
    assert_eq!(
        run(
            r"echo bin2hex(hash_hkdf('sha256', 'key', 8)), ' ', hash_pbkdf2('sha1', 'p', 's', 2, 10);"
        ),
        "80c724c4a65d7956 ea4b66d403"
    );
}

#[test]
fn serialization_round_trip() {
    assert_eq!(
        run(r"$c = hash_init('md5'); hash_update($c, 'abc');
            echo hash_final(unserialize(serialize($c)));"),
        "900150983cd24fb0d6963f7d28e17f72"
    );
}

#[test]
fn debug_info_shows_algo() {
    assert_eq!(
        run(r"var_dump(hash_init('xxh3'));"),
        "object(HashContext)#1 (1) {\n  [\"algo\"]=>\n  string(4) \"xxh3\"\n}\n"
    );
}
