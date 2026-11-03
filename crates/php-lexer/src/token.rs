//! Token kinds, spans and tokens.
//!
//! [`TokenKind`] mirrors the `T_*` token set of the Zend engine
//! (`Zend/zend_language_parser.y`) plus the single-character tokens that the
//! scanner returns as plain bytes.  The numeric ids returned by
//! [`TokenKind::id`] are the ones bison assigns in php-src master, so they
//! match the values of the `T_*` constants exposed by `ext/tokenizer`.

use std::fmt;

/// A byte range in the source plus the line on which it starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    /// Byte offset of the first byte.
    pub start: usize,
    /// Byte offset one past the last byte.
    pub end: usize,
    /// 1-based line number of the first byte (PHP's notion of the line).
    pub line: u32,
}

impl Span {
    /// Creates a new span.
    pub const fn new(start: usize, end: usize, line: u32) -> Self {
        Self { start, end, line }
    }

    /// Length of the span in bytes.
    pub const fn len(&self) -> usize {
        self.end - self.start
    }

    /// Whether the span is empty.
    pub const fn is_empty(&self) -> bool {
        self.end == self.start
    }

    /// Returns a span covering both `self` and `other`; the line is taken from
    /// `self`.
    #[must_use]
    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
            line: self.line,
        }
    }
}

/// A token produced by the [`Lexer`](crate::Lexer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Token {
    /// The kind of token.
    pub kind: TokenKind,
    /// Source range and starting line.
    pub span: Span,
    /// Line counter after the token was scanned, which is the line PHP
    /// reports when a syntax error happens at this token.
    pub end_line: u32,
}

impl Token {
    /// The raw source text of the token.
    pub fn text<'s>(&self, src: &'s [u8]) -> &'s [u8] {
        &src[self.span.start..self.span.end]
    }
}

macro_rules! token_kinds {
    (
        chars { $( $cvar:ident = $cch:literal $cstr:literal, )* }
        named { $( $var:ident = $id:literal, $name:literal, $alias:literal; )* }
    ) => {
        /// The kind of a PHP token.
        ///
        /// Variants named after characters are the single-byte tokens that the
        /// Zend scanner returns as their byte value; the others correspond to
        /// `T_*` tokens.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum TokenKind {
            /// End of input (`END` in the grammar).
            End,
            $(
                #[doc = concat!("The `", $cstr, "` character token.")]
                $cvar,
            )*
            $(
                #[doc = concat!("`", $name, "`")]
                $var,
            )*
        }

        impl TokenKind {
            /// All token kinds, in declaration order.
            pub const ALL: &'static [TokenKind] = &[
                TokenKind::End,
                $( TokenKind::$cvar, )*
                $( TokenKind::$var, )*
            ];

            /// The numeric token id, as returned by `token_get_all()`.
            ///
            /// Single-character tokens have their byte value as id, `End` is 0.
            pub const fn id(self) -> i64 {
                match self {
                    TokenKind::End => 0,
                    $( TokenKind::$cvar => $cch as i64, )*
                    $( TokenKind::$var => $id, )*
                }
            }

            /// The token constant name (e.g. `T_STRING`) as returned by
            /// `token_name()`, or `None` for single-character tokens and `End`.
            pub const fn name(self) -> Option<&'static str> {
                match self {
                    TokenKind::End => None,
                    $( TokenKind::$cvar => None, )*
                    $( TokenKind::$var => Some($name), )*
                }
            }

            /// The single character of a character token.
            pub const fn as_char(self) -> Option<u8> {
                match self {
                    $( TokenKind::$cvar => Some($cch), )*
                    _ => None,
                }
            }

            /// The character token for a byte, if there is one.
            pub const fn from_char(c: u8) -> Option<TokenKind> {
                match c {
                    $( $cch => Some(TokenKind::$cvar), )*
                    _ => None,
                }
            }

            /// Looks a token kind up by its numeric id.
            pub fn from_id(id: i64) -> Option<TokenKind> {
                match id {
                    0 => Some(TokenKind::End),
                    $( $id => Some(TokenKind::$var), )*
                    _ => u8::try_from(id).ok().and_then(TokenKind::from_char),
                }
            }

            /// The bison alias of the token as declared in the grammar
            /// (e.g. `"identifier"` or `'function'`), used to build syntax
            /// error messages.
            pub const fn grammar_alias(self) -> &'static str {
                match self {
                    TokenKind::End => "\"end of file\"",
                    $( TokenKind::$cvar => concat!("'", $cstr, "'"), )*
                    $( TokenKind::$var => $alias, )*
                }
            }
        }
    };
}

