// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Object lifetime of SPL objects: the filesystem objects' `dtor_obj`
//! closes their stream, and a child iterator released by
//! `RecursiveIteratorIterator` is destructed at once.

use php_vm::ext::Extension;
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

fn run(code: &str) -> (i32, String) {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![("error_reporting".into(), "32767".into())],
    };
    // The stream layer (`php://temp`, directory streams) as in the CLI.
    let streams = php_ext_streams::Streams;
    let spl = php_ext_spl::Spl;
    let arrays = php_ext_stdarr::StdArray;
    let exts: [&dyn Extension; 3] = [&streams, &spl, &arrays];
    let mut vm = Vm::with_extensions(&options, Box::new(sink.clone()), &exts);
    let status = vm.run_script(code.as_bytes(), "test.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    (status, output)
}

#[test]
fn shutdown_destructs_closes_streams_in_handle_order() {
    // bug #79710: objects alive at shutdown are destructed in creation
    // order; a file or directory object destructed before its user sees
    // "Object not initialized".
    let (status, output) = run(r#"<?php
class Target {
    public function __construct(public $it) {}
    public function __destruct() {
        try {
            $this->it instanceof SplFileObject ? $this->it->flock(1) : $this->it->valid();
            echo get_class($this->it), ": open\n";
        } catch (Error $e) {
            echo get_class($this->it), ": ", $e->getMessage(), "\n";
        }
    }
}
class Run { static $file; static $dir; static $t1; static $t2; }
Run::$file = new SplTempFileObject();
Run::$t1 = new Target(Run::$file);
Run::$dir = new DirectoryIterator(".");
Run::$t2 = new Target(Run::$dir);
$local = new SplTempFileObject();
$keep = new Target($local);
echo "end\n";
"#);
    assert_eq!(status, 0);
    assert_eq!(
        output,
        "end\nSplTempFileObject: open\nSplTempFileObject: Object not initialized\nDirectoryIterator: Object not initialized\n"
    );
}

#[test]
fn user_destructor_runs_before_the_stream_is_closed() {
    let (status, output) = run(r#"<?php
class F extends SplTempFileObject {
    public function __destruct() {
        $this->fwrite("x");
        $this->rewind();
        echo "dtor: ", $this->fgets(), "\n";
    }
}
$f = new F();
$f = null;
echo "end\n";
"#);
    assert_eq!(status, 0);
    assert_eq!(output, "dtor: x\nend\n");
}

#[test]
fn released_child_iterator_destructs_before_the_parent() {
    // bug #73423: the child's exception ends the walk; the parent, freed
    // while it unwinds, chains its own exception to it.
    let (status, output) = run(r#"<?php
class Node implements RecursiveIterator {
    public $items = [];
    public function current(): mixed { return current($this->items); }
    public function key(): mixed { return key($this->items); }
    public function next(): void { next($this->items); }
    public function rewind(): void { reset($this->items); }
    public function valid(): bool { return current($this->items) !== false; }
    public function getChildren(): ?RecursiveIterator { return current($this->items); }
    public function hasChildren(): bool { return (bool) count($this->items); }
}
class NodeFilter extends RecursiveFilterIterator {
    public function __destruct() { throw new Exception("d" . ++$GLOBALS['n']); }
    public function accept(): bool { return true; }
}
$n = 0;
$root = new Node();
$root->items[] = new Node();
function walk($root) {
    foreach (new RecursiveIteratorIterator(new NodeFilter($root)) as $leaf);
}
try {
    walk($root);
} catch (Exception $e) {
    echo $e->getMessage(), " <- ", $e->getPrevious()?->getMessage(), "\n";
}
echo "done\n";
"#);
    assert_eq!(status, 0);
    assert_eq!(output, "d2 <- d1\ndone\n");
}
