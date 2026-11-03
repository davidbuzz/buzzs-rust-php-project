//! Runs the `.phpt` scripts of `ext/soap/tests` (interop rounds, bug
//! regressions, client/server tests) with the test interpreter over the
//! extension and compares their output with the expected one.

mod common;

use std::path::{Path, PathBuf};

use common::php::run_file;
use common::{output_matches, phpt_sections, soap_tests_dir, with_big_stack, TestHost};

/// Tests whose scripts need engine features the test interpreter lacks
/// (sessions, references, `__get`/`__set`, Reflection, `unserialize`,
/// visibility-mangled dumps, pcntl, process spawning, generators, …).
/// They are exercised by the phpt runner once the extension is wired into
/// the VM.
const ENGINE_DEPENDENT: &[&str] = &[
    "GHSA-m33r-qmcv-p97q.phpt",
    "SoapServer/persistent-session-no-free-SoapFault.phpt",
    "bugs/bug28751.phpt",
    "bugs/bug29830.phpt",
    "bugs/bug30045.phpt",
    "bugs/bug30928.phpt",
    "bugs/bug31422.phpt",
    "bugs/bug39815.phpt",
    "bugs/bug42151.phpt",
    "bugs/bug46760.phpt",
    "bugs/bug47021.phpt",
    "bugs/bug47925.phpt",
    "bugs/bug54911.phpt",
    "bugs/bug61525.phpt",
    "bugs/bug69085.phpt",
    "bugs/bug70388.phpt",
    "bugs/bug70469.phpt",
    "bugs/bug73237.phpt",
    "bugs/bug73452.phpt",
    "bugs/bug75306.phpt",
    "bugs/bug77088.phpt",
    "bugs/bug79357.phpt",
    "bugs/gh16237.phpt",
    "bugs/gh16259.phpt",
    "bugs/gh16429.phpt",
    "bugs/gh18990.phpt",
    "bugs/gh21421.phpt",
    "bugs/gh22167.phpt",
    "bugs/gh22895.phpt",
    "bugs/segfault_assertion_props.phpt",
    "classmap004.phpt",
    "classmap_private_property_packed_array.phpt",
    "gh15711.phpt",
    "gh16318.phpt",
    "gh22585.phpt",
    "gh23447.phpt",
    "oom_soapclient_call.phpt",
    "oom_soapclient_dorequest_chunked.phpt",
    "oom_soapclient_dorequest_close.phpt",
    "oom_soapclient_dorequest_content_length.phpt",
    "oom_soapclient_response_headers.phpt",
    "oom_soapclient_soapcall.phpt",
    "oom_soapserver_handle_array.phpt",
    "server003.phpt",
    "server009.phpt",
    "server019.phpt",
    "server020.phpt",
    "server030.phpt",
    "server031.phpt",
];

/// Outcome of one test.
enum Outcome {
    Pass,
    Skip(String),
    Fail(String),
}

