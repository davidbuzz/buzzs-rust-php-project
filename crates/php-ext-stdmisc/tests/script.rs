//! End-to-end tests: PHP scripts run on a VM with the `standard` misc
//! extension.

#![allow(clippy::needless_raw_string_hashes)]

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

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

fn run(code: &str) -> String {
    let sink = Sink::default();
    let ext = php_ext_stdmisc::StdMisc;
    let exts: [&dyn Extension; 1] = [&ext];
    let mut vm = Vm::with_extensions(&VmOptions::default(), Box::new(sink.clone()), &exts);
    vm.run_script(format!("<?php {code}").as_bytes(), "test.php");
    drop(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn crypt_and_password() {
    let out = run(r#"echo crypt("rasmuslerdorf", '$1$rasmusle$'), '|';
        var_dump(password_verify("x", '$2y$04$usesomesillystringfore7hnbRJHxXVLeakoG8K30oukPsA.ztMG'));
        echo password_get_info('$2y$10$abcdefghijklmnopqrstuuG1eFXJp.XcXo5hn7rRYoB5S6Swl8yFG')['algoName'], '|', PASSWORD_BCRYPT;"#);
    assert_eq!(
        out,
        "$1$rasmusle$rISCgZzpwk3UhDidwXvin0|bool(false)\nbcrypt|2y"
    );
}

#[test]
fn shell_escaping_and_exec() {
    let out = run(
        r#"echo escapeshellarg("it's"), '|', escapeshellcmd("a;b"), '|';
        $r = exec("printf 'a\nb  \n'", $lines, $code);
        echo $r, '|', implode(',', $lines), '|', $code, '|';
        system("echo sys", $c2); echo $c2;"#,
    );
    assert_eq!(out, "'it'\\''s'|a\\;b|b|a,b|0|sys\n0");
}

#[test]
fn query_strings() {
    let out = run(
        r#"echo http_build_query(['a' => [1, 2], 'b' => 'x y'], 'n', ';', PHP_QUERY_RFC3986), '|';
        parse_str("x[]=1&x[]=2&y=a+b", $r); echo $r['x'][1], $r['y'];"#,
    );
    assert_eq!(out, "a%5B0%5D=1;a%5B1%5D=2;b=x%20y|2a b");
}

#[test]
fn network_conversions() {
    let out = run(
        r#"var_dump(ip2long("10.0.0.1"), long2ip(167772161), inet_ntop(inet_pton("::1")), ip2long("1.2.3"));"#,
    );
    assert_eq!(
        out,
        "int(167772161)\nstring(8) \"10.0.0.1\"\nstring(3) \"::1\"\nbool(false)\n"
    );
}

#[test]
fn environment() {
    let out = run(r#"putenv("PHPRUST_T=1"); echo getenv("PHPRUST_T"), '|';
        try { putenv("=x"); } catch (ValueError $e) { echo $e->getMessage(), '|'; }
        echo php_uname('s'), '|', is_int(getmypid()) ? 'pid' : '-';"#);
    assert_eq!(
        out,
        "1|putenv(): Argument #1 ($assignment) must have a valid syntax|Linux|pid"
    );
}

#[test]
fn highlighting() {
    let out = run(r#"echo highlight_string('<?php echo 1; ?>', true);"#);
    assert!(
        out.starts_with(
            "<pre><code style=\"color: #000000\"><span style=\"color: #0000BB\">&lt;?php "
        ),
        "{out}"
    );
    assert!(out.ends_with("</code></pre>"), "{out}");
}

#[test]
fn setlocale_rest_args_are_checked_lazily() {
    let out = run(r#"echo setlocale(LC_ALL, "C", ["ignored"]), '|';
        try { setlocale(LC_ALL, "zz_ZZ.nope", ["C"]); } catch (TypeError $e) { echo $e->getMessage(); }"#);
    assert_eq!(
        out,
        "C|setlocale(): Argument #3 must be of type ?string, array given"
    );
}

#[test]
fn ini_functions() {
    let out = run(
        r#"$a = parse_ini_string("x = 1\n[s]\ny[] = on\n", true, INI_SCANNER_TYPED);
        var_dump($a['x'], $a['s']['y'][0]);
        echo isset(ini_get_all(null, false)['precision']) ? 'ok' : 'missing';"#,
    );
    assert_eq!(out, "int(1)\nbool(true)\nok");
}

#[test]
fn credits_and_info() {
    let out = run(r#"phpcredits(CREDITS_GROUP); phpinfo(INFO_LICENSE);"#);
    assert!(out.starts_with("PHP Credits\n"), "{out}");
    assert!(out.contains(" PHP Group "), "{out}");
    assert!(out.contains("PHP is free software."), "{out}");
}

#[test]
fn getopt_without_argv_options() {
    let out = run(r#"var_dump(getopt("a:", ["long"], $rest), $rest);"#);
    assert!(out.ends_with("int(1)\n"), "{out}");
}
