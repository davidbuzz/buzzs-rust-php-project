//! Parse errors and the bison-compatible syntax error messages.

use crate::ast::Diagnostic;
use php_lexer::TokenKind;
use std::fmt;

/// The class of error PHP throws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// A `ParseError` (syntax errors and scanner errors). Reported as
    /// `Parse error: ...`.
    Parse,
    /// A `CompileError` raised while parsing or by the early checks of the
    /// compiler. Reported as `Fatal error: ...`.
    Compile,
}

/// A fatal error raised while parsing a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// Parse or compile error.
    pub kind: ErrorKind,
    /// The message, exactly as PHP prints it (without the `Parse error: `
    /// prefix and the ` in FILE on line N` suffix).
    pub message: String,
    /// The line PHP reports.
    pub line: u32,
    /// Non-fatal diagnostics raised before the error.
    pub warnings: Vec<Diagnostic>,
}

impl ParseError {
    /// Creates a `ParseError`-class error.
    pub fn parse(message: impl Into<String>, line: u32) -> Self {
        Self {
            kind: ErrorKind::Parse,
            message: message.into(),
            line,
            warnings: Vec::new(),
        }
    }

    /// Creates a `CompileError`-class error.
    pub fn compile(message: impl Into<String>, line: u32) -> Self {
        Self {
            kind: ErrorKind::Compile,
            message: message.into(),
            line,
            warnings: Vec::new(),
        }
    }

    /// The prefix PHP prints before the message (`Parse error` or
    /// `Fatal error`).
    pub fn label(&self) -> &'static str {
        match self.kind {
            ErrorKind::Parse => "Parse error",
            ErrorKind::Compile => "Fatal error",
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} on line {}", self.label(), self.message, self.line)
    }
}

impl std::error::Error for ParseError {}

/// Order of the terminals in bison's symbol table for
/// `Zend/zend_language_parser.y` (symbols sorted by declaration location).
/// Bison lists the expected tokens of a syntax error in this order.
const BISON_ORDER: &[TokenKind] = {
    use TokenKind as T;
    &[
        T::End,
        T::Equals,
        T::Question,
        T::Colon,
        T::Pipe,
        T::Caret,
        T::Lt,
        T::Gt,
        T::Dot,
        T::Plus,
        T::Minus,
        T::Star,
        T::Slash,
        T::Percent,
        T::Bang,
        T::Tilde,
        T::At,
        T::LNumber,
        T::DNumber,
        T::String,
        T::NameFullyQualified,
        T::NameRelative,
        T::NameQualified,
        T::Variable,
        T::InlineHtml,
        T::EncapsedAndWhitespace,
        T::ConstantEncapsedString,
        T::StringVarname,
        T::NumString,
        T::Include,
        T::IncludeOnce,
        T::Eval,
        T::Require,
        T::RequireOnce,
        T::LogicalOr,
        T::LogicalXor,
        T::LogicalAnd,
        T::Print,
        T::Yield,
        T::YieldFrom,
        T::Instanceof,
        T::New,
        T::Clone,
        T::Exit,
        T::If,
        T::ElseIf,
        T::Else,
        T::EndIf,
        T::Echo,
        T::Do,
        T::While,
        T::EndWhile,
        T::For,
        T::EndFor,
        T::Foreach,
        T::EndForeach,
        T::Declare,
        T::EndDeclare,
        T::As,
        T::Switch,
        T::EndSwitch,
        T::Case,
        T::Default,
        T::Match,
        T::Break,
        T::Continue,
        T::Goto,
        T::Function,
        T::Fn,
        T::Const,
        T::Return,
        T::Try,
        T::Catch,
        T::Finally,
        T::Throw,
        T::Use,
        T::Insteadof,
        T::Global,
        T::Static,
        T::Abstract,
        T::Final,
        T::Private,
        T::Protected,
        T::Public,
        T::PrivateSet,
        T::ProtectedSet,
        T::PublicSet,
        T::Readonly,
        T::Var,
        T::Unset,
        T::Isset,
        T::Empty,
        T::HaltCompiler,
        T::Class,
        T::Trait,
        T::Interface,
        T::Enum,
        T::Extends,
        T::Implements,
        T::Namespace,
        T::List,
        T::Array,
        T::Callable,
        T::Line,
        T::File,
        T::Dir,
        T::ClassC,
        T::TraitC,
        T::MethodC,
        T::FuncC,
        T::PropertyC,
        T::NsC,
        T::Attribute,
        T::PlusEqual,
        T::MinusEqual,
        T::MulEqual,
        T::DivEqual,
        T::ConcatEqual,
        T::ModEqual,
        T::AndEqual,
        T::OrEqual,
        T::XorEqual,
        T::SlEqual,
        T::SrEqual,
        T::CoalesceEqual,
        T::BooleanOr,
        T::BooleanAnd,
        T::IsEqual,
        T::IsNotEqual,
        T::IsIdentical,
        T::IsNotIdentical,
        T::IsSmallerOrEqual,
        T::IsGreaterOrEqual,
        T::Spaceship,
        T::Sl,
        T::Sr,
        T::Inc,
        T::Dec,
        T::IntCast,
        T::DoubleCast,
        T::StringCast,
        T::ArrayCast,
        T::ObjectCast,
        T::BoolCast,
        T::UnsetCast,
        T::VoidCast,
        T::ObjectOperator,
        T::NullsafeObjectOperator,
        T::DoubleArrow,
        T::Comment,
        T::DocComment,
        T::OpenTag,
        T::OpenTagWithEcho,
        T::CloseTag,
        T::Whitespace,
        T::StartHeredoc,
        T::EndHeredoc,
        T::DollarOpenCurlyBraces,
        T::CurlyOpen,
        T::PaamayimNekudotayim,
        T::NsSeparator,
        T::Ellipsis,
        T::Coalesce,
        T::Pow,
        T::PowEqual,
        T::PipeOp,
        T::AmpersandFollowedByVarOrVararg,
        T::AmpersandNotFollowedByVarOrVararg,
        T::BadCharacter,
        T::Error,
        T::Comma,
        T::RBracket,
        T::Semicolon,
        T::LParen,
        T::RParen,
        T::LBrace,
        T::RBrace,
        T::LBracket,
        T::Backtick,
        T::DoubleQuote,
        T::Dollar,
    ]
};

