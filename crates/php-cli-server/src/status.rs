//! Status lines, the always-present response headers, the built-in error
//! pages (`php_cli_server_send_error_page`) and the time formats the
//! server uses.

/// `http_status_map` (`main/http_status_codes.h`).
const STATUS_MAP: &[(i32, &str)] = &[
    (100, "Continue"),
    (101, "Switching Protocols"),
    (200, "OK"),
    (201, "Created"),
    (202, "Accepted"),
    (203, "Non-Authoritative Information"),
    (204, "No Content"),
    (205, "Reset Content"),
    (206, "Partial Content"),
    (300, "Multiple Choices"),
    (301, "Moved Permanently"),
    (302, "Found"),
    (303, "See Other"),
    (304, "Not Modified"),
    (305, "Use Proxy"),
    (307, "Temporary Redirect"),
    (308, "Permanent Redirect"),
    (400, "Bad Request"),
    (401, "Unauthorized"),
    (402, "Payment Required"),
    (403, "Forbidden"),
    (404, "Not Found"),
    (405, "Method Not Allowed"),
    (406, "Not Acceptable"),
    (407, "Proxy Authentication Required"),
    (408, "Request Timeout"),
    (409, "Conflict"),
    (410, "Gone"),
    (411, "Length Required"),
    (412, "Precondition Failed"),
    (413, "Request Entity Too Large"),
    (414, "Request-URI Too Long"),
    (415, "Unsupported Media Type"),
    (416, "Requested Range Not Satisfiable"),
    (417, "Expectation Failed"),
    (426, "Upgrade Required"),
    (428, "Precondition Required"),
    (429, "Too Many Requests"),
    (431, "Request Header Fields Too Large"),
    (451, "Unavailable For Legal Reasons"),
    (500, "Internal Server Error"),
    (501, "Not Implemented"),
    (502, "Bad Gateway"),
    (503, "Service Unavailable"),
    (504, "Gateway Timeout"),
    (505, "HTTP Version Not Supported"),
    (506, "Variant Also Negotiates"),
    (511, "Network Authentication Required"),
];

/// `get_status_string()`.
pub fn status_string(code: i32) -> &'static str {
    STATUS_MAP
        .binary_search_by_key(&code, |&(c, _)| c)
        .map_or("Unknown Status Code", |i| STATUS_MAP[i].1)
}

/// `append_http_status_line()`.
pub fn status_line(protocol_version: i32, code: i32) -> Vec<u8> {
    let code = if code == 0 { 200 } else { code };
    format!(
        "HTTP/{}.{} {} {}\r\n",
        protocol_version / 100,
        protocol_version % 100,
        code,
        status_string(code)
    )
    .into_bytes()
}

