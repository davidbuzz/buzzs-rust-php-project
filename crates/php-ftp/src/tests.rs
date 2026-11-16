// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests against a scripted in-process FTP server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use super::*;

/// A tiny FTP server: greets, then answers each command line with
/// `handler(cmd, &mut ctx)` until QUIT or EOF.
struct Ctx {
    out: TcpStream,
    /// Address from the last PORT command.
    port_addr: Option<String>,
    /// Listener from the last PASV command.
    pasv: Option<TcpListener>,
    /// Data received by the last STOR.
    stored: Vec<u8>,
}

impl Ctx {
    fn reply(&mut self, s: &str) {
        self.out.write_all(s.as_bytes()).unwrap();
    }

    fn data_conn(&mut self) -> TcpStream {
        if let Some(l) = self.pasv.take() {
            l.accept().unwrap().0
        } else {
            TcpStream::connect(self.port_addr.clone().unwrap()).unwrap()
        }
    }

    fn send_data(&mut self, payload: &[u8]) {
        self.reply("150 opening\r\n");
        let mut d = self.data_conn();
        d.write_all(payload).unwrap();
        drop(d);
        self.reply("226 done\r\n");
    }
}

fn serve(handler: fn(&str, &mut Ctx)) -> (u16, thread::JoinHandle<Vec<u8>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let mut ctx = Ctx {
            out: s.try_clone().unwrap(),
            port_addr: None,
            pasv: None,
            stored: Vec::new(),
        };
        ctx.reply("220-multi\r\n220 ready\r\n");
        let mut r = BufReader::new(s);
        let mut line = String::new();
        while r.read_line(&mut line).unwrap_or(0) > 0 {
            let cmd = line.trim_end_matches("\r\n").to_string();
            line.clear();
            if let Some(rest) = cmd.strip_prefix("PORT ") {
                let n: Vec<u16> = rest.split(',').map(|x| x.parse().unwrap()).collect();
                ctx.port_addr = Some(format!(
                    "{}.{}.{}.{}:{}",
                    n[0],
                    n[1],
                    n[2],
                    n[3],
                    (n[4] << 8) | n[5]
                ));
                ctx.reply("200 PORT ok\r\n");
            } else if cmd == "PASV" {
                let pl = TcpListener::bind("127.0.0.1:0").unwrap();
                let p = pl.local_addr().unwrap().port();
                ctx.pasv = Some(pl);
                ctx.reply(&format!(
                    "227 Entering Passive Mode (127,0,0,1,{},{})\r\n",
                    p >> 8,
                    p & 255
                ));
            } else if cmd.starts_with("TYPE ") {
                ctx.reply("200 type ok\r\n");
            } else if let Some(name) = cmd.strip_prefix("STOR ") {
                let _ = name;
                ctx.reply("150 go\r\n");
                let mut d = ctx.data_conn();
                let mut buf = Vec::new();
                d.read_to_end(&mut buf).unwrap();
                ctx.stored = buf;
                ctx.reply("226 stored\r\n");
            } else if cmd == "QUIT" {
                ctx.reply("221 bye\r\n");
                break;
            } else {
                handler(&cmd, &mut ctx);
            }
        }
        ctx.stored
    });
    (port, h)
}

fn basic(cmd: &str, ctx: &mut Ctx) {
    match cmd {
        "USER user" => ctx.reply("331 need pass\r\n"),
        "PASS pass" => ctx.reply("230 in\r\n"),
        "PWD" => ctx.reply("257 \"/home/x\" is cwd\r\n"),
        "SYST" => ctx.reply("215   UNIX Type: L8\r\n"),
        "HELP" => ctx.reply("214-help\r\n USER\r\n214 end\r\n"),
        "MKD plain" => ctx.reply("257 OK.\r\n"),
        "SIZE f" => ctx.reply("213 5368709120\r\n"),
        "MDTM f" => ctx.reply("213 19980615100045\r\n"),
        "SITE CHMOD 644 f" => ctx.reply("200 ok\r\n"),
        "ALLO 10" => ctx.reply("200 10 bytes allocated\r\n"),
        "RETR text" => ctx.send_data(b"a\r\nb\rc\r\n"),
        "RETR bin" => ctx.send_data(b"a\r\n\0"),
        "NLST" => ctx.send_data(b"one\r\ntwo\r\nlost"),
        "MLSD" => ctx.send_data(b"type=file;size=3; f\r\nbad\r\n"),
        "NLST empty" => ctx.reply("226 nothing\r\n"),
        _ => ctx.reply("500 unknown\r\n"),
    }
}

