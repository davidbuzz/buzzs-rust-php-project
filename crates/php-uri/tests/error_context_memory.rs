// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Regression test: validation-error contexts must not copy the rest of the
//! input for every error (that is quadratic; a 100 KiB input of `%` used to
//! need gigabytes).

use std::time::Instant;

use php_uri::api::WhatWgUrl;

#[test]
fn many_errors_share_the_input_buffer() {
    let mut input = b"https://example.com/".to_vec();
    let n = 1024 * 1024;
    input.extend(std::iter::repeat_n(b'%', n));

    let started = Instant::now();
    let (url, errors) = WhatWgUrl::new(&input, None).expect("a lone % is only a soft error");
    let elapsed = started.elapsed();

    assert_eq!(errors.len(), n);
    assert_eq!(url.path().len(), n + 1);
    // lexbor's contexts: the rest of the input from each offending byte.
    assert_eq!(errors[0].context.len(), n);
    assert_eq!(errors[n - 1].context.as_bytes(), b"%");
    assert!(errors
        .iter()
        .all(|e| e.context.shares_buffer_with(&errors[0].context)));
    // Each error is a small fixed-size record, so memory is O(errors).
    assert!(std::mem::size_of_val(&errors[0]) <= 64);
    assert!(
        elapsed.as_secs() < 20,
        "parsing took {elapsed:?}; contexts are probably being copied"
    );
}

#[test]
fn contexts_stop_at_nul_bytes() {
    let (_, errors) = WhatWgUrl::new(b"https://example.com/a b\0c d", None).unwrap();
    let contexts: Vec<&[u8]> = errors.iter().map(|e| e.context.as_bytes()).collect();
    assert_eq!(contexts, vec![&b" b"[..], &b"\0c d"[..0], &b" d"[..]]);
}
