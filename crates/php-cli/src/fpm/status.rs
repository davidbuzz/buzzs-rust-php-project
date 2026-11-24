// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ping.path` and `pm.status_path` (ref: `sapi/fpm/fpm/fpm_status.c`
//! `fpm_status_handle_request()`): the ping answer, and the pool's status
//! as text (default), HTML (`?html`), XML (`?xml`), JSON (`?json`) or
//! `OpenMetrics` (`?openmetrics`), with every worker (`?full`).
//!
//! What a worker shows of its request (method, URI, query string, user,
//! script) comes from the client: the HTML and XML pages escape it as
//! `htmlentities(ENT_XML1 | ENT_COMPAT | ENT_DISALLOWED)`, the JSON page
//! as a JSON string (CVE-2026-129292: unescaped, a request could inject
//! markup into the page an administrator opens).

use super::conf::Pm;
use super::scoreboard::{Proc, Scoreboard, Stage, Totals};
use php_cli_server::sapi::CgiReply;

const EXPIRES: &[u8] = b"Expires: Thu, 01 Jan 1970 00:00:00 GMT";
const NO_CACHE: &[u8] = b"Cache-Control: no-cache, no-store, must-revalidate, max-age=0";

/// What the status page reads.
pub struct Pool<'a> {
    pub name: &'a str,
    pub pm: Pm,
    pub scoreboard: Scoreboard,
    pub status_path: Option<&'a str>,
    pub ping_path: Option<&'a str>,
    pub ping_response: Option<&'a str>,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `strftime("%d/%b/%Y:%H:%M:%S %z")`.
pub fn clf_time(epoch: i64) -> String {
    let t = super::os::localtime(epoch);
    let off = t.gmtoff / 60;
    format!(
        "{:02}/{}/{}:{:02}:{:02}:{:02} {}{:02}{:02}",
        t.mday,
        MONTHS[usize::try_from(t.mon).unwrap_or(0) % 12],
        t.year,
        t.hour,
        t.min,
        t.sec,
        if off < 0 { '-' } else { '+' },
        off.abs() / 60,
        off.abs() % 60
    )
}

/// `fpm_php_is_key_in_table(_GET, key)` on the query string (a key with
/// or without a value; `parse_str` decoding of the key).
fn query_has(query: &[u8], key: &str) -> bool {
    query.split(|&b| b == b'&').any(|p| {
        let k = p.split(|&b| b == b'=').next().unwrap_or_default();
        php_stdstr::url::urldecode(k) == key.as_bytes()
    })
}

/// The page's format.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Html,
    Xml,
    Json,
    OpenMetrics,
}

/// `fpm_status_escape()`: `None` for an empty string.
fn escape(s: &str, format: Format) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    Some(match format {
        Format::Html | Format::Xml => {
            use php_stdstr::html::{ENT_COMPAT, ENT_DISALLOWED, ENT_XML1};
            let mut notices = Vec::new();
            let out = php_stdstr::html::escape_html_entities(
                s.as_bytes(),
                true,
                ENT_DISALLOWED | ENT_XML1 | ENT_COMPAT,
                Some(b"UTF-8"),
                true,
                false,
                "htmlentities",
                &mut notices,
            );
            String::from_utf8_lossy(&out).into_owned()
        }
        Format::Json => {
            let out = php_json::encode_string(s.as_bytes(), php_json::flags::INVALID_UTF8_IGNORE)
                .unwrap_or_default();
            // "remove quotes around the string".
            let inner = if out.len() >= 2 {
                &out[1..out.len() - 1]
            } else {
                &out[..]
            };
            String::from_utf8_lossy(inner).into_owned()
        }
        Format::Text | Format::OpenMetrics => s.to_string(),
    })
}

/// The request duration a slot shows (`timersub(&now, &proc->accepted)`
/// while it runs).
fn duration_us(p: &Proc, now_us: u64) -> u64 {
    if p.stage == Some(Stage::Accepting) {
        p.duration_us
    } else {
        now_us.saturating_sub(p.accepted_us)
    }
}

/// The last request's CPU use in percent of its duration (`0` while a
/// request runs).
fn cpu(p: &Proc) -> f64 {
    if p.stage != Some(Stage::Accepting) || p.duration_us == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let pct = p.cpu_us as f64 / p.duration_us as f64 * 100.0;
    pct
}

fn headers_plain() -> Vec<Vec<u8>> {
    vec![
        b"Content-Type: text/plain".to_vec(),
        EXPIRES.to_vec(),
        NO_CACHE.to_vec(),
    ]
}

