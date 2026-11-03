//! The `files` save handler: on-disk layout, locking, GC and messages
//! (`mod_files.c`, `ext/session/tests/mod_files/*.phpt`), plus interop
//! with `/usr/bin/php` when it is installed.
#![allow(clippy::too_many_lines)]

mod common;

use common::{s, show, tempdir, FakeHost};
use php_runtime::Value;
use php_session::api::*;
use php_session::ini;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

fn host(save_path: &str) -> FakeHost {
    let mut h = FakeHost::with_ini(&[
        ("session.use_strict_mode", "0"),
        ("session.gc_probability", "0"),
    ]);
    assert_eq!(
        h.call("ini_set", |h| ini::ini_set(
            h,
            "session.save_path",
            save_path.as_bytes()
        )),
        Some(true)
    );
    h
}

fn start_with_id(h: &mut FakeHost, id: &str) -> bool {
    h.call("session_id", |h| session_id(h, Some(&s(id))))
        .unwrap();
    h.call("session_start", |h| session_start(h, None)).unwrap()
}

#[test]
fn dirdepth_layout_and_mode() {
    let dir = tempdir("depth");
    std::fs::create_dir_all(dir.join("a/b")).unwrap();
    let mut h = host(&format!("2;0640;{}", dir.display()));
    assert!(start_with_id(&mut h, "abcdef"));
    h.set_var("k", Value::Int(7));
    assert!(h
        .call("session_write_close", |h| session_write_close(h))
        .unwrap());
    let file = dir.join("a/b/sess_abcdef");
    assert_eq!(std::fs::read(&file).unwrap(), b"k|i:7;");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        // The umask may clear bits, never add them.
        assert_eq!(mode & !0o640, 0, "{mode:o}");
    }
    // Shorter data truncates the file.
    assert!(start_with_id(&mut h, "abcdef"));
    h.set_var("k", Value::Int(1));
    h.call("session_unset", |h| session_unset(h)).unwrap();
    h.set_var("z", Value::Bool(true));
    assert!(h
        .call("session_write_close", |h| session_write_close(h))
        .unwrap());
    assert_eq!(std::fs::read(&file).unwrap(), b"z|b:1;");
    assert!(h.take_log().is_empty(), "{:?}", h.log);
}

#[test]
fn missing_directory_messages() {
    let dir = tempdir("missing");
    let gone = dir.join("nope");
    let mut h = host(&gone.to_string_lossy());
    assert!(!start_with_id(&mut h, "xyz"));
    assert_eq!(
        h.take_log(),
        vec![
            format!(
                "Warning: session_start(): open({}/sess_xxx, O_RDWR) failed: No such file or directory (2)",
                gone.display()
            )
            .replace("sess_xxx", "sess_xyz"),
            format!("Warning: session_start(): Failed to read session data: files (path: {})", gone.display()),
        ]
    );
    // Depth longer than the ID.
    let mut h = host(&format!("5;{}", dir.display()));
    assert!(!start_with_id(&mut h, "abc"));
    assert_eq!(
        h.take_log()[0],
        "Warning: session_start(): Failed to create session data file path. Too short session ID, invalid save_path or path length exceeds 4096 characters"
    );
    // Overflowing first parameter.
    let mut h = host(&format!("99999999999999999999;{}", dir.display()));
    assert!(!start_with_id(&mut h, "abc"));
    assert_eq!(
        h.take_log(),
        vec![
            "Warning: The first parameter in session.save_path is invalid".to_string(),
            format!(
                "Warning: session_start(): Failed to initialize storage module: files (path: 99999999999999999999;{})",
                dir.display()
            ),
        ]
    );
    let mut h = host(&format!("1;77777;{}", dir.display()));
    assert!(!start_with_id(&mut h, "abc"));
    assert_eq!(
        h.take_log()[0],
        "Warning: The second parameter in session.save_path is invalid"
    );
}

