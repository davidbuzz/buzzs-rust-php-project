//! User save handlers (`ext/session/tests/user_session_module/*.phpt`).
#![allow(clippy::too_many_lines)]

mod common;

use common::{s, show, FakeHost};
use php_runtime::{RuntimeError, Value};
use php_session::api::*;
use php_session::class::*;
use php_session::{ini, SessionStatus};
use std::cell::RefCell;
use std::rc::Rc;

type Trace = Rc<RefCell<Vec<String>>>;

fn args_str(args: &[Value]) -> String {
    args.iter().map(show).collect::<Vec<_>>().join(", ")
}

/// Procedural handlers that log every call and keep data in memory.
fn traced(h: &mut FakeHost, prefix: &str) -> Trace {
    let trace: Trace = Rc::default();
    let store: Rc<RefCell<Vec<u8>>> = Rc::default();
    for name in [
        "open",
        "close",
        "read",
        "write",
        "destroy",
        "gc",
        "create_sid",
        "validate_sid",
        "update_timestamp",
    ] {
        let t = trace.clone();
        let st = store.clone();
        let key = format!("{prefix}{name}");
        let name = name.to_string();
        h.on(&key, move |_, args| {
            t.borrow_mut().push(format!("{name}({})", args_str(args)));
            Ok(Some(match name.as_str() {
                "read" => Value::Str(st.borrow().clone().into()),
                "write" => {
                    *st.borrow_mut() = args[1].as_str().unwrap().as_bytes().to_vec();
                    Value::Bool(true)
                }
                "gc" => Value::Int(0),
                "create_sid" => Value::string("customsid"),
                _ => Value::Bool(true),
            }))
        });
    }
    trace
}

fn set_procedural(h: &mut FakeHost, names: &[&str]) -> Result<bool, RuntimeError> {
    let args = FakeHost::procedural_args(names);
    h.call("session_set_save_handler", |h| {
        session_set_save_handler(h, &args)
    })
}

#[test]
fn procedural_handlers_full_cycle() {
    let mut h = FakeHost::with_ini(&[
        ("session.use_strict_mode", "0"),
        ("session.save_path", "/sp"),
    ]);
    let trace = traced(&mut h, "");
    assert!(set_procedural(&mut h, &["open", "close", "read", "write", "destroy", "gc"]).unwrap());
    assert_eq!(
        h.take_log(),
        vec!["Deprecated: session_set_save_handler(): Providing individual callbacks instead of an object implementing SessionHandlerInterface is deprecated"]
    );
    assert_eq!(
        ini::ini_get(&mut h, "session.save_handler").unwrap(),
        b"user"
    );
    h.call("session_id", |h| session_id(h, Some(&s("abc"))))
        .unwrap();
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    h.set_var("foo", Value::string("hello"));
    assert!(h
        .call("session_write_close", |h| session_write_close(h))
        .unwrap());
    assert_eq!(
        *trace.borrow(),
        vec![
            "open(string(3) \"/sp\", string(9) \"PHPSESSID\")",
            "read(string(3) \"abc\")",
            "write(string(3) \"abc\", string(16) \"foo|s:5:\"hello\";\")",
            "close()",
        ]
    );
    trace.borrow_mut().clear();
    // Lazy write: unchanged data goes through write() again (no update_timestamp callback).
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    assert_eq!(h.session_len(), 1);
    assert!(h
        .call("session_write_close", |h| session_write_close(h))
        .unwrap());
    assert_eq!(
        trace.borrow()[2],
        "write(string(3) \"abc\", string(16) \"foo|s:5:\"hello\";\")"
    );
    trace.borrow_mut().clear();
    // With the optional callbacks.
    assert!(set_procedural(
        &mut h,
        &[
            "open",
            "close",
            "read",
            "write",
            "destroy",
            "gc",
            "create_sid",
            "validate_sid",
            "update_timestamp"
        ]
    )
    .unwrap());
    h.take_log();
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    assert!(h
        .call("session_write_close", |h| session_write_close(h))
        .unwrap());
    assert_eq!(
        trace.borrow()[2],
        "update_timestamp(string(3) \"abc\", string(16) \"foo|s:5:\"hello\";\")"
    );
    trace.borrow_mut().clear();
    // Strict mode: validate_sid rejects? it accepts here; regenerate uses create_sid.
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    assert!(h
        .call("session_regenerate_id", |h| session_regenerate_id(h, true))
        .unwrap());
    assert_eq!(h.state.id().unwrap().as_bytes(), b"customsid");
    assert_eq!(
        *trace.borrow(),
        vec![
            "open(string(3) \"/sp\", string(9) \"PHPSESSID\")",
            "read(string(3) \"abc\")",
            "destroy(string(3) \"abc\")",
            "close()",
            "open(string(3) \"/sp\", string(9) \"PHPSESSID\")",
            "create_sid()",
            "read(string(9) \"customsid\")",
        ]
    );
    assert!(h
        .headers
        .iter()
        .any(|l| l == "Set-Cookie: PHPSESSID=customsid; path=/; HttpOnly; SameSite=Lax"));
    assert!(h.take_log().is_empty());
}

