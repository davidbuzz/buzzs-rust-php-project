//! Tests mirroring `tests/output/*.phpt`.  [`TestHost`] plays the VM: it
//! displays diagnostics the way the CLI does (through the output layer,
//! "in %s on line %d" kept literally), tracks the active internal function
//! for docref prefixes, and runs callbacks that may re-enter the layer.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;

type Cb = Rc<dyn Fn(&mut TestHost, &[u8], i32) -> Option<Vec<u8>>>;

#[derive(Default)]
struct TestHost {
    stack: OutputStack<Cb>,
    out: Vec<u8>,
    funcs: Vec<&'static str>,
    fatal: bool,
    sapi_flushes: usize,
    headers_ok: Option<bool>,
    conflicts: Vec<(&'static str, &'static str)>,
}

impl OutputHost for TestHost {
    type Callback = Cb;

    fn output(&mut self) -> &mut OutputStack<Cb> {
        &mut self.stack
    }

    fn call_handler(&mut self, callback: &mut Cb, buffer: &[u8], mode: i32) -> Option<Vec<u8>> {
        let f = Rc::clone(callback);
        // A user function is now active: no internal function prefix.
        let saved = std::mem::take(&mut self.funcs);
        let r = f(self, buffer, mode);
        self.funcs = saved;
        r
    }

    fn sapi_write(&mut self, data: &[u8]) {
        self.out.extend_from_slice(data);
    }

    fn sapi_flush(&mut self) {
        self.sapi_flushes += 1;
    }

    fn send_headers(&mut self) -> bool {
        self.headers_ok.unwrap_or(true)
    }

    fn check_handler_conflicts(&mut self, name: &[u8]) -> bool {
        let conflicts = self.conflicts.clone();
        !conflicts.iter().any(|(new, old)| {
            new.as_bytes() == name && handler_conflict(self, name, old.as_bytes())
        })
    }

    fn report(&mut self, d: Diagnostic) {
        let msg = d.render(self.funcs.last().copied());
        let text = format!("\n{}: {} in %s on line %d\n", d.level.label(), msg);
        if d.level.is_fatal() {
            self.fatal = true;
        }
        write(self, text.as_bytes());
    }
}

impl TestHost {
    fn echo(&mut self, s: &str) {
        write(self, s.as_bytes());
    }

    fn call<R>(&mut self, name: &'static str, f: impl FnOnce(&mut Self) -> R) -> R {
        self.funcs.push(name);
        let r = f(self);
        self.funcs.pop();
        r
    }

    fn ob_start(&mut self) -> bool {
        self.call("ob_start", |h| {
            ob_start(h, StartHandler::Default, 0, i64::from(flags::STDFLAGS))
        })
    }

    fn ob_start_cb(&mut self, name: &str, chunk: i64, fl: i64, cb: Cb) -> bool {
        let handler = StartHandler::User {
            name: name.as_bytes().to_vec(),
            callback: cb,
        };
        self.call("ob_start", |h| ob_start(h, handler, chunk, fl))
    }

    fn end_flush(&mut self) -> bool {
        self.call("ob_end_flush", ob_end_flush)
    }

    fn end_clean(&mut self) -> bool {
        self.call("ob_end_clean", ob_end_clean)
    }

    fn flush(&mut self) -> bool {
        self.call("ob_flush", ob_flush)
    }

    fn clean(&mut self) -> bool {
        self.call("ob_clean", ob_clean)
    }

    fn get_clean(&mut self) -> Option<Vec<u8>> {
        self.call("ob_get_clean", ob_get_clean)
    }

    fn get_flush(&mut self) -> Option<Vec<u8>> {
        self.call("ob_get_flush", ob_get_flush)
    }

    fn get_contents(&mut self) -> Option<Vec<u8>> {
        ob_get_contents(self)
    }

    /// End of script: shutdown flushes all buffers.
    fn finish(mut self) -> String {
        request_shutdown(&mut self);
        String::from_utf8(self.out).unwrap()
    }
}

/// `var_dump()` of a `string|false` result.
fn dump(v: Option<&[u8]>) -> String {
    match v {
        Some(s) => format!("string({}) \"{}\"\n", s.len(), String::from_utf8_lossy(s)),
        None => "bool(false)\n".into(),
    }
}

fn dump_bool(b: bool) -> String {
    format!("bool({b})\n")
}

fn cb(f: impl Fn(&mut TestHost, &[u8], i32) -> Option<Vec<u8>> + 'static) -> Cb {
    Rc::new(f)
}

fn identity() -> Cb {
    cb(|_, s, _| Some(s.to_vec()))
}