fn run_phpt(path: &Path) -> Outcome {
    let Ok(raw) = std::fs::read(path) else {
        return Outcome::Skip("unreadable".into());
    };
    // Latin-1 view so that non-UTF-8 test files survive unchanged.
    let src: String = raw.iter().map(|&b| char::from(b)).collect();
    let sec = phpt_sections(&src);
    let Some(file) = sec.get("FILE") else {
        return Outcome::Skip("no FILE".into());
    };
    for unsupported in ["POST", "POST_RAW", "GET", "CGI", "ARGS", "STDIN", "EXPECTREGEX", "FILE_EXTERNAL", "PHPDBG", "SKIPIF_EXT"] {
        if sec.contains_key(unsupported) {
            return Outcome::Skip(format!("--{unsupported}--"));
        }
    }
    if let Some(skip) = sec.get("SKIPIF") {
        for needle in ["php_cli_server", "getenv(\"SKIP_ONLINE", "SKIP_SLOW", "PHP_ZTS", "die(\"skip"] {
            if skip.contains(needle) {
                return Outcome::Skip(format!("SKIPIF {needle}"));
            }
        }
    }
    if file.contains("php_cli_server") || file.contains("stream_socket_server") || file.contains("proc_open") {
        return Outcome::Skip("network server".into());
    }
    let (expected, fmt) = match (sec.get("EXPECT"), sec.get("EXPECTF")) {
        (Some(e), _) => (e.clone(), false),
        (_, Some(e)) => (e.clone(), true),
        _ => return Outcome::Skip("no EXPECT".into()),
    };
    let mut host = TestHost::new();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let script: PathBuf = path.with_extension("php");
    host.base_dir = dir;
    host.current_file = script.clone();
    host.script = script.to_string_lossy().into_owned();
    host.ini.insert("soap.wsdl_cache_enabled".into(), b"1".to_vec());
    if let Some(ini) = sec.get("INI") {
        for line in ini.lines() {
            if let Some((k, v)) = line.split_once('=') {
                host.ini.insert(k.trim().to_owned(), v.trim().as_bytes().to_vec());
            }
        }
    }
    php_soap::globals::request_init();
    php_soap::globals::with(|g| g.mem_cache.clear());
    let code: Vec<u8> = file.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).collect();
    if let Err(e) = run_file(&mut host, &script, &code) {
        host.report(&e);
    }
    let out = host.finish();
    let out: String = out.iter().map(|&b| char::from(b)).collect();
    let out = out.into_bytes();
    if output_matches(&expected, &out, fmt) {
        Outcome::Pass
    } else {
        Outcome::Fail(format!(
            "--- expected\n{}\n--- actual\n{}",
            expected.trim(),
            String::from_utf8_lossy(&out).trim()
        ))
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.filter_map(Result::ok) {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n != "schema" && n != "soap12") {
                collect(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "phpt") {
            out.push(p);
        }
    }
}

#[test]
fn soap_phpt_suite() {
    let Some(dir) = soap_tests_dir() else {
        eprintln!("reference tests not available; skipping");
        return;
    };
    let filter = std::env::var("SOAP_PHPT").ok();
    let verbose = std::env::var("SOAP_PHPT_VERBOSE").is_ok();
    let flt2 = filter.clone();
    let (pass, skip, fails) = with_big_stack(move || {
        let filter = flt2;
        let mut files = Vec::new();
        collect(&dir, &mut files);
        files.sort();
        let mut pass = 0;
        let mut skip = 0;
        let mut fails = Vec::new();
        for f in &files {
            let name = f.strip_prefix(&dir).unwrap_or(f).to_string_lossy().into_owned();
            if let Some(flt) = &filter {
                if !name.contains(flt.as_str()) {
                    continue;
                }
            }
            if std::env::var("SOAP_PHPT_TRACE").is_ok() {
                eprintln!("RUN {name}");
            }
            match run_phpt(f) {
                Outcome::Pass => pass += 1,
                Outcome::Skip(why) => {
                    skip += 1;
                    if verbose {
                        eprintln!("SKIP {name}: {why}");
                    }
                }
                Outcome::Fail(d) => fails.push((name, d)),
            }
        }
        (pass, skip, fails)
    });
    for (n, d) in &fails {
        if verbose {
            eprintln!("FAIL {n}\n{d}\n");
        } else {
            eprintln!("FAIL {n}");
        }
    }
    eprintln!("phpt: {pass} passed, {skip} skipped, {} failed", fails.len());
    let unexpected: Vec<&String> = fails
        .iter()
        .map(|(n, _)| n)
        .filter(|n| !ENGINE_DEPENDENT.contains(&n.as_str()))
        .collect();
    assert!(unexpected.is_empty(), "unexpected failures: {unexpected:?}");
    if filter.is_none() {
        assert!(pass >= 392, "only {pass} phpt tests passed");
    }
}
