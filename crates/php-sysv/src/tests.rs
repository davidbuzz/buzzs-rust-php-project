//! Tests against real kernel IPC objects (unique keys per process, every
//! object removed afterwards), with hand-ported `.phpt` expectations and
//! `/usr/bin/php` interoperability checks where the oracle is installed.

use crate::msg::{MessageQueue, Payload, QueueUpdate, MSG_IPC_NOWAIT, MSG_NOERROR};
use crate::sem::Semaphore;
use crate::shm::{SharedMemory, DEFAULT_INIT_MEM};
use crate::shmop::Shmop;
use crate::{key_from_arg, SysvError};
use std::process::Command;
use std::sync::atomic::{AtomicI64, Ordering};

const ORACLE: &str = "/usr/bin/php";

/// A key unique to this process and call.
fn key() -> i64 {
    static NEXT: AtomicI64 = AtomicI64::new(1);
    let pid = i64::from(std::process::id()) & 0xffff;
    0x3000_0000 | (pid << 12) | NEXT.fetch_add(1, Ordering::Relaxed)
}

fn php(code: &str) -> Option<String> {
    if !std::path::Path::new(ORACLE).exists() {
        return None;
    }
    let out = Command::new(ORACLE)
        .args(["-d", "memory_limit=1G", "-r", code])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn warning(e: &SysvError) -> &str {
    match e {
        SysvError::Warning(m) => m,
        other => panic!("expected warning, got {other:?}"),
    }
}

fn no_warn() -> impl FnMut(String) {
    |w| panic!("unexpected warning: {w}")
}

// ------------------------------------------------------------------ keys

#[test]
fn key_range_gh9945() {
    let oor = || SysvError::ValueError("Argument #1 ($key) is out of range".into());
    let big = 0x1_0000_0000;
    assert_eq!(key_from_arg(big), Err(oor()));
    assert_eq!(MessageQueue::exists(big), Err(oor()));
    assert_eq!(MessageQueue::get(big, 0o666), Err(oor()));
    assert_eq!(
        Semaphore::get(big, 1, 0o666, true, &mut no_warn()).unwrap_err(),
        oor()
    );
    assert_eq!(
        SharedMemory::attach(big, Some(0), 0o666, DEFAULT_INIT_MEM).unwrap_err(),
        oor()
    );
    assert_eq!(Shmop::open(big, b"", 0o644, 1).unwrap_err(), oor());
    assert_eq!(key_from_arg(-1), Ok(-1));
    assert_eq!(key_from_arg(i64::from(i32::MIN)), Ok(i32::MIN));
}

// -------------------------------------------------------------- messages

#[test]
fn msg_send_receive_001() {
    let q = MessageQueue::get(key(), 0o666).unwrap();
    q.send(1, b"s:5:\"hello\";", true).unwrap();
    let r = q.receive(0, 1024, 0).unwrap();
    assert_eq!(r.message_type, 1);
    assert_eq!(r.data, b"s:5:\"hello\";");
    assert!(q.remove());
}

#[test]
fn msg_queue_exists_003() {
    let k = key();
    assert!(!MessageQueue::exists(k).unwrap());
    let q = MessageQueue::get(k, 0o600).unwrap();
    assert!(MessageQueue::exists(k).unwrap());
    // A second get attaches to the same queue.
    assert_eq!(MessageQueue::get(k, 0o600).unwrap().id(), q.id());
    assert!(q.remove());
    assert!(!MessageQueue::exists(k).unwrap());
}

#[test]
fn msg_set_and_stat_004() {
    let q = MessageQueue::get(key(), 0o666).unwrap();
    let st = q.stat().unwrap();
    assert_eq!(st.mode & 0o777, 0o666);
    assert!(q.set(&QueueUpdate {
        mode: Some(0o600),
        ..QueueUpdate::default()
    }));
    assert_eq!(q.stat().unwrap().mode, 0o600);
    assert!(q.set(&QueueUpdate {
        uid: Some(st.uid),
        ..QueueUpdate::default()
    }));
    assert!(q.set(&QueueUpdate {
        gid: Some(st.gid),
        ..QueueUpdate::default()
    }));
    assert!(q.set(&QueueUpdate {
        qbytes: Some(st.qbytes - 1),
        ..QueueUpdate::default()
    }));
    let st2 = q.stat().unwrap();
    assert_eq!(st2.qbytes, st.qbytes - 1);
    let keys: Vec<_> = st2.entries().iter().map(|e| e.0).collect();
    assert_eq!(
        keys,
        [
            "msg_perm.uid",
            "msg_perm.gid",
            "msg_perm.mode",
            "msg_stime",
            "msg_rtime",
            "msg_ctime",
            "msg_qnum",
            "msg_qbytes",
            "msg_lspid",
            "msg_lrpid"
        ]
    );
    assert!(q.remove());
}

#[test]
fn msg_removed_queue_005() {
    let q = MessageQueue::get(key(), 0o666).unwrap();
    assert!(q.remove());
    assert!(!q.set(&QueueUpdate {
        qbytes: Some(1),
        ..QueueUpdate::default()
    }));
    assert_eq!(q.stat(), None);
    assert!(matches!(q.receive(0, 1, 0), Err(SysvError::Failed(e)) if e != 0));
    assert_eq!(
        q.receive(0, 0, 0),
        Err(SysvError::ValueError(
            "Argument #4 ($max_message_size) must be greater than 0".into()
        ))
    );
    let f = q.send(1, b"foo", true).unwrap_err();
    assert_eq!(f.errno, libc::EINVAL);
    assert_eq!(f.message(), "msgsnd failed: Invalid argument");
    assert!(!q.remove());
}

#[test]
fn msg_payloads_006() {
    let q = MessageQueue::get(key(), 0o600).unwrap();
    for (p, want) in [
        (Payload::Str(b"foo"), &b"foo"[..]),
        (Payload::Int(123), b"123"),
        (
            Payload::Float(9_223_372_036_854_775_808.0),
            b"9223372036854775808.000000",
        ),
        (Payload::Bool(true), b"1"),
        (Payload::Bool(false), b"0"),
        (Payload::Float(1.01), b"1.010000"),
    ] {
        q.send(1, &p.to_bytes(), true).unwrap();
        let r = q.receive(1, 1024, MSG_IPC_NOWAIT).unwrap();
        assert_eq!(r.data, want);
    }
    assert_eq!(
        crate::msg::payload_type_error("null"),
        SysvError::TypeError(
            "Argument #3 ($message) must be of type string|int|float|bool, null given".into()
        )
    );
    assert!(q.remove());
}

#[test]
fn msg_float_formatting_matches_oracle() {
    let vals = [
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        1e20,
        -0.0,
        123_456_789.123_456_79,
        1e300,
        -1.5,
        0.007_812_5,
        2.5e-7,
        1e-7,
    ];
    let ours: Vec<String> = vals
        .iter()
        .map(|v| String::from_utf8(Payload::Float(*v).to_bytes()).unwrap())
        .collect();
    let k = key();
    let code = format!(
        "$q=msg_get_queue({k},0600); foreach([INF,-INF,NAN,1e20,-0.0,123456789.123456789,1e300,-1.5,0.0078125,2.5e-7,1e-7] as $d){{msg_send($q,1,$d,false); msg_receive($q,1,$t,4096,$m,false); echo $m,\"\\n\";}} msg_remove_queue($q);"
    );
    let Some(out) = php(&code) else { return };
    let theirs: Vec<&str> = out.lines().collect();
    assert_eq!(ours, theirs);
}

#[test]
fn msg_receive_errors_and_flags() {
    let q = MessageQueue::get(key(), 0o600).unwrap();
    assert_eq!(
        q.receive(0, 1, MSG_IPC_NOWAIT),
        Err(SysvError::Failed(libc::ENOMSG))
    );
    q.send(3, b"abcdef", true).unwrap();
    assert_eq!(q.receive(0, 2, 0), Err(SysvError::Failed(libc::E2BIG)));
    let r = q.receive(0, 2, MSG_NOERROR).unwrap();
    assert_eq!((r.message_type, r.data.as_slice()), (3, &b"ab"[..]));
    // MSG_EXCEPT: first message whose type differs from the desired one.
    q.send(1, b"one", true).unwrap();
    q.send(2, b"two", true).unwrap();
    let r = q.receive(1, 100, crate::msg::MSG_EXCEPT).unwrap();
    assert_eq!(r.message_type, 2);
    // Negative desired type: lowest type <= |desired|.
    let r = q.receive(-5, 100, 0).unwrap();
    assert_eq!(r.data, b"one");
    // Huge max sizes do not allocate absurd buffers.
    q.send(9, b"big?", true).unwrap();
    assert_eq!(q.receive(0, i64::MAX, 0).unwrap().data, b"big?");
    // Non-positive message types are rejected by the kernel.
    assert_eq!(q.send(0, b"x", true).unwrap_err().errno, libc::EINVAL);
    assert_eq!(q.send(-5, b"x", false).unwrap_err().errno, libc::EINVAL);
    assert!(q.remove());
}

#[test]
fn msg_ipc_private() {
    assert!(!MessageQueue::exists(0).unwrap());
    let q = MessageQueue::get(0, 0o600).unwrap();
    let q2 = MessageQueue::get(0, 0o600).unwrap();
    assert_ne!(q.id(), q2.id());
    assert!(!MessageQueue::exists(0).unwrap());
    assert_eq!(q.stat().unwrap().mode & 0o777, 0o600);
    q.send(1, b"s:5:\"hello\";", true).unwrap();
    assert_eq!(q.receive(1, 1024, 0).unwrap().data, b"s:5:\"hello\";");
    assert!(q.remove());
    assert!(q2.remove());
}

#[test]
fn msg_mode_zero_queue() {
    let k = key();
    let q = MessageQueue::get(k, 0o000).unwrap();
    // Mode 0: even the owner cannot read it (non-root), but msgget() with
    // no requested mode bits still attaches, and the creator may remove it.
    let root = std::fs::read_to_string("/proc/self/status").is_ok_and(|s| {
        s.lines()
            .any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some("0"))
    });
    assert_eq!(q.stat().is_none(), !root);
    assert_eq!(MessageQueue::get(k, 0o600).unwrap().id(), q.id());
    assert!(q.remove());
}