/// Position of a token in bison's symbol order.
pub(crate) fn bison_rank(kind: TokenKind) -> usize {
    BISON_ORDER
        .iter()
        .position(|&k| k == kind)
        .unwrap_or(usize::MAX)
}

/// Describes the unexpected token like `zend_yytnamerr` does, e.g.
/// `token "}"`, `identifier "foo"`, `end of file`.
pub(crate) fn describe_unexpected(kind: TokenKind, text: &[u8]) -> String {
    match kind {
        TokenKind::End => return "end of file".to_owned(),
        TokenKind::NsSeparator => return "token \"\\\"".to_owned(),
        TokenKind::AmpersandNotFollowedByVarOrVararg => return "token \"&\"".to_owned(),
        TokenKind::DoubleQuote => return "double-quote mark".to_owned(),
        _ => {}
    }
    let alias = kind.grammar_alias();
    let toktype = alias
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(alias);
    if let Some(inner) = toktype.strip_prefix('\'') {
        let inner = inner.strip_suffix('\'').unwrap_or(inner);
        return format!("token \"{inner}\"");
    }
    if kind == TokenKind::BadCharacter && text.len() == 1 {
        return format!("character 0x{:02X}", text[0]);
    }
    let mut content = text;
    if let Some(nl) = content.iter().position(|&c| c == b'\n') {
        content = &content[..nl];
    }
    let mut toktype = toktype.to_owned();
    if !content.is_empty() && kind == TokenKind::ConstantEncapsedString {
        if content[0] == b'"' {
            "double-quoted string".clone_into(&mut toktype);
        } else if content[0] == b'\'' {
            "single-quoted string".clone_into(&mut toktype);
        }
    }
    if !content.is_empty() && (content[0] == b'\'' || content[0] == b'"') {
        content = &content[1..];
    }
    if let Some(&last) = content.last() {
        if last == b'\'' || last == b'"' {
            content = &content[..content.len() - 1];
        }
    }
    let content_str = String::from_utf8_lossy(content);
    if content.len() > 33 {
        let truncated = String::from_utf8_lossy(&content[..30]);
        format!("{toktype} \"{truncated}...\"")
    } else {
        format!("{toktype} \"{content_str}\"")
    }
}