#[test]
fn bool_return_validation() {
    // session_set_save_handler_type_error.phpt / _type_error2.phpt
    for (ret, expected_err, expected_log) in [
        (Value::empty_array(), Some("TypeError: Session callback must have a return value of type bool, array returned"), vec![]),
        (Value::Null, Some("TypeError: Session callback must have a return value of type bool, null returned"), vec![]),
        (Value::Int(1), Some("TypeError: Session callback must have a return value of type bool, int returned"), vec![]),
        (Value::Int(0), None, vec![
            "Deprecated: session_start(): Session callback must have a return value of type bool, int returned",
            "Warning: session_start(): Failed to read session data: user (path: )",
        ]),
    ] {
        let mut h = FakeHost::with_ini(&[]);
        let r = ret.clone();
        h.on("bad", move |_, _| Ok(Some(r.clone())));
        h.on("valid", |_, _| Ok(Some(Value::Bool(true))));
        set_procedural(&mut h, &["bad", "valid", "valid", "valid", "valid", "valid"]).unwrap();
        h.take_log();
        let res = h.call("session_start", |h| session_start(h, None));
        match expected_err {
            Some(msg) => assert_eq!(res.unwrap_err().to_string(), msg),
            None => assert!(!res.unwrap()),
        }
        assert_eq!(h.take_log(), expected_log);
        assert_eq!(h.state.status(), SessionStatus::None);
    }
}

#[test]
fn invalid_callbacks() {
    // session_set_save_handler_error4.phpt
    let mut h = FakeHost::with_ini(&[]);
    h.on("callback", |_, _| Ok(Some(Value::Bool(true))));
    for i in 1..6 {
        let mut names = vec!["callback"; 6];
        names[i] = "echo";
        let e = set_procedural(&mut h, &names).unwrap_err();
        let pname = ["open", "close", "read", "write", "destroy", "gc"][i];
        assert_eq!(
            e.to_string(),
            format!("TypeError: session_set_save_handler(): Argument #{} (${pname}) must be a valid callback, function \"echo\" not found or invalid function name", i + 1)
        );
    }
    set_procedural(&mut h, &["callback"; 6]).unwrap();
    h.take_log();
    assert!(!h.call("session_start", |h| session_start(h, None)).unwrap());
    assert_eq!(
        h.take_log(),
        vec!["Warning: session_start(): Failed to read session data: user (path: )"]
    );
    let e = set_procedural(&mut h, &["callback"; 3]).unwrap_err();
    assert_eq!(
        e.to_string(),
        "ArgumentCountError: session_set_save_handler() expects at least 6 arguments, 3 given"
    );
    let e = h
        .call("session_set_save_handler", |h| {
            session_set_save_handler(h, &[Value::string("x")])
        })
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        "TypeError: session_set_save_handler(): Argument #1 ($open) must be of type SessionHandlerInterface, string given"
    );
}