fn login(port: u16) -> Ftp {
    let mut f = Ftp::open("127.0.0.1", port, 5).unwrap();
    assert!(f.login(b"user", b"pass"));
    f
}

#[test]
fn session_commands() {
    let (port, h) = serve(basic);
    let mut f = login(port);
    assert_eq!(f.pwd().as_deref(), Some(&b"/home/x"[..]));
    assert_eq!(f.syst().as_deref(), Some(&b"UNIX"[..]));
    assert_eq!(
        f.raw(b"HELP").unwrap(),
        vec![b"214-help".to_vec(), b" USER".to_vec(), b"214 end".to_vec()]
    );
    assert_eq!(f.mkdir(b"plain").as_deref(), Some(&b"plain"[..]));
    assert_eq!(f.size(b"f"), 5_368_709_120);
    assert_eq!(f.mdtm(b"f"), 897_904_845);
    assert!(f.chmod(0o644, b"f"));
    assert_eq!(f.alloc(10), (true, Some(b"10 bytes allocated".to_vec())));
    assert!(!f.chdir(b"x"));
    assert_eq!(f.last_reply(), b"unknown");
    assert_eq!(f.last_code(), 500);
    // CR/LF injection is refused before anything is sent.
    assert!(!f.delete(b"a\r\nQUIT"));
    assert_eq!(f.last_reply(), b"unknown");
    assert!(f.quit());
    drop(f);
    h.join().unwrap();
}

#[test]
fn active_and_passive_downloads() {
    let (port, h) = serve(basic);
    let mut f = login(port);
    let mut out = Vec::new();
    assert!(f.get(&mut out, b"text", TransferType::Ascii, 0));
    assert_eq!(out, b"a\nb\rc\n");
    assert!(f.pasv(true));
    let mut out = Vec::new();
    assert!(f.get(&mut out, b"bin", TransferType::Image, 0));
    assert_eq!(out, b"a\r\n\0");
    assert_eq!(
        f.nlist(b"").unwrap(),
        vec![b"one".to_vec(), b"two".to_vec()]
    );
    assert_eq!(f.nlist(b"empty").unwrap(), Vec::<Vec<u8>>::new());
    let lines = f.mlsd(b"").unwrap();
    assert_eq!(lines.len(), 2);
    assert!(parse_mlsd_line(&lines[0]).is_ok());
    assert_eq!(parse_mlsd_line(&lines[1]), Err(MlsdError::MissingPathname));
    assert!(f.quit());
    drop(f);
    h.join().unwrap();
}

#[test]
fn nonblocking_download_and_ascii_upload() {
    let (port, h) = serve(basic);
    let mut f = login(port);
    let mut out = Vec::new();
    let mut st = f.nb_get(&mut out, b"text", TransferType::Ascii, 0);
    while st == NbStatus::MoreData {
        st = f.nb_continue_read(&mut out);
    }
    assert_eq!(st, NbStatus::Finished);
    assert_eq!(out, b"a\nb\rc\n");
    assert!(!f.nb_active());
    let mut src = SliceSource::new(&b"x\ny\n"[..]);
    assert!(f.put(b"up", &mut src, TransferType::Ascii, 0));
    assert!(f.quit());
    drop(f);
    assert_eq!(h.join().unwrap(), b"x\r\ny\r\n");
}

#[test]
fn greeting_must_be_220() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        s.write_all(b"421 go away\r\n").unwrap();
    });
    assert!(Ftp::open("127.0.0.1", port, 5).is_err());
    h.join().unwrap();
}

#[test]
fn overlong_reply_line_is_truncated() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        s.write_all(b"220 hi\r\n").unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        s.write_all(format!("\r\n{}\r\n", "*".repeat(4096)).as_bytes())
            .unwrap();
        let _ = r.read_line(&mut line);
    });
    let mut f = Ftp::open("127.0.0.1", port, 5).unwrap();
    assert!(f.syst().is_none());
    assert_eq!(f.last_reply().len(), 4095);
    drop(f);
    h.join().unwrap();
}
