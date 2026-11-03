//! Behaviour on the data files shipped with php-src's ext/bz2 tests.

use std::io::Cursor;

use php_bz2::file::BzFile;

fn open(data: &'static [u8]) -> BzFile<Cursor<Vec<u8>>> {
    BzFile::open(Cursor::new(data.to_vec()), "r").unwrap()
}

#[test]
fn test_003_partial_reads() {
    let mut f = open(include_bytes!("fixtures/003.txt.bz2"));
    assert_eq!(f.read(&mut []), Some(0));
    let mut b1 = [0u8; 1];
    assert_eq!(f.read(&mut b1), Some(1));
    assert_eq!(&b1, b"R");
    let mut b2 = [0u8; 2];
    assert_eq!(f.read(&mut b2), Some(2));
    assert_eq!(&b2, b"is");
    let mut rest = vec![0u8; 100_000];
    let n = f.read(&mut rest).unwrap();
    assert_eq!(n, 251);
    assert!(rest[..n].starts_with(b"ing up from the heart of the desert\n"));
    assert!(rest[..n].ends_with(b"Heading out for Jerusalem\n"));
}

#[test]
fn test_004_invalid_files() {
    let mut f1 = open(include_bytes!("fixtures/004_1.txt.bz2"));
    let mut f2 = open(include_bytes!("fixtures/004_2.txt.bz2"));
    assert_eq!(f1.error(), (0, "OK"));
    assert_eq!(f2.error(), (0, "OK"));
    assert_eq!(f1.read(&mut [0u8; 10]), None);
    assert_eq!(f1.error(), (-5, "DATA_ERROR_MAGIC"));
    assert_eq!(f2.read(&mut [0u8; 10]), None);
    assert_eq!(f2.error(), (-4, "DATA_ERROR"));
    assert_eq!(f1.read(&mut [0u8; 1024]), None);
    assert_eq!(f1.error(), (-5, "DATA_ERROR_MAGIC"));
    assert_eq!(f2.read(&mut [0u8; 1024]), None);
    assert_eq!(f2.error(), (-4, "DATA_ERROR"));
}

#[test]
fn bug72613_read_fails() {
    let mut f = open(include_bytes!("fixtures/72613.bz2"));
    let mut buf = [0u8; 1024];
    let mut failed = false;
    for _ in 0..100 {
        match f.read(&mut buf) {
            None => {
                failed = true;
                break;
            }
            Some(0) => break,
            Some(_) => {}
        }
    }
    assert!(failed);
}