#[test]
fn exception_in_open_stops_everything() {
    // session_set_save_handler_error3.phpt
    let mut h = FakeHost::with_ini(&[]);
    let trace = traced(&mut h, "t_");
    h.on("open", |_, _| {
        Err(RuntimeError::Custom {
            class: "Exception".into(),
            message: "Do something bad..!".into(),
        })
    });
    set_procedural(
        &mut h,
        &["open", "t_close", "t_read", "t_write", "t_destroy", "t_gc"],
    )
    .unwrap();
    h.take_log();
    let e = h
        .call("session_start", |h| session_start(h, None))
        .unwrap_err();
    assert_eq!(e.to_string(), "Exception: Do something bad..!");
    assert!(trace.borrow().is_empty(), "{:?}", trace.borrow());
    assert!(h.take_log().is_empty());
}

fn handler_object(
    h: &mut FakeHost,
    class: &str,
    extends_session_handler: bool,
    extra: &[&str],
) -> Value {
    let mut ifaces = vec!["SessionHandlerInterface"];
    if extends_session_handler {
        ifaces.push("SessionIdInterface");
    }
    let mut methods = vec!["open", "close", "read", "write", "destroy", "gc"];
    if extends_session_handler {
        methods.extend(["create_sid", "validateId"]);
    }
    methods.extend_from_slice(extra);
    h.object(class, &ifaces, &methods)
}

#[test]
fn object_handler_deprecations_and_recursion() {
    // session_regenerate_id_handler_closes_session.phpt
    let mut h = FakeHost::with_ini(&[("session.gc_probability", "0")]);
    let obj = handler_object(&mut h, "MySessionHandler", false, &[]);
    for m in ["open", "read", "write", "destroy", "gc"] {
        let ret = match m {
            "read" => Value::string(""),
            "gc" => Value::Int(0),
            _ => Value::Bool(true),
        };
        h.on(&format!("MySessionHandler::{m}"), move |_, _| {
            Ok(Some(ret.clone()))
        });
    }
    let destroyed = Rc::new(RefCell::new(false));
    h.on("MySessionHandler::close", move |h, _| {
        if !*destroyed.borrow() {
            *destroyed.borrow_mut() = true;
            h.call("session_destroy", |h| session_destroy(h))?;
        }
        Ok(Some(Value::Bool(true)))
    });
    let warnings = handler_interface_implemented("MySessionHandler", false, false, false, false);
    assert_eq!(warnings.len(), 2);
    assert!(h
        .call("session_set_save_handler", |h| session_set_save_handler(
            h,
            &[obj, Value::Bool(true)]
        ))
        .unwrap());
    assert!(h.shutdown_registered);
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    assert!(!h
        .call("session_regenerate_id", |h| session_regenerate_id(h, false))
        .unwrap());
    assert_eq!(session_status(&mut h), 1);
    assert_eq!(
        h.take_log(),
        vec![
            "Deprecated: session_set_save_handler(): Providing an object to argument #1 ($sessionhandler) which does not have the create_sid() method defined is deprecated",
            "Deprecated: session_set_save_handler(): Providing an object to argument #1 ($sessionhandler) which does not have the validateId() method defined is deprecated",
            "Warning: session_destroy(): Cannot call session save handler in a recursive manner",
            "Warning: session_destroy(): Session object destruction failed",
            "Warning: session_regenerate_id(): Session ID cannot be regenerated because the save handler closed the session",
        ]
    );
}

