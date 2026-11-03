//! Syntax highlighting and whitespace stripping: `highlight_string()`,
//! `highlight_file()`, `show_source()` and `php_strip_whitespace()`
//! (`Zend/zend_highlight.c`), driven by the `php-lexer` tokenizer.

use php_lexer::{LexMode, LexWarning, Lexer, LexerOptions, TokenKind};

/// The `highlight.*` INI colours (`zend_syntax_highlighter_ini`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightColors {
    /// `highlight.comment`.
    pub comment: Vec<u8>,
    /// `highlight.default`.
    pub default: Vec<u8>,
    /// `highlight.html`.
    pub html: Vec<u8>,
    /// `highlight.keyword`.
    pub keyword: Vec<u8>,
    /// `highlight.string`.
    pub string: Vec<u8>,
}

impl Default for HighlightColors {
    fn default() -> Self {
        HighlightColors {
            comment: b"#FF8000".to_vec(),
            default: b"#0000BB".to_vec(),
            html: b"#000000".to_vec(),
            keyword: b"#007700".to_vec(),
            string: b"#DD0000".to_vec(),
        }
    }
}

/// The INI entries and defaults (`HL_*_COLOR`).
pub const INI_DEFAULTS: &[(&str, &str)] = &[
    ("highlight.comment", "#FF8000"),
    ("highlight.default", "#0000BB"),
    ("highlight.html", "#000000"),
    ("highlight.keyword", "#007700"),
    ("highlight.string", "#DD0000"),
];

/// Which colour a token gets. Span changes compare the colour *values*
/// (equal INI strings are the same interned string in PHP).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    Html,
    Comment,
    Default,
    Keyword,
    String,
}

impl HighlightColors {
    fn get(&self, c: Category) -> &[u8] {
        match c {
            Category::Html => &self.html,
            Category::Comment => &self.comment,
            Category::Default => &self.default,
            Category::Keyword => &self.keyword,
            Category::String => &self.string,
        }
    }
}

/// `zend_html_puts()`.
fn html_puts(out: &mut Vec<u8>, s: &[u8]) {
    for &c in s {
        match c {
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            b'&' => out.extend_from_slice(b"&amp;"),
            b'\t' => out.extend_from_slice(b"    "),
            _ => out.push(c),
        }
    }
}

/// Tokens for which the scanner produces a semantic value (they get the
/// default colour instead of the keyword colour).
fn has_value(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::String
            | TokenKind::NameQualified
            | TokenKind::NameFullyQualified
            | TokenKind::NameRelative
            | TokenKind::Variable
            | TokenKind::StringVarname
            | TokenKind::NumString
            | TokenKind::LNumber
            | TokenKind::DNumber
            | TokenKind::InlineHtml
            | TokenKind::EncapsedAndWhitespace
            | TokenKind::ConstantEncapsedString
    )
}

fn category(kind: TokenKind) -> Option<Category> {
    Some(match kind {
        TokenKind::InlineHtml => Category::Html,
        TokenKind::Comment | TokenKind::DocComment => Category::Comment,
        TokenKind::OpenTag
        | TokenKind::OpenTagWithEcho
        | TokenKind::CloseTag
        | TokenKind::Line
        | TokenKind::File
        | TokenKind::Dir
        | TokenKind::TraitC
        | TokenKind::MethodC
        | TokenKind::FuncC
        | TokenKind::PropertyC
        | TokenKind::NsC
        | TokenKind::ClassC => Category::Default,
        TokenKind::DoubleQuote
        | TokenKind::EncapsedAndWhitespace
        | TokenKind::ConstantEncapsedString => Category::String,
        TokenKind::Whitespace => return None,
        k if has_value(k) => Category::Default,
        _ => Category::Keyword,
    })
}

fn lexer(src: &[u8], short_tags: bool) -> Lexer<'_> {
    Lexer::with_options(
        src,
        LexerOptions {
            mode: LexMode::Tokenizer,
            short_tags,
            ..LexerOptions::default()
        },
    )
}

/// `zend_highlight()`: the HTML rendering of `src` (`highlight_string()`
/// output). `short_tags` is the `short_open_tag` INI setting.
#[must_use]
pub fn highlight(src: &[u8], colors: &HighlightColors, short_tags: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() * 2 + 64);
    out.extend_from_slice(b"<pre><code style=\"color: ");
    out.extend_from_slice(&colors.html);
    out.extend_from_slice(b"\">");
    let mut last = Category::Html;
    let mut lx = lexer(src, short_tags);
    while let Some(tok) = lx.next_token() {
        let text = tok.text(src);
        let Some(next) = category(tok.kind) else {
            html_puts(&mut out, text);
            continue;
        };
        let html = colors.html.as_slice();
        if colors.get(last) != colors.get(next) {
            if colors.get(last) != html {
                out.extend_from_slice(b"</span>");
            }
            last = next;
            if colors.get(last) != html {
                out.extend_from_slice(b"<span style=\"color: ");
                out.extend_from_slice(colors.get(last));
                out.extend_from_slice(b"\">");
            }
        }
        html_puts(&mut out, text);
    }
    if colors.get(last) != colors.html.as_slice() {
        out.extend_from_slice(b"</span>");
    }
    out.extend_from_slice(b"</code></pre>");
    out
}

/// `zend_strip()`: `php_strip_whitespace()` output plus the scanner's
/// compile warnings (e.g. "Unterminated comment starting line N"), which
/// PHP reports.
#[must_use]
pub fn strip_whitespace(src: &[u8], short_tags: bool) -> (Vec<u8>, Vec<LexWarning>) {
    let mut out = Vec::with_capacity(src.len());
    let mut prev_space = false;
    let mut lx = lexer(src, short_tags);
    while let Some(tok) = lx.next_token() {
        match tok.kind {
            TokenKind::Whitespace => {
                if !prev_space {
                    out.push(b' ');
                    prev_space = true;
                }
            }
            TokenKind::Comment | TokenKind::DocComment => {}
            TokenKind::EndHeredoc => {
                out.extend_from_slice(tok.text(src));
                if let Some(next) = lx.next_token() {
                    if next.kind != TokenKind::Whitespace {
                        out.extend_from_slice(next.text(src));
                    }
                }
                out.push(b'\n');
                prev_space = true;
            }
            _ => {
                out.extend_from_slice(tok.text(src));
                prev_space = false;
            }
        }
    }
    let warnings = lx.take_warnings();
    (out, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple() {
        let h = highlight(b"<?php echo 1; ?>x", &HighlightColors::default(), true);
        assert_eq!(
            String::from_utf8(h).unwrap(),
            "<pre><code style=\"color: #000000\"><span style=\"color: #0000BB\">&lt;?php </span><span style=\"color: #007700\">echo </span><span style=\"color: #0000BB\">1</span><span style=\"color: #007700\">; </span><span style=\"color: #0000BB\">?&gt;</span>x</code></pre>"
        );
    }
}
