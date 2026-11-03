//! Parser feedback for `TOKEN_PARSE` mode.
//!
//! When `token_get_all()` runs with `TOKEN_PARSE`, PHP tokenizes with the
//! real parser. Whenever the grammar accepts a (semi-)reserved keyword as an
//! identifier it calls `zend_lex_tstring()`, which reports the token back to
//! ext/tokenizer (`ON_FEEDBACK`) so that it is returned as `T_STRING`.
//!
//! phprust's parser does not expose such a hook, so this module recomputes
//! the same decisions from the token stream of a program that is already
//! known to parse. The grammar positions that call `zend_lex_tstring()` are:
//!
//! * `identifier` after `::` (class constants, static methods, `Foo::class`,
//!   absolute trait method references);
//! * method names (`function [&] name (`) and `function readonly (`;
//! * class constant names and enum case names (`const NAME =`,
//!   `case NAME;`, `case NAME =`);
//! * named arguments and named placeholders (`f(name: ...)`);
//! * `namespace name;`;
//! * trait adaptations (`name as ...`, `as name`, `as modifier name`);
//! * `readonly(...)` used as a function call.

use php_lexer::TokenKind;

/// What an open bracket belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Open {
    /// The body of a class, interface, trait or enum.
    ClassBody,
    /// The `{ ... }` adaptation block of a trait `use`.
    TraitUse,
    /// Any other `{` (or `${` / `{$`).
    Brace,
    /// A function/closure/arrow-function parameter list.
    ParamList,
    /// Any other `(`.
    Paren,
    /// `[` or `#[`.
    Bracket,
}

/// Member modifiers (`member_modifier` in the grammar).
const fn is_modifier(k: TokenKind) -> bool {
    use TokenKind as T;
    matches!(
        k,
        T::Public
            | T::Protected
            | T::Private
            | T::PublicSet
            | T::ProtectedSet
            | T::PrivateSet
            | T::Static
            | T::Abstract
            | T::Final
            | T::Readonly
    )
}

const fn is_ampersand(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Ampersand
            | TokenKind::AmpersandFollowedByVarOrVararg
            | TokenKind::AmpersandNotFollowedByVarOrVararg
    )
}

/// The kind the parser sees for a token (`?>` is `;`, `<?=` is `echo`).
const fn parser_kind(k: TokenKind) -> TokenKind {
    match k {
        TokenKind::CloseTag => TokenKind::Semicolon,
        TokenKind::OpenTagWithEcho => TokenKind::Echo,
        other => other,
    }
}

/// Returns the indices (into `kinds`) of the keyword tokens that the parser
/// consumes as identifiers, i.e. that `token_get_all($code, TOKEN_PARSE)`
/// reports as `T_STRING`.
///
/// `kinds` is the complete token stream of a program that parses
/// successfully (trivia included, as returned by `token_get_all()`).
#[must_use]
pub fn identifier_positions(kinds: &[TokenKind]) -> Vec<usize> {
    let sig: Vec<(usize, TokenKind)> = kinds
        .iter()
        .enumerate()
        .filter(|(_, k)| !k.is_trivia())
        .map(|(i, &k)| (i, parser_kind(k)))
        .collect();
    let at = |j: usize| sig.get(j).map_or(TokenKind::End, |&(_, k)| k);

    let mut out = Vec::new();
    let mut stack: Vec<Open> = Vec::new();
    // Stack depth at which a class declaration / trait use / function
    // header is waiting for its `{` or `(`.
    let mut pending_class: Option<usize> = None;
    let mut pending_trait_use: Option<usize> = None;
    let mut pending_params: Option<usize> = None;
    // The effective kinds of the previous two significant tokens.
    let mut prev = TokenKind::End;
    let mut prev2 = TokenKind::End;

    for (j, &(index, kind)) in sig.iter().enumerate() {
        let next = at(j + 1);
        let top = stack.last().copied();
        let is_ident =
            kind.is_semi_reserved() && is_identifier_position(kind, prev, prev2, next, top);
        let eff = if is_ident {
            out.push(index);
            TokenKind::String
        } else {
            kind
        };

        let depth = stack.len();
        match eff {
            TokenKind::Class | TokenKind::Interface | TokenKind::Trait | TokenKind::Enum => {
                pending_class = Some(depth);
            }
            TokenKind::Use if top == Some(Open::ClassBody) => {
                pending_trait_use = Some(depth);
            }
            TokenKind::Function | TokenKind::Fn => pending_params = Some(depth),
            TokenKind::Semicolon => {
                pending_trait_use = None;
                pending_params = None;
            }
            TokenKind::LBrace | TokenKind::CurlyOpen | TokenKind::DollarOpenCurlyBraces => {
                let open = if eff != TokenKind::LBrace {
                    Open::Brace
                } else if pending_class == Some(depth) {
                    Open::ClassBody
                } else if pending_trait_use == Some(depth) {
                    Open::TraitUse
                } else {
                    Open::Brace
                };
                if pending_class == Some(depth) {
                    pending_class = None;
                }
                if pending_trait_use == Some(depth) {
                    pending_trait_use = None;
                }
                if pending_params == Some(depth) {
                    pending_params = None;
                }
                stack.push(open);
            }
            TokenKind::LParen => {
                if pending_params == Some(depth) {
                    pending_params = None;
                    stack.push(Open::ParamList);
                } else {
                    stack.push(Open::Paren);
                }
            }
            TokenKind::LBracket | TokenKind::Attribute => stack.push(Open::Bracket),
            TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => {
                stack.pop();
            }
            _ => {}
        }
        prev2 = prev;
        prev = eff;
    }
    out
}

