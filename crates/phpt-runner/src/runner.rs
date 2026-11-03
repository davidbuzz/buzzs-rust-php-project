//! Running a single `.phpt` test (ref: `run-tests.php` `run_test()`).

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use pcre2::bytes::{Regex, RegexBuilder};

use crate::bytes::{
    contains, explode_lines, lossy, normalize_crlf, php_empty, replace, starts_with_ignore_case, trim,
    trim_end, trim_start,
};
use crate::diff::generate_diff;
use crate::expectf::{expectf_to_regex, regex_matches, LineMatcher};
use crate::ini::{escapeshellarg, IniSettings};
use crate::phpt::{ExpectKind, TestFile};
use crate::process::{run, Capture, Env, Invocation};
use crate::result::{Status, TestResult};

/// Marker appended to the output of a test that hit the timeout.
const TIMEOUT_MARKER: &[u8] = b"** ERROR: process timed out **";

/// Extensions the target binary has loaded, from a probe run at start-up.
#[derive(Debug, Clone, Default)]
pub struct Extensions {
    /// `ini_get('extension_dir')`.
    pub ext_dir: Option<PathBuf>,
    /// Lower-cased names from `get_loaded_extensions()`.
    pub loaded: Vec<String>,
}

impl Extensions {
    /// Ask the PHP binary which extensions it has (`SkipCache::getExtensions`). Returns `None`
    /// if the binary cannot answer (e.g. an early-stage implementation without `-r`).
    pub fn probe(php: &Path, ini_args: &[String], env: &Env, cwd: &Path) -> Option<Self> {
        let mut argv: Vec<OsString> = vec![php.into()];
        argv.extend(ini_args.iter().map(OsString::from));
        for arg in [
            "-d",
            "opcache.file_cache=",
            "-d",
            "opcache.file_cache_only=0",
            "-d",
            "display_errors=0",
            "-r",
            "echo ini_get('extension_dir'), chr(0), implode(',', get_loaded_extensions());",
        ] {
            argv.push(arg.into());
        }
        let out = run(&Invocation {
            argv: &argv,
            env,
            cwd,
            stdin: None,
            capture: Capture::STDOUT_ONLY,
            discard_stderr: true,
            timeout: Duration::from_secs(30),
        })
        .ok()?;
        let nul = out.data.iter().position(|&b| b == 0)?;
        let ext_dir = lossy(&out.data[..nul]);
        let loaded = lossy(&out.data[nul + 1..])
            .split(',')
            .filter(|n| !n.is_empty() && *n != "Core")
            .map(|n| {
                if n == "Zend OPcache" {
                    "opcache".to_string()
                } else {
                    n.to_lowercase()
                }
            })
            .collect();
        Some(Extensions {
            ext_dir: (!ext_dir.is_empty()).then(|| PathBuf::from(ext_dir)),
            loaded,
        })
    }
}

/// Settings shared by all tests in a run.
#[derive(Debug, Clone)]
pub struct Config {
    /// The PHP binary under test.
    pub php: PathBuf,
    /// The php-src root; tests run with this as their working directory and paths are reported
    /// relative to it.
    pub srcdir: PathBuf,
    /// Per-process timeout.
    pub timeout: Duration,
    /// Base environment for every test.
    pub env: Env,
    /// INI settings passed to every test (`$ini_overwrites`).
    pub ini_overwrites: Vec<String>,
    /// Write `.out` / `.exp` / `.diff` next to failing tests (and keep the generated `.php`).
    pub write_artifacts: bool,
    /// Extensions available in the binary.
    pub extensions: Extensions,
}

/// Context for tests run through a `--REDIRECTTEST--`.
#[derive(Debug, Clone)]
struct Redirect {
    /// Directory of the redirecting test; generated files go there.
    dir: PathBuf,
    /// Title of the redirecting test, prefixed to every redirected test name.
    prefix: String,
    /// Extensions required by the redirecting test.
    extensions: Vec<String>,
    /// Environment for the redirected tests.
    env: Env,
}