#[test]
fn msg_interop_with_php() {
    let k = key();
    let q = MessageQueue::get(k, 0o600).unwrap();
    q.send(7, b"s:2:\"hi\";", true).unwrap();
    let Some(out) = php(&format!(
        "$q=msg_get_queue({k}); msg_receive($q,0,$t,100,$m); echo $t,':',$m; msg_send($q,4,[1,'a'=>true]);"
    )) else {
        q.remove();
        return;
    };
    assert_eq!(out, "7:hi");
    let r = q.receive(0, 1024, MSG_IPC_NOWAIT).unwrap();
    assert_eq!(r.message_type, 4);
    assert_eq!(r.data, b"a:2:{i:0;i:1;s:1:\"a\";b:1;}");
    assert!(q.remove());
}

// ------------------------------------------------------------ semaphores

#[test]
fn sem_acquire_release_remove() {
    let k = key();
    let mut s = Semaphore::get(k, 1, 0o666, true, &mut no_warn()).unwrap();
    s.acquire(false).unwrap();
    assert_eq!(s.count(), 1);
    let mut other = Semaphore::get(k, 1, 0o666, true, &mut no_warn()).unwrap();
    assert_eq!(other.id(), s.id());
    assert_eq!(other.acquire(true), Err(SysvError::Failed(libc::EAGAIN)));
    s.release().unwrap();
    other.acquire(true).unwrap();
    other.release().unwrap();
    let e = s.release().unwrap_err();
    assert_eq!(
        warning(&e),
        format!("SysV semaphore for key 0x{k:x} is not currently acquired")
    );
    s.remove().unwrap();
    assert_eq!(s.count(), -1);
    let e = other.remove().unwrap_err();
    assert_eq!(
        warning(&e),
        format!("SysV semaphore for key 0x{k:x} does not (any longer) exist")
    );
    let e = other.acquire(false).unwrap_err();
    assert_eq!(
        warning(&e),
        format!("Failed to acquire key 0x{k:x}: Invalid argument")
    );
}

