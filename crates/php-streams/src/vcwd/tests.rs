//! `tsrm_realpath()` and realpath cache tests (behaviour checked against
//! `/usr/bin/php`: `realpath()`, `realpath_cache_get()`, `clearstatcache()`).

use super::*;
use crate::test_util::TempDir;

fn real(p: &[u8]) -> Option<Vec<u8>> {
    virtual_file_ex(b"", p, Resolve::RealPath)
}

#[test]
fn key_is_fnv1() {
    assert_eq!(cache_key(b""), 2_166_136_261);
    assert_eq!(
        cache_key(b"a"),
        2_166_136_261_u64.wrapping_mul(16_777_619) ^ 97
    );
}

#[test]
fn resolves_like_canonicalize() {
    cache_clean();
    let d = TempDir::new();
    std::fs::create_dir_all(os_path(&d.file("a/b"))).unwrap();
    std::fs::write(os_path(&d.file("a/f")), b"x").unwrap();
    assert_eq!(real(&d.file("a/b")), Some(d.file("a/b")));
    assert_eq!(real(&d.file("a//b/./")), Some(d.file("a/b")));
    assert_eq!(real(&d.file("a/b/..")), Some(d.file("a")));
    assert_eq!(real(&d.file("a/b/../f")), Some(d.file("a/f")));
    assert_eq!(real(b"/"), Some(b"/".to_vec()));
    // Missing components and a file used as a directory.
    assert_eq!(real(&d.file("a/nope")), None);
    assert_eq!(real(&d.file("a/f/")), None);
    assert_eq!(real(&d.file("a/f/x")), None);
    // CWD_FILEPATH keeps a missing tail.
    assert_eq!(
        virtual_file_ex(b"", &d.file("a/b/../x/y"), Resolve::FilePath),
        Some(d.file("a/x/y"))
    );
    // Relative to a given directory.
    assert_eq!(
        virtual_file_ex(&d.path(), b"a/../a/f", Resolve::RealPath),
        Some(d.file("a/f"))
    );
}

#[test]
fn relative_without_cwd_stays_relative() {
    assert_eq!(real(b"."), Some(b".".to_vec()));
    assert_eq!(real(b"./"), Some(b".".to_vec()));
    assert_eq!(real(b""), None);
}

#[test]
fn cache_keeps_resolved_symlinks_until_cleared() {
    cache_clean();
    let d = TempDir::new();
    std::fs::create_dir(os_path(&d.file("dir"))).unwrap();
    std::os::unix::fs::symlink(os_path(&d.file("dir")), os_path(&d.file("l1"))).unwrap();
    std::os::unix::fs::symlink(os_path(&d.file("l1")), os_path(&d.file("l2"))).unwrap();
    assert_eq!(real(&d.file("l2")), Some(d.file("dir")));
    let entries = cache_entries();
    let l2 = entries
        .iter()
        .find(|b| b.path == d.file("l2"))
        .expect("l2 cached");
    assert_eq!(l2.realpath, d.file("dir"));
    assert!(l2.is_dir);
    assert_eq!(l2.key, cache_key(&d.file("l2")));
    let parent = entries
        .iter()
        .find(|b| b.path == d.path())
        .expect("parent cached");
    assert!(parent.is_dir);
    assert_eq!(parent.realpath, parent.path);
    assert!(cache_size() > 0);

    std::fs::remove_file(os_path(&d.file("l1"))).unwrap();
    // Still resolved from the cache.
    assert_eq!(real(&d.file("l2")), Some(d.file("dir")));
    cache_del(b"/foo/bar");
    assert_eq!(real(&d.file("l2")), Some(d.file("dir")));
    cache_del(&d.file("l2"));
    cache_del(&d.file("l1"));
    assert_eq!(real(&d.file("l2")), None);

    cache_clean();
    assert_eq!(cache_size(), 0);
    assert!(cache_entries().is_empty());
}

#[test]
fn bucket_sizes_match_php() {
    let same = Bucket {
        key: 0,
        path: b"/tmp".to_vec(),
        realpath: b"/tmp".to_vec(),
        is_dir: true,
        expires: 0,
    };
    assert_eq!(same.size(), 48 + 5);
    let other = Bucket {
        realpath: b"/private/tmp".to_vec(),
        ..same
    };
    assert_eq!(other.size(), 48 + 5 + 13);
}
