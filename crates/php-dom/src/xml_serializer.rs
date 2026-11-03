//! The spec-compliant XML serializer (`xml_serializer.c`).

use crate::error::DomResult;
use crate::raw::N;

/// `php_new_dom_dump_node_to_str_ex`.
pub(crate) fn dump_node_to_str_ex(_node: N, _options: i32, _format: bool, _encoding: Option<&[u8]>) -> DomResult<Option<Vec<u8>>> {
    Ok(None)
}

/// `php_new_dom_dump_node_to_file`.
pub(crate) fn dump_node_to_file(_filename: &[u8], _doc: N, _node: N, _format: bool) -> i64 {
    -1
}
