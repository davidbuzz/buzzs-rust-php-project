use super::*;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn oracle_table() {
    let data = include_str!("../../tests/data/pathname_oracle.txt");
    let mut n = 0;
    for line in data.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let path = unhex(f[1]);
        match f[0] {
            "B" => assert_eq!(
                basename(&path, &unhex(f[2])),
                unhex(f[3]),
                "basename {line}"
            ),
            "D" => assert_eq!(
                dirname_levels(&path, f[2].parse().unwrap()).unwrap(),
                unhex(f[3]),
                "dirname {line}"
            ),
            "P" => {
                let i = pathinfo(&path);
                let opt = |s: &str| (s != "-").then(|| unhex(s));
                assert_eq!(i.dirname, opt(f[2]), "pathinfo dirname {line}");
                assert_eq!(Some(i.basename), opt(f[3]), "pathinfo basename {line}");
                assert_eq!(i.extension, opt(f[4]), "pathinfo extension {line}");
                assert_eq!(Some(i.filename), opt(f[5]), "pathinfo filename {line}");
            }
            "F" => assert_eq!(
                pathinfo_part(&path, f[2].parse().unwrap()).unwrap(),
                unhex(f[3]),
                "pathinfo flag {line}"
            ),
            _ => unreachable!(),
        }
        n += 1;
    }
    assert!(n > 500);
}

#[test]
fn flag_errors() {
    assert_eq!(pathinfo_part(b"x", 0), Err(PathinfoFlagError::OutOfRange));
    assert_eq!(pathinfo_part(b"x", 16), Err(PathinfoFlagError::OutOfRange));
    assert_eq!(pathinfo_part(b"x", 3), Err(PathinfoFlagError::Multiple));
    assert!(dirname_levels(b"/a", 0).is_none());
}
