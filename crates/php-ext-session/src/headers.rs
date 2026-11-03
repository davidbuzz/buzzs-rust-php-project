//! A minimal SAPI header list (`SG(sapi_headers)`).
//!
//! The VM's `header()`/`headers_list()` keep no header state, and the CLI
//! SAPI discards headers anyway (its `header_handler` never adds them to the
//! list, so `headers_list()` is always empty in CLI). The session module
//! still emits its `Set-Cookie` and cache-limiter headers through
//! `sapi_add_header`; they are recorded here, which keeps
//! `delete_header_prefix` (`session_regenerate_id()` replacing the cookie)
//! well-defined and lets tests observe them.

use std::cell::RefCell;

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