#[test]
fn sem_max_acquire_and_auto_release() {
    let k = key();
    let mut a = Semaphore::get(k, 2, 0o600, true, &mut no_warn()).unwrap();
    a.acquire(false).unwrap();
    a.acquire(false).unwrap();
    assert_eq!(a.acquire(true), Err(SysvError::Failed(libc::EAGAIN)));
    // A second user does not reset the maximum.
    let mut b = Semaphore::get(k, 5, 0o600, true, &mut no_warn()).unwrap();
    assert_eq!(b.acquire(true), Err(SysvError::Failed(libc::EAGAIN)));
    // Dropping `a` releases both acquisitions.
    drop(a);
    b.acquire(true).unwrap();
    b.acquire(true).unwrap();
    assert_eq!(b.acquire(true), Err(SysvError::Failed(libc::EAGAIN)));
    b.release().unwrap();
    b.release().unwrap();
    b.remove().unwrap();
}

#[test]
fn sem_no_auto_release() {
    let k = key();
    let mut a = Semaphore::get(k, 1, 0o600, false, &mut no_warn()).unwrap();
    a.acquire(false).unwrap();
    drop(a);
    let mut b = Semaphore::get(k, 1, 0o600, true, &mut no_warn()).unwrap();
    // `a`'s SEM_UNDO adjustments stay with this process, so the semaphore
    // is still held.
    assert_eq!(b.acquire(true), Err(SysvError::Failed(libc::EAGAIN)));
    b.remove().unwrap();
}

