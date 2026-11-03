//! `setcookie()` / `setrawcookie()` header formatting (`php_setcookie()`
//! in `ext/standard/head.c`).

/// The cookie attributes (`php_setcookie()` parameters).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Cookie {
    /// Name.
    pub name: Vec<u8>,
    /// Value (`None` or empty deletes the cookie).
    pub value: Option<Vec<u8>>,
    /// Expiry timestamp (0 = session cookie).
    pub expires: i64,
    /// `path`.
    pub path: Option<Vec<u8>>,
    /// `domain`.
    pub domain: Option<Vec<u8>>,
    /// `secure`.
    pub secure: bool,
    /// `httponly`.
    pub httponly: bool,
    /// `samesite`.
    pub samesite: Option<Vec<u8>>,
    /// `partitioned`.
    pub partitioned: bool,
}

/// How a cookie was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CookieError {
    /// `zend_argument_must_not_be_empty_error(n)`.
    ArgumentEmpty(usize),
    /// `zend_argument_value_error(n, msg)`.
    ArgumentValue(usize, &'static str),
    /// `zend_value_error("func(): msg")` (message without the prefix).
    Value(&'static str),
}

const ILLEGAL_NAME: &str =
    "cannot contain \"=\", \",\", \";\", \" \", \"\\t\", \"\\r\", \"\\n\", \"\\013\", or \"\\014\"";
const ILLEGAL_VALUE: &str =
    "cannot contain \",\", \";\", \" \", \"\\t\", \"\\r\", \"\\n\", \"\\013\", or \"\\014\"";

fn has_any(s: &[u8], set: &[u8]) -> bool {
    // strpbrk(): C string semantics.
    s.iter().take_while(|&&c| c != 0).any(|c| set.contains(c))
}

impl Cookie {
    /// The `Set-Cookie:` header line; `now` is the current timestamp (for
    /// `Max-Age`).
    pub fn header_line(&self, url_encode: bool, now: i64) -> Result<Vec<u8>, CookieError> {
        const SEPS: &[u8] = b",; \t\r\n\x0b\x0c";
        if self.name.is_empty() {
            return Err(CookieError::ArgumentEmpty(1));
        }
        if has_any(&self.name, b"=,; \t\r\n\x0b\x0c") {
            return Err(CookieError::ArgumentValue(1, ILLEGAL_NAME));
        }
        if !url_encode && self.value.as_deref().is_some_and(|v| has_any(v, SEPS)) {
            return Err(CookieError::ArgumentValue(2, ILLEGAL_VALUE));
        }
        if self.path.as_deref().is_some_and(|v| has_any(v, SEPS)) {
            return Err(CookieError::Value(
                "\"path\" option cannot contain \",\", \";\", \" \", \"\\t\", \"\\r\", \"\\n\", \"\\013\", or \"\\014\"",
            ));
        }
        if self.domain.as_deref().is_some_and(|v| has_any(v, SEPS)) {
            return Err(CookieError::Value(
                "\"domain\" option cannot contain \",\", \";\", \" \", \"\\t\", \"\\r\", \"\\n\", \"\\013\", or \"\\014\"",
            ));
        }
        if self.expires >= 253_402_300_800 {
            return Err(CookieError::Value(
                "\"expires\" option cannot have a year greater than 9999",
            ));
        }
        if self.partitioned && !self.secure {
            return Err(CookieError::Value(
                "\"partitioned\" option cannot be used without \"secure\" option",
            ));
        }
        if let Some(s) = self.samesite.as_deref().filter(|s| !s.is_empty()) {
            if !(s.eq_ignore_ascii_case(b"Strict")
                || s.eq_ignore_ascii_case(b"Lax")
                || s.eq_ignore_ascii_case(b"None"))
            {
                return Err(CookieError::Value(
                    "\"samesite\" option must be \"Strict\", \"Lax\", \"None\", or \"\"",
                ));
            }
        }
        let mut buf = b"Set-Cookie: ".to_vec();
        buf.extend_from_slice(&self.name);
        match self.value.as_deref().filter(|v| !v.is_empty()) {
            None => {
                buf.extend_from_slice(b"=deleted; expires=Thu, 01 Jan 1970 00:00:01 GMT; Max-Age=0");
            }
            Some(v) => {
                buf.push(b'=');
                if url_encode {
                    php_stdmisc::query::url_encode_into(&mut buf, v, true);
                } else {
                    buf.extend_from_slice(v);
                }
                if self.expires > 0 {
                    buf.extend_from_slice(b"; expires=");
                    buf.extend_from_slice(http_date(self.expires).as_bytes());
                    buf.extend_from_slice(b"; Max-Age=");
                    buf.extend_from_slice((self.expires - now).max(0).to_string().as_bytes());
                }
            }
        }
        let mut attr = |name: &[u8], v: Option<&[u8]>| {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                buf.extend_from_slice(name);
                buf.extend_from_slice(v);
            }
        };
        attr(b"; path=", self.path.as_deref());
        attr(b"; domain=", self.domain.as_deref());
        if self.secure {
            buf.extend_from_slice(b"; secure");
        }
        if self.httponly {
            buf.extend_from_slice(b"; HttpOnly");
        }
        if let Some(s) = self.samesite.as_deref().filter(|s| !s.is_empty()) {
            buf.extend_from_slice(b"; SameSite=");
            buf.extend_from_slice(s);
        }
        if self.partitioned {
            buf.extend_from_slice(b"; Partitioned");
        }
        Ok(buf)
    }
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `date("D, d M Y H:i:s \G\M\T", ts)` in UTC.
pub fn http_date(ts: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let wd = usize::try_from(days.rem_euclid(7)).unwrap_or(0);
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        DAYS[wd],
        d,
        MONTHS[(m - 1) as usize],
        y,
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(name: &str, value: Option<&str>) -> Cookie {
        Cookie {
            name: name.as_bytes().to_vec(),
            value: value.map(|v| v.as_bytes().to_vec()),
            ..Cookie::default()
        }
    }

    #[test]
    fn dates() {
        assert_eq!(http_date(1), "Thu, 01 Jan 1970 00:00:01 GMT");
        assert_eq!(http_date(1_700_000_000), "Tue, 14 Nov 2023 22:13:20 GMT");
        assert_eq!(http_date(951_782_400), "Tue, 29 Feb 2000 00:00:00 GMT");
    }

    #[test]
    fn basic_and_encoded() {
        let c = cookie("a", Some("b c"));
        assert_eq!(c.header_line(true, 0).unwrap(), b"Set-Cookie: a=b%20c");
        assert_eq!(
            c.header_line(false, 0),
            Err(CookieError::ArgumentValue(2, ILLEGAL_VALUE))
        );
    }

    #[test]
    fn deleted_and_attributes() {
        let mut c = cookie("a", None);
        c.path = Some(b"/".to_vec());
        c.domain = Some(b"example.com".to_vec());
        c.secure = true;
        c.httponly = true;
        c.samesite = Some(b"Lax".to_vec());
        c.partitioned = true;
        assert_eq!(
            String::from_utf8(c.header_line(true, 0).unwrap()).unwrap(),
            "Set-Cookie: a=deleted; expires=Thu, 01 Jan 1970 00:00:01 GMT; Max-Age=0; path=/; domain=example.com; secure; HttpOnly; SameSite=Lax; Partitioned"
        );
    }

    #[test]
    fn expiry() {
        let mut c = cookie("a", Some("1"));
        c.expires = 1_700_000_100;
        assert_eq!(
            String::from_utf8(c.header_line(true, 1_700_000_000).unwrap()).unwrap(),
            "Set-Cookie: a=1; expires=Tue, 14 Nov 2023 22:15:00 GMT; Max-Age=100"
        );
        assert!(String::from_utf8(c.header_line(true, 1_800_000_000).unwrap())
            .unwrap()
            .ends_with("Max-Age=0"));
    }

    #[test]
    fn errors() {
        assert_eq!(cookie("", None).header_line(true, 0), Err(CookieError::ArgumentEmpty(1)));
        assert_eq!(
            cookie("a=b", None).header_line(true, 0),
            Err(CookieError::ArgumentValue(1, ILLEGAL_NAME))
        );
        let mut c = cookie("a", Some("b"));
        c.partitioned = true;
        assert!(matches!(c.header_line(true, 0), Err(CookieError::Value(_))));
        let mut c = cookie("a", Some("b"));
        c.samesite = Some(b"bogus".to_vec());
        assert!(matches!(c.header_line(true, 0), Err(CookieError::Value(_))));
    }
}
