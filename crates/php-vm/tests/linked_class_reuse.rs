// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Classes of a cached script are linked once and handed back in later
//! requests (php-src's inheritance cache, `zend_inheritance_cache_get` in
//! Zend/zend_inheritance.c), with nothing a program can see changed: every
//! request starts with fresh static properties, constants and static
//! variables, and a parent declared afresh links its subclasses afresh.
//!
//! The script cache here is a minimal stand-in for opcache: an included
//! file is compiled once and the same unit serves every later include, as
//! `opcache_precompiled_hook` serves it. Expected outputs from
//! `/usr/bin/php8.5 -d opcache.enable=1` through its built-in server, one
//! request after another.

use php_runtime::ZStr;
use php_vm::compiler::CompiledUnit;
use php_vm::include::CompileOutcome;
use php_vm::{Vm, VmOptions, VmResult};
use std::cell::RefCell;
use std::collections::HashMap;
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

thread_local! {
    static UNITS: RefCell<HashMap<Vec<u8>, Rc<CompiledUnit>>> = RefCell::new(HashMap::new());
}

fn compile_and_keep(
    vm: &mut Vm,
    src: Rc<[u8]>,
    filename: &ZStr,
    skip_shebang: bool,
) -> VmResult<CompileOutcome> {
    let out = vm.compile_source_default(src, filename, false, skip_shebang)?;
    if let CompileOutcome::Ok(unit) = &out {
        UNITS.with(|u| {
            u.borrow_mut()
                .insert(filename.as_bytes().to_vec(), unit.clone())
        });
    }
    Ok(out)
}

fn kept_unit(vm: &mut Vm, filename: &ZStr) -> VmResult<Option<Rc<CompiledUnit>>> {
    let Some(unit) = UNITS.with(|u| u.borrow().get(filename.as_bytes()).cloned()) else {
        return Ok(None);
    };
    unit.clear_runtime_caches();
    if !vm.install_cached_unit(&unit, filename) {
        return Ok(None);
    }
    Ok(Some(unit))
}

