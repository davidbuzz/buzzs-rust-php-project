//! Core runtime of phprust: PHP values, strings, the ordered hash table,
//! operators and type juggling.
//!
//! References: `Zend/zend_types.h`, `Zend/zend_string.h`, `Zend/zend_hash.c`,
//! `Zend/zend_operators.c`, `Zend/zend_strtod.c`, `ext/standard/var.c`.

pub mod float;
pub mod numeric;
pub mod sort;
pub mod string;

pub use string::ZStr;