token_kinds! {
    chars {
        Semicolon = b';' ";",
        Colon = b':' ":",
        Comma = b',' ",",
        Dot = b'.' ".",
        Pipe = b'|' "|",
        Caret = b'^' "^",
        Ampersand = b'&' "&",
        Plus = b'+' "+",
        Minus = b'-' "-",
        Slash = b'/' "/",
        Star = b'*' "*",
        Equals = b'=' "=",
        Percent = b'%' "%",
        Bang = b'!' "!",
        Tilde = b'~' "~",
        Dollar = b'$' "$",
        Lt = b'<' "<",
        Gt = b'>' ">",
        Question = b'?' "?",
        At = b'@' "@",
        LBracket = b'[' "[",
        RBracket = b']' "]",
        LParen = b'(' "(",
        RParen = b')' ")",
        LBrace = b'{' "{",
        RBrace = b'}' "}",
        DoubleQuote = b'"' "\"",
        Backtick = b'`' "`",
    }
    named {
        LNumber = 260, "T_LNUMBER", "\"integer\"";
        DNumber = 261, "T_DNUMBER", "\"floating-point number\"";
        String = 262, "T_STRING", "\"identifier\"";
        NameFullyQualified = 263, "T_NAME_FULLY_QUALIFIED", "\"fully qualified name\"";
        NameRelative = 264, "T_NAME_RELATIVE", "\"namespace-relative name\"";
        NameQualified = 265, "T_NAME_QUALIFIED", "\"namespaced name\"";
        Variable = 266, "T_VARIABLE", "\"variable\"";
        InlineHtml = 267, "T_INLINE_HTML", "T_INLINE_HTML";
        EncapsedAndWhitespace = 268, "T_ENCAPSED_AND_WHITESPACE", "\"string content\"";
        ConstantEncapsedString = 269, "T_CONSTANT_ENCAPSED_STRING", "\"quoted string\"";
        StringVarname = 270, "T_STRING_VARNAME", "\"variable name\"";
        NumString = 271, "T_NUM_STRING", "\"number\"";
        Include = 272, "T_INCLUDE", "\"'include'\"";
        IncludeOnce = 273, "T_INCLUDE_ONCE", "\"'include_once'\"";
        Eval = 274, "T_EVAL", "\"'eval'\"";
        Require = 275, "T_REQUIRE", "\"'require'\"";
        RequireOnce = 276, "T_REQUIRE_ONCE", "\"'require_once'\"";
        LogicalOr = 277, "T_LOGICAL_OR", "\"'or'\"";
        LogicalXor = 278, "T_LOGICAL_XOR", "\"'xor'\"";
        LogicalAnd = 279, "T_LOGICAL_AND", "\"'and'\"";
        Print = 280, "T_PRINT", "\"'print'\"";
        Yield = 281, "T_YIELD", "\"'yield'\"";
        YieldFrom = 282, "T_YIELD_FROM", "\"'yield from'\"";
        Instanceof = 283, "T_INSTANCEOF", "\"'instanceof'\"";
        New = 284, "T_NEW", "\"'new'\"";
        Clone = 285, "T_CLONE", "\"'clone'\"";
        Exit = 286, "T_EXIT", "\"'exit'\"";
        If = 287, "T_IF", "\"'if'\"";
        ElseIf = 288, "T_ELSEIF", "\"'elseif'\"";
        Else = 289, "T_ELSE", "\"'else'\"";
        EndIf = 290, "T_ENDIF", "\"'endif'\"";
        Echo = 291, "T_ECHO", "\"'echo'\"";
        Do = 292, "T_DO", "\"'do'\"";
        While = 293, "T_WHILE", "\"'while'\"";
        EndWhile = 294, "T_ENDWHILE", "\"'endwhile'\"";
        For = 295, "T_FOR", "\"'for'\"";
        EndFor = 296, "T_ENDFOR", "\"'endfor'\"";
        Foreach = 297, "T_FOREACH", "\"'foreach'\"";
        EndForeach = 298, "T_ENDFOREACH", "\"'endforeach'\"";
        Declare = 299, "T_DECLARE", "\"'declare'\"";
        EndDeclare = 300, "T_ENDDECLARE", "\"'enddeclare'\"";
        As = 301, "T_AS", "\"'as'\"";
        Switch = 302, "T_SWITCH", "\"'switch'\"";
        EndSwitch = 303, "T_ENDSWITCH", "\"'endswitch'\"";
        Case = 304, "T_CASE", "\"'case'\"";
        Default = 305, "T_DEFAULT", "\"'default'\"";
        Match = 306, "T_MATCH", "\"'match'\"";
        Break = 307, "T_BREAK", "\"'break'\"";
        Continue = 308, "T_CONTINUE", "\"'continue'\"";
        Goto = 309, "T_GOTO", "\"'goto'\"";
        Function = 310, "T_FUNCTION", "\"'function'\"";
        Fn = 311, "T_FN", "\"'fn'\"";
        Const = 312, "T_CONST", "\"'const'\"";
        Return = 313, "T_RETURN", "\"'return'\"";
        Try = 314, "T_TRY", "\"'try'\"";
        Catch = 315, "T_CATCH", "\"'catch'\"";
        Finally = 316, "T_FINALLY", "\"'finally'\"";
        Throw = 317, "T_THROW", "\"'throw'\"";
        Use = 318, "T_USE", "\"'use'\"";
        Insteadof = 319, "T_INSTEADOF", "\"'insteadof'\"";
        Global = 320, "T_GLOBAL", "\"'global'\"";
        Static = 321, "T_STATIC", "\"'static'\"";
        Abstract = 322, "T_ABSTRACT", "\"'abstract'\"";
        Final = 323, "T_FINAL", "\"'final'\"";
        Private = 324, "T_PRIVATE", "\"'private'\"";
        Protected = 325, "T_PROTECTED", "\"'protected'\"";
        Public = 326, "T_PUBLIC", "\"'public'\"";
        PrivateSet = 327, "T_PRIVATE_SET", "\"'private(set)'\"";
        ProtectedSet = 328, "T_PROTECTED_SET", "\"'protected(set)'\"";
        PublicSet = 329, "T_PUBLIC_SET", "\"'public(set)'\"";
        Readonly = 330, "T_READONLY", "\"'readonly'\"";
        Var = 331, "T_VAR", "\"'var'\"";
        Unset = 332, "T_UNSET", "\"'unset'\"";
        Isset = 333, "T_ISSET", "\"'isset'\"";
        Empty = 334, "T_EMPTY", "\"'empty'\"";
        HaltCompiler = 335, "T_HALT_COMPILER", "\"'__halt_compiler'\"";
        Class = 336, "T_CLASS", "\"'class'\"";
        Trait = 337, "T_TRAIT", "\"'trait'\"";
        Interface = 338, "T_INTERFACE", "\"'interface'\"";
        Enum = 339, "T_ENUM", "\"'enum'\"";
        Extends = 340, "T_EXTENDS", "\"'extends'\"";
        Implements = 341, "T_IMPLEMENTS", "\"'implements'\"";
        Namespace = 342, "T_NAMESPACE", "\"'namespace'\"";
        List = 343, "T_LIST", "\"'list'\"";
        Array = 344, "T_ARRAY", "\"'array'\"";
        Callable = 345, "T_CALLABLE", "\"'callable'\"";
        Line = 346, "T_LINE", "\"'__LINE__'\"";
        File = 347, "T_FILE", "\"'__FILE__'\"";
        Dir = 348, "T_DIR", "\"'__DIR__'\"";
        ClassC = 349, "T_CLASS_C", "\"'__CLASS__'\"";
        TraitC = 350, "T_TRAIT_C", "\"'__TRAIT__'\"";
        MethodC = 351, "T_METHOD_C", "\"'__METHOD__'\"";
        FuncC = 352, "T_FUNC_C", "\"'__FUNCTION__'\"";
        PropertyC = 353, "T_PROPERTY_C", "\"'__PROPERTY__'\"";
        NsC = 354, "T_NS_C", "\"'__NAMESPACE__'\"";
        Attribute = 355, "T_ATTRIBUTE", "\"'#['\"";
        PlusEqual = 356, "T_PLUS_EQUAL", "\"'+='\"";
        MinusEqual = 357, "T_MINUS_EQUAL", "\"'-='\"";
        MulEqual = 358, "T_MUL_EQUAL", "\"'*='\"";
        DivEqual = 359, "T_DIV_EQUAL", "\"'/='\"";
        ConcatEqual = 360, "T_CONCAT_EQUAL", "\"'.='\"";
        ModEqual = 361, "T_MOD_EQUAL", "\"'%='\"";
        AndEqual = 362, "T_AND_EQUAL", "\"'&='\"";
        OrEqual = 363, "T_OR_EQUAL", "\"'|='\"";
        XorEqual = 364, "T_XOR_EQUAL", "\"'^='\"";
        SlEqual = 365, "T_SL_EQUAL", "\"'<<='\"";
        SrEqual = 366, "T_SR_EQUAL", "\"'>>='\"";
        CoalesceEqual = 367, "T_COALESCE_EQUAL", "\"'??='\"";
        BooleanOr = 368, "T_BOOLEAN_OR", "\"'||'\"";
        BooleanAnd = 369, "T_BOOLEAN_AND", "\"'&&'\"";
        IsEqual = 370, "T_IS_EQUAL", "\"'=='\"";
        IsNotEqual = 371, "T_IS_NOT_EQUAL", "\"'!='\"";
        IsIdentical = 372, "T_IS_IDENTICAL", "\"'==='\"";
        IsNotIdentical = 373, "T_IS_NOT_IDENTICAL", "\"'!=='\"";
        IsSmallerOrEqual = 374, "T_IS_SMALLER_OR_EQUAL", "\"'<='\"";
        IsGreaterOrEqual = 375, "T_IS_GREATER_OR_EQUAL", "\"'>='\"";
        Spaceship = 376, "T_SPACESHIP", "\"'<=>'\"";
        Sl = 377, "T_SL", "\"'<<'\"";
        Sr = 378, "T_SR", "\"'>>'\"";
        Inc = 379, "T_INC", "\"'++'\"";
        Dec = 380, "T_DEC", "\"'--'\"";
        IntCast = 381, "T_INT_CAST", "\"'(int)'\"";
        DoubleCast = 382, "T_DOUBLE_CAST", "\"'(float)'\"";
        StringCast = 383, "T_STRING_CAST", "\"'(string)'\"";
        ArrayCast = 384, "T_ARRAY_CAST", "\"'(array)'\"";
        ObjectCast = 385, "T_OBJECT_CAST", "\"'(object)'\"";
        BoolCast = 386, "T_BOOL_CAST", "\"'(bool)'\"";
        UnsetCast = 387, "T_UNSET_CAST", "\"'(unset)'\"";
        VoidCast = 388, "T_VOID_CAST", "\"'(void)'\"";
        ObjectOperator = 389, "T_OBJECT_OPERATOR", "\"'->'\"";
        NullsafeObjectOperator = 390, "T_NULLSAFE_OBJECT_OPERATOR", "\"'?->'\"";
        DoubleArrow = 391, "T_DOUBLE_ARROW", "\"'=>'\"";
        Comment = 392, "T_COMMENT", "\"comment\"";
        DocComment = 393, "T_DOC_COMMENT", "\"doc comment\"";
        OpenTag = 394, "T_OPEN_TAG", "\"open tag\"";
        OpenTagWithEcho = 395, "T_OPEN_TAG_WITH_ECHO", "\"'<?='\"";
        CloseTag = 396, "T_CLOSE_TAG", "\"'?>'\"";
        Whitespace = 397, "T_WHITESPACE", "\"whitespace\"";
        StartHeredoc = 398, "T_START_HEREDOC", "\"heredoc start\"";
        EndHeredoc = 399, "T_END_HEREDOC", "\"heredoc end\"";
        DollarOpenCurlyBraces = 400, "T_DOLLAR_OPEN_CURLY_BRACES", "\"'${'\"";
        CurlyOpen = 401, "T_CURLY_OPEN", "\"'{$'\"";
        PaamayimNekudotayim = 402, "T_DOUBLE_COLON", "\"'::'\"";
        NsSeparator = 403, "T_NS_SEPARATOR", "\"'\\\\'\"";
        Ellipsis = 404, "T_ELLIPSIS", "\"'...'\"";
        Coalesce = 405, "T_COALESCE", "\"'??'\"";
        Pow = 406, "T_POW", "\"'**'\"";
        PowEqual = 407, "T_POW_EQUAL", "\"'**='\"";
        PipeOp = 408, "T_PIPE", "\"'|>'\"";
        AmpersandFollowedByVarOrVararg = 409, "T_AMPERSAND_FOLLOWED_BY_VAR_OR_VARARG", "\"'&'\"";
        AmpersandNotFollowedByVarOrVararg = 410, "T_AMPERSAND_NOT_FOLLOWED_BY_VAR_OR_VARARG", "\"amp\"";
        BadCharacter = 411, "T_BAD_CHARACTER", "\"invalid character\"";
        Error = 412, "T_ERROR", "T_ERROR";
    }
}