struct Tree(PathBuf);

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tree(name: &str, files: &[(&str, &str)]) -> Tree {
    let dir = std::env::temp_dir().join(format!(
        "php-vm-linked-class-reuse-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    for (f, body) in files {
        std::fs::write(dir.join(f), body).expect("write");
    }
    Tree(dir.canonicalize().expect("canonical temp dir"))
}

const BASE: &str = "<?php\nclass Base { public static $n = 0; public static $arr = [1]; const K = 'k1'; public static function bump() { return ++static::$n; } public function who() { return 'base1'; } }\ninterface Shape { const SIDES = 0; public function area(): float; }\n";

const CHILD: &str = "<?php\nrequire_once __DIR__ . '/base.php';\nclass Child extends Base implements Shape { public static $m = 10; const K2 = self::K . '+'; public function area(): float { return 1.5; } public function __toString(): string { return 'child'; } }\nclass Later extends Early { public static $z = 5; }\nclass Early { public function hi() { return 'early'; } }\nif (!function_exists('cond_fn')) { function cond_fn() { static $c = 0; return ++$c; } }\n";

fn index(dir: &std::path::Path) -> String {
    format!(
        "<?php\necho class_exists('Child', false) ? 'stale ' : 'fresh ';\nrequire_once '{}/child.php';\nBase::$arr[] = 2;\necho Base::bump(), Base::bump(), ' ', Child::$m++, Child::$m, ' ', Child::K2, ' ', (new Child)->who(), ' ', (string) new Child, ' ';\necho Later::$z++, Later::$z, ' ', (new Later)->hi(), ' ', cond_fn(), cond_fn(), ' ', count(Base::$arr), ' ';\necho Child::SIDES, ' ', (new Child) instanceof Stringable ? 'S' : '-', \"\\n\";\n",
        dir.display()
    )
}

fn class_addr(vm: &Vm, lc: &str) -> usize {
    vm.classes
        .get(&ZStr::from(lc))
        .map_or(0, |c| Rc::as_ptr(c) as usize)
}

#[test]
fn a_cached_scripts_classes_start_every_request_fresh() {
    Vm::set_compile_hook(compile_and_keep);
    Vm::set_precompiled_hook(kept_unit);
    let t = tree("fresh", &[("base.php", BASE), ("child.php", CHILD)]);
    let main = index(&t.0);
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    let mut addrs = Vec::new();
    for _ in 0..3 {
        sink.0.borrow_mut().clear();
        vm.run_script(main.as_bytes(), "index.php");
        assert_eq!(
            String::from_utf8_lossy(&sink.0.borrow()),
            "fresh 12 1011 k1+ base1 child 56 early 12 2 0 S\n"
        );
        addrs.push((class_addr(&vm, "base"), class_addr(&vm, "child")));
        vm.request_reset();
    }
    // Linked in the first request, handed back in the next two.
    assert_ne!(addrs[0].0, 0);
    assert_eq!(addrs[0], addrs[1]);
    assert_eq!(addrs[1], addrs[2]);

    // The parent's file changes and is compiled again (the cache drops
    // it, as opcache does on a newer modification time): the subclass of
    // the unchanged file is linked to the new parent.
    std::fs::write(t.0.join("base.php"), BASE.replace("base1", "base2")).expect("write");
    let base_path = t.0.join("base.php");
    UNITS.with(|u| {
        u.borrow_mut()
            .remove(base_path.to_string_lossy().as_bytes())
    });
    for _ in 0..2 {
        sink.0.borrow_mut().clear();
        vm.run_script(main.as_bytes(), "index.php");
        assert_eq!(
            String::from_utf8_lossy(&sink.0.borrow()),
            "fresh 12 1011 k1+ base2 child 56 early 12 2 0 S\n"
        );
        vm.request_reset();
    }
}

/// A class whose link depended on a class that is now another one (an
/// interface declared by a different file in each request) is linked
/// again, and one declared twice in a request is never handed out twice.
#[test]
fn a_kept_link_is_not_reused_when_what_it_looked_up_changed() {
    Vm::set_compile_hook(compile_and_keep);
    Vm::set_precompiled_hook(kept_unit);
    let t = tree(
        "deps",
        &[
            ("i1.php", "<?php interface I { function f(); }\n"),
            (
                "i2.php",
                "<?php interface I { function f(); function g(); }\n",
            ),
            (
                "c.php",
                "<?php abstract class C implements I { public static $s = 0; }\n",
            ),
        ],
    );
    let run = |vm: &mut Vm, sink: &Sink, iface: &str| {
        sink.0.borrow_mut().clear();
        let main = format!(
            "<?php\nrequire '{d}/{iface}';\nrequire '{d}/c.php';\nC::$s++; echo C::$s, ' ', implode(',', get_class_methods('C')), \"\\n\";\n",
            d = t.0.display()
        );
        vm.run_script(main.as_bytes(), "index.php");
        let out = String::from_utf8_lossy(&sink.0.borrow()).into_owned();
        vm.request_reset();
        out
    };
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    assert_eq!(run(&mut vm, &sink, "i1.php"), "1 f\n");
    assert_eq!(run(&mut vm, &sink, "i2.php"), "1 f,g\n");
    assert_eq!(run(&mut vm, &sink, "i2.php"), "1 f,g\n");
    assert_eq!(run(&mut vm, &sink, "i1.php"), "1 f\n");
}

/// The constant expressions of a cached script carry no request state
/// either: their run-time caches are cleared with the code's, so a class
/// constant read from a static property's default, a property's default,
/// a parameter's default or an attribute argument resolves its class in
/// each request again (autoloading it there), and the constant's private
/// constants are read from its own class. A cache kept from the first
/// request answered with that request's class, never declared in the
/// next: "Cannot access private constant self::A".
#[test]
fn constant_expressions_of_a_cached_script_resolve_classes_every_request() {
    Vm::set_compile_hook(compile_and_keep);
    Vm::set_precompiled_hook(kept_unit);
    let t = tree(
        "constexpr",
        &[
            (
                "Names.php",
                "<?php\nnamespace C;\nfinal class Names { private const A = ['a' => 1]; private const B = ['b' => 2]; public const LIST = self::A + self::B; }\n",
            ),
            (
                "Reader.php",
                "<?php\nnamespace C;\n#[\\Attribute] class Tag { public function __construct(public $v) {} }\n#[Tag(Names::LIST)] class Reader { private static $names = Names::LIST; public $copy = Names::LIST; public static function get() { return self::$names; } public function pick($x = Names::LIST) { return $x; } }\n",
            ),
        ],
    );
    let main = format!(
        "<?php\nspl_autoload_register(function ($c) {{ echo \"load($c) \"; @include '{}/' . substr($c, 2) . '.php'; }});\ntry {{ $r = new C\\Reader; echo count(C\\Reader::get()), count($r->copy), count($r->pick()), count((new ReflectionClass($r))->getAttributes()[0]->newInstance()->v), \"\\n\"; }} catch (Throwable $e) {{ echo get_class($e), ': ', $e->getMessage(), \"\\n\"; }}\n",
        t.0.display()
    );
    let sink = Sink::default();
    let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
    for _ in 0..3 {
        sink.0.borrow_mut().clear();
        vm.run_script(main.as_bytes(), "index.php");
        assert_eq!(
            String::from_utf8_lossy(&sink.0.borrow()),
            "load(C\\Reader) load(C\\Names) 2222\n"
        );
        vm.request_reset();
    }
}
