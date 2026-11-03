//! Parser and AST for PHP, a port of `Zend/zend_language_parser.y` and
//! `Zend/zend_ast.{h,c}`.
//!
//! ```
//! let program = php_parser::parse(b"<?php echo 1 + 2;", &php_parser::ParseOptions::default()).unwrap();
//! assert_eq!(program.statements.len(), 1);
//! ```

#![warn(missing_docs)]

pub mod ast;
mod error;
mod parser;

pub use ast::Program;
pub use error::{ErrorKind, ParseError};
pub use parser::ParseOptions;

/// Parses a PHP file (starting in inline-HTML mode, like a script file).
pub fn parse(src: &[u8], opts: &ParseOptions) -> Result<Program, ParseError> {
    parser::parse_program(src, opts)
}

/// Parses code passed to `eval()`: scanning starts in PHP mode (no `<?php`
/// needed).
pub fn parse_eval(src: &[u8]) -> Result<Program, ParseError> {
    let opts = ParseOptions {
        start_in_scripting: true,
        ..ParseOptions::default()
    };
    parser::parse_program(src, &opts)
}
