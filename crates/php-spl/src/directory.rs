//! `SplFileInfo` and the directory/file classes (`spl_directory.c`).

/// The state of `SplFileInfo` and its subclasses.
#[derive(Debug, Default)]
pub struct FileState {}

impl FileState {
    /// A new state for the internal class `base`.
    #[must_use]
    pub fn new(base: &str) -> Self {
        let _ = base;
        FileState {}
    }
}

use crate::host::{ObjectIterator, SplHost};
use crate::object::SplObject;
use php_runtime::{Obj, RtResult, Value};
use std::rc::Rc;

pub(crate) fn call(
    _host: &mut dyn SplHost,
    _class: &str,
    _method: &str,
    _this: Option<&Obj>,
    _args: &[Value],
) -> Option<RtResult<Value>> {
    None
}

/// Clone support.
pub fn clone_into(
    _host: &mut dyn SplHost,
    _old: &Obj,
    _fresh: &SplObject,
    _st: &SplObject,
) -> RtResult<()> {
    Ok(())
}

/// Native iterator.
pub fn native_iterator(
    _host: &mut dyn SplHost,
    _obj: &Obj,
    _st: &Rc<SplObject>,
) -> RtResult<Option<Box<dyn ObjectIterator>>> {
    Ok(None)
}

/// Whether `class` is one of the file/directory classes.
#[must_use]
pub fn is_file_class(class: &str) -> bool {
    matches!(
        class,
        "SplFileInfo"
            | "DirectoryIterator"
            | "FilesystemIterator"
            | "RecursiveDirectoryIterator"
            | "GlobIterator"
            | "SplFileObject"
            | "SplTempFileObject"
    )
}
