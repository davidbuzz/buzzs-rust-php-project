// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Reads with a length beyond `memory_limit` record the violation instead
//! of allocating (`fread_oom_length.phpt`, `gh15653.phpt`). Its own test
//! binary with a single test: it sets the process-wide limit.

use php_streams::file;
use php_streams::memory::memory_open;
use php_streams::mode::TempMode;

#[test]
fn lengths_beyond_memory_limit_record_fatal() {
    php_memlimit::set_limit(Some(8 << 20)).expect("limit");

    // fread(): php_stream_read_to_str() allocates the zend_string first.
    let mut s = memory_open(TempMode::ReadOnly, b"abc".to_vec());
    assert_eq!(file::fread(&mut s, 100 << 20).ok(), Some(None));
    let over = php_memlimit::take().expect("violation recorded");
    assert_eq!(over.limit, 8 << 20);
    assert!(over.tried > 100 << 20);
    php_memlimit::clear();
    // A length that fits is read normally.
    assert_eq!(file::fread(&mut s, 10).ok(), Some(Some(b"abc".to_vec())));

    // fgetcsv(): emalloc(len + 1) before reading the line.
    let mut s = memory_open(TempMode::ReadOnly, b"a,b\n".to_vec());
    let r = file::fgetcsv(&mut s, Some(i64::MAX - 1), |line, _| line);
    assert_eq!(r.ok(), Some(None));
    let over = php_memlimit::take().expect("violation recorded");
    assert_eq!(over.tried, usize::MAX >> 1);
    php_memlimit::clear();

    php_memlimit::set_limit(None).expect("unlimited");
}
