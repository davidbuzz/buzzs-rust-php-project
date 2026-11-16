// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Registration of the built-in wrappers and filters
//! (`basic_functions.c` MINIT order).

use std::rc::Rc;

use crate::filter::FilterRegistry;
use crate::wrapper::{Wrapper, WrapperTable};

thread_local! {
    static PLAIN: Rc<dyn Wrapper> = Rc::new(crate::plain::PlainFilesWrapper);
}

/// The shared plain files wrapper instance.
pub fn plain_wrapper() -> Rc<dyn Wrapper> {
    PLAIN.with(Rc::clone)
}

/// Registers `https` and `ftps` (ext/openssl's MINIT runs first in PHP),
/// then `php`, `file`, `glob`, `data`, `http`, `ftp` and the standard
/// filters.
pub(crate) fn register_builtin(wrappers: &mut WrapperTable, filters: &mut FilterRegistry) {
    let http: Rc<dyn Wrapper> = Rc::new(crate::http::HttpWrapper);
    let ftp: Rc<dyn Wrapper> = Rc::new(crate::ftp::FtpWrapper);
    wrappers.add(b"https", Rc::clone(&http));
    wrappers.add(b"ftps", Rc::clone(&ftp));
    wrappers.add(b"php", Rc::new(crate::php_wrapper::PhpWrapper));
    wrappers.add(b"file", plain_wrapper());
    wrappers.add(b"glob", Rc::new(crate::glob_wrapper::GlobWrapper));
    wrappers.add(b"data", Rc::new(crate::memory::DataWrapper));
    wrappers.add(b"http", http);
    wrappers.add(b"ftp", ftp);
    crate::filter::standard::register_standard_filters(filters);
}