#[test]
fn native_alias_has_internal_flags_and_failure_passthrough() {
    let mut h = TestHost::default();
    assert!(ob_start(
        &mut h,
        StartHandler::Alias {
            name: b"native".to_vec(),
            callback: cb(|_, _, _| None),
        },
        1,
        i64::from(flags::STDFLAGS),
    ));
    let st = h.stack.active_status().unwrap();
    assert_eq!(st.kind, flags::INTERNAL);
    assert_eq!(st.flags, flags::STDFLAGS);
    h.echo("x");
    let st = h.stack.active_status().unwrap();
    assert_eq!(st.kind, flags::INTERNAL);
    assert_ne!(st.flags & flags::DISABLED, 0);
    assert!(h.end_flush());
    assert_eq!(h.finish(), "x");
}

#[test]
fn running_handler_hooks_preserve_flushability() {
    let mut h = TestHost::default();
    assert_eq!(h.stack.running_flags(), None);
    assert!(h.ob_start_cb(
        "immutable",
        1,
        112,
        cb(|h, s, mode| {
            if mode & flags::START != 0 {
                assert_eq!(h.stack.running_flags(), Some(flags::STDFLAGS | flags::USER));
            }
            h.stack.set_running_immutable();
            Some(s.to_vec())
        }),
    ));
    h.echo("x");
    assert_eq!(h.stack.running_flags(), None);
    let st = h.stack.active_status().unwrap();
    assert_eq!(st.flags & flags::STDFLAGS, flags::FLUSHABLE);
    assert_eq!(h.finish(), "x");
}

#[test]
fn conflicts_are_ordered_and_preserve_the_existing_stack() {
    let mut h = TestHost::default();
    h.conflicts = vec![("native", DEFAULT_HANDLER_NAME), ("native", "native")];
    assert!(h.ob_start());
    assert!(!h.ob_start_cb("native", 0, 112, identity()));
    assert_eq!(h.stack.level(), 1);
    assert_eq!(
        h.finish(),
        "\nWarning: ob_start(): Output handler 'native' conflicts with 'default output handler' in %s on line %d\n\
         \nNotice: ob_start(): Failed to create buffer in %s on line %d\n"
    );

    let mut h = TestHost::default();
    h.conflicts = vec![("native", "native")];
    assert!(h.ob_start_cb("native", 1, 112, cb(|_, _, _| None)));
    h.echo("x");
    assert!(!h.ob_start_cb("native", 0, 112, identity()));
    assert_eq!(h.stack.level(), 1);
    assert_eq!(
        h.finish(),
        "x\nWarning: ob_start(): Output handler 'native' cannot be used twice in %s on line %d\n\
         \nNotice: ob_start(): Failed to create buffer in %s on line %d\n"
    );
}

#[test]
fn output_lock_precedes_conflict_checks() {
    let mut h = TestHost::default();
    h.conflicts = vec![("native", "native")];
    assert!(h.ob_start_cb(
        "native",
        0,
        112,
        cb(|h, _, _| {
            assert!(!h.ob_start_cb("native", 0, 112, identity()));
            None
        }),
    ));
    h.end_flush();
    assert!(h.fatal);
    assert_eq!(
        h.finish(),
        "\nFatal error: ob_start(): Cannot use output buffering in output buffering display handlers in %s on line %d\n"
    );
}

#[test]
fn ob_001_to_009() {
    let mut h = TestHost::default();
    h.echo("foo\n");
    assert_eq!(h.finish(), "foo\n");

    let mut h = TestHost::default();
    h.ob_start();
    h.echo("foo\n");
    assert_eq!(h.finish(), "foo\n");

    // ob_003
    let mut h = TestHost::default();
    h.ob_start();
    h.echo("foo\n");
    h.flush();
    h.echo("bar\n");
    h.flush();
    assert_eq!(h.finish(), "foo\nbar\n");

    // ob_004
    let mut h = TestHost::default();
    h.ob_start();
    h.echo("foo\n");
    h.clean();
    h.echo("bar\n");
    assert_eq!(h.finish(), "bar\n");

    // ob_005
    let mut h = TestHost::default();
    h.ob_start();
    h.echo("foo\n");
    h.ob_start();
    h.echo("bar\n");
    h.end_clean();
    h.echo("baz\n");
    assert_eq!(h.finish(), "foo\nbaz\n");

    // ob_006
    let mut h = TestHost::default();
    h.ob_start();
    h.echo("foo\n");
    h.end_flush();
    let lvl = ob_get_level(&mut h);
    h.echo(&format!("int({lvl})\n"));
    assert_eq!(h.finish(), "foo\nint(0)\n");

    // ob_007
    let mut h = TestHost::default();
    h.ob_start();
    h.echo("foo\n");
    let c = h.get_clean();
    h.echo(&dump(c.as_deref()));
    assert_eq!(h.finish(), "string(4) \"foo\n\"\n");

    // ob_008
    let mut h = TestHost::default();
    h.ob_start();
    h.echo("foo\n");
    let c = h.get_contents().unwrap();
    write(&mut h, &c);
    assert_eq!(h.finish(), "foo\nfoo\n");

    // ob_009
    let mut h = TestHost::default();
    h.ob_start();
    h.echo("foo\n");
    let c = h.get_flush();
    h.echo(&dump(c.as_deref()));
    assert_eq!(h.finish(), "foo\nstring(4) \"foo\n\"\n");
}