impl TokenKind {
    /// Whether this token is skipped by the parser (whitespace, comments and
    /// open tags).
    pub const fn is_trivia(self) -> bool {
        matches!(
            self,
            TokenKind::Whitespace | TokenKind::Comment | TokenKind::DocComment | TokenKind::OpenTag
        )
    }

    /// Whether this is a reserved word that is *not* a member modifier
    /// (`reserved_non_modifiers` in the grammar).
    pub const fn is_reserved_non_modifier(self) -> bool {
        use TokenKind as T;
        matches!(
            self,
            T::Include
                | T::IncludeOnce
                | T::Eval
                | T::Require
                | T::RequireOnce
                | T::LogicalOr
                | T::LogicalXor
                | T::LogicalAnd
                | T::Instanceof
                | T::New
                | T::Clone
                | T::Exit
                | T::If
                | T::ElseIf
                | T::Else
                | T::EndIf
                | T::Echo
                | T::Do
                | T::While
                | T::EndWhile
                | T::For
                | T::EndFor
                | T::Foreach
                | T::EndForeach
                | T::Declare
                | T::EndDeclare
                | T::As
                | T::Try
                | T::Catch
                | T::Finally
                | T::Throw
                | T::Use
                | T::Insteadof
                | T::Global
                | T::Var
                | T::Unset
                | T::Isset
                | T::Empty
                | T::Continue
                | T::Goto
                | T::Function
                | T::Const
                | T::Return
                | T::Print
                | T::Yield
                | T::List
                | T::Switch
                | T::EndSwitch
                | T::Case
                | T::Default
                | T::Break
                | T::Array
                | T::Callable
                | T::Extends
                | T::Implements
                | T::Namespace
                | T::Trait
                | T::Interface
                | T::Class
                | T::ClassC
                | T::TraitC
                | T::FuncC
                | T::MethodC
                | T::Line
                | T::File
                | T::Dir
                | T::NsC
                | T::Fn
                | T::Match
                | T::Enum
                | T::PropertyC
        )
    }