/// Builds `syntax error, unexpected X[, expecting A or B]`. The expected
/// list is only printed when it has between one and four entries (bison's
/// `YYARGS_MAX`).
pub(crate) fn syntax_error_message(kind: TokenKind, text: &[u8], expected: &[TokenKind]) -> String {
    let mut msg = format!(
        "syntax error, unexpected {}",
        describe_unexpected(kind, text)
    );
    let mut exp: Vec<TokenKind> = expected.to_vec();
    exp.sort_by_key(|&k| bison_rank(k));
    exp.dedup();
    if !exp.is_empty() && exp.len() <= 4 {
        msg.push_str(", expecting ");
        for (i, k) in exp.iter().enumerate() {
            if i > 0 {
                msg.push_str(" or ");
            }
            msg.push_str(&k.expected_name());
        }
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unexpected_descriptions() {
        assert_eq!(describe_unexpected(TokenKind::RBrace, b"}"), "token \"}\"");
        assert_eq!(describe_unexpected(TokenKind::End, b""), "end of file");
        assert_eq!(
            describe_unexpected(TokenKind::String, b"foo"),
            "identifier \"foo\""
        );
        assert_eq!(
            describe_unexpected(TokenKind::Variable, b"$x"),
            "variable \"$x\""
        );
        assert_eq!(
            describe_unexpected(TokenKind::ConstantEncapsedString, b"'abc'"),
            "single-quoted string \"abc\""
        );
        assert_eq!(
            describe_unexpected(TokenKind::ConstantEncapsedString, b"\"a\nb\""),
            "double-quoted string \"a\""
        );
        assert_eq!(
            describe_unexpected(TokenKind::StartHeredoc, b"<<<\"MYLABEL\"\n"),
            "heredoc start \"<<<\"MYLABEL\""
        );
        assert_eq!(describe_unexpected(TokenKind::Echo, b"<?="), "token \"echo\"");
        assert_eq!(
            describe_unexpected(TokenKind::BadCharacter, b"\x7f"),
            "character 0x7F"
        );
        assert_eq!(
            describe_unexpected(TokenKind::String, b"abcdefghijabcdefghijabcdefghijabcdefghij"),
            "identifier \"abcdefghijabcdefghijabcdefghij...\""
        );
        assert_eq!(describe_unexpected(TokenKind::NsSeparator, b"\\"), "token \"\\\"");
        assert_eq!(
            describe_unexpected(TokenKind::DoubleQuote, b"\""),
            "double-quote mark"
        );
    }

    #[test]
    fn expecting_lists() {
        assert_eq!(
            syntax_error_message(
                TokenKind::LBrace,
                b"{",
                &[TokenKind::Semicolon, TokenKind::Comma]
            ),
            "syntax error, unexpected token \"{\", expecting \",\" or \";\""
        );
        assert_eq!(
            syntax_error_message(
                TokenKind::EncapsedAndWhitespace,
                b"",
                &[
                    TokenKind::NumString,
                    TokenKind::Variable,
                    TokenKind::String,
                    TokenKind::Minus
                ]
            ),
            "syntax error, unexpected string content \"\", expecting \"-\" or identifier or variable or number"
        );
        assert_eq!(
            syntax_error_message(
                TokenKind::Semicolon,
                b";",
                &[
                    TokenKind::LBrace,
                    TokenKind::LBracket,
                    TokenKind::ObjectOperator,
                    TokenKind::NullsafeObjectOperator,
                    TokenKind::Dollar
                ]
            ),
            "syntax error, unexpected token \";\""
        );
    }

    #[test]
    fn bison_order_is_complete() {
        for &k in TokenKind::ALL {
            assert_ne!(bison_rank(k), usize::MAX, "{k:?}");
        }
    }
}