#[test]
fn ob_010_handler_output() {
    let mut h = TestHost::default();
    h.ob_start_cb("obh", 0, 0x70, identity());
    h.echo("foo\n");
    assert_eq!(h.finish(), "foo\n");
}

#[test]
fn ob_011_fatal_in_handler() {
    let mut h = TestHost::default();
    h.ob_start_cb("obh", 0, 0x70, cb(|h, _, _| h.get_flush()));
    h.echo("foo\n");
    assert!(!h.fatal);
    assert_eq!(
        h.finish(),
        "\nFatal error: ob_get_flush(): Cannot use output buffering in output buffering display handlers in %s on line %d\n"
    );
}

#[test]
fn bug65593_and_ob_start_error_005() {
    let mut h = TestHost::default();
    h.echo("Test\n");
    h.ob_start_cb(
        "{closure}",
        0,
        0x70,
        cb(|h, _, _| {
            h.ob_start();
            None
        }),
    );
    // Script end → handler runs at shutdown → fatal.
    assert_eq!(
        h.finish(),
        "Test\n\nFatal error: ob_start(): Cannot use output buffering in output buffering display handlers in %s on line %d\n"
    );
}

#[test]
fn gh20837_start_after_fatal_fails() {
    let mut h = TestHost::default();
    h.ob_start_cb(
        "{closure}",
        1,
        0x70,
        cb(|h, _, _| {
            h.ob_start();
            None
        }),
    );
    h.echo("foo");
    assert!(h.fatal);
    // Shutdown function: ob_start() now fails with a notice.
    let ok = h.ob_start_cb(
        "{closure}",
        0,
        0x70,
        cb(|h, s, _| {
            h.echo("bar");
            Some(s.to_ascii_uppercase())
        }),
    );
    assert!(!ok);
    assert_eq!(
        h.finish(),
        "\nFatal error: ob_start(): Cannot use output buffering in output buffering display handlers in %s on line %d\n\nNotice: ob_start(): Failed to create buffer in %s on line %d\n"
    );
}

#[test]
fn ob_012_nesting() {
    let mut h = TestHost::default();
    h.echo("0");
    h.ob_start();
    h.ob_start();
    h.ob_start();
    h.ob_start();
    h.echo("1");
    h.end_flush();
    h.echo("2");
    let ob = h.get_clean().unwrap();
    h.echo("3");
    h.flush();
    h.end_clean();
    h.echo("4");
    h.end_flush();
    write(&mut h, &ob);
    assert_eq!(h.finish(), "03412");
}

#[test]
fn ob_013_list_and_status() {
    let mut h = TestHost::default();
    h.ob_start();
    for name in [
        "a",
        "b",
        "c",
        "d",
        "E::f",
        "E::g",
        "E::__invoke",
        "{closure:%s:%d}",
    ] {
        h.ob_start_cb(name, 0, 0x70, identity());
    }
    h.ob_start();
    h.echo("foo\n");
    h.flush();
    h.end_clean();
    h.flush();
    let names: Vec<String> = ob_list_handlers(&mut h)
        .iter()
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "default output handler",
            "a",
            "b",
            "c",
            "d",
            "E::f",
            "E::g",
            "E::__invoke",
            "{closure:%s:%d}"
        ]
    );
    let top = h.stack.active_status().unwrap();
    assert_eq!(
        (
            top.kind,
            top.flags,
            top.level,
            top.chunk_size,
            top.buffer_size
        ),
        (1, 20593, 8, 0, 16384)
    );
    let full = h.stack.full_status();
    assert_eq!(full.len(), 9);
    assert_eq!(
        (
            full[0].kind,
            full[0].flags,
            full[0].level,
            full[0].buffer_used
        ),
        (0, 112, 0, 0)
    );
    assert_eq!((full[1].kind, full[1].flags, full[1].level), (1, 113, 1));
    assert_eq!(full[7].flags, 113);
    assert_eq!(h.finish(), "foo\n");
}

