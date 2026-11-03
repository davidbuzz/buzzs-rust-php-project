//! `zend.multibyte`: mbstring's script-encoding support
//! (`php_mb_zend_encoding_*` callbacks of `zend_multibyte.c`).
//!
//! With `zend.multibyte=On`, PHP lexes a script in its script encoding
//! (`declare(encoding="...")`, else `zend.script_encoding`) and converts
//! its text to the internal encoding. phprust does this through the VM's
//! source filter: the whole source before `__halt_compiler()` is converted
//! from the script encoding to the internal encoding before compilation
//! (padded so `__COMPILER_HALT_OFFSET__` keeps pointing into the original
//! file). This matches PHP whenever the converted source is lexable as is;
//! a script whose own encoding is not lexer-compatible (e.g. `Shift_JIS`
//! identifiers containing `0x5C` bytes, kept in `Shift_JIS`) is not supported.

use std::cell::Cell;

use php_mbstring::convert::{fast_convert, ErrorMode};
use php_mbstring::{encoding, Encoding};
use php_runtime::ZStr;
use php_vm::ext::{SourceFilterFn, VmResult};
use php_vm::Vm;

use crate::state;

thread_local! {
    static PREV_FILTER: Cell<Option<SourceFilterFn>> = const { Cell::new(None) };
}

/// Installs the source filter (chaining one installed before).
pub(crate) fn install(vm: &mut Vm) {
    PREV_FILTER.with(|c| c.set(vm.source_filter));
    vm.set_source_filter(filter);
}

/// The encoding named by a leading `declare(encoding="...")`.
fn declared_encoding(src: &[u8]) -> Option<&[u8]> {
    let lower = src
        .iter()
        .take(4096)
        .map(u8::to_ascii_lowercase)
        .collect::<Vec<u8>>();
    let at = lower.windows(7).position(|w| w == b"declare")?;
    let mut i = at + 7;
    let skip_ws = |i: &mut usize| {
        while lower.get(*i).is_some_and(u8::is_ascii_whitespace) {
            *i += 1;
        }
    };
    skip_ws(&mut i);
    if lower.get(i) != Some(&b'(') {
        return None;
    }
    i += 1;
    skip_ws(&mut i);
    if !lower[i..].starts_with(b"encoding") {
        return None;
    }
    i += 8;
    skip_ws(&mut i);
    if lower.get(i) != Some(&b'=') {
        return None;
    }
    i += 1;
    skip_ws(&mut i);
    let quote = *lower.get(i).filter(|q| matches!(q, b'"' | b'\''))?;
    let start = i + 1;
    let len = lower[start..].iter().position(|&b| b == quote)?;
    Some(&src[start..start + len])
}

/// The script encoding (`zend_multibyte_detect_encoding`: the first of a
/// list when it cannot be detected).
fn script_encoding(vm: &Vm, src: &[u8]) -> Option<&'static Encoding> {
    if let Some(name) = declared_encoding(src) {
        return encoding::by_name(name);
    }
    let list = vm.ini_string("zend.script_encoding")?;
    let names: Vec<&'static Encoding> = list
        .split(|&b| b == b',')
        .filter_map(|n| encoding::by_name(n.trim_ascii()))
        .collect();
    match names.len() {
        0 => None,
        1 => Some(names[0]),
        _ => php_mbstring::detect::guess_encoding_for_strings(&[src], &names, false, true),
    }
}

/// Offset of the `__halt_compiler` token, if any.
fn halt_offset(src: &[u8]) -> Option<usize> {
    const TOKEN: &[u8] = b"__halt_compiler";
    src.windows(TOKEN.len())
        .position(|w| w.eq_ignore_ascii_case(TOKEN))
}

fn filter(vm: &mut Vm, path: &ZStr, src: Vec<u8>) -> VmResult<Vec<u8>> {
    let src = match PREV_FILTER.with(Cell::get) {
        Some(prev) => prev(vm, path, src)?,
        None => src,
    };
    if !vm.ini_bool("zend.multibyte") {
        return Ok(src);
    }
    let Some(from) = script_encoding(vm, &src) else {
        return Ok(src);
    };
    let to = state::with(|s| s.internal_encoding());
    if from.no == to.no {
        return Ok(src);
    }
    let split = halt_offset(&src).unwrap_or(src.len());
    let (code, rest) = src.split_at(split);
    let (mut out, _) = fast_convert(code, from, to, u32::from(b'?'), ErrorMode::Char);
    if rest.is_empty() {
        return Ok(out);
    }
    // Keep `__COMPILER_HALT_OFFSET__` pointing into the original file.
    if out.len() < code.len() {
        out.resize(code.len(), b' ');
        // The padding must sit before the token, where whitespace is legal.
    }
    out.extend_from_slice(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_declared_encoding() {
        assert_eq!(
            declared_encoding(b"<?php\ndeclare(encoding=\"UTF-8\");"),
            Some(&b"UTF-8"[..])
        );
        assert_eq!(
            declared_encoding(b"<?php declare ( ENCODING = 'cp932' );"),
            Some(&b"cp932"[..])
        );
        assert_eq!(declared_encoding(b"<?php declare(strict_types=1);"), None);
    }

    #[test]
    fn finds_halt_compiler() {
        assert_eq!(halt_offset(b"<?php __HALT_COMPILER(); x"), Some(6));
        assert_eq!(halt_offset(b"<?php echo 1;"), None);
    }
}
