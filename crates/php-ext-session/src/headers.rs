//! The session module's `Set-Cookie` and cache-limiter headers
//! (`sapi_add_header`).
//!
//! They go to the active SAPI through [`Vm::add_response_header`] (`php -S`
//! and apache2handler send them; the CLI SAPI's `header_handler` drops
//! them, so `headers_list()` stays empty there) and are also recorded
//! here, which keeps `delete_header_prefix` (`session_regenerate_id()`
//! replacing the cookie) well-defined and lets tests observe them.

use std::cell::RefCell;

use php_runtime::{Value, ZStr};
use php_vm::{Vm, VmResult};

thread_local! {
    static HEADERS: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
}

/// The header name (bytes before the first `:`).
fn name(line: &[u8]) -> &[u8] {
    let end = line.iter().position(|&c| c == b':').unwrap_or(line.len());
    &line[..end]
}

/// `sapi_add_header_ex(line, replace)`.
pub(crate) fn add(line: &[u8], replace: bool) {
    HEADERS.with(|h| {
        let mut h = h.borrow_mut();
        if replace {
            let n = name(line);
            h.retain(|l| !name(l).eq_ignore_ascii_case(n));
        }
        h.push(line.to_vec());
    });
}

/// `sapi_header_op(SAPI_HEADER_DELETE_PREFIX)`: removes the headers longer
/// than `prefix` that start with it (ASCII case-insensitively).
pub(crate) fn delete_prefix(prefix: &[u8]) {
    HEADERS.with(|h| {
        h.borrow_mut().retain(|l| {
            !(l.len() > prefix.len() && l[..prefix.len()].eq_ignore_ascii_case(prefix))
        });
    });
}

/// `sapi_header_op(SAPI_HEADER_DELETE_PREFIX)` on the active SAPI's
/// header list (`headers_list()`). The SAPIs only remove headers by name,
/// so the other headers of that name are removed too and added back. The
/// CLI keeps no list.
pub(crate) fn sapi_delete_prefix(vm: &mut Vm, prefix: &[u8]) -> VmResult<()> {
    if vm.response_header_hook.is_none() {
        return Ok(());
    }
    let Value::Array(list) = vm.call_function("headers_list", Vec::new())? else {
        return Ok(());
    };
    let hit = |l: &[u8]| l.len() > prefix.len() && l[..prefix.len()].eq_ignore_ascii_case(prefix);
    let target = name(prefix);
    let same_name: Vec<Vec<u8>> = list
        .values()
        .filter_map(|v| match v {
            Value::Str(s) if name(s.as_bytes()).eq_ignore_ascii_case(target) => {
                Some(s.as_bytes().to_vec())
            }
            _ => None,
        })
        .collect();
    if !same_name.iter().any(|l| hit(l)) {
        return Ok(());
    }
    vm.call_function("header_remove", vec![Value::Str(ZStr::from(target))])?;
    for l in same_name.iter().filter(|l| !hit(l)) {
        vm.add_response_header(l, false)?;
    }
    Ok(())
}

/// The recorded headers, oldest first.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn list() -> Vec<Vec<u8>> {
    HEADERS.with(|h| h.borrow().clone())
}

/// Clears the list (request end).
pub(crate) fn reset() {
    HEADERS.with(|h| h.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_replace_and_delete_prefix() {
        reset();
        add(b"Cache-Control: no-store", true);
        add(b"Set-Cookie: PHPSESSID=a; path=/", false);
        add(b"Set-Cookie: other=b", false);
        add(b"cache-control: private", true);
        assert_eq!(
            list(),
            vec![
                b"Set-Cookie: PHPSESSID=a; path=/".to_vec(),
                b"Set-Cookie: other=b".to_vec(),
                b"cache-control: private".to_vec(),
            ]
        );
        delete_prefix(b"set-cookie: PHPSESSID=");
        assert_eq!(list().len(), 2);
        // Exactly the prefix is not removed (strictly longer only).
        add(b"X: ", false);
        delete_prefix(b"X: ");
        assert_eq!(list().len(), 3);
        reset();
        assert!(list().is_empty());
    }
}
