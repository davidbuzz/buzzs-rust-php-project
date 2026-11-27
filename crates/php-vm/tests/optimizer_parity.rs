// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Scripts run with opcache's optimizer off and on (`opcache.enable_cli`,
//! the default `opcache.optimization_level`) print the same: a call or an
//! operation evaluated at compile time gives exactly what the run-time one
//! gives, diagnostics included, or is left to run time (Zend/Optimizer/
//! sccp.c `ct_eval_func_call` discards an evaluation that warned or
//! threw; `zend_try_inline_call` never inlines a function with a return
//! type). Expected output from `/usr/bin/php8.5 -n`, except where php-src
//! master differs (noted at the script).

mod common;

use php_support::ini::{Access, IniDirective, ModifyHandler};
use php_vm::ext::{Extension, ModuleBuilder};
use php_vm::VmOptions;
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

/// The INI entries of ext/opcache that decide whether (and with which
/// passes) the VM optimizes a script, with their php-src defaults; the
/// opcache extension itself (php-ext-opcache) depends on this crate.
struct OpcacheSwitches;

impl Extension for OpcacheSwitches {
    fn name(&self) -> &'static str {
        "opcache switches"
    }

    fn register(&self, m: &mut ModuleBuilder<'_>) {
        let d = |name: &'static str, val: &'static str, handler: ModifyHandler| {
            IniDirective::new(name, Some(val), Access::SYSTEM, handler, "opcache")
        };
        m.ini(d("opcache.enable", "1", ModifyHandler::Bool))
            .ini(d("opcache.enable_cli", "0", ModifyHandler::Bool))
            .ini(d("opcache.file_cache_only", "0", ModifyHandler::Bool))
            .ini(d(
                "opcache.optimization_level",
                "0x7FFEBFFF",
                ModifyHandler::Long,
            ));
    }
}

/// Runs `source` as `t.php` with the optimizer on or off.
fn run(source: &str, optimize: bool) -> String {
    let sink = Sink::default();
    let options = VmOptions {
        ini: vec![
            ("fatal_error_backtraces".to_string(), "0".to_string()),
            ("error_reporting".to_string(), "-1".to_string()),
            (
                "opcache.enable_cli".to_string(),
                if optimize { "1" } else { "0" }.to_string(),
            ),
        ],
    };
    let arrays = php_ext_stdarr::StdArray;
    let strings = php_ext_stdstr::StdStr;
    let switches = OpcacheSwitches;
    let exts: [&dyn Extension; 3] = [&arrays, &strings, &switches];
    let mut vm = php_vm::Vm::with_extensions(&options, Box::new(sink.clone()), &exts);
    vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = sink.0.borrow().clone();
    String::from_utf8(output).expect("UTF-8 output")
}

/// `source` prints `expected` with the optimizer off and with it on.
#[track_caller]
fn check(source: &str, expected: &str) {
    assert_eq!(run(source, false), expected, "optimizer off");
    assert_eq!(run(source, true), expected, "optimizer on");
}

/// The prologue of the scripts: every diagnostic becomes an exception
/// whose message the script prints.
const THROWING_HANDLER: &str = "<?php
set_error_handler(function ($no, $msg) { throw new Exception($msg); });
";

