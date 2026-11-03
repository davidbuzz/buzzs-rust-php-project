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

/// Registers `php`, `file`, `glob`, `data` and the standard filters.
pub(crate) fn register_builtin(wrappers: &mut WrapperTable, filters: &mut FilterRegistry) {
    wrappers.add(b"php", Rc::new(crate::php_wrapper::PhpWrapper));
    wrappers.add(b"file", plain_wrapper());
    wrappers.add(b"glob", Rc::new(crate::glob_wrapper::GlobWrapper));
    wrappers.add(b"data", Rc::new(crate::memory::DataWrapper));
    crate::filter::standard::register_standard_filters(filters);
}