#[test]
fn session_handler_parent_delegation() {
    // session_set_save_handler_class_*.phpt: MySession extends SessionHandler
    // and calls parent::open()/read()/write() (the files module).
    let dir = common::tempdir("parent");
    let path = dir.to_string_lossy().into_owned();
    let mut h = FakeHost::with_ini(&[
        ("session.use_strict_mode", "0"),
        ("session.save_path", &path),
    ]);
    let obj = handler_object(&mut h, "MySession", true, &[]);
    let trace: Trace = Rc::default();
    let t = trace.clone();
    h.on("MySession::open", move |h, a| {
        t.borrow_mut().push("open".into());
        let (p, n) = (
            a[0].as_str().unwrap().clone(),
            a[1].as_str().unwrap().clone(),
        );
        Ok(Some(Value::Bool(h.call("SessionHandler::open", |h| {
            session_handler_open(h, p.as_bytes(), n.as_bytes())
        })?)))
    });
    h.on("MySession::close", |h, _| {
        Ok(Some(Value::Bool(h.call("SessionHandler::close", |h| {
            session_handler_close(h)
        })?)))
    });
    h.on("MySession::read", |h, a| {
        let id = a[0].as_str().unwrap().clone();
        h.call("SessionHandler::read", |h| {
            session_handler_read(h, id.as_bytes())
        })
        .map(Some)
    });
    h.on("MySession::write", |h, a| {
        let (id, d) = (
            a[0].as_str().unwrap().clone(),
            a[1].as_str().unwrap().clone(),
        );
        Ok(Some(Value::Bool(h.call("SessionHandler::write", |h| {
            session_handler_write(h, id.as_bytes(), d.as_bytes())
        })?)))
    });
    h.on("MySession::destroy", |h, a| {
        let id = a[0].as_str().unwrap().clone();
        Ok(Some(Value::Bool(
            h.call("SessionHandler::destroy", |h| {
                session_handler_destroy(h, id.as_bytes())
            })?,
        )))
    });
    h.on("MySession::gc", |h, a| {
        let n = a[0].as_int().unwrap();
        h.call("SessionHandler::gc", |h| session_handler_gc(h, n))
            .map(Some)
    });
    h.on("MySession::create_sid", |h, _| {
        h.call("SessionHandler::create_sid", |h| {
            session_handler_create_sid(h)
        })
        .map(Some)
    });
    h.on("MySession::validateid", |h, a| {
        let id = a[0].as_str().unwrap().clone();
        Ok(Some(Value::Bool(
            h.call("SessionHandler::validateId", |h| {
                session_handler_validate_id(h, id.as_bytes())
            })?,
        )))
    });
    // Outside an active session the parent refuses.
    let e = session_handler_open(&mut h, b"", b"").unwrap_err();
    assert_eq!(e.to_string(), "Error: Session is not active");

    assert!(h
        .call("session_set_save_handler", |h| session_set_save_handler(
            h,
            &[obj]
        ))
        .unwrap());
    h.call("session_id", |h| session_id(h, Some(&s("parentid"))))
        .unwrap();
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    h.set_var("foo", Value::string("hello"));
    assert!(h
        .call("session_write_close", |h| session_write_close(h))
        .unwrap());
    assert_eq!(
        std::fs::read(dir.join("sess_parentid")).unwrap(),
        b"foo|s:5:\"hello\";"
    );
    let ok = h.call("session_start", |h| session_start(h, None)).unwrap();
    assert!(ok, "{:?}", h.log);
    assert_eq!(h.session_len(), 1);
    // session_create_id() goes through the user create_sid -> parent files.
    let id = h
        .call("session_create_id", |h| {
            session_create_id(h, Some(&s("pre-")))
        })
        .unwrap();
    let id = id.as_str().unwrap().to_string_lossy();
    assert!(id.starts_with("pre-") && id.len() == 36, "{id}");
    assert!(h.call("session_destroy", |h| session_destroy(h)).unwrap());
    assert!(!dir.join("sess_parentid").exists());
    assert!(h.take_log().is_empty(), "{:?}", h.log);
    assert_eq!(trace.borrow().len(), 2);
    // A handler that never opens its parent: parent calls warn.
    h.on("MySession::open", |_, _| Ok(Some(Value::Bool(true))));
    h.on("MySession::read", |_, _| Ok(Some(Value::string(""))));
    h.on("MySession::close", |_, _| Ok(Some(Value::Bool(true))));
    let ok = h.call("session_start", |h| session_start(h, None)).unwrap();
    assert!(ok, "{:?}", h.log);
    assert_eq!(
        h.take_log(),
        vec!["Warning: SessionHandler::create_sid(): Parent session handler is not open, defaulting to session_create_id()"]
    );
    assert!(!h
        .call("SessionHandler::close", |h| session_handler_close(h))
        .unwrap());
    assert!(h
        .call("SessionHandler::validateId", |h| {
            session_handler_validate_id(h, b"x")
        })
        .unwrap());
    let sid = h
        .call("SessionHandler::create_sid", |h| {
            session_handler_create_sid(h)
        })
        .unwrap();
    assert_eq!(sid.as_str().unwrap().len(), 32);
    assert_eq!(
        show(
            &h.call("SessionHandler::read", |h| session_handler_read(h, b"x"))
                .unwrap()
        ),
        "bool(false)"
    );
    assert_eq!(
        h.take_log(),
        vec![
            "Warning: SessionHandler::close(): Parent session handler is not open",
            "Warning: SessionHandler::validateId(): Parent session handler is not open, ignoring ID validation",
            "Warning: SessionHandler::create_sid(): Parent session handler is not open, defaulting to session_create_id()",
            "Warning: SessionHandler::read(): Parent session handler is not open",
        ]
    );
}

