// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The file name an included file is compiled under: its opened path, as
//! `open_file_for_scanning` takes `file_handle->opened_path` when there is
//! one. For a plain file that is the real path `php_resolve_path()`
//! (`tsrm_realpath`) resolved, so neither `..` segments nor symbolic links
//! show in `__FILE__`, `__DIR__`, `getFileName()`, backtraces or messages,
//! and `get_included_files()` lists the same paths. Expected outputs follow
//! php 8.3 (`/usr/bin/php -n`) on the same trees.

use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
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

/// A directory tree for one test, removed again on drop. Its root is a
/// real path, so the expected paths are built from it directly.
struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "phprust-include-paths-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("temp dir");
        Tree(std::fs::canonicalize(&base).expect("real temp dir"))
    }

    /// Writes `contents` to `rel` (creating its directory).
    fn file(&self, rel: &str, contents: &str) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, contents).expect("file");
    }

    /// Makes `rel` a symbolic link to `target` (relative to `rel`'s directory).
    fn link(&self, rel: &str, target: &str) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::os::unix::fs::symlink(target, &path).expect("symlink");
    }

    /// The absolute spelling of `rel` inside the tree, as given (nothing
    /// resolved).
    fn path(&self, rel: &str) -> String {
        format!("{}/{rel}", self.0.display())
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs the script at `path` (written there too, so that `..` segments
/// through its directory resolve); returns the output and the paths the
/// VM recorded for `get_included_files()`.
fn run(opts: &VmOptions, tree: &Tree, rel: &str, source: &str) -> (String, Vec<String>) {
    tree.file(rel, source);
    let sink = Sink::default();
    let mut vm = Vm::new(opts, Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), &tree.path(rel));
    let included = vm.included.iter().map(|f| f.to_string_lossy()).collect();
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    (output, included)
}

#[test]
fn included_files_are_compiled_under_their_real_path() {
    let t = Tree::new("real");
    t.file(
        "lib/inc.php",
        "<?php\n\
         echo __FILE__, \"\\n\", __DIR__, \"\\n\";\n\
         $e = new Exception('x');\n\
         echo $e->getFile(), ':', $e->getLine(), \"\\n\";\n\
         class IncPathsC { static function trace() { return inc_paths_inner(); } }\n\
         function inc_paths_inner() { return debug_backtrace(); }\n",
    );
    t.file("real/target.php", "<?php echo __FILE__, \"\\n\";\n");
    t.link("lib/link.php", "../real/target.php");
    t.link("alias", "real");
    t.file("lib/once.php", "<?php function inc_paths_f() {}\n");
    t.file("ip/inip.php", "<?php echo __FILE__, \"\\n\";\n");
    let script = format!(
        "<?php\n\
         include '{bin}/../lib/inc.php';\n\
         include '{lib}/link.php';\n\
         include '{root}/alias/target.php';\n\
         include_once '{bin}/../lib/once.php';\n\
         var_dump(include_once '{lib}/../lib/once.php');\n\
         ini_set('include_path', '{bin}/../ip');\n\
         include 'inip.php';\n\
         echo (new ReflectionClass('IncPathsC'))->getFileName(), \"\\n\";\n\
         echo (new ReflectionFunction('inc_paths_f'))->getFileName(), \"\\n\";\n\
         echo IncPathsC::trace()[0]['file'], \"\\n\";\n",
        bin = t.path("bin"),
        lib = t.path("lib"),
        root = t.0.display(),
    );
    let (out, included) = run(&VmOptions::default(), &t, "bin/main.php", &script);
    let (inc, lib, target) = (
        t.path("lib/inc.php"),
        t.path("lib"),
        t.path("real/target.php"),
    );
    let (once, inip) = (t.path("lib/once.php"), t.path("ip/inip.php"));
    assert_eq!(
        out,
        format!(
            "{inc}\n{lib}\n{inc}:3\n{target}\n{target}\nbool(true)\n{inip}\n\
             {inc}\n{once}\n{inc}\n"
        )
    );
    assert_eq!(included, [inc, target, once, inip]);
}

#[test]
fn prepended_files_are_compiled_under_their_real_path() {
    let t = Tree::new("prepend");
    t.file("lib/pre.php", "<?php echo __FILE__, \"\\n\";\n");
    std::fs::create_dir_all(t.0.join("bin")).expect("dir");
    let opts = VmOptions {
        ini: vec![(
            "auto_prepend_file".to_string(),
            t.path("bin/../lib/pre.php"),
        )],
    };
    let (out, _) = run(&opts, &t, "main.php", "<?php echo __FILE__, \"\\n\";\n");
    assert_eq!(
        out,
        format!("{}\n{}\n", t.path("lib/pre.php"), t.path("main.php"))
    );
}
