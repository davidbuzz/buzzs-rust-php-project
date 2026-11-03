//! Unit tests; the oracle tests compare against `/usr/bin/php` when it is
//! installed (and are skipped otherwise).

#![allow(clippy::format_push_string, clippy::format_collect)]
use super::*;
use std::fmt::Write as _;
use std::os::unix::ffi::OsStringExt;
use std::process::Command;

const ORACLE: &str = "/usr/bin/php";

/// Runs PHP code in the oracle, returning stdout (or `None` when there is
/// no oracle with ext/posix).
fn php(code: &str) -> Option<String> {
    if !std::path::Path::new(ORACLE).exists() {
        return None;
    }
    let out = Command::new(ORACLE)
        .args([
            "-n",
            "-d",
            "memory_limit=1G",
            "-d",
            "extension=posix",
            "-r",
            code,
        ])
        .output()
        .ok()?;
    let out2;
    let out = if String::from_utf8_lossy(&out.stdout).contains("Call to undefined function") {
        out2 = Command::new(ORACLE)
            .args(["-d", "memory_limit=1G", "-r", code])
            .output()
            .ok()?;
        &out2
    } else {
        &out
    };
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn dump_str(out: &mut String, s: &[u8]) {
    let _ = write!(
        out,
        "string({}) \"{}\"",
        s.len(),
        String::from_utf8_lossy(s)
    );
}

fn dump_field(out: &mut String, f: &Field, indent: usize) {
    let pad = " ".repeat(indent);
    match f {
        Field::Str(s) => dump_str(out, s),
        Field::Int(i) => {
            let _ = write!(out, "int({i})");
        }
        Field::Null => out.push_str("NULL"),
        Field::List(items) => {
            let _ = writeln!(out, "array({}) {{", items.len());
            for (i, s) in items.iter().enumerate() {
                let _ = write!(out, "{pad}  [{i}]=>\n{pad}  ");
                dump_str(out, s);
                out.push('\n');
            }
            let _ = write!(out, "{pad}}}");
        }
    }
}

fn dump_entries<K: AsRef<str>>(entries: &[(K, Field)]) -> String {
    let mut out = format!("array({}) {{\n", entries.len());
    for (k, v) in entries {
        let _ = write!(out, "  [\"{}\"]=>\n  ", k.as_ref());
        dump_field(&mut out, v, 2);
        out.push('\n');
    }
    out.push_str("}\n");
    out
}

fn dump_bool<T>(r: &PosixResult<T>) -> &'static str {
    if r.is_ok() {
        "bool(true)\n"
    } else {
        "bool(false)\n"
    }
}