#[test]
fn ob_014_handler_call_fails() {
    // str_rot13 called with 2 args throws: the call fails, the buffer passes.
    let mut h = TestHost::default();
    h.ob_start_cb("str_rot13", 0, 0x70, cb(|_, _, _| None));
    h.echo("foo\n");
    assert!(h.end_flush());
    h.echo("ArgumentCountError: str_rot13() expects exactly 1 argument, 2 given\n");
    assert_eq!(
        h.finish(),
        "foo\nArgumentCountError: str_rot13() expects exactly 1 argument, 2 given\n"
    );

    // ob_015: chunk size 1 → fails on the echo, handler disabled.
    let mut h = TestHost::default();
    h.ob_start_cb("str_rot13", 1, 0x70, cb(|_, _, _| None));
    h.echo("foo\n");
    h.echo("ArgumentCountError\n");
    h.end_flush();
    assert_eq!(h.finish(), "foo\nArgumentCountError\n");
}

#[test]
fn ob_017_modes() {
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    let l2 = Rc::clone(&log);
    let mut h = TestHost::default();
    h.ob_start_cb(
        "oh",
        3,
        0x70,
        cb(move |_, s, m| {
            l2.borrow_mut()
                .push(format!("{m}: {}", String::from_utf8_lossy(s)));
            Some(s.to_vec())
        }),
    );
    h.echo("yes");
    h.echo("!\n");
    h.flush();
    h.echo("no");
    h.clean();
    h.echo("yes!\n");
    h.echo("no");
    h.end_clean();
    assert_eq!(h.finish(), "yes!\nyes!\n");
    assert_eq!(
        *log.borrow(),
        ["1: yes", "4: !\n", "2: no", "0: yes!\n", "10: no"]
    );
}

#[test]
fn ob_020_list_handlers() {
    let mut h = TestHost::default();
    assert!(ob_list_handlers(&mut h).is_empty());
    h.ob_start();
    h.ob_start();
    assert_eq!(ob_list_handlers(&mut h).len(), 2);
    h.end_flush();
    assert_eq!(
        ob_list_handlers(&mut h),
        vec![b"default output handler".to_vec()]
    );
    h.end_flush();
    assert!(ob_list_handlers(&mut h).is_empty());
}

#[test]
fn notices_without_buffers() {
    let mut h = TestHost::default();
    let r = h.clean();
    h.echo(&dump_bool(r));
    let r = h.end_clean();
    h.echo(&dump_bool(r));
    let r = h.end_flush();
    h.echo(&dump_bool(r));
    let r = h.flush();
    h.echo(&dump_bool(r));
    let r = h.get_flush();
    h.echo(&dump(r.as_deref()));
    let r = h.get_clean();
    h.echo(&dump(r.as_deref()));
    let r = h.get_contents();
    h.echo(&dump(r.as_deref()));
    assert_eq!(
        h.finish(),
        "\nNotice: ob_clean(): Failed to delete buffer. No buffer to delete in %s on line %d\nbool(false)\n\
         \nNotice: ob_end_clean(): Failed to delete buffer. No buffer to delete in %s on line %d\nbool(false)\n\
         \nNotice: ob_end_flush(): Failed to delete and flush buffer. No buffer to delete or flush in %s on line %d\nbool(false)\n\
         \nNotice: ob_flush(): Failed to flush buffer. No buffer to flush in %s on line %d\nbool(false)\n\
         \nNotice: ob_get_flush(): Failed to delete and flush buffer. No buffer to delete or flush in %s on line %d\nbool(false)\n\
         bool(false)\nbool(false)\n"
    );
}

#[test]
fn ob_clean_basic_001() {
    let mut h = TestHost::default();
    h.ob_start();
    h.echo("You should never see this.");
    let r = h.clean();
    h.echo(&dump_bool(r));
    h.echo("Ensure the buffer is still active after the clean.");
    let out = h.get_clean();
    h.echo(&dump(out.as_deref()));
    assert_eq!(
        h.finish(),
        "string(61) \"bool(true)\nEnsure the buffer is still active after the clean.\"\n"
    );
}

#[test]
fn ob_get_length_and_contents() {
    let mut h = TestHost::default();
    assert_eq!(ob_get_length(&mut h), None);
    h.ob_start();
    assert_eq!(ob_get_length(&mut h), Some(0));
    h.echo("hello\n");
    assert_eq!(ob_get_length(&mut h), Some(6));
    h.flush();
    assert_eq!(ob_get_length(&mut h), Some(0));
    assert_eq!(h.get_contents(), Some(Vec::new()));
    h.end_flush();
    assert_eq!(h.get_contents(), None);
    assert_eq!(h.finish(), "hello\n");
}