#[test]
fn sem_negative_key_formats() {
    // Removing a semaphore of a negative key prints the key as unsigned.
    let mut s = Semaphore::get(0, 1, 0o600, true, &mut no_warn()).unwrap();
    s.remove().unwrap();
    let e = s.remove().unwrap_err();
    assert_eq!(
        warning(&e),
        "SysV semaphore for key 0x0 does not (any longer) exist"
    );
    assert_eq!(crate::xkey(-1), "ffffffff");
    assert_eq!(crate::xlong(-1), "ffffffffffffffff");
}

#[test]
fn sem_interop_with_php() {
    let k = key();
    let mut s = Semaphore::get(k, 1, 0o666, true, &mut no_warn()).unwrap();
    s.acquire(false).unwrap();
    let Some(out) = php(&format!(
        "$s=sem_get({k}); var_dump(sem_acquire($s, true));"
    )) else {
        s.remove().unwrap();
        return;
    };
    assert_eq!(out.trim(), "bool(false)");
    s.release().unwrap();
    let out = php(&format!(
        "$s=sem_get({k}); var_dump(sem_acquire($s, true));"
    ))
    .unwrap();
    assert_eq!(out.trim(), "bool(true)");
    s.remove().unwrap();
}

// --------------------------------------------------------- shared memory

#[test]
fn shm_attach_errors_002() {
    let msg = SysvError::ValueError("Argument #2 ($size) must be greater than 0".into());
    for (k, size) in [(-1, 0), (0, -1), (123, -1), (key(), 0)] {
        assert_eq!(
            SharedMemory::attach(k, Some(size), 0o666, DEFAULT_INIT_MEM).unwrap_err(),
            msg
        );
    }
    // A null size with a bad init_mem is reported the same way.
    assert_eq!(
        SharedMemory::attach(key(), None, 0o666, 0).unwrap_err(),
        msg
    );
    let k = key();
    for perms in [0o666, 0o600] {
        let s = SharedMemory::attach(k, Some(1024), perms, DEFAULT_INIT_MEM).unwrap();
        s.remove().unwrap();
    }
    let s = SharedMemory::attach(k, None, 0o666, DEFAULT_INIT_MEM).unwrap();
    let raw = Shmop::open(k, b"a", 0, 0).unwrap();
    assert_eq!(raw.size(), DEFAULT_INIT_MEM);
    s.remove().unwrap();
}

