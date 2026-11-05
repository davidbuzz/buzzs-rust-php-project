//! End-to-end tests: PHP snippets run on a VM with this extension.

use super::Pdo;
use php_vm::ext::Extension;
use php_vm::{Vm, VmOptions};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

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

/// Runs `code` (without the `<?php` tag) and returns its output.
fn run(code: &str) -> String {
    let sink = Sink::default();
    let streams = php_ext_streams::Streams;
    let ext = Pdo;
    let exts: [&dyn Extension; 2] = [&streams, &ext];
    let opts = VmOptions { ini: Vec::new() };
    let mut vm = Vm::with_extensions(&opts, Box::new(sink.clone()), &exts);
    let src = format!("<?php\n{code}");
    vm.run_script(src.as_bytes(), "test.php");
    // The streams extension's STDIN/STDOUT/STDERR resources close the
    // process descriptors when dropped, which would take the test
    // harness's output with them: keep the VM alive.
    std::mem::forget(vm);
    let out = sink.0.borrow().clone();
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn pdo_query_and_fetch_modes() {
    let out = run(r#"
        $db = new PDO('sqlite::memory:');
        $db->exec('CREATE TABLE t(id int, name text)');
        $db->exec("INSERT INTO t VALUES(1,'a'),(2,'b')");
        foreach ($db->query('SELECT * FROM t', PDO::FETCH_NUM) as $k => $r) {
            echo $k, ':', implode(',', $r), ';';
        }
        $s = $db->prepare('SELECT name FROM t WHERE id = ?');
        $s->execute([2]);
        echo $s->fetchColumn(), ';', $s->queryString;
    "#);
    assert_eq!(out, "0:1,a;1:2,b;b;SELECT name FROM t WHERE id = ?");
}

#[test]
fn pdo_exception_carries_error_info() {
    let out = run(r#"
        $db = new PDO('sqlite::memory:', null, null, [PDO::ATTR_ERRMODE => PDO::ERRMODE_EXCEPTION]);
        try { $db->query('SELECT * FROM nope'); }
        catch (PDOException $e) {
            echo get_class($e), '|', $e->getCode(), '|', $e->errorInfo[2], '|', $e instanceof RuntimeException ? 'rt' : 'x';
        }
    "#);
    assert_eq!(out, "PDOException|HY000|no such table: nope|rt");
}

#[test]
fn bound_column_and_param_references() {
    let out = run(r#"
        $db = new Pdo\Sqlite('sqlite::memory:');
        $db->exec('CREATE TABLE t(v int)');
        $ins = $db->prepare('INSERT INTO t VALUES(?)');
        $ins->bindParam(1, $x);
        foreach ([3, 4] as $x) { $ins->execute(); }
        $s = $db->query('SELECT v FROM t');
        $s->bindColumn(1, $v);
        while ($s->fetch(PDO::FETCH_BOUND)) { echo $v; }
        echo get_class($db), PDO::getAvailableDrivers()[0];
    "#);
    assert_eq!(out, "34Pdo\\Sqlitesqlite");
}

#[test]
fn sqlite_udf_and_clone_refusal() {
    let out = run(r#"
        $db = new Pdo\Sqlite('sqlite::memory:');
        $db->createFunction('twice', fn($v) => $v * 2);
        echo $db->query('SELECT twice(21)')->fetchColumn(), ';';
        try { clone $db; } catch (Error $e) { echo $e->getMessage(); }
    "#);
    assert_eq!(
        out,
        "42;Trying to clone an uncloneable object of class Pdo\\Sqlite"
    );
}

#[test]
fn sqlite3_classes() {
    let out = run(r#"
        $db = new SQLite3(':memory:');
        $db->exec('CREATE TABLE x(a)');
        $st = $db->prepare('INSERT INTO x VALUES(:a)');
        $st->bindValue(':a', 7, SQLITE3_INTEGER);
        $st->execute();
        $r = $db->query('SELECT a FROM x');
        echo $r->fetchArray(SQLITE3_NUM)[0], ';', $db->querySingle('SELECT count(*) FROM x'), ';';
        $db->enableExceptions(true);
        try { $db->exec('bogus'); } catch (SQLite3Exception $e) { echo get_class($e); }
    "#);
    assert_eq!(out, "7;1;SQLite3Exception");
}

#[test]
fn pdorow_cannot_be_constructed() {
    let out = run(r#"
        try { new PDORow(); } catch (PDOException $e) { echo $e->getMessage(); }
    "#);
    assert_eq!(out, "You may not create a PDORow manually");
}

/// pdo_035: `&$row[0]` reads the column with the "Indirect modification"
/// notice; `&$row[]` throws from `row_dim_read`.
#[test]
fn pdorow_dimension_fetch_by_reference() {
    let out = run(r#"
        set_error_handler(function ($no, $msg) { echo $no, ':', $msg, ';'; return true; });
        $db = new PDO('sqlite::memory:');
        $row = $db->query('SELECT 1 AS a')->fetch(PDO::FETCH_LAZY);
        try { $r = &$row[0]; echo 'ok;'; } catch (Error $e) { echo $e->getMessage(), ';'; }
        try { $r = &$row[]; } catch (Error $e) { echo $e->getMessage(), ';'; }
        echo $row[0];
    "#);
    assert_eq!(
        out,
        "8:Indirect modification of overloaded element of PDORow has no effect;ok;\
         Cannot append to PDORow offset;1"
    );
}

#[test]
fn blob_stream_resource() {
    let out = run(r#"
        $db = new SQLite3(':memory:');
        $db->exec("CREATE TABLE b(d BLOB); INSERT INTO b VALUES('hello')");
        $s = $db->openBlob('b', 'd', 1);
        echo get_resource_type($s), ':', stream_get_contents($s);
    "#);
    assert_eq!(out, "stream:hello");
}
