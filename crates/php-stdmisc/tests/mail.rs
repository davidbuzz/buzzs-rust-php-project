//! Oracle tests for `mail()` (fixtures from PHP child processes with
//! `sendmail_path="cat > FILE"`), plus header validation and `error_log`.

mod common;

use common::{fields, fixture_lines, show, unhex};
use php_runtime::serialize::{unserialize, UnserializeOptions};
use php_runtime::{CollectingContext, Value};
use php_stdmisc::mail::{self, ErrorLogSettings, HeadersArg, LogTarget, MailSettings, ScriptInfo};

fn tmpfile(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("stdmisc-{tag}-{}", std::process::id()))
}

#[test]
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
fn mail_matches_oracle() {
    let out = tmpfile("mail");
    let uid =
        php_stdmisc::system::PageInfo::for_script(Some(env!("CARGO_MANIFEST_DIR").as_bytes()));
    let script = ScriptInfo {
        filename: b"/some/dir/mail_child.php".to_vec(),
        lineno: 1,
        uid: uid.uid,
    };
    let mut n = 0;
    for line in fixture_lines("mail.txt") {
        let f = fields(&line);
        let settings = MailSettings {
            sendmail_path: format!("cat > {}", out.display()).into_bytes(),
            add_x_header: f[0] == "1",
            ..MailSettings::default()
        };
        let mut ctx = CollectingContext::new();
        let case = unserialize(&unhex(f[1]), &UnserializeOptions::default(), &mut ctx)
            .unwrap()
            .unwrap();
        let case = case.as_array().unwrap().clone();
        let s = |i: i64| {
            case.get_int(i)
                .unwrap()
                .as_str()
                .unwrap()
                .as_bytes()
                .to_vec()
        };
        let (to, subject, message) = (s(0), s(1), s(2));
        let headers_v = case.get_int(3).unwrap().clone();
        let headers = match &headers_v {
            Value::Null => HeadersArg::None,
            Value::Str(h) => HeadersArg::Str(h.as_bytes()),
            Value::Array(a) => HeadersArg::Array(a),
            _ => unreachable!(),
        };
        let extra = case
            .get_int(4)
            .unwrap()
            .as_str()
            .map(|z| z.as_bytes().to_vec());
        let _ = std::fs::remove_file(&out);
        let date = |_: i64| String::new();
        let r = mail::mail(
            &to,
            &subject,
            &message,
            headers,
            extra.as_deref(),
            &settings,
            &script,
            &date,
            &mut ctx,
        );
        let result = match r {
            Ok(b) => {
                if b {
                    "true".to_string()
                } else {
                    "false".to_string()
                }
            }
            Err(e) => format!("{}: {}", e.class_name().unwrap(), e.message().unwrap()),
        };
        let warnings: String = ctx.diagnostics.iter().fold(String::new(), |mut acc, d| {
            acc.push_str(&d.message);
            acc.push('\n');
            acc
        });
        let body = std::fs::read(&out).unwrap_or_default();
        let desc = format!("x={} to={:?} headers={headers_v:?}", f[0], show(&to));
        assert_eq!(result, show(&unhex(f[2])), "{desc}");
        // zpp's "Passing null to parameter" deprecation belongs to the VM.
        let expect_warnings: String = show(&unhex(f[3]))
            .lines()
            .filter(|l| !l.contains("Passing null to parameter"))
            .fold(String::new(), |mut acc, l| {
                acc.push_str(l);
                acc.push('\n');
                acc
            });
        assert_eq!(warnings, expect_warnings, "{desc}");
        assert_eq!(show(&body), show(&unhex(f[4])), "{desc}");
        n += 1;
    }
    let _ = std::fs::remove_file(&out);
    assert!(n > 100);
}