#[test]
fn shm_detach_003_007() {
    let k = key();
    let mut s = SharedMemory::attach(k, Some(1024), 0o666, DEFAULT_INIT_MEM).unwrap();
    s.remove().unwrap();
    s.detach().unwrap();
    let destroyed = SysvError::Error("Shared memory block has already been destroyed".into());
    assert_eq!(s.detach().unwrap_err(), destroyed);
    assert_eq!(s.remove().unwrap_err(), destroyed);
    assert_eq!(s.get_var(1).unwrap_err(), destroyed);
    assert_eq!(s.has_var(1).unwrap_err(), destroyed);
    assert_eq!(s.remove_var(1).unwrap_err(), destroyed);
    assert_eq!(s.ensure_attached().unwrap_err(), destroyed);
    // gh16591: detached between ensure_attached() and put_var().
    assert_eq!(
        s.put_var(1, b"N;").unwrap_err(),
        SysvError::Error(
            "Shared memory block has been destroyed by the serialization function".into()
        )
    );
}

#[test]
fn shm_put_get_004_005_006() {
    let mut s = SharedMemory::attach(key(), Some(1024), 0o666, DEFAULT_INIT_MEM).unwrap();
    s.put_var(-1, b"s:6:\"qwerty\";").unwrap();
    s.put_var(10, b"s:6:\"qwerty\";").unwrap();
    s.put_var(10, b"s:6:\"qwerty\";").unwrap();
    let big = format!("s:2048:\"{}\";", "test".repeat(512));
    assert_eq!(
        s.put_var(11, big.as_bytes()).unwrap_err(),
        SysvError::Warning("Not enough shared memory left".into())
    );
    assert_eq!(
        warning(&s.get_var(1000).unwrap_err()),
        "Variable key 1000 doesn't exist"
    );
    assert_eq!(
        warning(&s.get_var(-10000).unwrap_err()),
        "Variable key -10000 doesn't exist"
    );
    assert_eq!(s.get_var(-1).unwrap(), b"s:6:\"qwerty\";");
    assert!(s.has_var(10).unwrap());
    s.remove_var(10).unwrap();
    assert!(!s.has_var(10).unwrap());
    assert_eq!(
        warning(&s.remove_var(10).unwrap_err()),
        "Variable key 10 doesn't exist"
    );
    for v in [&b"s:4:\"test\";"[..], b"i:1;", b"N;"] {
        s.put_var(3, v).unwrap();
    }
    assert_eq!(s.get_var(3).unwrap(), b"N;");
    s.remove().unwrap();
}

#[test]
fn shm_attach_existing_segment_size_and_too_small() {
    let k = key();
    let raw = Shmop::open(k, b"n", 0o600, 4096).unwrap();
    let mut s = SharedMemory::attach(k, Some(10 * 1024 * 1024), 0o666, DEFAULT_INIT_MEM).unwrap();
    let big = vec![b'A'; 1024 * 1024];
    assert!(s.put_var(1, &big).is_err());
    s.put_var(2, b"s:2:\"ok\";").unwrap();
    assert_eq!(s.get_var(2).unwrap(), b"s:2:\"ok\";");
    s.remove().unwrap();
    drop(raw);

    let k = key();
    let raw = Shmop::open(k, b"n", 0o600, 8).unwrap();
    let e = SharedMemory::attach(k, Some(1024), 0o666, DEFAULT_INIT_MEM).unwrap_err();
    assert_eq!(
        warning(&e),
        format!("Failed for key 0x{k:x}: segment too small")
    );
    raw.delete().unwrap();

    // Creating a segment smaller than the header.
    let k = key();
    let e = SharedMemory::attach(k, Some(10), 0o666, DEFAULT_INIT_MEM).unwrap_err();
    assert_eq!(
        warning(&e),
        format!("Failed for key 0x{k:x}: memorysize too small")
    );
}

