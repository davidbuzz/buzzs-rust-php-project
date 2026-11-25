// Derived from php-src ext/phar/stub.h. Similarity metric to original: 52%
//   Copyright © The PHP Group and Contributors. Authors: Gregory Beaver <cellog@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
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

/// The release-branch stub, still the one PHP 8.5.12 writes (master adds a
/// few casts and fixes an HTML typo). Only used through [`set_flavor`], for
/// byte-exact comparisons against `/usr/bin/php8.5`.
const RELEASE: [&[u8]; 6] = [
    include_bytes!("stub/release/newstub0.txt"),
    include_bytes!("stub/release/newstub1_0.txt"),
    include_bytes!("stub/release/newstub1_1.txt"),
    include_bytes!("stub/release/newstub2.txt"),
    include_bytes!("stub/release/newstub3_0.txt"),
    include_bytes!("stub/release/newstub3_1.txt"),
];
const RELEASE_LEN: usize = 6623;

/// Which `stub.h` the default stub is generated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Flavor {
    /// php-src master (the reference implementation).
    #[default]
    Master,
    /// The release branches' (for oracle tests against `/usr/bin/php8.5`).
    Release,
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
            [
                NEWSTUB0, NEWSTUB1_0, NEWSTUB1_1, NEWSTUB2, NEWSTUB3_0, NEWSTUB3_1,
            ],
            NEWSTUB_LEN,
        ),
        Flavor::Release => (RELEASE, RELEASE_LEN),
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

    /// `/usr/bin/php8.5` writes this stub (`const LEN = 6643;`).
    #[test]
    fn release_flavor() {
        set_flavor(Flavor::Release);
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