    /// Whether this is a semi-reserved word (`semi_reserved` in the grammar):
    /// any reserved word, including member modifiers, that may be used as a
    /// member name.
    pub const fn is_semi_reserved(self) -> bool {
        self.is_reserved_non_modifier()
            || matches!(
                self,
                TokenKind::Static
                    | TokenKind::Abstract
                    | TokenKind::Final
                    | TokenKind::Private
                    | TokenKind::Protected
                    | TokenKind::Public
                    | TokenKind::Readonly
            )
    }

    /// The display name of the token in "expecting ..." lists of syntax
    /// errors (`zend_yytnamerr` applied to an expected token).
    pub fn expected_name(self) -> String {
        match self {
            TokenKind::NsSeparator => "\"\\\"".to_owned(),
            TokenKind::AmpersandNotFollowedByVarOrVararg => "token \"&\"".to_owned(),
            TokenKind::End => "end of file".to_owned(),
            _ => {
                let alias = self.grammar_alias();
                let inner = alias
                    .strip_prefix('"')
                    .and_then(|s| s.strip_suffix('"'))
                    .unwrap_or(alias);
                inner.replace('\'', "\"")
            }
        }
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = self.name() {
            f.write_str(name)
        } else if let Some(c) = self.as_char() {
            write!(f, "{}", c as char)
        } else {
            f.write_str("END")
        }
    }
}