#[test]
fn write_failure_messages() {
    let mut h = FakeHost::with_ini(&[("session.save_path", "/sp")]);
    h.simple_handlers("");
    h.on("write", |_, _| Ok(Some(Value::Bool(false))));
    set_procedural(&mut h, &["open", "close", "read", "write", "destroy", "gc"]).unwrap();
    h.take_log();
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    h.set_var("x", Value::Int(1));
    assert!(h
        .call("session_write_close", |h| session_write_close(h))
        .unwrap());
    assert_eq!(
        h.take_log(),
        vec!["Warning: session_write_close(): Failed to write session data using user defined save handler. (session.save_path: /sp, handler: write)"]
    );
    // Invalid key with the php serializer: no write at all.
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    h.set_var("bad|key", Value::Int(1));
    assert!(h
        .call("session_write_close", |h| session_write_close(h))
        .unwrap());
    assert_eq!(
        h.take_log(),
        vec!["Warning: session_write_close(): Failed to write session data. Data contains invalid key \"bad|key\""]
    );
}

#[test]
fn create_sid_type_error() {
    // session_set_save_handler_sid_002.phpt
    let mut h = FakeHost::with_ini(&[]);
    h.simple_handlers("");
    h.on("create_sid", |_, _| Ok(Some(Value::Bool(false))));
    set_procedural(
        &mut h,
        &[
            "open",
            "close",
            "read",
            "write",
            "destroy",
            "gc",
            "create_sid",
        ],
    )
    .unwrap();
    h.take_log();
    let e = h
        .call("session_start", |h| session_start(h, None))
        .unwrap_err();
    assert_eq!(
        e.to_string(),
        "TypeError: Session id must be of type string, bool given"
    );
    assert!(h.take_log().is_empty());
}

#[test]
fn strict_mode_regenerates_unknown_ids() {
    let mut h = FakeHost::with_ini(&[("session.use_strict_mode", "1")]);
    h.simple_handlers("");
    h.on("validate", |_, a| {
        Ok(Some(Value::Bool(
            a[0].as_str().unwrap().as_bytes() == b"known",
        )))
    });
    h.on("create", |_, _| Ok(Some(Value::string("fresh"))));
    set_procedural(
        &mut h,
        &[
            "open", "close", "read", "write", "destroy", "gc", "create", "validate",
        ],
    )
    .unwrap();
    h.take_log();
    h.set_global("_COOKIE", &[("PHPSESSID", Value::string("attacker"))]);
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    assert_eq!(h.state.id().unwrap().as_bytes(), b"fresh");
    assert_eq!(
        h.headers[0],
        "Set-Cookie: PHPSESSID=fresh; path=/; HttpOnly; SameSite=Lax"
    );
    h.call("session_abort", |h| session_abort(h)).unwrap();
    // The ID survives session_abort(): no new lookup happens.
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    assert_eq!(h.state.id().unwrap().as_bytes(), b"fresh");
    h.call("session_abort", |h| session_abort(h)).unwrap();
    let mut h2 = FakeHost::with_ini(&[("session.use_strict_mode", "1")]);
    h2.callbacks = h.callbacks.clone();
    let mut h = h2;
    set_procedural(
        &mut h,
        &[
            "open", "close", "read", "write", "destroy", "gc", "create", "validate",
        ],
    )
    .unwrap();
    h.set_global("_COOKIE", &[("PHPSESSID", Value::string("known"))]);
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    assert_eq!(h.state.id().unwrap().as_bytes(), b"known");
    // The ID came from the cookie: no Set-Cookie is sent again.
    assert!(!h.headers.iter().any(|l| l.starts_with("Set-Cookie")));
}

