// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! What a builtin writes to a by-reference argument that is a typed
//! property (or a variable bound to one): the write is checked against the
//! property's type and made when the builtin returns
//! (`zend_try_assign_typed_ref`, Zend/zend_API.c), on every call, not only
//! the first one at a call site: the calls below run in functions and in
//! loops, where the tape's native call makes them. A value of another type
//! is converted or refused there, with the `TypeError` raised by that call.
//! Expected output from `/usr/bin/php8.5 -n`.
mod common;

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

fn run(source: &str) -> String {
    let sink = Sink::default();
    let mut vm = common::vm_with_stdlib(&VmOptions::default(), Box::new(sink.clone()));
    let status = vm.run_script(source.as_bytes(), "t.php");
    drop(vm);
    let output = String::from_utf8(sink.0.borrow().clone()).expect("UTF-8 output");
    assert_eq!(status, 0, "{output}");
    output
}

#[test]
fn builtin_writes_to_typed_properties_happen_at_every_call() {
    assert_eq!(
        run(r###"<?php
class P {
    private array $m = [];
    private int $count = 0;
    private string $out = '';
    public ?int $ni = null;
    public string $s = '';
    public array $arr = [];
    public ?array $na = null;
    public $untyped;
    function run($s) {
        preg_match('/(\w)(\w)/', $s, $this->m);
        $this->out = str_replace('a', 'b', $s, $this->count);
        return $this->m[2] . $this->count . $this->out;
    }
}
function methods() { $p = new P; $o = []; foreach (['ab', 'ca', 'aa'] as $s) { $o[] = $p->run($s); } return implode(',', $o); }
function arrays() { $a = new P; $o = ''; for ($i = 0; $i < 3; $i++) { preg_match('/(\d+)/', "x$i", $a->arr); preg_match('/(\d+)/', "y$i", $a->na); preg_match('/(\d+)/', "z$i", $a->untyped); $o .= $a->arr[1] . $a->na[1] . $a->untyped[1] . ','; } return $o; }
function coerced() { $a = new P; $o = ''; for ($i = 0; $i < 3; $i++) { str_replace('a', 'b', str_repeat('a', $i + 2), $a->s); $o .= var_export($a->s, true) . ','; $a->s = ''; } return $o; }
function refused() { $a = new P; $o = ''; for ($i = 0; $i < 3; $i++) { try { preg_match('/(\d+)/', 'x12', $a->ni); $o .= 'no error,'; } catch (TypeError $e) { $o .= "caught $i: " . $e->getMessage() . ','; } $o .= var_export($a->ni, true) . ' '; } return $o; }
function through_a_variable() { $a = new P; $o = ''; $r = &$a->arr; for ($i = 0; $i < 3; $i++) { preg_match('/(\d+)/', "v$i", $r); $o .= $a->arr[1] . ','; } return $o; }
function then_another_call() { $a = new P; preg_match('/(\d)/', 'x1', $a->arr); preg_match('/(\d)/', 'x2', $a->arr); $n = strlen('abc'); $k = count($a->arr); return $a->arr[1] . $n . $k; }
echo methods(), "\n", arrays(), "\n", coerced(), "\n", refused(), "\n", through_a_variable(), "\n", then_another_call(), "\n";
"###),
        r###"b1bb,a1cb,a2bb
000,111,222,
'2','3','4',
caught 0: Cannot assign array to reference held by property P::$ni of type ?int,NULL caught 1: Cannot assign array to reference held by property P::$ni of type ?int,NULL caught 2: Cannot assign array to reference held by property P::$ni of type ?int,NULL 
0,1,2,
232
"###
    );
}

/// More property types under the same rule: `?array` and `string`
/// receiving preg_match()'s array, `float` and `bool` receiving
/// similar_text()'s percentage, a typed static property, a builtin that
/// takes its argument by reference as an array (sort()), and a variable
/// bound to a typed property.
#[test]
fn builtin_writes_coerce_or_refuse_for_each_property_type() {
    let line = r###"["Cannot assign array to reference held by property T::$s of type string","abc",1,["a","a"],2,100,2,true,"bb",2,"sort(): Argument #1 ($array) must be of type array, string given","abc",[1,3],1,100]"###;
    assert_eq!(
        run(r###"<?php
class T { public ?int $ni = null; public string $s = 'abc'; public float $f = 1.5; public ?array $a = null; public bool $b = false; public static int $si = 0; }
function f($i) {
  $t = new T; $o = [];
  try { $o[] = preg_match('/(a)/', 'xa', $t->s); } catch (TypeError $e) { $o[] = $e->getMessage(); } $o[] = $t->s;
  try { $o[] = preg_match('/(a)/', 'xa', $t->a); } catch (TypeError $e) { $o[] = $e->getMessage(); } $o[] = $t->a;
  try { $o[] = similar_text('ab', 'ab', $t->f); } catch (TypeError $e) { $o[] = $e->getMessage(); } $o[] = $t->f;
  try { $o[] = similar_text('ab', 'ab', $t->b); } catch (TypeError $e) { $o[] = $e->getMessage(); } $o[] = $t->b;
  try { $o[] = str_replace('a', 'b', 'aa', T::$si); } catch (TypeError $e) { $o[] = $e->getMessage(); } $o[] = T::$si;
  try { $o[] = sort($t->s); } catch (TypeError $e) { $o[] = $e->getMessage(); } $o[] = $t->s;
  $t->a = [3, 1]; sort($t->a); $o[] = $t->a;
  $r = &$t->ni; try { $o[] = similar_text('a', 'a', $r); } catch (TypeError $e) { $o[] = $e->getMessage(); } $o[] = $t->ni;
  return $o;
}
for ($i = 0; $i < 3; $i++) { echo json_encode(f($i)), "\n"; }
"###),
        format!("{line}\n{line}\n{line}\n")
    );
}