/// The semantic value attached to a token in parser mode.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenValue {
    /// An integer literal value.
    Int(i64),
    /// A floating point literal value.
    Float(f64),
    /// A processed string (escape sequences resolved, heredoc indentation
    /// removed).
    Str(Vec<u8>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_roundtrip() {
        for &k in TokenKind::ALL {
            assert_eq!(TokenKind::from_id(k.id()), Some(k), "{k:?}");
        }
    }

    #[test]
    fn names() {
        assert_eq!(TokenKind::LNumber.id(), 260);
        assert_eq!(TokenKind::Error.id(), 412);
        assert_eq!(
            TokenKind::PaamayimNekudotayim.name(),
            Some("T_DOUBLE_COLON")
        );
        assert_eq!(TokenKind::Semicolon.id(), i64::from(b';'));
        assert_eq!(TokenKind::Semicolon.name(), None);
    }

    #[test]
    fn expected_names() {
        assert_eq!(TokenKind::String.expected_name(), "identifier");
        assert_eq!(TokenKind::Function.expected_name(), "\"function\"");
        assert_eq!(TokenKind::Semicolon.expected_name(), "\";\"");
        assert_eq!(TokenKind::NsSeparator.expected_name(), "\"\\\"");
        assert_eq!(
            TokenKind::AmpersandNotFollowedByVarOrVararg.expected_name(),
            "token \"&\""
        );
        assert_eq!(
            TokenKind::AmpersandFollowedByVarOrVararg.expected_name(),
            "\"&\""
        );
        assert_eq!(TokenKind::End.expected_name(), "end of file");
    }
}