#[test]
fn unerasable_buffers() {
    let counter = Rc::new(RefCell::new(0));
    let make = |c: Rc<RefCell<i32>>| {
        cb(move |_, s, _| {
            *c.borrow_mut() += 1;
            let mut v = format!("[callback:{}]", c.borrow()).into_bytes();
            v.extend_from_slice(s);
            v.push(b'\n');
            Some(v)
        })
    };
    // ob_start_basic_unerasable_002
    let mut h = TestHost::default();
    h.ob_start_cb("callback", 0, 0, make(Rc::clone(&counter)));
    h.echo("All of the following calls will fail to clean/remove the topmost buffer:\n");
    let r = h.clean();
    h.echo(&dump_bool(r));
    let r = h.end_clean();
    h.echo(&dump_bool(r));
    let r = h.end_flush();
    h.echo(&dump_bool(r));
    let lvl = ob_get_level(&mut h);
    h.echo(&format!("int({lvl})\n"));
    assert_eq!(
        h.finish(),
        "[callback:1]All of the following calls will fail to clean/remove the topmost buffer:\n\
         \nNotice: ob_clean(): Failed to delete buffer of callback (0) in %s on line %d\nbool(false)\n\
         \nNotice: ob_end_clean(): Failed to discard buffer of callback (0) in %s on line %d\nbool(false)\n\
         \nNotice: ob_end_flush(): Failed to send buffer of callback (0) in %s on line %d\nbool(false)\nint(1)\n\n"
    );

    // ob_start_basic_unerasable_003: ob_get_clean
    let counter = Rc::new(RefCell::new(0));
    let mut h = TestHost::default();
    h.ob_start_cb("callback", 0, 0, make(Rc::clone(&counter)));
    h.echo("This call will obtain the content, but will not clean the buffer.");
    let s = h.get_clean();
    // The notices land in the buffer; flush everything at shutdown.
    let s = dump(s.as_deref());
    h.echo(&s);
    assert_eq!(
        h.finish(),
        "[callback:1]This call will obtain the content, but will not clean the buffer.\
         \nNotice: ob_get_clean(): Failed to discard buffer of callback (0) in %s on line %d\n\
         \nNotice: ob_get_clean(): Failed to delete buffer of callback (0) in %s on line %d\n\
         string(65) \"This call will obtain the content, but will not clean the buffer.\"\n\n"
    );

    // ob_start_basic_unerasable_004: ob_get_flush
    let mut h = TestHost::default();
    h.ob_start_cb("callback", 0, 0, make(Rc::new(RefCell::new(0))));
    h.echo("x");
    let s = h.get_flush();
    assert_eq!(s.as_deref(), Some(&b"x"[..]));
    let out = h.finish();
    assert!(out.contains("ob_get_flush(): Failed to send buffer of callback (0)"));
    assert!(out.contains("ob_get_flush(): Failed to delete buffer of callback (0)"));

    // bug46897 / unerasable_005: ob_flush fails, content kept.
    let mut h = TestHost::default();
    h.ob_start_cb("callback", 0, 0, make(Rc::new(RefCell::new(0))));
    h.echo("Attempt to flush unerasable buffer - should fail...");
    let r = h.flush();
    h.echo(&dump_bool(r));
    let c = h.get_contents();
    h.echo(&dump(c.as_deref()));
    let out = h.finish();
    assert!(out.starts_with(
        "[callback:1]Attempt to flush unerasable buffer - should fail...\nNotice: ob_flush(): Failed to flush buffer of callback (0) in %s on line %d\nbool(false)\nstring("
    ));
}

#[test]
fn ob_start_basic_002_return_values() {
    let cases: [(&str, Option<&str>, &str); 5] = [
        ("return_empty_string", Some(""), ""),
        ("return_false", None, "My output."),
        ("return_null", Some(""), ""),
        (
            "return_string",
            Some("I stole your output."),
            "I stole your output.",
        ),
        ("return_zero", Some("0"), "0"),
    ];
    for (name, ret, expected) in cases {
        let mut h = TestHost::default();
        let ret = ret.map(|r| r.as_bytes().to_vec());
        h.ob_start_cb(name, 0, 0x70, cb(move |_, _, _| ret.clone()));
        h.echo("My output.");
        h.end_flush();
        assert_eq!(h.finish(), expected, "{name}");
    }
}