#[test]
#[allow(clippy::too_many_lines)]
fn cr_lf_modes_and_log() {
    let out = tmpfile("mail-lf");
    let log = tmpfile("mail-log");
    let _ = std::fs::remove_file(&log);
    let mut ctx = CollectingContext::new();
    let settings = MailSettings {
        sendmail_path: format!("cat > {}", out.display()).into_bytes(),
        cr_lf_mode: b"lf".to_vec(),
        log: log.to_str().unwrap().as_bytes().to_vec(),
        ..MailSettings::default()
    };
    let script = ScriptInfo {
        filename: b"/x/mail_log.php".to_vec(),
        lineno: 7,
        uid: 0,
    };
    let date = |ts: i64| mail::format_log_date_utc(ts);
    // ext/standard/tests/mail/gh19188_lf_mode.phpt
    assert!(mail::mail(
        b"a@b",
        b"subj",
        b"l1\r\nl2\n",
        HeadersArg::Str(b"X-Test: 1"),
        None,
        &settings,
        &script,
        &date,
        &mut ctx
    )
    .unwrap());
    assert_eq!(
        show(&std::fs::read(&out).unwrap()),
        "To: a@b\nSubject: subj\nX-Test: 1\n\nl1\nl2\n\n"
    );
    // mail_log.phpt: "[%d-%s-%d %d:%d:%d UTC] mail() on [%smail_log.php:%d]: To: ..."
    let l = std::fs::read_to_string(&log).unwrap();
    assert!(l.starts_with('[') && l.contains(" UTC] mail() on [/x/mail_log.php:7]: To: a@b -- Headers: X-Test: 1 -- Subject: subj\n"), "{l}");
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&log);
    assert_eq!(
        mail::format_log_date_utc(1_793_728_800),
        "03-Nov-2026 18:00:00 UTC"
    );
    assert_eq!(mail::format_log_date_utc(0), "01-Jan-1970 00:00:00 UTC");
    assert_eq!(
        mail::format_log_date_utc(951_782_400),
        "29-Feb-2000 00:00:00 UTC"
    );
    // Failing sendmail.
    let bad = MailSettings {
        sendmail_path: b"exit 3".to_vec(),
        ..MailSettings::default()
    };
    assert!(!mail::mail(
        b"a",
        b"b",
        b"c",
        HeadersArg::None,
        None,
        &bad,
        &script,
        &date,
        &mut ctx
    )
    .unwrap());
    let killed = MailSettings {
        sendmail_path: b"kill -9 $$".to_vec(),
        ..MailSettings::default()
    };
    assert!(!mail::mail(
        b"a",
        b"b",
        b"c",
        HeadersArg::None,
        None,
        &killed,
        &script,
        &date,
        &mut ctx
    )
    .unwrap());
    assert_eq!(
        ctx.messages(),
        [
            "Warning: mail(): Sendmail exited with non-zero exit code 3",
            "Warning: mail(): Sendmail killed by signal 9 (Killed)",
        ]
    );
    // No sendmail_path.
    assert!(!mail::mail(
        b"a",
        b"b",
        b"c",
        HeadersArg::None,
        None,
        &MailSettings::default(),
        &script,
        &date,
        &mut ctx
    )
    .unwrap());
    let e = mail::mail(
        b"a\0",
        b"b",
        b"c",
        HeadersArg::None,
        None,
        &bad,
        &script,
        &date,
        &mut ctx,
    )
    .unwrap_err();
    assert_eq!(
        e.message(),
        Some("mail(): Argument #1 ($to) must not contain any null bytes")
    );
}

struct Capture(Vec<Vec<u8>>);
impl LogTarget for Capture {
    fn sapi_log_message(&mut self, m: &[u8]) -> bool {
        self.0.push(m.to_vec());
        true
    }
    fn format_date(&mut self, _: i64) -> String {
        "03-Nov-2026 18:00:00 UTC".into()
    }
}

#[test]
fn error_log_types() {
    let mut ctx = CollectingContext::new();
    let mut cap = Capture(Vec::new());
    let ms = MailSettings::default();
    let script = ScriptInfo::default();
    let none = ErrorLogSettings::default();
    // type 0 without error_log INI: SAPI logger.
    assert!(
        mail::error_log(b"hello", 0, None, None, &none, &ms, &script, &mut cap, &mut ctx).unwrap()
    );
    // type 4: SAPI logger.
    assert!(
        mail::error_log(b"four", 4, None, None, &none, &ms, &script, &mut cap, &mut ctx).unwrap()
    );
    assert_eq!(cap.0, [b"hello".to_vec(), b"four".to_vec()]);
    // type 0 with error_log file: "[date] message\n".
    let f = tmpfile("errlog");
    let _ = std::fs::remove_file(&f);
    let s = ErrorLogSettings {
        error_log: Some(f.to_str().unwrap().as_bytes().to_vec()),
        error_log_mode: 0o600,
    };
    assert!(mail::error_log(b"msg", 0, None, None, &s, &ms, &script, &mut cap, &mut ctx).unwrap());
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "[03-Nov-2026 18:00:00 UTC] msg\n"
    );
    // type 3: append raw.
    let p = f.to_str().unwrap().as_bytes().to_vec();
    assert!(mail::error_log(
        b"raw",
        3,
        Some(&p),
        None,
        &none,
        &ms,
        &script,
        &mut cap,
        &mut ctx
    )
    .unwrap());
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "[03-Nov-2026 18:00:00 UTC] msg\nraw"
    );
    let _ = std::fs::remove_file(&f);
    // type 2 is gone; type 1 needs a destination.
    let e =
        mail::error_log(b"x", 2, None, None, &none, &ms, &script, &mut cap, &mut ctx).unwrap_err();
    assert_eq!(
        e.message(),
        Some("TCP/IP option is not available for error logging")
    );
    assert!(
        !mail::error_log(b"x", 1, None, None, &none, &ms, &script, &mut cap, &mut ctx).unwrap()
    );
}