#[test]
fn shm_failed_attach_cleans_up() {
    // Root can attach to a mode-0 segment; the scenario needs a normal user.
    if std::fs::read_to_string("/proc/self/status").is_ok_and(|s| {
        s.lines()
            .any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some("0"))
    }) {
        return;
    }
    let k = key();
    let e = SharedMemory::attach(k, Some(1024), 0, DEFAULT_INIT_MEM).unwrap_err();
    assert_eq!(
        warning(&e),
        format!("Failed for key 0x{k:x}: Permission denied")
    );
    let mut s = SharedMemory::attach(k, Some(1024), 0o600, DEFAULT_INIT_MEM).unwrap();
    s.put_var(1, b"s:5:\"value\";").unwrap();
    assert_eq!(s.get_var(1).unwrap(), b"s:5:\"value\";");
    s.remove().unwrap();
}

#[test]
fn shm_too_large_bug72858() {
    let k = key();
    let e = SharedMemory::attach(k, Some(0x3fff_ffff + 0x1337), 0o666, DEFAULT_INIT_MEM);
    match e {
        Err(SysvError::Warning(m)) => {
            assert!(m.starts_with(&format!("Failed for key 0x{k:x}: ")), "{m}");
        }
        Ok(s) => s.remove().unwrap(), // systems with a huge SHMMAX
        Err(other) => panic!("{other:?}"),
    }
}

#[test]
fn shm_interop_with_php() {
    let k = key();
    let mut s = SharedMemory::attach(k, Some(2048), 0o666, DEFAULT_INIT_MEM).unwrap();
    s.put_var(1, b"s:5:\"hello\";").unwrap();
    s.put_var(-7, b"a:1:{i:0;i:2;}").unwrap();
    let Some(out) = php(&format!(
        "$s=shm_attach({k}); echo shm_get_var($s,1), json_encode(shm_get_var($s,-7)); shm_remove_var($s,1); shm_put_var($s,5,[true,1.5]); shm_put_var($s,-7,'x');"
    )) else {
        s.remove().unwrap();
        return;
    };
    assert_eq!(out, "hello[2]");
    assert!(!s.has_var(1).unwrap());
    assert_eq!(s.get_var(5).unwrap(), b"a:2:{i:0;b:1;i:1;d:1.5;}");
    assert_eq!(s.get_var(-7).unwrap(), b"s:1:\"x\";");
    s.remove().unwrap();
}

// ------------------------------------------------------------------ shmop

#[test]
fn shmop_basic_001() {
    let k = key();
    let d1 = b"test #1 of the shmop() extension";
    let d2 = b"test #2 append data to shared memory segment";
    let mut w = Shmop::open(k, b"n", 0o644, 1024).unwrap();
    assert_eq!(w.size(), 1024);
    assert_eq!(w.write(d1, 0).unwrap(), 32);
    assert_eq!(w.read(0, 32).unwrap(), d1);
    let mut r = Shmop::open(k, b"a", 0o644, 1024).unwrap();
    assert_eq!(r.read(0, 32).unwrap(), d1);
    assert_eq!(
        r.write(d1, 32).unwrap_err(),
        SysvError::Error("Read-only segment cannot be written".into())
    );
    let mut w2 = Shmop::open(k, b"w", 0o644, 1024).unwrap();
    assert_eq!(w2.write(d2, 32).unwrap(), 44);
    let mut both = d1.to_vec();
    both.extend_from_slice(d2);
    assert_eq!(w2.read(0, 76).unwrap(), both);
    // Size 0 reads to the end; writes are clipped to the segment.
    assert_eq!(w2.read(1000, 0).unwrap().len(), 24);
    assert_eq!(w2.write(&[b'z'; 100], 1000).unwrap(), 24);
    assert_eq!(w2.write(b"", 1024).unwrap(), 0);
    w2.delete().unwrap();
    drop(w);
}