#[test]
fn constants_match_oracle() {
    let Some(expected) =
        php(r#"foreach (get_defined_constants(true)["posix"] as $k => $v) echo "$k=$v\n";"#)
    else {
        return;
    };
    let ours: std::collections::HashMap<_, _> = constants().into_iter().collect();
    for line in expected.lines() {
        let (k, v) = line.split_once('=').expect("k=v");
        assert_eq!(
            ours.get(k).map(ToString::to_string).as_deref(),
            Some(v),
            "{k}"
        );
    }
}

#[test]
fn constant_values_linux() {
    let c: std::collections::HashMap<_, _> = constants().into_iter().collect();
    assert_eq!(c["POSIX_F_OK"], 0);
    assert_eq!(c["POSIX_R_OK"], 4);
    assert_eq!(c["POSIX_S_IFBLK"], 24576);
    assert_eq!(c["POSIX_RLIMIT_INFINITY"], -1);
    assert_eq!(c["POSIX_RLIMIT_NOFILE"], 7);
    assert_eq!(c["POSIX_PC_SYMLINK_MAX"], 19);
}

#[test]
fn strerror_matches_oracle() {
    let codes: Vec<i64> = (-3..140).chain([999, 9999, -999, 1 << 33]).collect();
    let list = codes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let Some(expected) = php(&format!(
        "foreach ([{list}] as $c) echo posix_strerror($c), \"\\n\";"
    )) else {
        return;
    };
    let p = Posix::new();
    let ours: String = codes
        .iter()
        .map(|&c| format!("{}\n", String::from_utf8_lossy(&p.strerror(c))))
        .collect();
    assert_eq!(ours, expected);
}

#[test]
fn strerror_known() {
    let p = Posix::new();
    assert_eq!(p.strerror(1), b"Operation not permitted");
    assert_eq!(p.strerror(9999), b"Unknown error 9999");
    assert_eq!(p.strerror(-1), b"Unknown error -1");
}

#[test]
fn identity_functions_match_oracle() {
    let Some(expected) = php(
        "echo posix_getuid(), ' ', posix_geteuid(), ' ', posix_getgid(), ' ', posix_getegid(), ' ', posix_getpgid(1), ' ', posix_getsid(1);",
    ) else {
        return;
    };
    let mut p = Posix::new();
    let ours = format!(
        "{} {} {} {} {} {}",
        p.getuid(),
        p.geteuid(),
        p.getgid(),
        p.getegid(),
        p.getpgid(1).unwrap_or(-1),
        p.getsid(1).unwrap_or(-1)
    );
    assert_eq!(ours, expected);
}

#[test]
fn getpid_is_process_id() {
    let p = Posix::new();
    assert_eq!(p.getpid(), i64::from(std::process::id()));
    assert!(p.getppid() > 0);
    assert!(p.getpgrp() > 0);
}

#[test]
fn uname_matches_oracle() {
    let Some(expected) = php("var_dump(posix_uname());") else {
        return;
    };
    let mut p = Posix::new();
    assert_eq!(dump_entries(&p.uname().expect("uname").entries()), expected);
}

#[test]
fn passwd_and_group_match_oracle() {
    let mut p = Posix::new();
    let uid = p.getuid();
    let gid = p.getgid();
    let Some(expected) = php(&format!(
        "var_dump(posix_getpwuid(0)); var_dump(posix_getgrgid(0)); var_dump(posix_getpwuid({uid})); var_dump(posix_getgrgid({gid})); var_dump(posix_getpwnam('root')); var_dump(posix_getgrnam('root'));"
    )) else {
        return;
    };
    let mut ours = String::new();
    ours += &dump_entries(&p.getpwuid(0).expect("root").entries());
    ours += &dump_entries(&p.getgrgid(0).expect("root").entries());
    ours += &dump_entries(&p.getpwuid(uid).expect("me").entries());
    ours += &dump_entries(&p.getgrgid(gid).expect("me").entries());
    ours += &dump_entries(&p.getpwnam(b"root").expect("root").entries());
    ours += &dump_entries(&p.getgrnam(b"root").expect("root").entries());
    assert_eq!(ours, expected);
}

#[test]
fn group_with_members_matches_oracle() {
    // Find a group with members in /etc/group to exercise the members list.
    let Ok(text) = std::fs::read_to_string("/etc/group") else {
        return;
    };
    let Some(name) = text
        .lines()
        .find(|l| l.rsplit(':').next().is_some_and(|m| !m.is_empty()))
        .and_then(|l| l.split(':').next())
    else {
        return;
    };
    let Some(expected) = php(&format!("var_dump(posix_getgrnam('{name}'));")) else {
        return;
    };
    let mut p = Posix::new();
    let g = p.getgrnam(name.as_bytes()).expect("group");
    assert!(!g.members.is_empty());
    assert_eq!(dump_entries(&g.entries()), expected);
}

#[test]
fn lookups_not_found() {
    let Some(expected) = php(
        "var_dump(posix_getgrnam('nonexistent_grp_x'), posix_get_last_error(), posix_getpwnam('nonexistent_usr_x'), posix_get_last_error(), posix_getpwuid(987654), posix_get_last_error(), posix_getgrgid(987654), posix_get_last_error());",
    ) else {
        return;
    };
    let mut p = Posix::new();
    let mut ours = String::new();
    ours += dump_bool(&p.getgrnam(b"nonexistent_grp_x"));
    ours += &format!("int({})\n", p.get_last_error());
    ours += dump_bool(&p.getpwnam(b"nonexistent_usr_x"));
    ours += &format!("int({})\n", p.get_last_error());
    ours += dump_bool(&p.getpwuid(987_654));
    ours += &format!("int({})\n", p.get_last_error());
    ours += dump_bool(&p.getgrgid(987_654));
    ours += &format!("int({})\n", p.get_last_error());
    assert_eq!(ours, expected);
}

#[test]
fn getrlimit_matches_oracle() {
    let Some(expected) = php("var_dump(posix_getrlimit()); var_dump(posix_getrlimit(POSIX_RLIMIT_NOFILE)); var_dump(posix_getrlimit(12345), posix_get_last_error());") else {
        return;
    };
    let mut p = Posix::new();
    let Rlimits::All(all) = p.getrlimit(None).expect("rlimits") else {
        panic!("expected all");
    };
    let entries: Vec<(String, Field)> = all.into_iter().map(|(k, v)| (k, v.to_field())).collect();
    let mut ours = dump_entries(&entries);
    let Rlimits::Single(soft, hard) = p.getrlimit(Some(7)).expect("nofile") else {
        panic!("expected single");
    };
    ours += "array(2) {\n  [0]=>\n  ";
    dump_field(&mut ours, &soft.to_field(), 2);
    ours += "\n  [1]=>\n  ";
    dump_field(&mut ours, &hard.to_field(), 2);
    ours += "\n}\n";
    ours += dump_bool(&p.getrlimit(Some(12345)));
    ours += &format!("int({})\n", p.get_last_error());
    assert_eq!(ours, expected);
}

#[test]
fn setrlimit_value_errors() {
    let mut p = Posix::new();
    assert_eq!(
        p.setrlimit(7, 129, 128),
        Err(PosixError::ValueError(
            "Argument #2 ($soft_limit) must be lower or equal to 128".into()
        ))
    );
    assert_eq!(
        p.setrlimit(7, -2, -1),
        Err(PosixError::ValueError(
            "Argument #2 ($soft_limit) must be greater than or equal to -1, or POSIX_RLIMIT_INFINITY"
                .into()
        ))
    );
    assert_eq!(
        p.setrlimit(7, -1, -2),
        Err(PosixError::ValueError(
            "Argument #3 ($hard_limit) must be greater than or equal to -1, or POSIX_RLIMIT_INFINITY"
                .into()
        ))
    );
    // Setting the current values back is always allowed.
    let Rlimits::Single(soft, hard) = p.getrlimit(Some(4)).expect("core") else {
        panic!()
    };
    let raw = |v: RlimitValue| match v {
        RlimitValue::Unlimited => -1,
        RlimitValue::Limit(n) => n,
    };
    assert!(p.setrlimit(4, raw(soft), raw(hard)).is_ok());
    // Unknown resource -> EINVAL.
    assert_eq!(
        p.setrlimit(9999, 1, 1),
        Err(PosixError::Failed(libc::EINVAL))
    );
    assert_eq!(p.get_last_error(), i64::from(libc::EINVAL));
}

#[test]
fn kill_and_pid_checks() {
    let mut p = Posix::new();
    assert_eq!(
        p.kill(i64::MAX, 15),
        Err(PosixError::ValueError(
            "Argument #1 ($process_id) must be between -2147483648 and 2147483647".into()
        ))
    );
    assert_eq!(
        p.kill(i64::MIN, 15).unwrap_err().to_string(),
        "Argument #1 ($process_id) must be between -2147483648 and 2147483647"
    );
    // posix_errno_variation2: nonexistent pid -> ESRCH (3)
    assert_eq!(p.kill((1 << 22) + 1, 9), Err(PosixError::Failed(3)));
    assert_eq!(p.errno(), 3);
    // invalid signal
    assert_eq!(
        p.kill(p.getpid(), 999),
        Err(PosixError::Failed(libc::EINVAL))
    );
    // signal 0 to self succeeds
    assert!(p.kill(p.getpid(), 0).is_ok());
    assert_eq!(
        p.setpgid(i64::MAX, 1).unwrap_err().to_string(),
        "Argument #1 ($process_id) must be between 0 and 2147483647"
    );
    assert_eq!(
        p.setpgid(1, -2).unwrap_err().to_string(),
        "Argument #2 ($process_group_id) must be between 0 and 2147483647"
    );
    assert_eq!(
        p.getsid(-1).unwrap_err().to_string(),
        "Argument #1 ($process_id) must be between 0 and 2147483647"
    );
    assert_eq!(p.getpgid(-1), Err(PosixError::Failed(libc::ESRCH)));
}

#[test]
fn fd_functions() {
    let mut p = Posix::new();
    let w = "Argument #1 ($file_descriptor) must be between 0 and 2147483647";
    assert_eq!(p.isatty(-1), Err(PosixError::Warning(w.into())));
    assert_eq!(p.get_last_error(), i64::from(libc::EBADF));
    assert_eq!(p.isatty((1 << 50) + 1), Err(PosixError::Warning(w.into())));
    assert_eq!(p.isatty(10024), Err(PosixError::Failed(libc::EBADF)));
    assert_eq!(p.ttyname(-1), Err(PosixError::Warning(w.into())));
    assert_eq!(p.fpathconf(-1, 4), Err(PosixError::Warning(w.into())));
    assert_eq!(p.strerror(p.get_last_error()), b"Bad file descriptor");
    assert_eq!(
        fd_type_warning("string"),
        "Argument #1 ($file_descriptor) must be of type int|resource, string given"
    );
    assert_eq!(
        stream_cast_warning("user-space"),
        "Could not use stream of type 'user-space'"
    );
}

#[test]
fn pathconf_behaviour() {
    let mut p = Posix::new();
    assert_eq!(
        p.pathconf(b"", 4),
        Err(PosixError::ValueError(
            "Argument #1 ($path) must not be empty".into()
        ))
    );
    let long = b"non_existent".repeat(4096);
    assert!(matches!(p.pathconf(&long, 3), Err(PosixError::Failed(_))));
    assert_ne!(p.errno(), 0);
    assert_eq!(p.pathconf(b"/tmp", 4), Ok(4096));
    let Some(expected) =
        php("foreach ([0,1,2,3,4,5,6,7,18,19] as $n) var_dump(posix_pathconf('/', $n));")
    else {
        return;
    };
    let mut ours = String::new();
    for n in [0, 1, 2, 3, 4, 5, 6, 7, 18, 19] {
        match p.pathconf(b"/", n) {
            Ok(v) => ours += &format!("int({v})\n"),
            Err(_) => ours += "bool(false)\n",
        }
    }
    assert_eq!(ours, expected);
}

#[test]
fn sysconf_matches_oracle() {
    let ids = [0, 1, 2, 4, 30, 83, -5, 9999];
    let Some(expected) = php(&format!(
        "foreach ([{}] as $n) echo posix_sysconf($n), \"\\n\";",
        ids.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    )) else {
        return;
    };
    let p = Posix::new();
    let ours: String = ids.iter().map(|&n| format!("{}\n", p.sysconf(n))).collect();
    assert_eq!(ours, expected);
}

#[test]
fn access_and_eaccess() {
    let mut p = Posix::new();
    assert!(p.access(b"/", 0).is_ok());
    assert_eq!(
        p.access(b"/nonexistent_dir_x/y", 0),
        Err(PosixError::Failed(libc::ENOENT))
    );
    assert_eq!(p.get_last_error(), 2);
    assert_eq!(p.access(b"", 0), Err(PosixError::Failed(libc::EIO)));
    assert_eq!(
        p.access(b"a\0b", 0),
        Err(PosixError::ValueError(
            "Argument #1 ($filename) must not contain any null bytes".into()
        ))
    );
    assert_eq!(
        p.eaccess(&b"bogus path".repeat(1042), 0),
        Err(PosixError::ValueError(
            "Argument #1 ($filename) must not be empty".into()
        ))
    );
    assert!(p.eaccess(b"/", 4).is_ok());
}

#[test]
fn mknod_and_mkfifo() {
    let mut p = Posix::new();
    assert!(matches!(
        p.mknod(b"", 0, 0, 0),
        Err(PosixError::Failed(libc::ENOENT))
    ));
    assert_eq!(
        p.mknod(b"/tmp/x", 8192, 0, 0),
        Err(PosixError::ValueError(
            "Argument #3 ($major) cannot be 0 for the POSIX_S_IFCHR and POSIX_S_IFBLK modes".into()
        ))
    );
    let dir = std::env::temp_dir().join(format!("php-posix-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let fifo = dir.join("fifo");
    let path = fifo.to_str().expect("utf8").as_bytes().to_vec();
    assert!(p.mkfifo(&path, 0o600).is_ok());
    assert_eq!(
        p.mkfifo(&path, 0o600),
        Err(PosixError::Failed(libc::EEXIST))
    );
    let reg = dir.join("regular");
    assert!(p
        .mknod(
            reg.to_str().expect("utf8").as_bytes(),
            i64::from(libc::S_IFREG) | 0o600,
            0,
            0
        )
        .is_ok());
    assert!(reg.is_file());
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn misc_simple() {
    let mut p = Posix::new();
    assert!(!p.initgroups(b"", 0));
    assert_eq!(p.ctermid().as_deref(), Ok(&b"/dev/tty"[..]));
    let times = p.times().expect("times");
    assert_eq!(times.entries().len(), 5);
    assert_eq!(times.entries()[0].0, "ticks");
    assert_eq!(
        p.getcwd().expect("cwd"),
        std::env::current_dir()
            .expect("cwd")
            .into_os_string()
            .into_vec()
    );
    assert!(p.getgroups().is_ok());
}

#[test]
fn expand_filepath_rules() {
    assert_eq!(expand_filepath(b"", b"/x"), None);
    assert_eq!(expand_filepath(&b"a".repeat(5000), b"/x"), None);
    assert_eq!(
        expand_filepath(b"nonexist_a/./b/../c", b"/nonexist_root"),
        Some(b"/nonexist_root/nonexist_a/c".to_vec())
    );
    assert_eq!(
        expand_filepath(b"/nonexist_q//../r/", b"/x"),
        Some(b"/r".to_vec())
    );
    assert_eq!(expand_filepath(b"/", b"/x"), Some(b"/".to_vec()));
}
