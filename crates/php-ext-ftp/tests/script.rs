// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! End-to-end tests: PHP scripts using the `ftp` extension against a
//! scripted FTP server thread. Expected outputs match `/usr/bin/php`.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::thread;

use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};

/// A `Write` sink shared with the test.
#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Runs `code` (without `<?php`) and returns its output.
fn run(code: &str) -> String {
    let sink = Sink::default();
    let (streams, arr, ftp) = (
        php_ext_streams::Streams,
        php_ext_stdarr::StdArray,
        php_ext_ftp::Ftp,
    );
    let exts: [&dyn Extension; 3] = [&streams, &arr, &ftp];
    let opts = VmOptions { ini: vec![] };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

/// A one-connection server: answers PORT/RETR/QUIT itself and the rest
/// from a fixed table.
#[allow(clippy::many_single_char_names)]
fn server() -> (u16, thread::JoinHandle<()>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let h = thread::spawn(move || {
        let (s, _) = l.accept().unwrap();
        let mut out = s.try_clone().unwrap();
        out.write_all(b"220 ready\r\n").unwrap();
        let mut data_addr = String::new();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        while r.read_line(&mut line).unwrap_or(0) > 0 {
            let cmd = line.trim_end().to_string();
            line.clear();
            let reply: &str = match cmd.as_str() {
                "USER user" => "331 need password\r\n",
                "PASS pass" => "230 logged in\r\n",
                "PASS bad" => "530 Login incorrect\r\n",
                "PWD" => "257 \"/srv\" is current\r\n",
                "TYPE I" | "TYPE A" => "200 ok\r\n",
                "QUIT" => {
                    out.write_all(b"221 bye\r\n").unwrap();
                    break;
                }
                c if c.starts_with("PORT ") => {
                    let n: Vec<u16> = c[5..].split(',').map(|x| x.parse().unwrap()).collect();
                    data_addr =
                        format!("{}.{}.{}.{}:{}", n[0], n[1], n[2], n[3], (n[4] << 8) | n[5]);
                    "200 port ok\r\n"
                }
                "RETR story" => {
                    out.write_all(b"150 opening\r\n").unwrap();
                    let mut d = TcpStream::connect(&data_addr).unwrap();
                    d.write_all(b"line1\r\nline2\r\n").unwrap();
                    drop(d);
                    "226 done\r\n"
                }
                _ => "550 nope\r\n",
            };
            out.write_all(reply.as_bytes()).unwrap();
        }
    });
    (port, h)
}

#[test]
fn session_transfer_and_errors() {
    let (port, h) = server();
    let out = run(&format!(
        r#"$f = ftp_connect('127.0.0.1', {port});
var_dump($f);
var_dump(ftp_login($f, 'user', 'bad'));
var_dump(ftp_login($f, 'user', 'pass'));
var_dump(ftp_pwd($f));
$m = fopen('php://memory', 'w+');
var_dump(ftp_fget($f, $m, 'story', FTP_ASCII));
rewind($m); var_dump(stream_get_contents($m));
try {{ ftp_fget($f, $m, 'story', 3); }} catch (ValueError $e) {{ echo $e->getMessage(), "\n"; }}
try {{ ftp_set_option($f, FTP_TIMEOUT_SEC, 'x'); }} catch (TypeError $e) {{ echo $e->getMessage(), "\n"; }}
try {{ ftp_set_option($f, 99, 1); }} catch (ValueError $e) {{ echo $e->getMessage(), "\n"; }}
var_dump(ftp_set_option($f, FTP_AUTOSEEK, false), ftp_get_option($f, FTP_AUTOSEEK), ftp_get_option($f, FTP_TIMEOUT_SEC));
var_dump(ftp_nb_continue($f));
var_dump(ftp_delete($f, 'x'));
var_dump(ftp_close($f), ftp_close($f));
try {{ ftp_pwd($f); }} catch (ValueError $e) {{ echo $e->getMessage(), "\n"; }}
try {{ new FTP\Connection(); }} catch (Error $e) {{ echo $e->getMessage(), "\n"; }}
try {{ ftp_connect('127.0.0.1', 21, 0); }} catch (ValueError $e) {{ echo $e->getMessage(), "\n"; }}
"#
    ));
    h.join().unwrap();
    assert_eq!(
        out,
        r#"object(FTP\Connection)#1 (0) {
}

Warning: ftp_login(): Login incorrect in test.php on line 3
bool(false)
bool(true)
string(4) "/srv"
bool(true)
string(12) "line1
line2
"
ftp_fget(): Argument #4 ($mode) must be either FTP_ASCII or FTP_BINARY
ftp_set_option(): Argument #3 ($value) must be of type int for the FTP_TIMEOUT_SEC option, string given
ftp_set_option(): Argument #2 ($option) must be one of FTP_TIMEOUT_SEC, FTP_AUTOSEEK, or FTP_USEPASVADDRESS
bool(true)
bool(false)
int(90)

Warning: ftp_nb_continue(): No non-blocking transfer to continue in test.php on line 13
int(0)

Warning: ftp_delete(): nope in test.php on line 14
bool(false)
bool(true)
bool(true)
FTP\Connection is already closed
Cannot directly construct FTP\Connection, use ftp_connect() or ftp_ssl_connect() instead
ftp_connect(): Argument #3 ($timeout) must be greater than 0
"#
    );
}