#[test]
fn ob_start_basic_004_chunk_sizes() {
    let run = |cs: i64, pieces: &[&str]| {
        let n = Rc::new(RefCell::new(0));
        let n2 = Rc::clone(&n);
        let mut h = TestHost::default();
        h.ob_start_cb(
            "callback",
            cs,
            0x70,
            cb(move |_, s, _| {
                *n2.borrow_mut() += 1;
                Some(
                    format!(
                        "f[call:{}; len:{}]{}\n",
                        n2.borrow(),
                        s.len(),
                        String::from_utf8_lossy(s)
                    )
                    .into_bytes(),
                )
            }),
        );
        for p in pieces {
            h.echo(p);
        }
        h.end_flush();
        h.finish()
    };
    let ones = ["1", "2", "3", "4", "5", "6", "7", "8"];
    assert_eq!(run(-1, &ones), "f[call:1; len:8]12345678\n");
    assert_eq!(run(0, &ones), "f[call:1; len:8]12345678\n");
    let one = run(1, &ones);
    assert!(one.starts_with("f[call:1; len:1]1\nf[call:2; len:1]2\n"));
    assert!(one.ends_with("f[call:8; len:1]8\nf[call:9; len:0]\n"));
    assert_eq!(
        run(2, &ones),
        "f[call:1; len:2]12\nf[call:2; len:2]34\nf[call:3; len:2]56\nf[call:4; len:2]78\nf[call:5; len:0]\n"
    );
    assert_eq!(
        run(3, &ones),
        "f[call:1; len:3]123\nf[call:2; len:3]456\nf[call:3; len:2]78\n"
    );
    assert_eq!(
        run(5, &ones),
        "f[call:1; len:5]12345\nf[call:2; len:3]678\n"
    );
    assert_eq!(
        run(8, &ones),
        "f[call:1; len:8]12345678\nf[call:2; len:0]\n"
    );
    assert_eq!(run(9, &ones), "f[call:1; len:8]12345678\n");
    let fours = ["1234", "5678"];
    assert_eq!(
        run(1, &fours),
        "f[call:1; len:4]1234\nf[call:2; len:4]5678\nf[call:3; len:0]\n"
    );
    assert_eq!(
        run(4, &fours),
        "f[call:1; len:4]1234\nf[call:2; len:4]5678\nf[call:3; len:0]\n"
    );
    assert_eq!(
        run(5, &fours),
        "f[call:1; len:8]12345678\nf[call:2; len:0]\n"
    );
    assert_eq!(run(9, &fours), "f[call:1; len:8]12345678\n");
}

#[test]
fn bug60768_chunked_discard() {
    let storage: Rc<RefCell<Vec<u8>>> = Rc::default();
    let s2 = Rc::clone(&storage);
    let mut h = TestHost::default();
    h.ob_start_cb(
        "{closure}",
        20,
        0x70,
        cb(move |_, b, _| {
            s2.borrow_mut().extend_from_slice(b);
            Some(Vec::new())
        }),
    );
    h.echo(&"0".repeat(20));
    for i in 0..10 {
        h.echo(&format!("{i:>9}\n"));
    }
    h.end_flush();
    assert_eq!(storage.borrow().len(), 120);
    assert_eq!(h.finish(), "");
}

#[test]
fn gh16135_buffer_sizes() {
    let mut h = TestHost::default();
    for cs in [0, 1, 2, 8191, 8192, 8193] {
        h.call("ob_start", |h| ob_start(h, StartHandler::Default, cs, 0x70));
    }
    let sizes: Vec<usize> = h
        .stack
        .full_status()
        .iter()
        .map(|s| s.buffer_size)
        .collect();
    assert_eq!(sizes, [16384, 4096, 4096, 8192, 8192, 12288]);
    // Growth: appending more than the free space grows by max(chunk, need).
    h.echo(&"x".repeat(13000));
    assert_eq!(h.stack.active_status().unwrap().buffer_size, 12288 * 2);
}

#[test]
fn ob_get_status_default() {
    let mut h = TestHost::default();
    assert_eq!(h.stack.active_status(), None);
    assert!(h.stack.full_status().is_empty());
    h.ob_start();
    let s = h.stack.active_status().unwrap();
    assert_eq!(
        s,
        HandlerStatus {
            name: b"default output handler".to_vec(),
            kind: 0,
            flags: 112,
            level: 0,
            chunk_size: 0,
            buffer_size: 16384,
            buffer_used: 0
        }
    );
    h.end_clean();
}

#[test]
fn ob_start_flags_filtered() {
    let mut h = TestHost::default();
    h.ob_start_cb(
        "{closure}",
        0,
        i64::from(0x70 | 0x1000 | 0x2000 | 0x4000),
        identity(),
    );
    let f = h.stack.active_status().unwrap().flags;
    assert_eq!(
        (f & 0x70, f & 1, f & 0x1000, f & 0x2000, f & 0x4000),
        (112, 1, 0, 0, 0)
    );
}

#[test]
fn invalid_callback_warns() {
    let mut h = TestHost::default();
    let ok = h.call("ob_start", |h| {
        ob_start(
            h,
            StartHandler::Invalid("function \"no\" not found or invalid function name".into()),
            0,
            0x70,
        )
    });
    h.echo(&dump_bool(ok));
    assert_eq!(
        h.finish(),
        "\nWarning: ob_start(): function \"no\" not found or invalid function name in %s on line %d\n\
         \nNotice: ob_start(): Failed to create buffer in %s on line %d\nbool(false)\n"
    );
}