#[test]
fn gc_with_dirdepth_is_selective() {
    // mod_files/gc_dirdepth_selective.phpt
    let base = tempdir("gc");
    std::fs::create_dir_all(base.join("a")).unwrap();
    let now = SystemTime::now();
    let set_mtime = |p: &Path, age: u64| {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(now - Duration::from_secs(age)).unwrap();
    };
    let expired = base.join("a/sess_aexpired0000000000000000");
    let fresh = base.join("a/sess_afresh000000000000000000");
    let other = base.join("a/other_file");
    for (p, age) in [(&expired, 100), (&fresh, 1), (&other, 100)] {
        std::fs::write(p, b"user|s:5:\"alice\";").unwrap();
        set_mtime(p, age);
    }
    let mut h = host("");
    h.now = i64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    h.call("ini_set", |h| {
        ini::ini_set(h, "session.gc_maxlifetime", b"10")
    });
    h.call("session_save_path", |h| {
        session_save_path(h, Some(&s(&format!("1;{}", base.display()))))
    })
    .unwrap();
    assert!(start_with_id(&mut h, "a0000000000000000000000000"));
    let r = h.call("session_gc", |h| session_gc(h)).unwrap();
    assert_eq!(show(&r), "int(1)");
    assert!(h.call("session_destroy", |h| session_destroy(h)).unwrap());
    assert!(!expired.exists());
    assert!(fresh.exists());
    assert!(other.exists());
    // Probability-driven GC runs at start when probability == divisor.
    std::fs::write(&expired, b"").unwrap();
    set_mtime(&expired, 100);
    h.call("ini_set", |h| {
        ini::ini_set(h, "session.gc_probability", b"1")
    });
    h.call("ini_set", |h| ini::ini_set(h, "session.gc_divisor", b"1"));
    assert!(start_with_id(&mut h, "a0000000000000000000000000"));
    assert!(!expired.exists());
    // Without an active session.
    h.call("session_abort", |h| session_abort(h)).unwrap();
    assert_eq!(
        show(&h.call("session_gc", |h| session_gc(h)).unwrap()),
        "bool(false)"
    );
    assert_eq!(
        h.take_log(),
        vec!["Warning: session_gc(): Session cannot be garbage collected when there is no active session"]
    );
}

#[test]
fn lazy_write_touches_instead_of_writing() {
    let dir = tempdir("lazy");
    let mut h = host(&dir.to_string_lossy());
    assert!(start_with_id(&mut h, "lazy1"));
    h.set_var("a", Value::Int(1));
    h.call("session_write_close", |h| session_write_close(h))
        .unwrap();
    let file = dir.join("sess_lazy1");
    let old = SystemTime::now() - Duration::from_secs(1000);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(old)
        .unwrap();
    assert!(start_with_id(&mut h, "lazy1"));
    h.call("session_write_close", |h| session_write_close(h))
        .unwrap();
    let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
    assert!(mtime > old + Duration::from_secs(500));
    assert_eq!(std::fs::read(&file).unwrap(), b"a|i:1;");
}

#[test]
fn strict_mode_rejects_unknown_ids() {
    let dir = tempdir("strict");
    let mut h = host(&dir.to_string_lossy());
    h.call("ini_set", |h| {
        ini::ini_set(h, "session.use_strict_mode", b"1")
    });
    h.set_global("_COOKIE", &[("PHPSESSID", Value::string("unknownid"))]);
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    let id = h.state.id().unwrap().to_string_lossy();
    assert_eq!(id.len(), 32);
    assert_ne!(id, "unknownid");
    assert_eq!(
        h.headers[0],
        format!("Set-Cookie: PHPSESSID={id}; path=/; HttpOnly; SameSite=Lax")
    );
    h.call("session_write_close", |h| session_write_close(h))
        .unwrap();
    // An existing ID is accepted.
    let mut h2 = host(&dir.to_string_lossy());
    h2.call("ini_set", |h| {
        ini::ini_set(h, "session.use_strict_mode", b"1")
    });
    h2.set_global("_COOKIE", &[("PHPSESSID", Value::string(id.as_str()))]);
    assert!(h2
        .call("session_start", |h| session_start(h, None))
        .unwrap());
    assert_eq!(h2.state.id().unwrap().to_string_lossy(), id);
    // regenerate keeps the old file unless asked to delete it.
    assert!(h2
        .call("session_regenerate_id", |h| session_regenerate_id(h, false))
        .unwrap());
    let new_id = h2.state.id().unwrap().to_string_lossy();
    assert_ne!(new_id, id);
    assert!(dir.join(format!("sess_{id}")).exists());
    assert!(h2
        .call("session_regenerate_id", |h| session_regenerate_id(h, true))
        .unwrap());
    assert!(!dir.join(format!("sess_{new_id}")).exists());
    assert_eq!(
        h2.headers
            .iter()
            .filter(|l| l.starts_with("Set-Cookie"))
            .count(),
        1,
        "{:?}",
        h2.headers
    );
}

