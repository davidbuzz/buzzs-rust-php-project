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

/// Stack size of the parsing thread. The parser is recursive; bison's
/// stack allows nesting up to depth 10000, which needs a deep native stack.
const PARSER_STACK_SIZE: usize = 512 * 1024 * 1024;

/// Parses a PHP file (starting in inline-HTML mode, like a script file).
///
/// Parsing runs on a helper thread with a large stack so that deeply nested
/// code behaves like PHP (which fails with `memory exhausted` only beyond
/// bison's maximum depth).
pub fn parse(src: &[u8], opts: &ParseOptions) -> Result<Program, ParseError> {
    std::thread::scope(|s| {
        match std::thread::Builder::new()
            .name("php-parser".into())
            .stack_size(PARSER_STACK_SIZE)
            .spawn_scoped(s, || parser::parse_program(src, opts))
        {
            Ok(handle) => match handle.join() {
                Ok(r) => r,
                Err(panic) => std::panic::resume_unwind(panic),
            },
            Err(_) => parser::parse_program(src, opts),
        }
    })
}

/// Parses code passed to `eval()`: scanning starts in PHP mode (no `<?php`
/// needed).
pub fn parse_eval(src: &[u8]) -> Result<Program, ParseError> {
    let opts = ParseOptions {
        start_in_scripting: true,
        ..ParseOptions::default()
    };
    parse(src, &opts)
}