/// Runs tests; shareable across worker threads.
#[derive(Debug)]
pub struct Runner {
    config: Config,
    skip_cache: Mutex<HashMap<(PathBuf, Vec<String>, Vec<u8>), Vec<u8>>>,
    line_matcher: LineMatcher,
}

enum Once {
    Done(TestResult),
    Many(Vec<TestResult>),
    Retry,
}

struct Paths {
    test_dir: PathBuf,
    stem: String,
}

impl Paths {
    fn file(&self, ext: &str) -> PathBuf {
        self.test_dir.join(format!("{}{ext}", self.stem))
    }
}

fn os(s: &str) -> OsString {
    OsString::from(s)
}

fn set_env(env: &mut Env, key: &str, value: impl Into<OsString>) {
    env.insert(os(key), value.into());
}

fn os_from_bytes(b: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(b.to_vec())
}

/// Replace `{NAME:(\S+)}` placeholders (PCRE semantics: the greedy `\S+` backtracks to the last
/// `}` of the non-space run).
fn replace_placeholders(s: &str, name: &str, mut f: impl FnMut(&str) -> String) -> String {
    let open = format!("{{{name}:");
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find(&open) {
        let after = &rest[pos + open.len()..];
        let run_len = after.find(char::is_whitespace).unwrap_or(after.len());
        let candidate = &after[..run_len];
        match candidate.rfind('}') {
            Some(close) if close > 0 => {
                out.push_str(&rest[..pos]);
                out.push_str(&f(&candidate[..close]));
                rest = &after[close + 1..];
            }
            _ => {
                out.push_str(&rest[..pos + open.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn regex(pattern: &str) -> Regex {
    RegexBuilder::new()
        .caseless(true)
        .build(pattern)
        .unwrap_or_else(|e| panic!("invalid built-in regex {pattern}: {e}"))
}

/// `is_flaky()`.
fn is_flaky(test: &TestFile) -> bool {
    static FLAKY_FUNCS: OnceLock<Regex> = OnceLock::new();
    if test.has_section("FLAKY") {
        return true;
    }
    if let Some(skipif) = test.section("SKIPIF") {
        if contains(skipif, b"SKIP_PERF_SENSITIVE") {
            return true;
        }
    }
    let Some(file) = test.section("FILE") else {
        return false;
    };
    FLAKY_FUNCS
        .get_or_init(|| regex(r"\b(disk_free_space|hrtime|microtime|sleep|usleep)\("))
        .is_match(file)
        .unwrap_or(false)
}

/// `is_flaky_output()`.
fn is_flaky_output(output: &[u8]) -> bool {
    static FLAKY_OUTPUT: OnceLock<Regex> = OnceLock::new();
    FLAKY_OUTPUT
        .get_or_init(|| {
            regex(
                r"\b(404: page not found|address already in use|connection refused|deadlock|mailbox already exists|timed out)\b",
            )
        })
        .is_match(output)
        .unwrap_or(false)
}

/// The `(.+)` capture of `/^PREFIX\s*(.+)/i` (or `\s+` when `require_space`), i.e. the rest of
/// the first line after optional whitespace.
fn prefix_reason(rest: &[u8], require_space: bool) -> Option<String> {
    if require_space && !rest.first().is_some_and(u8::is_ascii_whitespace) {
        return None;
    }
    let start = rest
        .iter()
        .position(|b| !b.is_ascii_whitespace() && *b != 0x0B)
        .unwrap_or(rest.len());
    let line = &rest[start..];
    let line = &line[..line.iter().position(|&b| b == b'\n').unwrap_or(line.len())];
    (!line.is_empty()).then(|| lossy(line))
}

fn remove_request_vars(env: &Env) -> Env {
    let mut env = env.clone();
    for k in ["REQUEST_METHOD", "QUERY_STRING", "PATH_TRANSLATED", "SCRIPT_FILENAME"] {
        env.remove(&os(k));
    }
    env
}

impl Runner {
    /// Create a runner.
    pub fn new(config: Config) -> Self {
        Runner {
            config,
            skip_cache: Mutex::new(HashMap::new()),
            line_matcher: LineMatcher::default(),
        }
    }

    /// The run configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Path of `file` relative to the php-src root (as shown in result lines).
    pub fn shortname(&self, file: &Path) -> String {
        file.strip_prefix(&self.config.srcdir)
            .unwrap_or(file)
            .to_string_lossy()
            .into_owned()
    }

    /// Run one test file. Usually yields one result; a `--REDIRECTTEST--` yields one per
    /// redirected test.
    pub fn run_file(&self, file: &Path) -> Vec<TestResult> {
        self.run_test(file, None)
    }

    fn run_test(&self, file: &Path, redirect: Option<&Redirect>) -> Vec<TestResult> {
        let start = Instant::now();
        let mut retried = false;
        loop {
            match self.run_once(file, redirect, retried) {
                Once::Retry => retried = true,
                Once::Many(results) => return results,
                Once::Done(mut result) => {
                    result.duration_ms =
                        u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                    return vec![result];
                }
            }
        }
    }

    fn php_argv(&self, extra: &[String]) -> Vec<OsString> {
        let mut argv = vec![self.config.php.clone().into_os_string()];
        argv.extend(extra.iter().map(OsString::from));
        argv
    }

    fn exec(&self, argv: &[OsString], env: &Env, stdin: Option<&[u8]>, capture: Capture) -> Vec<u8> {
        let result = run(&Invocation {
            argv,
            env,
            cwd: &self.config.srcdir,
            stdin,
            capture,
            discard_stderr: true,
            timeout: self.config.timeout,
        });
        match result {
            Ok(out) => out.data,
            Err(e) => format!("phpt-runner: cannot execute {:?}: {e}\n", argv[0]).into_bytes(),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn run_once(&self, file: &Path, redirect: Option<&Redirect>, retried: bool) -> Once {
        let cfg = &self.config;
        let file_dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
        let base_name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let paths = Paths {
            test_dir: redirect.map_or_else(|| file_dir.clone(), |r| r.dir.clone()),
            stem: base_name
                .strip_suffix("phpt")
                .unwrap_or(&base_name)
                .to_string(),
        };
        let shortname = if redirect.is_some() {
            self.shortname(&paths.file("phpt"))
        } else {
            self.shortname(file)
        };
        let result = |status: Status, name: String, info: String| TestResult {
            path: shortname.clone(),
            name,
            result: status,
            info,
            duration_ms: 0,
            diff: None,
        };

        let mut test = match TestFile::load(file, redirect.is_some()) {
            Ok(t) => t,
            Err(e) => return Once::Done(result(Status::Bork, e.0, String::new())),
        };
        let mut tested = test.name();
        if let Some(r) = redirect {
            tested = format!("{} {tested}", r.prefix);
        }
        let skip = |reason: &str| Once::Done(result(Status::Skip, tested.clone(), format!("reason: {reason}")));

        let capture = match test.section("CAPTURE_STDIO") {
            Some(c) => {
                let c = lossy(c).to_uppercase();
                Capture {
                    stdin: c.contains("STDIN"),
                    stdout: c.contains("STDOUT"),
                    stderr: c.contains("STDERR"),
                }
            }
            None => Capture::ALL,
        };
        if test.is_cgi() {
            return skip("CGI not available");
        }
        if test.has_section("PHPDBG") {
            return skip("phpdbg not available");
        }

        let test_file = paths.file("php");
        let skip_file = paths.file("skip.php");
        let clean_file = paths.file("clean.php");
        for ext in [
            "diff", "log", "exp", "stdin", "out", "mem", "sh", "php", "skip.php", "post",
            "clean.php", "preload.php",
        ] {
            let _ = fs::remove_file(paths.file(ext));
        }

        // Environment.
        let mut env = redirect.map_or_else(|| cfg.env.clone(), |r| r.env.clone());
        for k in [
            "REDIRECT_STATUS",
            "QUERY_STRING",
            "PATH_TRANSLATED",
            "SCRIPT_FILENAME",
            "REQUEST_METHOD",
            "CONTENT_TYPE",
            "CONTENT_LENGTH",
            "TZ",
        ] {
            set_env(&mut env, k, "");
        }
        let pwd = file_dir.to_string_lossy().into_owned();
        if test.section_not_empty("ENV") {
            let env_str = replace(test.section_or_empty("ENV"), b"{PWD}", pwd.as_bytes());
            for line in env_str.split(|&b| b == b'\n') {
                let line = trim(line);
                let mut parts = line.splitn(2, |&b| b == b'=');
                if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                    if !php_empty(k) {
                        env.insert(os_from_bytes(k), os_from_bytes(v));
                    }
                }
            }
        }

        // Required extensions.
        let mut ini = IniSettings::new();
        let mut extensions: Vec<String> = test
            .section("EXTENSIONS")
            .map(|s| {
                lossy(trim(s))
                    .split(['\n', '\r'])
                    .filter(|e| !e.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if let Some(r) = redirect {
            extensions.extend(r.extensions.iter().cloned());
        }
        let mut missing = Vec::new();
        for ext in &extensions {
            if cfg.extensions.loaded.iter().any(|l| l == ext) {
                continue;
            }
            let shared = cfg
                .extensions
                .ext_dir
                .as_ref()
                .map(|d| d.join(format!("{ext}.so")))
                .filter(|p| p.is_file());
            match shared {
                Some(path) => {
                    let key = if ext == "opcache" || ext == "xdebug" {
                        "zend_extension"
                    } else {
                        "extension"
                    };
                    ini.set(key, &path.to_string_lossy());
                }
                None => missing.push(ext.clone()),
            }
        }
        if !missing.is_empty() {
            let plural = if missing.len() > 1 { "s" } else { "" };
            return skip(&format!(
                "Required extension{plural} missing: {}",
                missing.join(", ")
            ));
        }

        ini.apply_lines(cfg.ini_overwrites.iter().map(String::as_str));
        let orig_ini_args = ini.to_arguments();

        if let Some(section) = test.section("INI") {
            let text = lossy(section)
                .replace("{PWD}", &pwd)
                .replace("{TMP}", &std::env::temp_dir().to_string_lossy());
            let text = replace_placeholders(&text, "MAIL", |f| format!("tee {f} >/dev/null"));
            let mut missing_env = None;
            let text = replace_placeholders(&text, "ENV", |name| {
                if let Ok(v) = std::env::var(name) {
                    v
                } else {
                    missing_env.get_or_insert_with(|| name.to_string());
                    String::new()
                }
            });
            if let Some(name) = missing_env {
                return skip(&format!("Environment variable {name} is not set"));
            }
            ini.apply_lines(text.split(['\n', '\r']));
        }
        let test_ini_args = ini.to_arguments();
        set_env(&mut env, "TEST_PHP_EXTRA_ARGS", format!(" {}", ini.to_params()));

        // SKIPIF.
        let mut info = String::new();
        let mut warn = false;
        if test.section_not_empty("SKIPIF") {
            let code = test.section_or_empty("SKIPIF").to_vec();
            let mut skip_env = remove_request_vars(&env);
            set_env(&mut skip_env, "TEST_PHP_EVALUATING_SKIPIF", "1");
            let mut args = vec!["-q".to_string()];
            args.extend(orig_ini_args.iter().cloned());
            for a in [
                "-d",
                "opcache.file_cache=",
                "-d",
                "opcache.file_cache_only=0",
                "-d",
                "display_errors=1",
                "-d",
                "display_startup_errors=0",
            ] {
                args.push(a.to_string());
            }
            let output = self.check_skip(&args, &code, &skip_file, &skip_env);

            if starts_with_ignore_case(&output, b"skip") {
                let reason = prefix_reason(&output[4..], false)
                    .map(|r| format!("reason: {r}"))
                    .unwrap_or_default();
                return Once::Done(result(Status::Skip, tested, reason));
            }
            if let Some(r) = starts_with_ignore_case(&output, b"info")
                .then(|| prefix_reason(&output[4..], false))
                .flatten()
            {
                info = format!(" (info: {r})");
            } else if let Some(r) = starts_with_ignore_case(&output, b"warn")
                .then(|| prefix_reason(&output[4..], true))
                .flatten()
            {
                warn = true;
                info = format!(" (warn: {r})");
            } else if starts_with_ignore_case(&output, b"xfail") {
                test.set_section("XFAIL", trim_start(&output[5..]).to_vec());
            } else if starts_with_ignore_case(&output, b"xleak") {
                test.set_section("XLEAK", trim_start(&output[5..]).to_vec());
            } else if starts_with_ignore_case(&output, b"flaky") {
                test.set_section("FLAKY", trim_start(&output[5..]).to_vec());
            } else if !output.is_empty() {
                return Once::Done(result(
                    Status::Bork,
                    lossy(&output),
                    "reason: invalid output from SKIPIF".into(),
                ));
            }
        }

        if let Some(code) = test.section("REDIRECTTEST") {
            if redirect.is_none() {
                return match self.run_redirect(file, code, &tested, &extensions, &orig_ini_args) {
                    Ok(results) => Once::Many(results),
                    Err(msg) => Once::Done(result(Status::Bork, msg, String::new())),
                };
            }
            return Once::Done(result(
                Status::Bork,
                "Redirected test did not contain redirection info".into(),
                String::new(),
            ));
        }

        // Write and run the test.
        let Some(code) = test.section("FILE") else {
            return Once::Done(result(Status::Bork, "missing section --FILE--".into(), String::new()));
        };
        if let Err(e) = fs::write(&test_file, code) {
            return Once::Done(result(
                Status::Bork,
                format!("cannot write {}: {e}", test_file.display()),
                String::new(),
            ));
        }
        let test_file_os = test_file.clone().into_os_string();
        set_env(&mut env, "REDIRECT_STATUS", "1");
        if env.get(&os("PATH_TRANSLATED")).is_none_or(|v| v.is_empty()) {
            set_env(&mut env, "PATH_TRANSLATED", test_file_os.clone());
        }
        if env.get(&os("SCRIPT_FILENAME")).is_none_or(|v| v.is_empty()) {
            set_env(&mut env, "SCRIPT_FILENAME", test_file_os.clone());
        }
        set_env(&mut env, "HTTP_COOKIE", "");
        set_env(&mut env, "REQUEST_METHOD", "GET");
        set_env(&mut env, "CONTENT_TYPE", "");
        set_env(&mut env, "CONTENT_LENGTH", "");
        if test.has_section("XLEAK") {
            set_env(&mut env, "ZEND_ALLOC_PRINT_LEAKS", "0");
        }

        let mut argv = self.php_argv(&test_ini_args);
        argv.push(os("-f"));
        argv.push(test_file_os);
        if let Some(args) = test.section("ARGS") {
            let mut shell = argv
                .iter()
                .map(|a| escapeshellarg(&a.to_string_lossy()))
                .collect::<Vec<_>>()
                .join(" ");
            shell.push_str(" -- ");
            shell.push_str(&lossy(args));
            argv = vec![os("/bin/sh"), os("-c"), os(&format!("exec {shell}"))];
        }
        let stdin = test.section("STDIN");
        let out = self.exec(&argv, &env, stdin, capture);

        // CLEAN.
        let mut clean_output = Vec::new();
        if test.section_not_empty("CLEAN") {
            if fs::write(&clean_file, trim(test.section_or_empty("CLEAN"))).is_ok() {
                let mut args = vec!["-q".to_string()];
                args.extend(orig_ini_args.iter().cloned());
                for a in ["-d", "opcache.file_cache=", "-d", "opcache.file_cache_only=0"] {
                    args.push(a.to_string());
                }
                let mut argv = self.php_argv(&args);
                argv.push(clean_file.clone().into_os_string());
                clean_output = self.exec(&argv, &remove_request_vars(&env), None, Capture::STDOUT_ONLY);
            }
            let _ = fs::remove_file(&clean_file);
        }

        // Compare.
        let output = normalize_crlf(trim(&out));
        let Some((kind, expected)) = test.expectation() else {
            return Once::Done(result(
                Status::Bork,
                "missing section --EXPECT--, --EXPECTF-- or --EXPECTREGEX--".into(),
                String::new(),
            ));
        };
        let wanted = normalize_crlf(trim(expected));
        let passed = match kind {
            ExpectKind::Exact => output == wanted,
            ExpectKind::Format => regex_matches(&expectf_to_regex(&wanted), &output),
            ExpectKind::Regex => regex_matches(&wanted, &output),
        };

        let timed_out = contains(&out, TIMEOUT_MARKER);
        if !passed && !retried && !timed_out && (is_flaky_output(&output) || is_flaky(&test)) {
            return Once::Retry;
        }

        if passed {
            let _ = fs::remove_file(&test_file);
            if !php_empty(&clean_output) {
                return Once::Done(result(
                    Status::Bork,
                    lossy(&output),
                    "reason: invalid output from CLEAN".into(),
                ));
            }
            let (status, info) = if test.has_section("XFAIL") {
                (Status::Warn, " (warn: XFAIL section but test passes)".to_string())
            } else if retried {
                (Status::Warn, " (warn: Test passed on retry attempt)".to_string())
            } else if warn {
                (Status::Warn, info)
            } else {
                (Status::Pass, String::new())
            };
            return Once::Done(result(status, tested, info));
        }

        let (status, info) = match test.section("XFAIL") {
            Some(reason) => (
                Status::Xfail,
                format!("  XFAIL REASON: {}", lossy(trim_end(reason))),
            ),
            None => (Status::Fail, info),
        };
        let mut diff = match kind {
            ExpectKind::Exact => generate_diff(&wanted, &output, &|a, b| a == b),
            ExpectKind::Format | ExpectKind::Regex => {
                generate_diff(&wanted, &output, &|e, a| self.line_matcher.matches(e, a))
            }
        };
        if redirect.is_some() {
            diff = format!("# original source file: {}\n{diff}", self.shortname(file));
        }
        if cfg.write_artifacts {
            let _ = fs::write(paths.file("exp"), &wanted);
            let _ = fs::write(paths.file("out"), &output);
            let _ = fs::write(paths.file("diff"), &diff);
        } else {
            let _ = fs::remove_file(&test_file);
        }
        let mut res = result(status, tested, info);
        res.diff = Some(diff);
        Once::Done(res)
    }

    /// Run a SKIPIF section, caching by directory and code (`SkipCache::checkSkip`).
    fn check_skip(&self, args: &[String], code: &[u8], skip_file: &Path, env: &Env) -> Vec<u8> {
        let dir = skip_file.parent().unwrap_or(Path::new(".")).to_path_buf();
        let key = (dir, args.to_vec(), code.to_vec());
        if let Some(hit) = self
            .skip_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            return hit.clone();
        }
        if let Err(e) = fs::write(skip_file, code) {
            return format!("cannot write {}: {e}", skip_file.display()).into_bytes();
        }
        let mut argv = self.php_argv(args);
        argv.push(skip_file.as_os_str().to_owned());
        let out = self.exec(&argv, env, None, Capture::STDOUT_ONLY);
        let _ = fs::remove_file(skip_file);
        let mut result = trim(&out).to_vec();
        if result.starts_with(b"nocache") {
            result.clear();
        } else {
            self.skip_cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, result.clone());
        }
        result
    }

    /// Evaluate a `--REDIRECTTEST--` section with the target binary and run the tests it names.
    fn run_redirect(
        &self,
        file: &Path,
        code: &[u8],
        tested: &str,
        extensions: &[String],
        ini_args: &[String],
    ) -> Result<Vec<TestResult>, String> {
        const BORK: &str =
            "Redirect info must contain exactly one TEST string to be used as redirect directory.";
        let dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
        let srcdir_literal = format!(
            "'{}'",
            self.config
                .srcdir
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('\'', "\\'")
        );
        let body = lossy(code).replace("__DIR__", &srcdir_literal);
        let script = format!(
            "<?php\n$__r = (function () {{\n{body}\n}})();\n\
             foreach (($__r['ENV'] ?? []) as $__k => $__v) {{ echo \"ENV\\t\", $__k, \"\\t\", $__v, \"\\n\"; }}\n\
             echo \"TESTS\\t\", $__r['TESTS'] ?? '', \"\\n\";\n"
        );
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let script_file = dir.join(format!("{stem}.redirect.php"));
        fs::write(&script_file, script).map_err(|e| e.to_string())?;
        let mut args = vec!["-q".to_string()];
        args.extend(ini_args.iter().cloned());
        let mut argv = self.php_argv(&args);
        argv.push(script_file.clone().into_os_string());
        let out = self.exec(&argv, &self.config.env, None, Capture::STDOUT_ONLY);
        let _ = fs::remove_file(&script_file);

        let mut env = self.config.env.clone();
        let mut tests_dir = None;
        for line in explode_lines(&out) {
            let mut parts = line.splitn(3, |&b| b == b'\t');
            match (parts.next(), parts.next(), parts.next()) {
                (Some(b"ENV"), Some(k), Some(v)) => {
                    env.insert(os_from_bytes(k), os_from_bytes(v));
                }
                (Some(b"TESTS"), Some(t), None) if !t.is_empty() => {
                    tests_dir = Some(PathBuf::from(lossy(t)));
                }
                _ => {}
            }
        }
        let tests_dir = tests_dir.ok_or_else(|| BORK.to_string())?;
        let tests_dir = tests_dir.canonicalize().map_err(|_| BORK.to_string())?;
        set_env(
            &mut env,
            "REDIR_TEST_DIR",
            format!("{}/", tests_dir.to_string_lossy()),
        );
        let ctx = Redirect {
            dir,
            prefix: tested.to_string(),
            extensions: extensions.to_vec(),
            env,
        };
        let files = crate::discover::find_tests(&tests_dir);
        Ok(files
            .iter()
            .flat_map(|f| self.run_test(f, Some(&ctx)))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_replacement() {
        let s = replace_placeholders("a={ENV:FOO} b={ENV:BAR}x", "ENV", |n| format!("<{n}>"));
        assert_eq!(s, "a=<FOO> b=<BAR>x");
        let s = replace_placeholders("{MAIL:/tmp/x}", "MAIL", |n| format!("tee {n}"));
        assert_eq!(s, "tee /tmp/x");
        assert_eq!(replace_placeholders("{ENV:}", "ENV", |_| "!".into()), "{ENV:}");
    }

    #[test]
    fn flaky_detection() {
        let t = TestFile::parse(
            Path::new("/x/t.phpt"),
            b"--TEST--\nx\n--FILE--\n<?php usleep(1);\n--EXPECT--\n",
            false,
        )
        .unwrap();
        assert!(is_flaky(&t));
        assert!(is_flaky_output(b"Connection refused by peer"));
        assert!(!is_flaky_output(b"all good"));
    }
}
