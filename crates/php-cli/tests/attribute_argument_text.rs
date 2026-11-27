// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ReflectionAttribute::__toString()` prints each argument as the constant
//! expression that was compiled (`zend_ast_export`): it needs neither the
//! source file, which code from `eval()` does not have, nor the argument's
//! value, which `new MissingClass` cannot have. Names are resolved in the
//! namespace the attribute is used in, with that file's imports. A
//! framework hashes this text to notice changed classes. Expected output
//! from `/usr/bin/php8.5 -n`.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn attribute_arguments_are_printed_not_evaluated() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("attribute_argument_text.php");
    std::fs::write(&path, SCRIPT).expect("write script");
    let out = Command::new(env!("CARGO_BIN_EXE_php"))
        .arg("-n")
        .arg(&path)
        .output()
        .expect("spawn php");
    let _ = std::fs::remove_file(&path);
    assert_eq!(String::from_utf8_lossy(&out.stdout), EXPECTED);
}

const SCRIPT: &str = r###"<?php
namespace App;

use Lib\Types as T;

#[\Attribute(\Attribute::TARGET_ALL)]
class Foo { public function __construct(...$a) {} }

function show(string $what, array $attributes): void {
    foreach ($attributes as $a) {
        echo $what, ': ', str_replace("\n", ' | ', trim((string) $a)), "\n";
    }
}

// Code from eval() has no source file to read again.
eval('namespace App; #[Foo(new MissingClass)] class FromEval {}');
show('eval', (new \ReflectionClass(FromEval::class))->getAttributes());
eval('namespace App; use Lib\Types as T; #[Foo(new T(Other::X), limit: new \Lib\Limit(5))] class FromEval2 {}');
show('eval, imported name', (new \ReflectionClass(FromEval2::class))->getAttributes());
eval('namespace Elsewhere; #[\App\Foo(new Local, \strlen(...))] function fromEval() {}');
show('eval, function in another namespace', (new \ReflectionFunction('Elsewhere\fromEval'))->getAttributes());
// The same eval() line twice, with different code each time.
foreach (['First', 'Second'] as $name) {
    eval("namespace App; #[Foo(new Missing$name)] class Twice$name {}");
}
show('eval, first of two', (new \ReflectionClass(TwiceFirst::class))->getAttributes());
show('eval, second of two', (new \ReflectionClass(TwiceSecond::class))->getAttributes());

#[Foo(new MissingClass(1, 'a'), T::C)]
class InFile {
    #[Foo(new T)] public $p;
    #[Foo(new MissingClass)] const K = 1;
    #[Foo(new MissingClass)] public function m(#[Foo(new T(2))] $x) {}
}
$c = new \ReflectionClass(InFile::class);
show('file, class', $c->getAttributes());
show('file, property', $c->getProperty('p')->getAttributes());
show('file, constant', $c->getReflectionConstant('K')->getAttributes());
show('file, method', $c->getMethod('m')->getAttributes());
show('file, parameter', $c->getMethod('m')->getParameters()[0]->getAttributes());
// Nothing was instantiated or autoloaded on the way.
var_dump(class_exists('App\MissingClass', false), class_exists('Lib\Types', false));
"###;

const EXPECTED: &str = r###"eval: Attribute [ App\Foo ] { |   - Arguments [1] { |     Argument #0 [ new \App\MissingClass() ] |   } | }
eval, imported name: Attribute [ App\Foo ] { |   - Arguments [2] { |     Argument #0 [ new \Lib\Types(\App\Other::X) ] |     Argument #1 [ limit = new \Lib\Limit(5) ] |   } | }
eval, function in another namespace: Attribute [ App\Foo ] { |   - Arguments [2] { |     Argument #0 [ new \Elsewhere\Local() ] |     Argument #1 [ \strlen(...) ] |   } | }
eval, first of two: Attribute [ App\Foo ] { |   - Arguments [1] { |     Argument #0 [ new \App\MissingFirst() ] |   } | }
eval, second of two: Attribute [ App\Foo ] { |   - Arguments [1] { |     Argument #0 [ new \App\MissingSecond() ] |   } | }
file, class: Attribute [ App\Foo ] { |   - Arguments [2] { |     Argument #0 [ new \App\MissingClass(1, 'a') ] |     Argument #1 [ \Lib\Types::C ] |   } | }
file, property: Attribute [ App\Foo ] { |   - Arguments [1] { |     Argument #0 [ new \Lib\Types() ] |   } | }
file, constant: Attribute [ App\Foo ] { |   - Arguments [1] { |     Argument #0 [ new \App\MissingClass() ] |   } | }
file, method: Attribute [ App\Foo ] { |   - Arguments [1] { |     Argument #0 [ new \App\MissingClass() ] |   } | }
file, parameter: Attribute [ App\Foo ] { |   - Arguments [1] { |     Argument #0 [ new \Lib\Types(2) ] |   } | }
bool(false)
bool(false)
"###;
