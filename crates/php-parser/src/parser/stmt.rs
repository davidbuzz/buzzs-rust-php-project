//! Statements.

use super::{PResult, Parser};
use crate::ast::Stmt;

impl Parser<'_> {
    /// Parses `top_statement_list` up to the end of the file.
    pub(crate) fn parse_top_level(&mut self) -> PResult<(Vec<Stmt>, Option<usize>)> {
        Err(self.unexpected(&[]))
    }
}
