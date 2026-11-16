// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `zend_test_compile_string()`: `compile_string()` + `zend_execute()`
//! of a source string under a given file name, in the global scope.

use php_runtime::{Value, ZStr};
use php_vm::compiler::{compile, CompileOptions};
use php_vm::ext::{Args, VmResult};
use php_vm::vm::{Frame, FrameKind, Throw};
use php_vm::Vm;
use std::rc::Rc;

/// `ZEND_COMPILE_POSITION_AT_SHEBANG`.
const AT_SHEBANG: i64 = 0;
/// `ZEND_COMPILE_POSITION_AFTER_OPEN_TAG`.
const AFTER_OPEN_TAG: i64 = 2;

/// The source as the scanner sees it at `position`.
fn source_at(src: &[u8], position: i64) -> Vec<u8> {
    if position == AFTER_OPEN_TAG {
        let mut v = b"<?php ".to_vec();
        v.extend_from_slice(src);
        v
    } else {
        src.to_vec()
    }
}

/// `zend_test_compile_string(string $source_string, string $filename, int $position)`.
pub(crate) fn compile_string(vm: &mut Vm, a: &mut Args) -> VmResult<Value> {
    let src = a.str(0);
    let filename = a.str(1);
    let position = a.int(2);
    if filename.as_bytes().contains(&0) {
        return Err(a.value_error(vm, 2, "must not contain any null bytes"));
    }
    let code = source_at(src.as_bytes(), position);
    let opts = php_parser::ParseOptions {
        skip_shebang: position == AT_SHEBANG,
        short_open_tag: vm.ini_bool("short_open_tag"),
        ..Default::default()
    };
    let program = match php_parser::parse(&code, &opts) {
        Ok(p) => p,
        Err(e) => {
            let o = vm.create_exception("ParseError", &e.message, 0)?;
            vm.set_property_raw(&o, "file", Value::Str(filename.clone()));
            vm.set_property_raw(&o, "line", Value::Int(i64::from(e.line)));
            return Err(Throw::Exception(o));
        }
    };
    let copts = CompileOptions {
        filename: filename.clone(),
        assertions: vm.ini_long("zend.assertions"),
        ..Default::default()
    };
    let unit = match compile(&program, Rc::from(code.as_slice()), copts) {
        Ok(u) => u,
        Err(e) => {
            vm.raise(64, &e.message)?;
            return Err(Throw::Fatal);
        }
    };
    let globals = vm.globals.clone();
    let map: Rc<[u32]> = {
        let mut st = globals.borrow_mut();
        unit.main
            .code
            .cv_names
            .iter()
            .map(|n: &ZStr| st.find_or_add(n.as_bytes()))
            .collect::<Vec<_>>()
            .into()
    };
    let f = Frame::with_vars(unit.main.code.clone(), FrameKind::Eval, globals, Some(map));
    vm.frames.push(f);
    let base = vm.frames.len() - 1;
    let r = vm.execute(base);
    vm.frames.truncate(base);
    r.map(|_| Value::Null)
}

#[cfg(test)]
mod tests {
    #[test]
    fn positions() {
        assert_eq!(super::source_at(b"x;", 2), b"<?php x;".to_vec());
        assert_eq!(super::source_at(b"<?php x;", 1), b"<?php x;".to_vec());
    }
}
