//! `is_uploaded_file()` / `move_uploaded_file()` (ext/standard) accept only
//! the paths in the request's `SG(rfc1867_uploaded_files)`
//! (`Vm::rfc1867_uploaded_files`), whichever SAPI filled it.

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

/// The VM interrupt flag is process-wide: this binary's VMs run one at a
/// time.
static VM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run(code: &str, uploads: Vec<PathBuf>) -> (String, Vec<PathBuf>) {
    let _serial = VM_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sink = Sink::default();
    let streams = php_ext_streams::Streams;
    let exts: [&dyn Extension; 1] = [&streams];
    let opts = VmOptions { ini: vec![] };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.rfc1867_uploaded_files = uploads;
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    let left = std::mem::take(&mut vm.rfc1867_uploaded_files);
    drop(vm);
    let out = sink.0.borrow().clone();
    (String::from_utf8_lossy(&out).into_owned(), left)
}

#[test]
fn only_uploads_are_uploaded_files() {
    let dir = std::env::temp_dir().join(format!("phprust-uploads-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let up = dir.join("php_upload");
    std::fs::write(&up, b"data").unwrap();
    let other = dir.join("other");
    std::fs::write(&other, b"x").unwrap();
    let dest = dir.join("moved");
    let code = format!(
        r#"
var_dump(is_uploaded_file('{up}'), is_uploaded_file('{other}'));
try {{ is_uploaded_file("a\0b"); }} catch (ValueError $e) {{ echo $e->getMessage(), "\n"; }}
var_dump(move_uploaded_file('{other}', '{dest}'));
var_dump(move_uploaded_file('{up}', '{dest}'), file_get_contents('{dest}'));
var_dump(is_uploaded_file('{up}'), move_uploaded_file('{up}', '{dest}'));
"#,
        up = up.display(),
        other = other.display(),
        dest = dest.display(),
    );
    let (out, left) = run(&code, vec![up.clone()]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        out,
        "bool(true)\nbool(false)\n\
         is_uploaded_file(): Argument #1 ($filename) must not contain any null bytes\n\
         bool(false)\nbool(true)\nstring(4) \"data\"\nbool(false)\nbool(false)\n"
    );
    assert!(left.is_empty());
}

#[test]
fn no_uploads_under_the_cli() {
    let (out, _) = run(
        "var_dump(is_uploaded_file('/etc/passwd'), move_uploaded_file('/etc/passwd', '/nonexistent/x'));",
        Vec::new(),
    );
    assert_eq!(out, "bool(false)\nbool(false)\n");
}
