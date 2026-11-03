//! Crate-level tests.

use super::*;

#[test]
fn version_is_sane() {
    let (major, minor) = version_numbers();
    assert_eq!(major, 10);
    assert!(minor >= 30, "{}", pcre_version());
    assert!(!unicode_version().is_empty());
}
