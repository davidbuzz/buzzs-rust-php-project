//! Tape instructions for the array elements of a variable.
//!
//! See the "Families" section of [`super`] for the contract of a handler
//! and of [`lower`].

use super::Inst;
use crate::opcode::OpArray;

/// The specialised instruction of the op at `i` when this family has one.
pub(super) fn lower(_code: &OpArray, _i: usize) -> Option<Inst> {
    None
}