/// `append_essential_headers()`: `Date` (unless the script set one) and
/// `Connection: close`.
pub fn essential_headers(out: &mut Vec<u8>, date: bool, now: i64) {
    if date {
        out.extend_from_slice(b"Date: ");
        out.extend_from_slice(http_date(now).as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Connection: close\r\n");
}

/// Days since the epoch → (year, month 1-12, day 1-31).
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

const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `php_format_date("D, d M Y H:i:s", t, 0)` + `" GMT"`.
pub fn http_date(t: i64) -> String {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let wd = usize::try_from(days.rem_euclid(7)).unwrap_or(0);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[wd],
        d,
        MONTHS[(m as usize).saturating_sub(1)],
        y,
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

/// The current Unix time in seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// `php_cli_server_get_system_time()`: `asctime()` of the local time
/// without the trailing newline, e.g. `Tue Nov  3 12:00:00 2026`.
pub fn log_time() -> String {
    let t = now();
    let Some(tm) = local_tm(t) else {
        return "unknown time, can't be fetched".to_string();
    };
    format!(
        "{} {}{:3} {:02}:{:02}:{:02} {}",
        ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
            .get(usize::try_from(tm.tm_wday).unwrap_or(0))
            .copied()
            .unwrap_or("???"),
        MONTHS
            .get(usize::try_from(tm.tm_mon).unwrap_or(0))
            .copied()
            .unwrap_or("???"),
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        i64::from(tm.tm_year) + 1900
    )
}

/// `localtime_r()`.
fn local_tm(t: i64) -> Option<libc::tm> {
    let tt: libc::time_t = t;
    // SAFETY: an all-zero `tm` is a valid value (plain integers and a
    // nullable pointer).
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r reads `tt` and writes into `tm`, both owned here;
    // it is the thread-safe variant (tzset is called internally).
    let r = unsafe { libc::localtime_r(&raw const tt, &raw mut tm) };
    (!r.is_null()).then_some(tm)
}

/// `template_map`: the body of the error pages.
fn template(code: i32) -> Option<&'static str> {
    Some(match code {
        400 => "<h1>%s</h1><p>Your browser sent a request that this server could not understand.</p>",
        404 => "<h1>%s</h1><p>The requested resource <code class=\"url\">%s</code> was not found on this server.</p>",
        405 => "<h1>%s</h1><p>Requested method not allowed.</p>",
        413 => "<h1>%s</h1><p>The request body exceeds the configured <code>post_max_size</code> of %d bytes.</p>",
        500 => "<h1>%s</h1><p>The server is temporarily unavailable.</p>",
        501 => "<h1>%s</h1><p>Request method not supported.</p>",
        _ => return None,
    })
}

/// `php_cli_server_css`.
pub const CSS: &str = "<style>\n\
body { background-color: #fcfcfc; color: #333333; margin: 0; padding:0; }\n\
h1 { font-size: 1.5em; font-weight: normal; background-color: #9999cc; min-height:2em; line-height:2em; border-bottom: 1px inset black; margin: 0; }\n\
h1, p { padding-left: 10px; }\n\
code.url { background-color: #eeeeee; font-family:monospace; padding:0 2px;}\n\
</style>\n";

/// `php_escape_html_entities_ex(.., ENT_QUOTES, double_encode=0)` for the
/// request URI shown on error pages.
fn escape_html(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            b'&' => {
                // double_encode = 0: keep valid entities.
                if let Some(len) = entity_len(&s[i..]) {
                    out.extend_from_slice(&s[i..i + len]);
                    i += len;
                    continue;
                }
                out.extend_from_slice(b"&amp;");
            }
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            b'"' => out.extend_from_slice(b"&quot;"),
            b'\'' => out.extend_from_slice(b"&#039;"),
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

/// Length of a syntactically valid entity (`&name;`, `&#123;`, `&#x1f;`)
/// at the start of `s`.
fn entity_len(s: &[u8]) -> Option<usize> {
    let end = s.iter().take(34).position(|&c| c == b';')?;
    let body = &s[1..end];
    let ok = if let Some(num) = body.strip_prefix(b"#") {
        if let Some(hex) = num.strip_prefix(b"x").or_else(|| num.strip_prefix(b"X")) {
            !hex.is_empty() && hex.iter().all(u8::is_ascii_hexdigit)
        } else {
            !num.is_empty() && num.iter().all(u8::is_ascii_digit)
        }
    } else {
        !body.is_empty()
            && body[0].is_ascii_alphabetic()
            && body.iter().all(u8::is_ascii_alphanumeric)
    };
    ok.then_some(end + 1)
}

/// Builds a full error response (head and, unless `head_only`, the HTML
/// body) as `php_cli_server_send_error_page` does.
pub fn error_page(
    protocol_version: i32,
    status: i32,
    request_uri: &[u8],
    head_only: bool,
    post_max_size: i64,
    now: i64,
) -> Vec<u8> {
    let status_str = status_string(status);
    let mut body = Vec::new();
    if !head_only {
        body.extend_from_slice(
            format!(
                "<!doctype html><html><head><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{status} {status_str}</title>"
            )
            .as_bytes(),
        );
        body.extend_from_slice(CSS.as_bytes());
        body.extend_from_slice(b"</head><body>");
        let tpl = template(status).unwrap_or("<h1>%s</h1>");
        let mut parts = tpl.splitn(2, "%s");
        body.extend_from_slice(parts.next().unwrap_or_default().as_bytes());
        body.extend_from_slice(status_str.as_bytes());
        let rest = parts.next().unwrap_or_default();
        if status == 413 {
            body.extend_from_slice(
                rest.replacen("%d", &post_max_size.to_string(), 1)
                    .as_bytes(),
            );
        } else if let Some((a, b)) = rest.split_once("%s") {
            body.extend_from_slice(a.as_bytes());
            body.extend_from_slice(&escape_html(request_uri));
            body.extend_from_slice(b.as_bytes());
        } else {
            body.extend_from_slice(rest.as_bytes());
        }
        body.extend_from_slice(b"</body></html>");
    }
    let mut out = status_line(protocol_version, status);
    essential_headers(&mut out, true, now);
    out.extend_from_slice(format!("X-Powered-By: PHP/{}\r\n", php_vm::PHP_VERSION).as_bytes());
    out.extend_from_slice(b"Content-Type: text/html; charset=UTF-8\r\n");
    out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    if status == 405 {
        out.extend_from_slice(b"Allow: GET, HEAD, POST\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_strings() {
        assert_eq!(status_string(200), "OK");
        assert_eq!(status_string(451), "Unavailable For Legal Reasons");
        assert_eq!(status_string(299), "Unknown Status Code");
        assert_eq!(status_line(101, 0), b"HTTP/1.1 200 OK\r\n");
        assert_eq!(status_line(100, 404), b"HTTP/1.0 404 Not Found\r\n");
    }

    #[test]
    fn dates() {
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(http_date(1_793_702_574), "Tue, 03 Nov 2026 10:42:54 GMT");
        assert_eq!(http_date(951_782_400), "Tue, 29 Feb 2000 00:00:00 GMT");
    }

    #[test]
    fn log_time_shape() {
        let t = log_time();
        assert_eq!(t.len(), 24, "{t}");
    }

    #[test]
    fn not_found_page() {
        let page = error_page(101, 404, b"/a<b>&amp;", false, 0, 0);
        let s = String::from_utf8(page).unwrap();
        assert!(s.starts_with("HTTP/1.1 404 Not Found\r\nDate: Thu, 01 Jan 1970 00:00:00 GMT\r\nConnection: close\r\nX-Powered-By: PHP/"));
        assert!(s.contains("Content-Type: text/html; charset=UTF-8\r\nContent-Length: "));
        assert!(s.contains("<title>404 Not Found</title><style>"));
        assert!(s.ends_with("</head><body><h1>Not Found</h1><p>The requested resource <code class=\"url\">/a&lt;b&gt;&amp;</code> was not found on this server.</p></body></html>"));
        let (head, body) = s.split_once("\r\n\r\n").unwrap();
        assert!(head.contains(&format!("Content-Length: {}", body.len())));
    }

    #[test]
    fn head_only_and_405() {
        let s = String::from_utf8(error_page(101, 405, b"/", true, 0, 0)).unwrap();
        assert!(s.ends_with("Content-Length: 0\r\nAllow: GET, HEAD, POST\r\n\r\n"));
        let s = String::from_utf8(error_page(101, 413, b"/", false, 10, 0)).unwrap();
        assert!(s.contains("<code>post_max_size</code> of 10 bytes.</p>"));
    }
}
