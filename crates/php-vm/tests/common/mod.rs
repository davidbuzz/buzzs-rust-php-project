// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A VM for script tests that call the standard library: the array, string,
//! JSON and preg functions and SPL's classes are registered with it, as in
//! the `php` binary (`crates/php-cli/src/extensions.rs` has the full list).

use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};
use std::io::Write;

/// A VM with `options`, writing to `out`, with ext/standard's array and
/// string functions, ext/json, ext/pcre and ext/spl.
#[must_use]
#[allow(dead_code)] // a test file may bring its own extension list
pub fn vm_with_stdlib(options: &VmOptions, out: Box<dyn Write>) -> Vm {
    let arrays = php_ext_stdarr::StdArray;
    let strings = php_ext_stdstr::StdStr;
    let json = php_ext_json::Json;
    let pcre = php_ext_pcre::Pcre;
    let spl = php_ext_spl::Spl;
    let exts: [&dyn Extension; 5] = [&arrays, &strings, &json, &pcre, &spl];
    Vm::with_extensions(options, out, &exts)
}
