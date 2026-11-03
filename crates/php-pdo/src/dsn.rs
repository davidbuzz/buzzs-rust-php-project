//! `php_pdo_parse_data_source()`: splits a driver data source such as
//! `host=localhost;dbname=test;user=a;;b` into the requested options
//! (`;;` escapes a semicolon inside a value).

/// Parses `data_source` and fills `params` (`(option name, value)`; values
/// already present are defaults replaced on match). Returns the number of
/// matched options.
pub fn parse_data_source(data_source: &[u8], params: &mut [(&str, Option<Vec<u8>>)]) -> usize {
    let len = data_source.len();
    let mut i = 0;
    let mut optstart = 0;
    let mut matches = 0;
    while i < len {
        // looking for NAME=
        if data_source[i] == 0 {
            break;
        }
        if data_source[i] != b'=' {
            i += 1;
            continue;
        }
        i += 1;
        let valstart = i;
        // now looking for VALUE; or just VALUE<NUL>
        let mut semi: Option<usize> = None;
        let mut n_semicolumns = 0;
        while i < len {
            if data_source[i] == 0 {
                semi = Some(i);
                i += 1;
                break;
            }
            if data_source[i] == b';' {
                if i + 1 >= len || data_source[i + 1] != b';' {
                    semi = Some(i);
                    i += 1;
                    break;
                }
                n_semicolumns += 1;
                i += 2;
                continue;
            }
            i += 1;
        }
        let semi = semi.unwrap_or(i);
        let name = &data_source[optstart..valstart - 1];
        if let Some(slot) = params.iter_mut().find(|(n, _)| n.as_bytes() == name) {
            let raw = &data_source[valstart..semi.max(valstart)];
            let value = if n_semicolumns == 0 {
                raw.to_vec()
            } else {
                let mut out = Vec::with_capacity(raw.len());
                let mut j = 0;
                while j < raw.len() && raw[j] != 0 {
                    out.push(raw[j]);
                    j += if raw[j] == b';' { 2 } else { 1 };
                }
                out
            };
            slot.1 = Some(value);
            matches += 1;
        }
        while i < len && data_source[i].is_ascii_whitespace() {
            i += 1;
        }
        optstart = i;
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_options() {
        let mut p = [
            ("host", None),
            ("dbname", Some(b"def".to_vec())),
            ("user", None),
        ];
        let n = parse_data_source(b"host=localhost; dbname=a;;b;user=me", &mut p);
        assert_eq!(n, 3);
        assert_eq!(p[0].1.as_deref(), Some(&b"localhost"[..]));
        assert_eq!(p[1].1.as_deref(), Some(&b"a;b"[..]));
        assert_eq!(p[2].1.as_deref(), Some(&b"me"[..]));
    }

    #[test]
    fn unknown_options_are_ignored() {
        let mut p = [("host", None)];
        assert_eq!(parse_data_source(b"port=1;other=2", &mut p), 0);
        assert_eq!(p[0].1, None);
    }
}
