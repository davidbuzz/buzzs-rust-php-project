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

/// The PHP 8.3 release-branch stub (differs from master in a few casts and
/// an HTML typo fix). Only used through [`set_flavor`], for byte-exact
/// comparisons against a PHP 8.3 binary.
const PHP83: [&[u8]; 6] = [
    include_bytes!("stub/php83/newstub0.txt"),
    include_bytes!("stub/php83/newstub1_0.txt"),
    include_bytes!("stub/php83/newstub1_1.txt"),
    include_bytes!("stub/php83/newstub2.txt"),
    include_bytes!("stub/php83/newstub3_0.txt"),
    include_bytes!("stub/php83/newstub3_1.txt"),
];
const PHP83_LEN: usize = 6623;

/// Which `stub.h` the default stub is generated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Flavor {
    /// php-src master (the reference implementation).
    #[default]
    Master,
    /// The PHP 8.3 branch (for oracle tests against `/usr/bin/php` 8.3).
    Php83,
}

thread_local! {
    static FLAVOR: std::cell::Cell<Flavor> = const { std::cell::Cell::new(Flavor::Master) };
}

/// Selects the default stub text (testing aid).
pub fn set_flavor(f: Flavor) {
    FLAVOR.with(|c| c.set(f));
}

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
    let (parts, base) = match FLAVOR.with(std::cell::Cell::get) {
        Flavor::Master => (
            [NEWSTUB0, NEWSTUB1_0, NEWSTUB1_1, NEWSTUB2, NEWSTUB3_0, NEWSTUB3_1],
            NEWSTUB_LEN,
        ),
        Flavor::Php83 => (PHP83, PHP83_LEN),
    };
    let total = index.len() + 1 + web.len() + 1 + base;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(parts[0]);
    out.extend_from_slice(web);
    out.extend_from_slice(parts[1]);
    out.extend_from_slice(parts[2]);
    out.extend_from_slice(index);
    out.extend_from_slice(parts[3]);
    out.extend_from_slice(total.to_string().as_bytes());
    out.extend_from_slice(parts[4]);
    out.extend_from_slice(parts[5]);
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
    fn php83_flavor() {
        set_flavor(Flavor::Php83);
        let s = create_default_stub(None, None).unwrap();
        set_flavor(Flavor::Master);
        assert_eq!(s.len(), 6641);
        assert!(crate::util::find(&s, b"const LEN = 6643;").is_some());
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