#[test]
fn shutdown_bailout_reports_recursion() {
    // bug60634_error_3.phpt: write() dies with a fatal error at shutdown.
    let mut h = FakeHost::with_ini(&[]);
    h.simple_handlers("");
    h.on("write", |_, _| {
        Err(RuntimeError::Fatal("Uncaught Error".into()))
    });
    set_procedural(&mut h, &["open", "close", "read", "write", "destroy", "gc"]).unwrap();
    h.take_log();
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    h.func = "PHP Request Shutdown".into();
    assert!(php_session::request_shutdown(&mut h).is_err());
    assert_eq!(
        h.take_log(),
        vec![
            "Warning: PHP Request Shutdown: Cannot call session save handler in a recursive manner"
        ]
    );
}

/// Registers the native `SessionHandler` methods as callbacks.
fn native_session_handler(h: &mut FakeHost) -> Value {
    fn bytes(v: &Value) -> Vec<u8> {
        v.as_str()
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_default()
    }
    h.on("SessionHandler::open", |h, a| {
        let (p, n) = (bytes(&a[0]), bytes(&a[1]));
        session_handler_open(h, &p, &n).map(|b| Some(Value::Bool(b)))
    });
    h.on("SessionHandler::close", |h, _| {
        session_handler_close(h).map(|b| Some(Value::Bool(b)))
    });
    h.on("SessionHandler::read", |h, a| {
        session_handler_read(h, &bytes(&a[0])).map(Some)
    });
    h.on("SessionHandler::write", |h, a| {
        session_handler_write(h, &bytes(&a[0]), &bytes(&a[1])).map(|b| Some(Value::Bool(b)))
    });
    h.on("SessionHandler::destroy", |h, a| {
        session_handler_destroy(h, &bytes(&a[0])).map(|b| Some(Value::Bool(b)))
    });
    h.on("SessionHandler::gc", |h, a| {
        session_handler_gc(h, a[0].as_int().unwrap_or(0)).map(Some)
    });
    h.on("SessionHandler::create_sid", |h, _| {
        session_handler_create_sid(h).map(Some)
    });
    h.on("SessionHandler::validateid", |h, a| {
        session_handler_validate_id(h, &bytes(&a[0])).map(|b| Some(Value::Bool(b)))
    });
    h.object(
        "SessionHandler",
        &["SessionHandlerInterface", "SessionIdInterface"],
        &[
            "open",
            "close",
            "read",
            "write",
            "destroy",
            "gc",
            "create_sid",
            "validateId",
        ],
    )
}

