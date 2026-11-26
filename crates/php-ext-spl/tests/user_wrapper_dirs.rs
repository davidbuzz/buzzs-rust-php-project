// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Directory iterators over a user stream wrapper, stepped without a
//! method call: by `foreach`, by an iterator built around them and by
//! `iterator_count()`/`iterator_to_array()`. Each step reads the directory
//! through the wrapper's `dir_readdir()`/`dir_rewinddir()`, so it needs
//! the VM lent to the stream layer (`php_ext_streams::bridge`); without
//! it the directory read as empty (seen with vfsStream under Drupal's
//! `RegexDirectoryIterator` and `FileStorage::deleteAll()`).
//! Expected output: PHP 8.5.

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

fn run(code: &str) -> String {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![("error_reporting".into(), "32767".into())],
    };
    let mut vm = Vm::with_extensions(
        &options,
        Box::new(sink.clone()),
        &[
            &php_ext_streams::Streams,
            &php_ext_spl::Spl,
            &php_ext_stdarr::StdArray,
            &php_ext_stdstr::StdStr,
        ],
    );
    vm.run_script(code.as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8(out).expect("UTF-8 output")
}

#[test]
fn directory_iterators_read_a_user_wrapper_when_stepped_natively() {
    let out = run(r#"<?php
class W {
    public $context;
    private $i;
    function dir_opendir($p, $o) { $this->i = 0; return true; }
    function dir_readdir() { return ['a.yml', 'b', 'c.txt'][$this->i++] ?? false; }
    function dir_rewinddir() { $this->i = 0; return true; }
    function dir_closedir() { return true; }
    function url_stat($p, $f) {
        return ['mode' => substr($p, -2) === '/b' ? 040755 : 0100644, 'size' => 3];
    }
}
stream_wrapper_register('wr', 'W');
function names($it) { $o = []; foreach ($it as $k => $f) { $o[] = $f->getFilename(); } return implode(',', $o); }
echo names(new DirectoryIterator('wr://dir')), "\n";
foreach (new FilesystemIterator('wr://dir') as $k => $f) { echo $k, ' ', $f->isFile() ? 'file' : 'dir', "\n"; }
echo names(new IteratorIterator(new DirectoryIterator('wr://dir'))), "\n";
echo names(new LimitIterator(new FilesystemIterator('wr://dir'), 1, 2)), "\n";
$a = new AppendIterator(); $a->append(new DirectoryIterator('wr://dir')); echo names($a), "\n";
$x = new IteratorIterator(new DirectoryIterator('wr://dir')); $x->rewind(); $x->next(); echo $x->current()->getFilename(), "\n";
echo iterator_count(new FilesystemIterator('wr://dir')), ' ', count(iterator_to_array(new FilesystemIterator('wr://dir'))), "\n";
"#);
    assert_eq!(
        out,
        r"a.yml,b,c.txt
wr://dir/a.yml file
wr://dir/b dir
wr://dir/c.txt file
a.yml,b,c.txt
b,c.txt
a.yml,b,c.txt
b
3 3
"
    );
}
