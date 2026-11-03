//! Stubs: the default stub (`phar_create_default_stub()` / `stub.h`,
//! byte-exact with php-src master) and user stub normalisation.

use crate::consts::{HALT_TOKEN, STUB_END_SEQUENCE};
use crate::util::find_ci;

const NEWSTUB0: &[u8] = include_bytes!("stub/newstub0.txt");
const NEWSTUB1_0: &[u8] = include_bytes!("stub/newstub1_0.txt");
const NEWSTUB1_1: &[u8] = include_bytes!("stub/newstub1_1.txt");
const NEWSTUB2: &[u8] = include_bytes!("stub/newstub2.txt");
const NEWSTUB3_0: &[u8] = include_bytes!("stub/newstub3_0.txt");
const NEWSTUB3_1: &[u8] = include_bytes!("stub/newstub3_1.txt");
/// `newstub_len` in `stub.h`.
const NEWSTUB_LEN: usize = 6641;

/// `phar_create_default_stub()`. `None` arguments mean `"index.php"`.
/// Errors carry the `PharException` message.
pub fn create_default_stub(index: Option<&[u8]>, web: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let index = index.unwrap_or(b"index.php");
    let web = web.unwrap_or(b"index.php");
    if index.len() > 400 {
        return Err(format!(
            "Illegal filename passed in for stub creation, was {} characters long, and only 400 or less is allowed",
            index.len()
        ));
    }
    if web.len() > 400 {
        return Err(format!(
            "Illegal web filename passed in for stub creation, was {} characters long, and only 400 or less is allowed",
            web.len()
        ));
    }
    // `%s` stops at an embedded NUL byte.
    let index = crate::util::cstr(index);
    let web = crate::util::cstr(web);
    let total = index.len() + 1 + web.len() + 1 + NEWSTUB_LEN;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(NEWSTUB0);
    out.extend_from_slice(web);
    out.extend_from_slice(NEWSTUB1_0);
    out.extend_from_slice(NEWSTUB1_1);
    out.extend_from_slice(index);
    out.extend_from_slice(NEWSTUB2);
    out.extend_from_slice(total.to_string().as_bytes());
    out.extend_from_slice(NEWSTUB3_0);
    out.extend_from_slice(NEWSTUB3_1);
    // strpprintf() truncates at its size argument.
    out.truncate(total);
    Ok(out)
}

/// Normalises a user stub the way the flush code writes it: everything up
/// to and including the (case-insensitive) `__HALT_COMPILER();` token
/// followed by `" ?>\r\n"`. `None` when the token is missing.
pub fn normalize_user_stub(stub: &[u8]) -> Option<Vec<u8>> {
    let pos = find_ci(stub, HALT_TOKEN)?;
    let mut out = stub[..pos + HALT_TOKEN.len()].to_vec();
    out.extend_from_slice(STUB_END_SEQUENCE);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_stub_shape() {
        let s = create_default_stub(None, None).unwrap();
        assert_eq!(s.len(), 6659);
        assert!(s.starts_with(b"<?php\n\n$web = 'index.php';"));
        assert!(s.ends_with(b"__HALT_COMPILER(); ?>"));
        assert!(crate::util::find(&s, b"const LEN = 6661;").is_some());
        let e = create_default_stub(Some(&[b'a'; 401]), None).unwrap_err();
        assert!(e.contains("was 401 characters long"));
    }

    #[test]
    fn user_stub() {
        assert_eq!(
            normalize_user_stub(b"<?php __halt_compiler(); junk").unwrap(),
            b"<?php __halt_compiler(); ?>\r\n"
        );
        assert!(normalize_user_stub(b"<?php").is_none());
    }
}