fn echoing(name: &'static str, ret: &'static str, log: &Rc<RefCell<Vec<String>>>) -> Cb {
    let log = Rc::clone(log);
    cb(move |h, s, _| {
        log.borrow_mut()
            .push(format!("{name}: <<<{}>>>", String::from_utf8_lossy(s)));
        h.echo(name);
        Some(ret.as_bytes().to_vec())
    })
}

#[test]
fn functions_that_output() {
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    let mut h = TestHost::default();
    h.echo("\n\nTesting: handle_echo");
    h.ob_start_cb(
        "handle_echo",
        0,
        0x70,
        echoing("handle_echo", "echo\n", &log),
    );
    h.echo("Inside of handle_echo\n");
    h.end_flush();
    h.echo("\nEnd of handle_echo, log was:\n");
    let l = log.borrow().join("\n");
    h.echo(&l);
    assert_eq!(
        h.finish(),
        "\n\nTesting: handle_echo\nDeprecated: ob_end_flush(): Producing output from user output handler handle_echo is deprecated in %s on line %d\necho\n\nEnd of handle_echo, log was:\nhandle_echo: <<<Inside of handle_echo\n>>>"
    );
}

#[test]
fn multiple_handlers_output() {
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    let mut h = TestHost::default();
    h.ob_start_cb(
        "first_handler",
        0,
        0x70,
        echoing("first_handler", "FIRST\n", &log),
    );
    h.ob_start_cb(
        "second_handler",
        0,
        0x70,
        echoing("second_handler", "SECOND\n", &log),
    );
    h.ob_start_cb(
        "third_handler",
        0,
        0x70,
        echoing("third_handler", "THIRD\n", &log),
    );
    h.echo("Testing...");
    h.end_flush();
    h.end_flush();
    h.end_flush();
    h.echo("\n\nLog:\n");
    let l = log.borrow().join("\n");
    h.echo(&l);
    let dep = |n: &str| {
        format!("\nDeprecated: ob_end_flush(): Producing output from user output handler {n} is deprecated in %s on line %d\n")
    };
    let expected = format!(
        "{}FIRST\n\n\nLog:\nthird_handler: <<<Testing...>>>\nsecond_handler: <<<{}THIRD\n>>>\nfirst_handler: <<<{}SECOND\n>>>",
        dep("first_handler"),
        dep("third_handler"),
        dep("second_handler")
    );
    assert_eq!(h.finish(), expected);
}

#[test]
fn handler_inconsistent_echo() {
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    let l2 = Rc::clone(&log);
    let mut h = TestHost::default();
    h.ob_start_cb(
        "handler",
        0,
        0x70,
        cb(move |h, s, _| {
            l2.borrow_mut()
                .push(format!("handler: <<<{}>>>", String::from_utf8_lossy(s)));
            if s == b"DO ECHO\n" {
                h.echo("handler");
            }
            Some(s.to_vec())
        }),
    );
    h.echo("DO ECHO\n");
    h.flush();
    h.echo("NO ECHO\n");
    h.flush();
    h.echo("DO ECHO\n");
    h.flush();
    h.echo("LAST ONE\n");
    h.end_flush();
    let dep = "Deprecated: ob_flush(): Producing output from user output handler handler is deprecated in %s on line %d\n";
    assert_eq!(
        h.finish(),
        format!("\n{dep}DO ECHO\nNO ECHO\n\n{dep}DO ECHO\nLAST ONE\n")
    );
    assert_eq!(log.borrow().len(), 4);
}

#[test]
fn gh15181_disabled_handler_not_flushed_again() {
    let mut h = TestHost::default();
    h.ob_start_cb("{closure}", 0, 0x70, cb(|_, _, _| None));
    assert!(h.flush());
    assert!(h.flush());
    h.echo("===DONE===\n");
    assert_eq!(h.finish(), "===DONE===\n");
}

#[test]
fn implicit_flush_and_sapi_flush() {
    let mut h = TestHost::default();
    ob_implicit_flush(&mut h, true);
    h.ob_start();
    h.echo("buffered");
    assert_eq!(h.sapi_flushes, 0);
    h.end_clean();
    h.echo("Done");
    assert_eq!(h.sapi_flushes, 1);
    assert_eq!(h.stack.status() & status::IMPLICITFLUSH, 1);
    ob_implicit_flush(&mut h, false);
    assert_eq!(h.finish(), "Done");
}

