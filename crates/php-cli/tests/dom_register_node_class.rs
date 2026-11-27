// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `DOMDocument::registerNodeClass()` and `Dom\Document::registerNodeClass()`
//! take their two classes as `C` parameters (`zend_parse_parameters(...,
//! "CC!", ...)`, ext/dom/document.c): each name is looked up with
//! `zend_lookup_class`, so the autoloader runs for a class that is not
//! declared yet, a leading backslash is accepted, and what the autoloader
//! throws is what the call throws. A wiki engine's parser registers its
//! node classes from the constructor of its document class, before any of
//! them is loaded: without the autoload every page it parsed ended in a
//! `TypeError`. The messages: an unknown or unrelated base class and an
//! unknown extended class are the parameter's `TypeError`s, an extended
//! class that does not derive from the base is an `Error`
//! (`zend_argument_error(NULL, ...)`). Expected output from
//! `/usr/bin/php8.5 -n -d extension=dom`, except the four `TypeError`
//! messages, which are php-src master's (`zend_wrong_class_name_error`,
//! Zend/zend_API.c): 8.5 says `derived from DOMNode, Nope given` and `a valid
//! class name or null, NoSuchClassXyz given` where master says `derived from
//! DOMNode, "Nope" given` and `a valid class name or null, string given`
//! (ext/dom/tests/modern/html/interactions/HTMLDocument_registerNodeClass_01.phpt
//! pins the first).

use std::path::PathBuf;
use std::process::Command;

#[test]
fn the_classes_are_looked_up_as_class_parameters_are() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("dom_register_node_class.php");
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
spl_autoload_register(function ($c) {
    echo "(autoload $c)\n";
    if ($c === 'LazyAttr') { eval('class LazyAttr extends DOMAttr {}'); }
    if ($c === 'LazyBase') { eval('class LazyBase extends DOMElement {}'); }
    if ($c === 'LazyEl') { eval('class LazyEl extends LazyBase {}'); }
    if ($c === 'Thrower') { throw new RuntimeException('from the autoloader'); }
    if ($c === 'LazyModern') { eval('class LazyModern extends Dom\Element {}'); }
});
function t(string $what, callable $f) {
    try { var_dump($f()); echo "$what: ok\n"; }
    catch (Throwable $e) { echo "$what: ", get_class($e), ": ", $e->getMessage(), "\n"; }
}
$d = new DOMDocument();
t('unknown base', fn () => $d->registerNodeClass('Nope', null));
t('base not a node class', fn () => $d->registerNodeClass('stdClass', null));
t('unknown extended', fn () => $d->registerNodeClass('DOMAttr', 'NoSuchClassXyz'));
t('extended not derived', fn () => $d->registerNodeClass('DOMAttr', 'stdClass'));
t('extended autoloaded', fn () => $d->registerNodeClass('DOMAttr', 'LazyAttr'));
t('base autoloaded', fn () => $d->registerNodeClass('LazyBase', 'LazyEl'));
t('autoloader throws', fn () => $d->registerNodeClass('DOMAttr', 'Thrower'));
t('leading backslash', fn () => $d->registerNodeClass('\DOMAttr', '\LazyAttr'));
$m = Dom\XMLDocument::createEmpty();
t('modern, autoloaded', fn () => $m->registerNodeClass('Dom\Element', 'LazyModern'));
t('modern, unknown', fn () => $m->registerNodeClass('Dom\Element', 'Nope2'));
$d->loadXML('<r a="1"/>');
echo get_class($d->documentElement->getAttributeNode('a')), "\n";
"###;

const EXPECTED: &str = r###"(autoload Nope)
unknown base: TypeError: DOMDocument::registerNodeClass(): Argument #1 ($baseClass) must be a class name derived from DOMNode, "Nope" given
base not a node class: TypeError: DOMDocument::registerNodeClass(): Argument #1 ($baseClass) must be a class name derived from DOMNode, "stdClass" given
(autoload NoSuchClassXyz)
unknown extended: TypeError: DOMDocument::registerNodeClass(): Argument #2 ($extendedClass) must be a valid class name or null, string given
extended not derived: Error: DOMDocument::registerNodeClass(): Argument #2 ($extendedClass) must be a class name derived from DOMAttr or null, stdClass given
(autoload LazyAttr)
bool(true)
extended autoloaded: ok
(autoload LazyBase)
(autoload LazyEl)
bool(true)
base autoloaded: ok
(autoload Thrower)
autoloader throws: RuntimeException: from the autoloader
bool(true)
leading backslash: ok
(autoload LazyModern)
NULL
modern, autoloaded: ok
(autoload Nope2)
modern, unknown: TypeError: Dom\Document::registerNodeClass(): Argument #2 ($extendedClass) must be a valid class name or null, string given
LazyAttr
"###;
