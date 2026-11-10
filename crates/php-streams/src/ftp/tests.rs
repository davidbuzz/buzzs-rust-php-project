use super::*;
use std::cell::RefCell;
use std::rc::Rc;

/// A control/data connection that serves canned server output and records
/// what the client sent.
#[derive(Debug)]
struct Scripted {
    input: Vec<u8>,
    pos: usize,
    sent: Rc<RefCell<Vec<u8>>>,
}

impl StreamOps for Scripted {
    fn label(&self) -> &'static str {
        "scripted"
    }

    fn read(&mut self, st: &mut StreamState, buf: &mut [u8]) -> Result<usize, OpFailed> {
        let n = buf.len().min(self.input.len() - self.pos);
        buf[..n].copy_from_slice(&self.input[self.pos..self.pos + n]);
        self.pos += n;
        if n == 0 {
            st.eof = true;
        }
        Ok(n)
    }

    fn write(&mut self, _st: &mut StreamState, buf: &[u8]) -> Result<usize, OpFailed> {
        self.sent.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn scripted(input: &[u8]) -> (Stream, Rc<RefCell<Vec<u8>>>) {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let ops = Scripted {
        input: input.to_vec(),
        pos: 0,
        sent: Rc::clone(&sent),
    };
    (Stream::new(Box::new(ops), "r+"), sent)
}

#[test]
fn reply_codes_skip_continuation_lines() {
    let (mut s, _) =
        scripted(b"220----- PHP FTP server 0.3 -----\r\n220 Service ready\r\n550 x\r\n");
    let mut line = Vec::new();
    assert_eq!(get_result(&mut s, &mut line), 220);
    assert_eq!(line, b"220 Service ready\r\n");
    assert_eq!(get_result(&mut s, &mut line), 550);
    // Nothing left: the code of an empty buffer is 0.
    assert_eq!(get_result(&mut s, &mut line), 0);
    assert!(line.is_empty());
}

#[test]
fn reply_without_final_line_keeps_the_last_line() {
    let (mut s, _) = scripted(b"220-only a continuation\r\n");
    let mut line = Vec::new();
    assert_eq!(get_result(&mut s, &mut line), 220);
    assert_eq!(line, b"220-only a continuation\r\n");
}

#[test]
fn pasv_reply_parsing() {
    assert_eq!(
        parse_pasv(b"227 Entering Passive Mode. (127,0,0,1,4,210)\r\n"),
        (1234, Some(b"127.0.0.1".to_vec()))
    );
    assert_eq!(
        parse_pasv(b"227 =10,1,2,3,255,255"),
        (65535, Some(b"10.1.2.3".to_vec()))
    );
    assert_eq!(parse_pasv(b"227 no numbers"), (0, None));
    assert_eq!(parse_pasv(b"227 (1,2,3)"), (0, None));
    assert_eq!(parse_pasv(b"227 (1,2,3,4,5)"), (0, None));
    // The host buffer holds 15 characters.
    assert_eq!(
        parse_pasv(b"227 (1111,2222,3333,4444,0,21)").1,
        Some(b"1111.2222.3333.".to_vec())
    );
}

#[test]
fn epsv_reply_parsing() {
    assert_eq!(
        parse_epsv(b"229 Entering Extended Passive Mode (|||6446|)\r\n"),
        6446
    );
    assert_eq!(parse_epsv(b"229 (||6446|)"), 0);
    assert_eq!(parse_epsv(b"229 (|||x|)"), 0);
}

#[test]
fn passive_mode_falls_back_from_epsv_to_pasv() {
    let (mut s, sent) = scripted(
        b"550 Extended passive mode not supported.\r\n227 Entering Passive Mode. (127,0,0,1,4,210)\r\n",
    );
    assert_eq!(do_pasv(&mut s), (1234, Some(b"127.0.0.1".to_vec())));
    assert_eq!(&*sent.borrow(), b"EPSV\r\nPASV\r\n");

    let (mut s, sent) = scripted(b"229 ok (|||2121|)\r\n");
    assert_eq!(do_pasv(&mut s), (2121, None));
    assert_eq!(&*sent.borrow(), b"EPSV\r\n");

    let (mut s, _) = scripted(b"500 no\r\n500 no\r\n");
    assert_eq!(do_pasv(&mut s).0, 0);
}

#[test]
fn recursive_mkdir_walks_up_then_creates_each_level() {
    // ext/standard/tests/streams/bug77680.phpt: every CWD fails.
    let (mut s, sent) =
        scripted(b"550 a\r\n550 b\r\n550 c\r\n550 d\r\n257 e\r\n257 f\r\n257 g\r\n257 h\r\n");
    assert_eq!(
        mkdir_recursive(&mut s, b"/one/two/three/", REPORT_ERRORS),
        257
    );
    assert_eq!(
        String::from_utf8_lossy(&sent.borrow()),
        "CWD /one/two/three\r\nCWD /one/two\r\nCWD /one\r\nCWD /\r\n\
         MKD /\r\nMKD /one\r\nMKD /one/two\r\nMKD /one/two/three\r\n"
    );

    // An existing parent stops the walk; when the direct parent exists
    // the whole path is created at once.
    let (mut s, sent) = scripted(b"250 ok\r\n257 b\r\n");
    assert_eq!(mkdir_recursive(&mut s, b"/a/b/c", REPORT_ERRORS), 257);
    assert_eq!(
        String::from_utf8_lossy(&sent.borrow()),
        "CWD /a/b\r\nMKD /a/b/c\r\n"
    );
    let (mut s, sent) = scripted(b"550 a\r\n250 ok\r\n257 b\r\n257 c\r\n");
    assert_eq!(mkdir_recursive(&mut s, b"/a/b/c", REPORT_ERRORS), 257);
    assert_eq!(
        String::from_utf8_lossy(&sent.borrow()),
        "CWD /a/b\r\nCWD /a\r\nMKD /a/b\r\nMKD /a/b/c\r\n"
    );

    // A failing level warns with the server's reply.
    let _ = diag::take();
    let (mut s, sent) = scripted(b"550 x\r\n250 ok\r\n257 b\r\n550 denied\r\n");
    assert_eq!(mkdir_recursive(&mut s, b"/a/b/c", REPORT_ERRORS), 550);
    assert!(String::from_utf8_lossy(&sent.borrow()).ends_with("MKD /a/b/c\r\n"));
    let msgs: Vec<String> = diag::take().iter().map(diag::Diagnostic::text).collect();
    assert_eq!(msgs, ["550 denied\r\n"]);
}

#[test]
fn dirent_names() {
    assert_eq!(dirent_name(b"file1\r\n"), b"file1");
    assert_eq!(dirent_name(b"\n"), b"");
    assert_eq!(dirent_name(b"/"), b"");
    assert_eq!(dirent_name(b"dir/b0rk \t\r\n"), b"b0rk");
    assert_eq!(dirent_name(b"a\0b\n"), b"a");
}

#[test]
fn dir_stream_reads_nlst_lines() {
    let (data, _) = scripted(b"file1\r\n\nb0rk\r\n/");
    let mut dir = Stream::new(
        Box::new(FtpDirOps {
            data,
            control: None,
        }),
        "r",
    );
    let mut names = Vec::new();
    while let Some(n) = dir.readdir() {
        names.push(String::from_utf8_lossy(&n).into_owned());
    }
    assert_eq!(names, ["file1", "", "b0rk", ""]);
    assert_eq!(dir.ops_label(), "ftpdir");
}

#[test]
fn mdtm_fields() {
    assert_eq!(scan_mdtm(b"20230102030405"), Some([2023, 1, 2, 3, 4, 5]));
    assert_eq!(scan_mdtm(b"2023 1 2 3 4 5"), Some([2023, 1, 2, 3, 4, 5]));
    assert_eq!(scan_mdtm(b"2023"), None);
    assert_eq!(mdtm_time(b"213 garbage"), None);
    assert!(mdtm_time(b"213 19700101000000\r\n").is_some());
}

#[test]
fn closing_a_transfer_checks_the_result_and_quits() {
    let _ = diag::take();
    let (control, sent) = scripted(b"226 Closing data Connection.\r\n");
    close_transfer(control, true);
    assert_eq!(&*sent.borrow(), b"QUIT\r\n");
    assert!(diag::take().is_empty());

    let (control, sent) = scripted(b"451 boom\r\n");
    close_transfer(control, true);
    assert_eq!(&*sent.borrow(), b"QUIT\r\n");
    let msgs: Vec<String> = diag::take().iter().map(diag::Diagnostic::text).collect();
    assert_eq!(msgs, ["FTP server error 451:451 boom\r\n"]);

    // The closer runs after the data stream's own close.
    let (control, sent) = scripted(b"226 ok\r\n");
    let (mut data, _) = scripted(b"");
    data.set_wrapper_closer(Box::new(move || close_transfer(control, true)));
    data.close();
    assert_eq!(&*sent.borrow(), b"QUIT\r\n");
}

#[test]
fn open_mode_checks() {
    let err = open_url(b"ftp://127.0.0.1:1/x", "r+", 0, None).unwrap_err();
    assert_eq!(
        err.messages,
        [b"FTP does not support simultaneous read/write connections".to_vec()]
    );
    let err = open_url(b"ftp://127.0.0.1:1/x", "x", 0, None).unwrap_err();
    assert_eq!(err.messages, [b"Unknown file open mode".to_vec()]);
    let ctx = Rc::new(RefCell::new(crate::context::StreamContext::default()));
    ctx.borrow_mut()
        .set("ftp", "proxy", Value::str("tcp://127.0.0.1:1"));
    let err = open_url(b"ftp://127.0.0.1:1/x", "w", 0, Some(&ctx)).unwrap_err();
    assert_eq!(
        err.messages,
        [b"FTP proxy may only be used in read mode".to_vec()]
    );
}

#[test]
fn registered_for_ftp_and_ftps() {
    crate::wrapper::reset_volatile();
    let ftp = crate::wrapper::find(b"ftp").unwrap();
    let ftps = crate::wrapper::find(b"ftps").unwrap();
    assert!(Rc::ptr_eq(&ftp, &ftps));
    assert_eq!(ftp.label(), "ftp");
    assert!(ftp.is_url());
    assert!(ftp.has_dir_opener() && ftp.has_unlink() && ftp.has_rename());
}