#[test]
fn request_lifecycle_and_ini() {
    // output_buffering=4096 starts a default handler with that chunk size.
    let mut h = TestHost::default();
    request_startup(&mut h, None, 4096, false);
    assert_eq!(h.stack.active_status().unwrap().chunk_size, 4096);
    // bug60322: drop all buffers, then ob_get_clean() is false without notice.
    while h.call("ob_end_clean", ob_end_clean) {}
    assert_eq!(h.get_clean(), None);
    let out = h.finish();
    assert_eq!(
        out,
        "\nNotice: ob_end_clean(): Failed to delete buffer. No buffer to delete in %s on line %d\n"
    );

    // output_buffering=1 → chunk size 0.
    let mut h = TestHost::default();
    request_startup(&mut h, None, 1, false);
    assert_eq!(h.stack.active_status().unwrap().chunk_size, 0);

    // output_handler takes precedence.
    let mut h = TestHost::default();
    request_startup(
        &mut h,
        Some(StartHandler::User {
            name: b"up".to_vec(),
            callback: cb(|_, s, _| Some(s.to_ascii_uppercase())),
        }),
        0,
        true,
    );
    h.echo("abc");
    assert_eq!(h.stack.status() & status::IMPLICITFLUSH, 0);
    assert_eq!(h.finish(), "ABC");

    // implicit_flush only when no buffering.
    let mut h = TestHost::default();
    request_startup(&mut h, None, 0, true);
    assert_eq!(h.stack.status() & status::IMPLICITFLUSH, 1);

    // After deactivation, writes go straight through (php_output_direct).
    let mut h = TestHost::default();
    deactivate(&mut h);
    assert!(!h.stack.is_activated());
    assert_eq!(write(&mut h, b"direct"), 6);
    assert_eq!(h.out, b"direct");
    h.stack.set_status(status::DISABLED);
    assert_eq!(write(&mut h, b"x"), 0);
}

#[test]
fn headers_failure_disables_output() {
    let mut h = TestHost {
        headers_ok: Some(false),
        ..TestHost::default()
    };
    h.echo("lost");
    assert!(h.out.is_empty());
    assert_eq!(h.stack.status() & status::DISABLED, status::DISABLED);
}

#[test]
fn status_flags() {
    let mut h = TestHost::default();
    assert_eq!(h.stack.status(), 0);
    h.ob_start();
    h.echo("x");
    assert_eq!(h.stack.status(), status::ACTIVE | status::WRITTEN);
    h.end_flush();
    assert_eq!(h.stack.status() & status::SENT, status::SENT);
    assert!(!h.stack.handler_started(b"default output handler"));
}

#[test]
fn clean_all_discard_all_flush_all() {
    let mut h = TestHost::default();
    h.ob_start();
    h.echo("a");
    h.ob_start();
    h.echo("b");
    flush_all(&mut h);
    // FLUSH op through the stack: top passes "b" down, bottom keeps "ab".
    assert_eq!(h.get_contents(), Some(Vec::new()));
    h.end_flush();
    assert_eq!(h.get_contents(), Some(Vec::new()));
    assert_eq!(h.out, b"ab");
    h.ob_start();
    h.echo("c");
    clean_all(&mut h);
    assert_eq!(h.get_contents(), Some(Vec::new()));
    h.echo("d");
    discard_all(&mut h);
    assert_eq!(ob_get_level(&mut h), 0);
    assert_eq!(h.finish(), "ab");
}

#[test]
fn internal_and_devnull_handlers() {
    let mut h = TestHost::default();
    h.call("ob_start", |h| {
        ob_start(
            h,
            StartHandler::Internal {
                name: b"ob_upper".to_vec(),
                func: Box::new(|s: &[u8], _| Some(s.to_ascii_uppercase())),
            },
            0,
            0x70,
        )
    });
    h.echo("hi");
    h.end_flush();
    assert!(start(&mut h, StartHandler::DevNull, 0, 0));
    let st = h.stack.active_status().unwrap();
    assert_eq!(
        (st.name.as_slice(), st.flags & 0x70),
        (&b"null output handler"[..], 0)
    );
    h.echo("swallowed");
    end_all(&mut h);
    assert_eq!(h.finish(), "HI");
}

#[test]
fn simple_output_host() {
    #[derive(Clone, Default)]
    struct Shared(Rc<RefCell<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let sink = Shared::default();
    let mut o = SimpleOutput::new(Box::new(sink.clone()));
    ob_start(
        &mut o,
        StartHandler::User {
            name: b"rev".to_vec(),
            callback: Box::new(|s: &[u8], _| Some(s.iter().rev().copied().collect())),
        },
        0,
        0x70,
    );
    write(&mut o, b"abc");
    ob_end_flush(&mut o);
    ob_end_flush(&mut o);
    assert_eq!(*sink.0.borrow(), b"cba");
    assert_eq!(o.diagnostics.len(), 1);
    assert_eq!(
        o.diagnostics[0].render(Some("ob_end_flush")),
        "ob_end_flush(): Failed to delete and flush buffer. No buffer to delete or flush"
    );
    assert!(format!("{o:?}").contains("SimpleOutput"));
}
