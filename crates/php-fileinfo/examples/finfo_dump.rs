//! Prints `finfo_file()` / `finfo_buffer()` results for the given files
//! under several flag combinations, one line each, in the same format as
//! the PHP oracle script used to cross-check this crate:
//! `php -d memory_limit=1G finfo_dump.php FILES…`.

use php_fileinfo::{
    Finfo, FILEINFO_APPLE, FILEINFO_EXTENSION, FILEINFO_MIME, FILEINFO_MIME_ENCODING,
    FILEINFO_MIME_TYPE, FILEINFO_NONE,
};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;

fn main() {
    let modes = [
        FILEINFO_NONE,
        FILEINFO_MIME,
        FILEINFO_MIME_TYPE,
        FILEINFO_MIME_ENCODING,
        FILEINFO_EXTENSION,
        FILEINFO_APPLE,
    ];
    let mut warn = |w: php_fileinfo::Warning| eprintln!("Warning: {}", w.message);
    let mut finfo = Finfo::open(0, None, &mut warn).expect("default database");
    let mut out = std::io::stdout().lock();
    for arg in std::env::args_os().skip(1) {
        let path = arg.as_bytes();
        let data = std::fs::read(&arg).unwrap_or_default();
        for mode in modes {
            let f = finfo.file(path, mode, &mut warn).unwrap_or_default();
            let b = finfo.buffer(&data, mode, &mut warn).unwrap_or_default();
            let _ = out.write_all(path);
            let _ = write!(out, "\t{mode}\tfile\t");
            let _ = out.write_all(&f);
            let _ = out.write_all(b"\n");
            let _ = out.write_all(path);
            let _ = write!(out, "\t{mode}\tbuffer\t");
            let _ = out.write_all(&b);
            let _ = out.write_all(b"\n");
        }
    }
}