#[test]
fn shmop_errors_002() {
    let mode = SysvError::ValueError("Argument #2 ($mode) must be a valid access mode".into());
    assert_eq!(Shmop::open(1338, b"", 0o644, 1024).unwrap_err(), mode);
    assert_eq!(Shmop::open(1338, b"b", 0o644, 1024).unwrap_err(), mode);
    assert_eq!(Shmop::open(1338, b"ww", 0o644, 1024).unwrap_err(), mode);
    let e = Shmop::open(0, b"a", 0o644, 1024).unwrap_err();
    assert!(warning(&e).starts_with("Unable to attach or create shared memory segment \""));
    assert_eq!(
        Shmop::open(key(), b"c", 0o666, 0).unwrap_err(),
        SysvError::ValueError(
            "Argument #4 ($size) must be greater than 0 for the \"c\" and \"n\" access modes"
                .into()
        )
    );
    let mut s = Shmop::open(key(), b"n", 0o600, 1024).unwrap();
    assert_eq!(
        s.read(-10, 0).unwrap_err(),
        SysvError::ValueError(
            "Argument #2 ($offset) must be between 0 and the segment size".into()
        )
    );
    assert_eq!(
        s.read(1025, 0).unwrap_err().to_string(),
        "Argument #2 ($offset) must be between 0 and the segment size"
    );
    let oor = SysvError::ValueError("Argument #3 ($size) is out of range".into());
    assert_eq!(s.read(0, -10).unwrap_err(), oor);
    assert_eq!(s.read(1, 1024).unwrap_err(), oor);
    assert_eq!(s.read(1, i64::MAX).unwrap_err(), oor);
    assert_eq!(
        s.write(b"text to try write", -10).unwrap_err(),
        SysvError::ValueError("Argument #3 ($offset) is out of range".into())
    );
    s.delete().unwrap();
    // Exclusive creation of an existing key fails.
    let k = key();
    let a = Shmop::open(k, b"n", 0o600, 64).unwrap();
    let e = Shmop::open(k, b"n", 0o600, 64).unwrap_err();
    assert_eq!(
        warning(&e),
        "Unable to attach or create shared memory segment \"File exists\""
    );
    a.delete().unwrap();
}

#[test]
fn shmop_gh14537_and_private() {
    let k = key();
    let a = Shmop::open(k, b"c", 0o644, 11).unwrap();
    a.delete().unwrap();
    // On Linux a segment marked for deletion releases its key at once, so
    // (unlike the platforms gh14537.phpt targets) a new segment is created.
    let b = Shmop::open(k, b"c", 0o644, 21).unwrap();
    assert_eq!(b.size(), 21);
    b.delete().unwrap();
    drop(a);

    let mut p1 = Shmop::open(0, b"c", 0o777, 1024).unwrap();
    p1.write(b"test", 0).unwrap();
    let p2 = Shmop::open(0, b"c", 0o777, 1024).unwrap();
    assert_ne!(p2.read(0, 4).unwrap(), b"test");
    p1.delete().unwrap();
    p2.delete().unwrap();
}

#[test]
fn shmop_tsrm_shmat() {
    let mut s = Shmop::open(0, b"c", 0o644, 65472).unwrap();
    let mut payload = vec![b'x'; 65471];
    payload.push(b'z');
    assert_eq!(s.size(), 65472);
    assert_eq!(s.write(&payload, 0).unwrap(), 65472);
    assert_eq!(s.read(65471, 1).unwrap(), b"z");
    s.delete().unwrap();
    // Linux accepts IPC_RMID again while still attached.
    s.delete().unwrap();
}