/// Whether the keyword `kind`, between `prev2 prev` and `next`, with `top`
/// the innermost open bracket, is an `identifier` for the grammar.
fn is_identifier_position(
    kind: TokenKind,
    prev: TokenKind,
    prev2: TokenKind,
    next: TokenKind,
    top: Option<Open>,
) -> bool {
    use TokenKind as T;
    // `Foo::kw`, `const KW =`, `case KW =`, `namespace kw;`.
    if matches!(prev, T::PaamayimNekudotayim | T::Namespace) || next == T::Equals {
        return true;
    }
    // Method names and `function readonly()`.
    if next == T::LParen && (prev == T::Function || (is_ampersand(prev) && prev2 == T::Function)) {
        return true;
    }
    // Named arguments / named placeholders.
    if next == T::Colon && matches!(prev, T::LParen | T::Comma) && top == Some(Open::Paren) {
        return true;
    }
    match top {
        Some(Open::ClassBody) if prev == T::Case => return true,
        Some(Open::TraitUse) => {
            if matches!(prev, T::LBrace | T::Semicolon) && matches!(next, T::As | T::Insteadof) {
                return true;
            }
            if prev == T::As && kind.is_reserved_non_modifier() {
                return true;
            }
            if is_modifier(prev) && prev2 == T::As {
                return true;
            }
        }
        _ => {}
    }
    // `readonly(...)` as a function call (not the property modifier).
    if kind == T::Readonly && next == T::LParen {
        return match top {
            Some(Open::ClassBody) => false,
            Some(Open::ParamList) => {
                !(matches!(prev, T::LParen | T::Comma | T::RBracket) || is_modifier(prev))
            }
            _ => true,
        };
    }
    false
}

#[cfg(test)]
mod tests {
    use super::identifier_positions;
    use php_lexer::{token_get_all, TokenKind};

    /// The texts of the tokens converted to `T_STRING`.
    fn converted(src: &str) -> Vec<String> {
        let toks = token_get_all(src.as_bytes());
        let kinds: Vec<TokenKind> = toks.iter().map(|t| t.kind).collect();
        identifier_positions(&kinds)
            .into_iter()
            .map(|i| String::from_utf8_lossy(toks[i].text).into_owned())
            .collect()
    }

    #[test]
    fn member_access() {
        assert_eq!(
            converted("<?php X::continue; X::continue(); X::class; X::$continue;"),
            ["continue", "continue", "class"]
        );
    }

    #[test]
    fn class_members() {
        let src = "<?php class A { const CONST = 1, FOR = 2; const int LIST = 3;
            public function list() {} static function &new() {} public readonly int $x; }";
        assert_eq!(converted(src), ["CONST", "FOR", "LIST", "list", "new"]);
    }

    #[test]
    fn enum_cases_and_switch() {
        let src = "<?php enum E { case DEFAULT; case CLASS = 1; }
            switch ($x) { case 1: default: }";
        assert_eq!(converted(src), ["DEFAULT", "CLASS"]);
    }

    #[test]
    fn named_arguments() {
        assert_eq!(
            converted("<?php foo(class: 1, list: 2); $a ? static::x() : 1;"),
            ["class", "list", "x"][..2]
        );
    }

    #[test]
    fn trait_adaptations() {
        let src = "<?php class A { use T { list as foreach; T::new insteadof U; echo as protected print; exit as public; } }
            foreach ($a as list($b)) {}";
        assert_eq!(
            converted(src),
            ["list", "foreach", "new", "echo", "print", "exit"]
        );
    }

    #[test]
    fn readonly_calls() {
        let src = "<?php readonly(); function readonly() {}
            class A { public readonly (X&Y)|null $p; function __construct(readonly (X&Y)|null $q, $r = readonly()) {} }";
        assert_eq!(converted(src), ["readonly", "readonly", "readonly"]);
    }

    #[test]
    fn namespace_name() {
        assert_eq!(converted("<?php namespace class;"), ["class"]);
    }

    #[test]
    fn anonymous_class_body() {
        let src = "<?php new class(function () {}) { const IF = 1; };";
        assert_eq!(converted(src), ["IF"]);
    }
}