/// The status page or the ping answer for `uri`, if it is one of them;
/// `head`: `HEAD` (headers only).
#[allow(clippy::too_many_lines)]
pub fn handle(pool: &Pool<'_>, uri: &[u8], query: &[u8], head: bool) -> Option<CgiReply> {
    if let (Some(ping), Some(resp)) = (pool.ping_path, pool.ping_response) {
        if ping.as_bytes() == uri {
            return Some(CgiReply {
                status: 200,
                headers: headers_plain(),
                body: if head {
                    Vec::new()
                } else {
                    resp.as_bytes().to_vec()
                },
            });
        }
    }
    let status = pool.status_path?;
    if status.as_bytes() != uri {
        return None;
    }
    let full = query_has(query, "full");
    let t = pool.scoreboard.totals();
    let (idle, active) = pool.scoreboard.count();
    let t = Totals { idle, active, ..t };
    let mut headers = vec![EXPIRES.to_vec(), NO_CACHE.to_vec()];
    if head {
        return Some(CgiReply {
            status: 200,
            headers,
            body: Vec::new(),
        });
    }
    let format = if query_has(query, "html") {
        Format::Html
    } else if query_has(query, "xml") {
        Format::Xml
    } else if query_has(query, "json") {
        Format::Json
    } else if query_has(query, "openmetrics") {
        Format::OpenMetrics
    } else {
        Format::Text
    };
    headers.push(match format {
        Format::Html => b"Content-Type: text/html".to_vec(),
        Format::Xml => b"Content-Type: text/xml".to_vec(),
        Format::Json => b"Content-Type: application/json".to_vec(),
        Format::OpenMetrics => {
            b"Content-Type: application/openmetrics-text; version=1.0.0; charset=utf-8".to_vec()
        }
        Format::Text => b"Content-Type: text/plain".to_vec(),
    });
    let (secs, frac) = super::os::now();
    let micros = u64::try_from(secs).unwrap_or(0) * 1_000_000 + u64::from(frac);
    let since = |epoch: i64| u64::try_from(secs - epoch).unwrap_or(0);
    let time = |epoch: i64| match format {
        Format::Html | Format::Text => clf_time(epoch),
        _ => epoch.to_string(),
    };
    let total = t.idle + t.active;
    let max_active = t.active_max.max(t.active);
    let (name, pm) = (pool.name, pool.pm.name());
    let st = time(t.start_epoch);
    let ss = since(t.start_epoch);
    let mut body = match format {
        Format::Html => format!(
            "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd\">\n\
<html xmlns=\"http://www.w3.org/1999/xhtml\" xml:lang=\"en\" lang=\"en\">\n\
<head><title>PHP-FPM Status Page</title></head>\n\
<body>\n\
<table>\n\
<tr><th>pool</th><td>{name}</td></tr>\n\
<tr><th>process manager</th><td>{pm}</td></tr>\n\
<tr><th>start time</th><td>{st}</td></tr>\n\
<tr><th>start since</th><td>{ss}</td></tr>\n\
<tr><th>accepted conn</th><td>{}</td></tr>\n\
<tr><th>listen queue</th><td>{}</td></tr>\n\
<tr><th>max listen queue</th><td>{}</td></tr>\n\
<tr><th>listen queue len</th><td>{}</td></tr>\n\
<tr><th>idle processes</th><td>{}</td></tr>\n\
<tr><th>active processes</th><td>{}</td></tr>\n\
<tr><th>total processes</th><td>{total}</td></tr>\n\
<tr><th>max active processes</th><td>{max_active}</td></tr>\n\
<tr><th>max children reached</th><td>{}</td></tr>\n\
<tr><th>slow requests</th><td>{}</td></tr>\n\
<tr><th>memory peak</th><td>{}</td></tr>\n\
</table>\n",
            t.requests, t.lq, t.lq_max, t.lq_len, t.idle, t.active, t.max_children_reached, t.slow, t.memory_peak
        ),
        Format::Xml => format!(
            "<?xml version=\"1.0\" ?>\n\
<status>\n\
<pool>{name}</pool>\n\
<process-manager>{pm}</process-manager>\n\
<start-time>{st}</start-time>\n\
<start-since>{ss}</start-since>\n\
<accepted-conn>{}</accepted-conn>\n\
<listen-queue>{}</listen-queue>\n\
<max-listen-queue>{}</max-listen-queue>\n\
<listen-queue-len>{}</listen-queue-len>\n\
<idle-processes>{}</idle-processes>\n\
<active-processes>{}</active-processes>\n\
<total-processes>{total}</total-processes>\n\
<max-active-processes>{max_active}</max-active-processes>\n\
<max-children-reached>{}</max-children-reached>\n\
<slow-requests>{}</slow-requests>\n\
<memory-peak>{}</memory-peak>\n",
            t.requests, t.lq, t.lq_max, t.lq_len, t.idle, t.active, t.max_children_reached, t.slow, t.memory_peak
        ),
        Format::Json => format!(
            "{{\"pool\":\"{name}\",\"process manager\":\"{pm}\",\"start time\":{st},\"start since\":{ss},\"accepted conn\":{},\"listen queue\":{},\"max listen queue\":{},\"listen queue len\":{},\"idle processes\":{},\"active processes\":{},\"total processes\":{total},\"max active processes\":{max_active},\"max children reached\":{},\"slow requests\":{},\"memory peak\":{}",
            t.requests, t.lq, t.lq_max, t.lq_len, t.idle, t.active, t.max_children_reached, t.slow, t.memory_peak
        ),
        Format::OpenMetrics => format!(
            "# TYPE phpfpm_up gauge\n\
# HELP phpfpm_up Could pool {name} using a {pm} PM on PHP-FPM be reached?\n\
phpfpm_up 1\n\
# TYPE phpfpm_start_since counter\n\
# HELP phpfpm_start_since The number of seconds since FPM has started.\n\
phpfpm_start_since {ss}\n\
# TYPE phpfpm_accepted_connections counter\n\
# HELP phpfpm_accepted_connections The number of requests accepted by the pool.\n\
phpfpm_accepted_connections {}\n\
# TYPE phpfpm_listen_queue gauge\n\
# HELP phpfpm_listen_queue The number of requests in the queue of pending connections.\n\
phpfpm_listen_queue {}\n\
# TYPE phpfpm_max_listen_queue counter\n\
# HELP phpfpm_max_listen_queue The maximum number of requests in the queue of pending connections since FPM has started.\n\
phpfpm_max_listen_queue {}\n\
# TYPE phpfpm_listen_queue_length gauge\n\
# HELP phpfpm_listen_queue_length The size of the socket queue of pending connections.\n\
phpfpm_listen_queue_length {}\n\
# TYPE phpfpm_idle_processes gauge\n\
# HELP phpfpm_idle_processes The number of idle processes.\n\
phpfpm_idle_processes {}\n\
# TYPE phpfpm_active_processes gauge\n\
# HELP phpfpm_active_processes The number of active processes.\n\
phpfpm_active_processes {}\n\
# TYPE phpfpm_total_processes gauge\n\
# HELP phpfpm_total_processes The number of idle + active processes.\n\
phpfpm_total_processes {total}\n\
# TYPE phpfpm_max_active_processes counter\n\
# HELP phpfpm_max_active_processes The maximum number of active processes since FPM has started.\n\
phpfpm_max_active_processes {max_active}\n\
# TYPE phpfpm_max_children_reached counter\n\
# HELP phpfpm_max_children_reached The number of times, the process limit has been reached, when pm tries to start more children (works only for pm 'dynamic' and 'ondemand').\n\
phpfpm_max_children_reached {}\n\
# TYPE phpfpm_slow_requests counter\n\
# HELP phpfpm_slow_requests The number of requests that exceeded your 'request_slowlog_timeout' value.\n\
phpfpm_slow_requests {}\n\
# TYPE phpfpm_memory_peak gauge\n\
# HELP phpfpm_memory_peak The memory usage peak since FPM has started.\n\
phpfpm_memory_peak {}\n\
# EOF\n",
            t.requests, t.lq, t.lq_max, t.lq_len, t.idle, t.active, t.max_children_reached, t.slow, t.memory_peak
        ),
        Format::Text => format!(
            "pool:                 {name}\n\
process manager:      {pm}\n\
start time:           {st}\n\
start since:          {ss}\n\
accepted conn:        {}\n\
listen queue:         {}\n\
max listen queue:     {}\n\
listen queue len:     {}\n\
idle processes:       {}\n\
active processes:     {}\n\
total processes:      {total}\n\
max active processes: {max_active}\n\
max children reached: {}\n\
slow requests:        {}\n\
memory peak:          {}\n",
            t.requests, t.lq, t.lq_max, t.lq_len, t.idle, t.active, t.max_children_reached, t.slow, t.memory_peak
        ),
    };
    // `short_post`, and the workers' part (`full_pre`, `full_syntax`
    // with `full_separator`, `full_post`).
    let (short_post, pre, sep, post) = match (format, full) {
        (Format::Html, false) => (Some("</body></html>"), "", "", ""),
        (Format::Html, true) => (
            None,
            "<table border=\"1\">\n<tr><th>pid</th><th>state</th><th>start time</th><th>start since</th><th>requests</th><th>request duration</th><th>request method</th><th>request uri</th><th>content length</th><th>user</th><th>script</th><th>last request cpu</th><th>last request memory</th></tr>\n",
            "",
            "</table></body></html>",
        ),
        (Format::Xml, false) => (Some("</status>"), "", "", ""),
        (Format::Xml, true) => (None, "<processes>\n", "", "</processes>\n</status>"),
        (Format::Json, false) => (Some("}"), "", "", ""),
        (Format::Json, true) => (None, ", \"processes\":[", ",", "]}"),
        (Format::OpenMetrics, false) => (Some(""), "", "", ""),
        (Format::OpenMetrics | Format::Text, _) => (None, "", "", ""),
    };
    if let Some(p) = short_post {
        body.push_str(p);
    }
    if full && format != Format::OpenMetrics {
        body.push_str(pre);
        let mut first = true;
        for p in pool.scoreboard.procs() {
            if !first {
                body.push_str(sep);
            }
            first = false;
            let method = escape(&p.method, format);
            let uri = escape(&p.uri, format);
            let query = escape(&p.query, format);
            let user = escape(&p.user, format);
            let script = escape(&p.script, format);
            let method = method.as_deref().unwrap_or("-");
            let uri = uri.as_deref().unwrap_or("-");
            let q = if query.is_some() { "?" } else { "" };
            let query = query.as_deref().unwrap_or("");
            let user = user.as_deref().unwrap_or("-");
            let script = script.as_deref().unwrap_or("-");
            let pid = p.pid;
            let state = p.stage.map_or("", Stage::name);
            let st = time(p.start_epoch);
            let ss = since(p.start_epoch);
            let requests = p.requests;
            let dur = duration_us(&p, micros);
            let len = p.content_length;
            let cpu = cpu(&p);
            let mem = if p.stage == Some(Stage::Accepting) {
                p.memory
            } else {
                0
            };
            let row = match format {
                Format::Html => format!(
                    "<tr><td>{pid}</td><td>{state}</td><td>{st}</td><td>{ss}</td><td>{requests}</td><td>{dur}</td><td>{method}</td><td>{uri}{q}{query}</td><td>{len}</td><td>{user}</td><td>{script}</td><td>{cpu:.2}</td><td>{mem}</td></tr>\n"
                ),
                Format::Xml => format!(
                    "<process><pid>{pid}</pid><state>{state}</state><start-time>{st}</start-time><start-since>{ss}</start-since><requests>{requests}</requests><request-duration>{dur}</request-duration><request-method>{method}</request-method><request-uri>{uri}{q}{query}</request-uri><content-length>{len}</content-length><user>{user}</user><script>{script}</script><last-request-cpu>{cpu:.2}</last-request-cpu><last-request-memory>{mem}</last-request-memory></process>\n"
                ),
                Format::Json => format!(
                    "{{\"pid\":{pid},\"state\":\"{state}\",\"start time\":{st},\"start since\":{ss},\"requests\":{requests},\"request duration\":{dur},\"request method\":\"{method}\",\"request uri\":\"{uri}{q}{query}\",\"content length\":{len},\"user\":\"{user}\",\"script\":\"{script}\",\"last request cpu\":{cpu:.2},\"last request memory\":{mem}}}"
                ),
                _ => format!(
                    "\n************************\npid:                  {pid}\nstate:                {state}\nstart time:           {st}\nstart since:          {ss}\nrequests:             {requests}\nrequest duration:     {dur}\nrequest method:       {method}\nrequest URI:          {uri}{q}{query}\ncontent length:       {len}\nuser:                 {user}\nscript:               {script}\nlast request cpu:     {cpu:.2}\nlast request memory:  {mem}\n"
                ),
            };
            body.push_str(&row);
        }
        body.push_str(post);
    }
    Some(CgiReply {
        status: 200,
        headers,
        body: body.into_bytes(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_keys() {
        assert!(query_has(b"full&json", "json"));
        assert!(query_has(b"full=1", "full"));
        assert!(!query_has(b"fully", "full"));
        assert!(query_has(b"%6Aso%6E", "json"));
    }

    #[test]
    fn client_strings_are_escaped() {
        assert_eq!(
            escape("/x?<script>\"'&", Format::Html).as_deref(),
            Some("/x?&lt;script&gt;&quot;'&amp;")
        );
        assert_eq!(
            escape("/a\"b\\c</", Format::Json).as_deref(),
            Some("\\/a\\\"b\\\\c<\\/")
        );
        assert_eq!(escape("<b>", Format::Text).as_deref(), Some("<b>"));
        assert_eq!(escape("", Format::Html), None);
    }
}