#[test]
fn strict_mode_with_plain_session_handler() {
    // session_strict_mode_sessionhandler.phpt
    let dir = common::tempdir("plainsh");
    let path = dir.to_string_lossy().into_owned();
    let mut h = FakeHost::with_ini(&[
        ("session.use_strict_mode", "1"),
        ("session.use_cookies", "0"),
        ("session.cache_limiter", ""),
        ("session.gc_probability", "0"),
        ("session.save_path", &path),
    ]);
    std::fs::write(dir.join("sess_knownfilesessionid"), b"value|s:5:\"files\";").unwrap();
    let obj = native_session_handler(&mut h);
    assert!(h
        .call("session_set_save_handler", |h| session_set_save_handler(
            h,
            &[obj, Value::Bool(true)]
        ))
        .unwrap());
    h.call("session_id", |h| {
        session_id(h, Some(&s("knownfilesessionid")))
    })
    .unwrap();
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    assert_eq!(h.state.id().unwrap().as_bytes(), b"knownfilesessionid");
    let sess = h.session();
    assert_eq!(
        show(sess.as_array().unwrap().get_str(b"value").unwrap()),
        "string(5) \"files\""
    );
    h.call("session_write_close", |h| session_write_close(h))
        .unwrap();
    h.call("session_id", |h| {
        session_id(h, Some(&s("attackerchosensessionid")))
    })
    .unwrap();
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    assert_ne!(h.state.id().unwrap().as_bytes(), b"attackerchosensessionid");
    h.call("session_write_close", |h| session_write_close(h))
        .unwrap();
    assert!(h.take_log().is_empty(), "{:?}", h.log);
    // gc through the parent (bug55688.phpt / bug78624.phpt).
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    let n = h.call("session_gc", |h| session_gc(h)).unwrap();
    assert_eq!(show(&n), "int(0)");
}

#[test]
fn failure_messages_name_the_handler() {
    // gh7787.phpt
    let mut h = FakeHost::with_ini(&[("session.use_strict_mode", "0")]);
    let obj = h.object(
        "MySessionHandler",
        &[
            "SessionHandlerInterface",
            "SessionIdInterface",
            "SessionUpdateTimestampHandlerInterface",
        ],
        &[
            "open",
            "close",
            "read",
            "write",
            "destroy",
            "gc",
            "create_sid",
            "validateId",
            "updateTimestamp",
        ],
    );
    for (m, v) in [
        ("open", Value::Bool(true)),
        ("close", Value::Bool(true)),
        ("read", Value::string("foo|s:3:\"foo\";")),
        ("write", Value::Bool(false)),
        ("destroy", Value::Bool(true)),
        ("gc", Value::Bool(true)),
        (
            "create_sid",
            Value::string("0123456789abcdef0123456789abcdef01234567"),
        ),
        ("validateid", Value::Bool(true)),
        ("updatetimestamp", Value::Bool(false)),
    ] {
        h.on(&format!("MySessionHandler::{m}"), move |_, _| {
            Ok(Some(v.clone()))
        });
    }
    h.call("session_set_save_handler", |h| {
        session_set_save_handler(h, &[obj])
    })
    .unwrap();
    for change in [true, false] {
        assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
        if change {
            h.set_var("foo", Value::string("bar"));
        }
        h.call("session_write_close", |h| session_write_close(h))
            .unwrap();
    }
    assert_eq!(
        h.take_log(),
        vec![
            "Warning: session_write_close(): Failed to write session data using user defined save handler. (session.save_path: , handler: MySessionHandler::write)",
            "Warning: session_write_close(): Failed to write session data using user defined save handler. (session.save_path: , handler: MySessionHandler::updateTimestamp)",
        ]
    );
}

#[test]
fn create_id_without_validate_id() {
    // gh9583.phpt: no validateId() → session_create_id() must not loop.
    let mut h = FakeHost::with_ini(&[("session.use_strict_mode", "1")]);
    let obj = h.object(
        "SessionHandlerTester",
        &["SessionHandlerInterface"],
        &["open", "close", "read", "write", "destroy", "gc"],
    );
    for (m, v) in [
        ("open", Value::Bool(true)),
        ("close", Value::Bool(true)),
        ("read", Value::string("")),
        ("write", Value::Bool(true)),
        ("destroy", Value::Bool(true)),
        ("gc", Value::Int(1)),
    ] {
        h.on(&format!("SessionHandlerTester::{m}"), move |_, _| {
            Ok(Some(v.clone()))
        });
    }
    h.call("session_set_save_handler", |h| {
        session_set_save_handler(h, &[obj])
    })
    .unwrap();
    assert!(h.call("session_start", |h| session_start(h, None)).unwrap());
    let id = h
        .call("session_create_id", |h| session_create_id(h, None))
        .unwrap();
    assert_eq!(id.as_str().unwrap().len(), 32);
    assert_eq!(h.take_log().len(), 2);
}
