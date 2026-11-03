use super::*;

fn kinds(src: &str) -> Vec<(TokenKind, String, u32)> {
    token_get_all(src.as_bytes())
        .into_iter()
        .map(|t| (t.kind, String::from_utf8_lossy(t.text).into_owned(), t.line))
        .collect()
}

#[test]
fn basic() {
    use TokenKind as T;
    let t = kinds("<?php echo $a + 1;");
    let k: Vec<TokenKind> = t.iter().map(|x| x.0).collect();
    assert_eq!(
        k,
        vec![
            T::OpenTag,
            T::Echo,
            T::Whitespace,
            T::Variable,
            T::Whitespace,
            T::Plus,
            T::Whitespace,
            T::LNumber,
            T::Semicolon
        ]
    );
}