/// strlen_deprecation_to_exception.phpt, chr_out_of_range.phpt,
/// ord_not_1_byte.phpt: the deprecations of these calls of literals are
/// raised (and an error handler can turn them into exceptions).
#[test]
fn deprecations_of_literal_calls_are_raised() {
    let src = format!(
        "{THROWING_HANDLER}
try {{ strlen(null); }} catch (Exception $e) {{ echo $e->getMessage(), \"\\n\"; }}
try {{ var_dump(chr(256)); }} catch (Exception $e) {{ echo $e->getMessage(), \"\\n\"; }}
try {{ var_dump(ord('')); }} catch (Exception $e) {{ echo $e->getMessage(), \"\\n\"; }}
try {{ var_dump(ord('Hello')); }} catch (Exception $e) {{ echo $e->getMessage(), \"\\n\"; }}
var_dump(chr(65), ord('A'), strlen('abc'));
"
    );
    check(
        &src,
        "strlen(): Passing null to parameter #1 ($string) of type string is deprecated
chr(): Providing a value not in-between 0 and 255 is deprecated, this is because a byte value must be in the [0, 255] interval. The value used will be constrained using % 256
ord(): Providing an empty string is deprecated
ord(): Providing a string that is not one byte long is deprecated. Use ord($str[0]) instead
string(1) \"A\"
int(65)
int(3)
",
    );
}

/// is_float_alias.phpt, is_int_aliases.phpt: the aliases deprecated since
/// 8.6 (php-src master; php8.5 does not deprecate them) deprecate every
/// call, a call of a literal included.
#[test]
fn deprecated_aliases_are_called() {
    let src = format!(
        "{THROWING_HANDLER}
try {{ var_dump(is_double(1.5)); }} catch (Exception $e) {{ echo $e->getMessage(), \"\\n\"; }}
try {{ var_dump(is_integer(1)); }} catch (Exception $e) {{ echo $e->getMessage(), \"\\n\"; }}
try {{ var_dump(is_long(1)); }} catch (Exception $e) {{ echo $e->getMessage(), \"\\n\"; }}
try {{ var_dump(doubleval('1')); }} catch (Exception $e) {{ echo $e->getMessage(), \"\\n\"; }}
var_dump(is_float(1.5), is_int(1), floatval('1'));
"
    );
    check(
        &src,
        "Function is_double() is deprecated since 8.6, use is_float() instead
Function is_integer() is deprecated since 8.6, use is_int() instead
Function is_long() is deprecated since 8.6, use is_int() instead
Function doubleval() is deprecated since 8.6, use floatval() instead
bool(true)
bool(true)
float(1)
",
    );
}

/// trim.phpt: the default list of trim(), ltrim(), rtrim() and chop() is
/// php-src master's " \f\t\n\r\v\0" (php8.5 leaves the form feed).
/// min()/max() of equal values of different types return one of them by
/// position, which a folded call must not change.
#[test]
fn string_and_math_calls_of_literals() {
    check(
        "<?php
var_dump(trim(\"\\x0c x \\x0c\") === 'x', ltrim(\"\\x0cx\") === 'x', rtrim(\"x\\x0c\") === 'x', chop(\"x\\x0c\\0\") === 'x');
var_dump(min(1, '1'), max('1', 1), min(1.0, 1), min(3, 1, 2), max(-0.0, 0.0));
var_dump(round(2.5), round(-0.5), floor(5), sqrt(16), abs(PHP_INT_MIN), abs(-3));
var_dump(str_repeat('ab', 3), strlen(str_repeat('ab', 40000)), hex2bin('4142'));
",
        "bool(true)
bool(true)
bool(true)
bool(true)
string(1) \"1\"
string(1) \"1\"
int(1)
int(1)
float(-0)
float(3)
float(-1)
float(5)
float(4)
float(9.223372036854776E+18)
int(3)
string(6) \"ababab\"
int(80000)
string(2) \"AB\"
",
    );
}

/// or_null.phpt, iterable/or_null.phpt, mixed_return_*_error.phpt: a
/// function with a return type and no `return` throws when called; only
/// an untyped or `void` function's call is null.
#[test]
fn typed_functions_without_return_are_not_inlined() {
    check(
        "<?php
function a(): ?int {}
function b(): mixed {}
function c(): iterable {}
function d(): void {}
function e() {}
function f(): never {}
try { var_dump(a()); } catch (TypeError $t) { echo $t->getMessage(), \"\\n\"; }
try { var_dump(b()); } catch (TypeError $t) { echo $t->getMessage(), \"\\n\"; }
try { var_dump(c()); } catch (TypeError $t) { echo $t->getMessage(), \"\\n\"; }
try { f(); } catch (TypeError $t) { echo $t->getMessage(), \"\\n\"; }
var_dump(d(), e());
",
        "a(): Return value must be of type ?int, none returned
b(): Return value must be of type mixed, none returned
c(): Return value must be of type Traversable|array, none returned
f(): never-returning function must not implicitly return
NULL
NULL
",
    );
}

/// An arrow function binds every variable its body names from the scope
/// that makes the closure (`find_implicit_binds`), so none of them is the
/// undefined local a function's other variables are at entry: `$key ?? 0`
/// is the captured `$key`. (A framework's configuration builder wraps a
/// scalar as `static fn ($v) => [$key ?? 0 => $v]`; folded to `[0 => $v]`,
/// every such option was refused as `Unrecognized option "0"`.)
#[test]
fn arrow_functions_see_the_variables_they_capture() {
    check(
        r"<?php
function wrap(array $types, ?string $key = null): array {
    $out = [];
    foreach ($types as $type) {
        $out[] = static fn ($v) => [$key ?? 0 => $v];
    }
    return $out;
}
var_dump(wrap(['string'], 'algorithm')[0]('auto'), wrap(['bool'])[0](true));
function outer(?string $key, $nul = null) {
    return [
        'coalesce' => fn () => $key ?? 'none',
        'coalesce of null' => fn () => $nul ?? 'none',
        'coalesce assign' => fn () => $key ??= 'assigned',
        'nested' => fn () => (fn () => $key ?? 'none')(),
        'default' => fn ($a = 1) => $key ?? $a,
        'variadic' => fn (...$r) => $key ?? $r,
        'typed' => fn (int $i = 3): string => ($key ?? 'none') . $i,
        'cast' => fn () => (int) ($key ?? 7),
        'not in the enclosing scope' => fn () => $nowhere ?? 'undefined',
        'generator' => fn () => yield $key ?? 'none',
    ];
}
foreach (outer('K') as $what => $f) {
    $r = $f();
    echo $what, ': ';
    var_dump($r instanceof Generator ? $r->current() : $r);
}
class C {
    private $x = 'px';
    function f(?string $k) {
        return [fn () => $k ?? 'none', static fn () => $k ?? 'none', fn () => $this->x ?? 'none'];
    }
}
foreach ((new C)->f('ck') as $f) {
    var_dump($f());
}
$g = 'global';
var_dump((fn () => $g ?? 'none')());
// A local of an ordinary function that nothing assigned is known undefined.
function plain($a = 1) { return $u ?? $a; }
var_dump(plain());
",
        r#"array(1) {
  ["algorithm"]=>
  string(4) "auto"
}
array(1) {
  [0]=>
  bool(true)
}
coalesce: string(1) "K"
coalesce of null: string(4) "none"
coalesce assign: string(1) "K"
nested: string(1) "K"
default: string(1) "K"
variadic: string(1) "K"
typed: string(2) "K3"
cast: int(0)
not in the enclosing scope: string(9) "undefined"
generator: string(1) "K"
string(2) "ck"
string(2) "ck"
string(2) "px"
string(6) "global"
int(1)
"#,
    );
}

/// The closure of an arrow function holds every variable its body names
/// for as long as it lives, one that only a dead branch reads or that the
/// body only assigns included: reflection lists it, and an object it holds
/// is destroyed with the closure, not when the enclosing function returns.
#[test]
fn arrow_functions_keep_the_variables_they_capture() {
    check(
        r#"<?php
class D {
    public function __construct(public string $n) {}
    public function __destruct() { echo "destruct {$this->n}\n"; }
}
function dead_branch() { $o = new D('dead branch'); return fn () => false ? $o : 1; }
function assigned_only() { $o = new D('assigned only'); return fn () => ($o = 3) + 1; }
foreach (['dead_branch', 'assigned_only'] as $make) {
    $f = $make();
    echo "made\n";
    var_dump(array_keys((new ReflectionFunction($f))->getClosureUsedVariables()), $f());
    unset($f);
    echo "released\n";
}
"#,
        r#"made
array(1) {
  [0]=>
  string(1) "o"
}
int(1)
destruct dead branch
released
made
array(1) {
  [0]=>
  string(1) "o"
}
int(4)
destruct assigned only
released
"#,
    );
}
