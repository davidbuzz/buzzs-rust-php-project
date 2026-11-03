use super::*;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::new(), |mut s, c| {
        let _ = write!(s, "{c:02x}");
        s
    })
}

#[test]
fn data_url_oracle() {
    let data = include_str!("../../tests/data/data_url_oracle.txt");
    let mut n = 0;
    for line in data.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let url = unhex(f[0]);
        let got = parse_data_url(&url);
        if let Some(err) = f[1].strip_prefix("E:") {
            assert_eq!(
                got.err().map(str::as_bytes),
                Some(&unhex(err)[..]),
                "{}",
                String::from_utf8_lossy(&url)
            );
        } else {
            let meta = f[1].strip_prefix("M:").unwrap();
            let got = got.unwrap_or_else(|e| panic!("{e} for {}", String::from_utf8_lossy(&url)));
            let m: Vec<String> = got
                .meta
                .iter()
                .map(|(k, v)| match v {
                    Value::Bool(b) => format!("{k}={b}"),
                    Value::Str(s) => format!("{k}={}", hex(s)),
                    _ => unreachable!(),
                })
                .collect();
            assert_eq!(m.join(","), meta, "{}", String::from_utf8_lossy(&url));
            assert_eq!(
                hex(&got.data),
                f.get(2).copied().unwrap_or(""),
                "{}",
                String::from_utf8_lossy(&url)
            );
        }
        n += 1;
    }
    assert!(n > 40);
}

#[test]
fn data_stream_meta_and_readonly() {
    let url = parse_data_url(b"data:text/plain;a=b,abc").unwrap();
    let mut s = data_url_stream(url, "r");
    s.set_wrapper(data_wrapper_info());
    s.orig_path = Some(b"data:text/plain;a=b,abc".to_vec());
    let meta = s.meta_data();
    let keys: Vec<&str> = meta.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "mediatype",
            "a",
            "base64",
            "wrapper_type",
            "stream_type",
            "mode",
            "unread_bytes",
            "seekable",
            "uri"
        ]
    );
    assert_eq!(s.ops_label(), "RFC2397");
    let _ = crate::diag::take();
    assert!(s.write(b"x").is_err());
    assert_eq!(crate::diag::take()[0].text(), "Stream is not writable");
    assert_eq!(s.read_vec(10).unwrap(), b"abc");
}

#[test]
fn base64_strictness() {
    assert_eq!(base64_decode(b"Zm9v YmFy", true).unwrap(), b"foobar");
    assert!(base64_decode(b"Zm9vY=x", true).is_none());
    assert!(base64_decode(b"Z", true).is_none());
    assert_eq!(base64_decode(b"Z#m9v", false).unwrap(), b"foo");
    assert_eq!(url_decode(b"a+b%41%4"), b"a bA%4");
}
