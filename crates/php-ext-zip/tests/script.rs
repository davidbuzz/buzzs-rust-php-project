// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP scripts run on a VM with the `zip` extension
//! (plus the stream and string functions they use). Expected outputs follow
//! `/usr/bin/php` with php-src master messages.

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

/// Keeps the test process's standard descriptors: `php-ext-streams`
/// really closes them when the VM's `STDIN`/`STDOUT`/`STDERR` resources
/// are released (GH-8827), which would silence the test harness.
struct SavedFds([i32; 3]);

impl SavedFds {
    fn save() -> Self {
        // SAFETY: duplicating the standard descriptors has no memory effects.
        SavedFds([0, 1, 2].map(|fd| unsafe { libc::dup(fd) }))
    }
}

impl Drop for SavedFds {
    fn drop(&mut self) {
        for (fd, saved) in (0..).zip(self.0) {
            if saved >= 0 {
                // SAFETY: `saved` is a descriptor we own; it is moved back
                // onto the standard slot and then closed.
                unsafe {
                    libc::dup2(saved, fd);
                    libc::close(saved);
                }
            }
        }
    }
}

/// A fresh directory for one test.
fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("php-ext-zip-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// Runs `code` (without `<?php`, `$dir` set to a scratch directory) and
/// returns its output.
fn run(name: &str, code: &str) -> String {
    // One VM at a time: the descriptors are process-wide.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _fds = SavedFds::save();
    let dir = tmpdir(name);
    let sink = Sink::default();
    let streams = php_ext_streams::Streams;
    let stdstr = php_ext_stdstr::StdStr;
    let zip = php_ext_zip::Zip;
    let exts: [&dyn Extension; 3] = [&stdstr, &streams, &zip];
    let opts = VmOptions { ini: vec![] };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    let src = format!("<?php $dir = '{}'; {code}", dir.display());
    vm.run_script(src.as_bytes(), "test.php");
    drop(vm);
    let _ = std::fs::remove_dir_all(&dir);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn create_reopen_and_read_back() {
    let out = run(
        "roundtrip",
        r#"$z = new ZipArchive;
        var_dump($z->open("$dir/a.zip", ZipArchive::CREATE));
        var_dump($z->addFromString('x.txt', 'hello'), $z->addEmptyDir('d'));
        var_dump($z->numFiles, $z->count(), $z->close());
        var_dump($z->open("$dir/a.zip"), $z->getFromName('x.txt'), $z->locateName('d/'));
        var_dump($z->getNameIndex(5), $z->statName('x.txt')['size']);"#,
    );
    assert_eq!(
        out,
        "bool(true)\nbool(true)\nbool(true)\nint(2)\nint(2)\nbool(true)\n\
         bool(true)\nstring(5) \"hello\"\nint(1)\nbool(false)\nint(5)\n"
    );
}

#[test]
fn virtual_properties_are_read_only() {
    let out = run(
        "props",
        r#"$z = new ZipArchive;
        try { $z->status = 1; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        var_dump($z->lastId, isset($z->filename), empty($z->numFiles));
        try { clone $z; } catch (Error $e) { echo $e->getMessage(), "\n"; }
        var_dump($z);"#,
    );
    assert_eq!(
        out,
        "Cannot write read-only property ZipArchive::$status\nint(-1)\nbool(true)\nbool(true)\n\
         Trying to clone an uncloneable object of class ZipArchive\n\
         object(ZipArchive)#1 (6) {\n  [\"lastId\"]=>\n  int(-1)\n  [\"status\"]=>\n  int(0)\n  \
         [\"statusSys\"]=>\n  int(0)\n  [\"numFiles\"]=>\n  int(0)\n  [\"filename\"]=>\n  \
         string(0) \"\"\n  [\"comment\"]=>\n  string(0) \"\"\n}\n"
    );
}

#[test]
fn uninitialized_object_throws() {
    let out = run(
        "uninit",
        r#"$z = new ZipArchive;
        try { $z->count(); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }
        try { $z->open(''); } catch (ValueError $e) { echo $e->getMessage(), "\n"; }"#,
    );
    assert_eq!(
        out,
        "Invalid or uninitialized Zip object\n\
         ZipArchive::open(): Argument #1 ($filename) must not be empty\n"
    );
}

#[test]
fn callbacks_run_on_close_and_exceptions_propagate() {
    let out = run(
        "callbacks",
        r#"$z = new ZipArchive;
        $z->open("$dir/c.zip", ZipArchive::CREATE);
        $z->addFromString('x', 'y');
        $z->registerProgressCallback(0.5, function ($r) { echo "progress $r\n"; });
        var_dump($z->close());
        $z->open("$dir/d.zip", ZipArchive::CREATE);
        $z->addFromString('x', 'y');
        $z->registerCancelCallback(function () { throw new Exception('stop'); });
        try { $z->close(); } catch (Exception $e) { echo $e->getMessage(), "\n"; }"#,
    );
    assert!(out.starts_with("progress 0\n"), "{out}");
    assert!(out.contains("progress 1\nbool(true)\n"), "{out}");
    assert!(out.ends_with("stop\n"), "{out}");
}

#[test]
fn streams_keep_the_archive_alive() {
    let out = run(
        "streams",
        r#"$z = new ZipArchive;
        $z->open("$dir/s.zip", ZipArchive::CREATE);
        $z->addFromString('e', 'entry data');
        $z->close();
        $z->open("$dir/s.zip");
        $s = $z->getStream('e');
        unset($z);
        echo get_resource_type($s), ' ', fread($s, 5), '|', stream_get_contents($s), "\n";
        fclose($s);
        echo file_get_contents("zip://$dir/s.zip#e"), "\n";"#,
    );
    assert_eq!(out, "stream entry| data\nentry data\n");
}

#[test]
fn procedural_api_is_deprecated() {
    let out = run(
        "procedural",
        r#"error_reporting(E_ALL & ~E_DEPRECATED);
        $z = new ZipArchive;
        $z->open("$dir/p.zip", ZipArchive::CREATE);
        $z->addFromString('n', 'abc');
        $z->close();
        $d = zip_open("$dir/p.zip");
        $e = zip_read($d);
        var_dump(zip_entry_name($e), zip_entry_filesize($e), zip_entry_read($e));
        var_dump(zip_entry_close($e));
        try { zip_entry_name($e); } catch (TypeError $t) { echo $t->getMessage(), "\n"; }
        zip_close($d);
        try { zip_read($d); } catch (TypeError $t) { echo $t->getMessage(), "\n"; }"#,
    );
    assert_eq!(
        out,
        "string(1) \"n\"\nint(3)\nstring(3) \"abc\"\nbool(true)\n\
         zip_entry_name(): supplied resource is not a valid Zip Entry resource\n\
         zip_read(): supplied resource is not a valid Zip Directory resource\n"
    );
}