#[test]
fn open_basedir_denial() {
    // session_save_path_variation4.phpt
    let mut h = FakeHost::with_ini(&[]);
    h.temp = tempdir("basedir");
    h.deny_basedir = true;
    assert_eq!(
        h.call("ini_set", |h| ini::ini_set(
            h,
            "session.save_path",
            b"/elsewhere"
        )),
        Some(false)
    );
    assert!(!h.call("session_start", |h| session_start(h, None)).unwrap());
    assert!(!h.call("session_destroy", |h| session_destroy(h)).unwrap());
    let temp = h.temp.display().to_string();
    assert_eq!(
        h.take_log(),
        vec![
            "Warning: ini_set(): open_basedir restriction in effect. File(/elsewhere) is not within the allowed path(s): (.)".to_string(),
            format!("Warning: session_start(): open_basedir restriction in effect. File({temp}) is not within the allowed path(s): (.)"),
            "Warning: session_start(): Failed to initialize storage module: files (path: )".to_string(),
            "Warning: session_destroy(): Trying to destroy uninitialized session".to_string(),
        ]
    );
}

fn php() -> Option<Command> {
    if !Path::new("/usr/bin/php").exists() {
        return None;
    }
    let mut c = Command::new("/usr/bin/php");
    c.args([
        "-d",
        "memory_limit=1G",
        "-d",
        "session.use_strict_mode=0",
        "-d",
        "session.use_cookies=0",
        "-d",
        "session.cache_limiter=",
    ]);
    Some(c)
}

#[test]
fn interop_with_php() {
    if php().is_none() {
        return;
    }
    let dir = tempdir("interop");
    std::fs::create_dir_all(dir.join("i")).unwrap();
    let sp = format!("1;{}", dir.display());
    for (n, handler) in ["php", "php_binary", "php_serialize"]
        .into_iter()
        .enumerate()
    {
        let id = format!("interop{n}");
        // PHP writes, we read.
        let out = php()
            .unwrap()
            .args(["-d", &format!("session.save_path=\"{sp}\""), "-d", &format!("session.serialize_handler={handler}"), "-r"])
            .arg(format!("session_id('{id}'); session_start(); $o = new stdClass; $o->p = [1, 'x']; $_SESSION['obj'] = $o; $_SESSION['again'] = $o; $_SESSION['f'] = 1.5; $r = 'shared'; $_SESSION['r1'] = &$r; $_SESSION['r2'] = &$r; session_write_close();"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let on_disk = std::fs::read(dir.join(format!("i/sess_{id}")))
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stderr)));
        let mut h = host(&sp);
        h.call("ini_set", |h| {
            ini::ini_set(h, "session.serialize_handler", handler.as_bytes())
        });
        assert!(start_with_id(&mut h, &id));
        assert_eq!(h.session_len(), 5, "{handler}");
        // Writing it back unchanged reproduces PHP's bytes.
        let enc = h.call("session_encode", |h| session_encode(h)).unwrap();
        assert_eq!(
            enc.as_str().unwrap().as_bytes(),
            on_disk.as_slice(),
            "{handler}"
        );
        // We write, PHP reads.
        h.set_var("ours", Value::string("from rust"));
        assert!(h
            .call("session_write_close", |h| session_write_close(h))
            .unwrap());
        assert!(h.take_log().is_empty(), "{:?}", h.log);
        let out = php()
            .unwrap()
            .args(["-d", &format!("session.save_path=\"{sp}\""), "-d", &format!("session.serialize_handler={handler}"), "-r"])
            .arg(format!("session_id('{id}'); session_start(); echo $_SESSION['ours'], '|', $_SESSION['obj']->p[1], '|', $_SESSION['obj'] === $_SESSION['again'] ? 'same' : 'diff';"))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "from rust|x|same",
            "{handler}"
        );
    }
}

