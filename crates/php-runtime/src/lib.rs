//! Core runtime of phprust: PHP values, strings, the ordered hash table,
//! operators and type juggling.
//!
//! References: `Zend/zend_types.h`, `Zend/zend_string.h`, `Zend/zend_hash.c`,
//! `Zend/zend_operators.c`, `Zend/zend_strtod.c`, `ext/standard/var.c`.

pub mod array;
pub mod convert;
pub mod error;
pub mod float;
pub mod numeric;
pub mod ops;
pub mod sort;
pub mod string;
pub mod value;

pub use array::{ArrayKey, HashTable, IterId, ZArray};
pub use error::{
    BinaryOp, CastTarget, CollectingContext, Diagnostic, ErrorLevel, ObjectDump, ObjectKind,
    PropertyPurpose, PropertySlot, RtResult, RuntimeContext, RuntimeError,
};
pub use string::ZStr;
pub use value::{Obj, PhpObject, PhpRef, Resource, Value};