#[test]
fn upload_progress() {
    // rfc1867.phpt: cookie ID wins over GET/POST, cleanup=0 keeps the record.
    use php_session::rfc1867;
    let dir = tempdir("rfc1867");
    let mut h = FakeHost::new();
    let path = dir.to_string_lossy().into_owned();
    let cfg = [
        ("session.save_path", path.as_str()),
        ("session.use_strict_mode", "0"),
        ("session.use_only_cookies", "0"),
        ("session.upload_progress.cleanup", "0"),
        ("session.upload_progress.min_freq", "0.000000001"),
    ];
    let map: std::collections::HashMap<String, Vec<u8>> = cfg
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.as_bytes().to_vec()))
        .collect();
    php_session::module_startup(&mut h, &|n| map.get(n).cloned());
    h.take_log();
    h.state.set_modules_activated(false);
    h.set_global("_COOKIE", &[("PHPSESSID", Value::string("rfc1867"))]);
    h.set_global("_GET", &[("PHPSESSID", Value::string("rfc1867-get"))]);
    let mut p = rfc1867::start(&mut h, 600).unwrap();
    assert!(rfc1867::formdata(
        &mut h,
        &mut p,
        b"PHPSESSID",
        b"rfc1867-post"
    ));
    assert!(rfc1867::formdata(
        &mut h,
        &mut p,
        b"PHP_SESSION_UPLOAD_PROGRESS",
        b"rfc1867.php"
    ));
    assert!(rfc1867::file_start(
        &mut h,
        &mut p,
        b"file1",
        b"file1.txt",
        300
    ));
    assert!(rfc1867::file_data(&mut h, &mut p, 0, 1, 350));
    assert!(rfc1867::file_end(
        &mut h,
        &mut p,
        Some(b"/tmp/php1"),
        0,
        360
    ));
    assert!(rfc1867::file_start(
        &mut h,
        &mut p,
        b"file2",
        b"file2.txt",
        450
    ));
    assert!(rfc1867::file_data(&mut h, &mut p, 0, 1, 500));
    assert!(rfc1867::file_end(
        &mut h,
        &mut p,
        Some(b"/tmp/php2"),
        0,
        510
    ));
    rfc1867::end(&mut h, p, 600);
    assert!(h.session_global.is_none());
    assert!(h.take_log().is_empty(), "{:?}", h.log);
    // The request proper.
    php_session::request_startup(&mut h).unwrap();
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    assert_eq!(h.state.id().unwrap().as_bytes(), b"rfc1867");
    let sess = h.session();
    let rec = sess
        .as_array()
        .unwrap()
        .get_str(b"upload_progress_rfc1867.php")
        .unwrap()
        .clone();
    let rec = rec.as_array().unwrap();
    let keys: Vec<Vec<u8>> = rec.keys().map(php_runtime::ArrayKey::to_bytes).collect();
    assert_eq!(
        keys,
        [
            &b"start_time"[..],
            b"content_length",
            b"bytes_processed",
            b"done",
            b"files"
        ]
    );
    assert_eq!(show(rec.get_str(b"done").unwrap()), "bool(true)");
    assert_eq!(show(rec.get_str(b"bytes_processed").unwrap()), "int(600)");
    let files = rec.get_str(b"files").unwrap().as_array().unwrap().clone();
    assert_eq!(files.len(), 2);
    let f1 = files.get_int(0).unwrap().as_array().unwrap().clone();
    let keys: Vec<Vec<u8>> = f1.keys().map(php_runtime::ArrayKey::to_bytes).collect();
    assert_eq!(
        keys,
        [
            &b"field_name"[..],
            b"name",
            b"tmp_name",
            b"error",
            b"done",
            b"start_time",
            b"bytes_processed"
        ]
    );
    assert_eq!(
        show(f1.get_str(b"tmp_name").unwrap()),
        "string(9) \"/tmp/php1\""
    );
    assert_eq!(show(f1.get_str(b"bytes_processed").unwrap()), "int(1)");
    // Cancelling: the user sets cancel_upload in the record.
    h.call("session_write_close", |h| session_write_close(h))
        .unwrap();
    let data = std::fs::read(dir.join("sess_rfc1867")).unwrap();
    let mut patched = String::from_utf8(data).unwrap();
    patched = patched.replace(
        "s:4:\"done\";b:1;s:5:\"files\"",
        "s:13:\"cancel_upload\";b:1;s:4:\"done\";b:1;s:5:\"files\"",
    );
    patched = patched.replace("a:5:{s:10:\"start_time\"", "a:6:{s:10:\"start_time\"");
    assert!(patched.contains("a:6:"), "{patched}");
    std::fs::write(dir.join("sess_rfc1867"), patched).unwrap();
    php_session::request_shutdown(&mut h).unwrap();
    h.state.set_modules_activated(false);
    let mut p = rfc1867::start(&mut h, 600).unwrap();
    assert!(rfc1867::formdata(
        &mut h,
        &mut p,
        b"PHP_SESSION_UPLOAD_PROGRESS",
        b"rfc1867.php"
    ));
    assert!(!rfc1867::file_start(
        &mut h,
        &mut p,
        b"file1",
        b"file1.txt",
        300
    ));
}
