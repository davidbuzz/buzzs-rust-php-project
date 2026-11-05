//! Running a single `.phpt` test (ref: `run-tests.php` `run_test()`).

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use pcre2::bytes::{Regex, RegexBuilder};

use crate::bytes::{
    contains, explode_lines, lossy, normalize_crlf, php_empty, replace, starts_with_ignore_case,
    trim, trim_end, trim_start,
};
use crate::diff::generate_diff;
use crate::expectf::{expectf_to_regex, regex_matches, LineMatcher};
use crate::ini::{escapeshellarg, IniSettings};
use crate::phpt::{ExpectKind, TestFile};
use crate::process::{run, Capture, Env, Invocation};
use crate::result::{Status, TestResult};

/// Marker appended to the output of a test that hit the timeout.
const TIMEOUT_MARKER: &[u8] = b"** ERROR: process timed out **";

/// Printed by the CLI SAPI before the first run of a `--repeat` script (`sapi/cli/php_cli.c`).
const REPEAT_FIRST: &[u8] = b"Executing for the first time...\n";
/// Printed by the CLI SAPI between runs of a `--repeat` script.
const REPEAT_AGAIN: &[u8] = b"Finished execution, repeating...\n";

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

/// Ask the binary for its `E_ALL` value (`php -r 'echo E_ALL;'`).
pub fn probe_e_all(php: &Path, env: &Env, cwd: &Path) -> Option<u32> {
    let argv: Vec<OsString> = vec![
        php.into(),
        os("-d"),
        os("memory_limit=128M"),
        os("-r"),
        os("echo E_ALL;"),
    ];
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
    std::str::from_utf8(trim(&out.data)).ok()?.parse().ok()
}

/// Does the binary load php.ini files (`php_ini_loaded_file()` / `php_ini_scanned_files()`)?
/// False if it cannot say.
pub fn probe_loads_ini_files(php: &Path, env: &Env, cwd: &Path) -> bool {
    let argv: Vec<OsString> = vec![
        php.into(),
        os("-d"),
        os("memory_limit=128M"),
        os("-r"),
        os("echo (php_ini_loaded_file() !== false || php_ini_scanned_files() !== false) ? 'yes' : 'no';"),
    ];
    run(&Invocation {
        argv: &argv,
        env,
        cwd,
        stdin: None,
        capture: Capture::STDOUT_ONLY,
        discard_stderr: true,
        timeout: Duration::from_secs(30),
    })
    .is_ok_and(|out| trim(&out.data) == b"yes")
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
    /// `--repeat N`: run each test script N times in one process and check the output of the
    /// last run (run-tests.php `$num_repeats`; values below 2 mean a single run).
    pub repeat: u32,
}

impl Config {
    /// Whether tests run in `--repeat` mode.
    pub fn repeating(&self) -> bool {
        self.repeat > 1
    }
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

/// SKIPIF cache key: directory, PHP arguments and code.
type SkipKey = (PathBuf, Vec<OsString>, Vec<u8>);

/// Runs tests; shareable across worker threads.
#[derive(Debug)]
pub struct Runner {
    config: Config,
    skip_cache: Mutex<HashMap<SkipKey, Vec<u8>>>,
}

enum Once {
    Done(TestResult),
    Many(Vec<TestResult>),
    Retry,
}

/// Output of SKIPIF that annotates the result (`info ...` / `warn ...`).
#[derive(Debug, Default)]
struct SkipInfo {
    text: String,
    warn: bool,
}

/// Environment and INI arguments for one test.
struct Prepared {
    env: Env,
    extensions: Vec<String>,
    /// INI arguments without the test's `--INI--` section (used for SKIPIF and CLEAN).
    orig_ini_args: Vec<String>,
    /// INI arguments including the `--INI--` section.
    test_ini_args: Vec<String>,
}

/// One attempt at running a parsed test.
struct Attempt<'a> {
    file: &'a Path,
    redirect: Option<&'a Redirect>,
    paths: Paths,
    shortname: String,
    test: TestFile,
    tested: String,
}

impl Attempt<'_> {
    fn result(&self, status: Status, name: String, info: String) -> TestResult {
        TestResult {
            path: self.shortname.clone(),
            name,
            result: status,
            info,
            duration_ms: 0,
            diff: None,
            title: None,
            junit_message: None,
        }
    }

    fn skip(&self, reason: &str) -> Once {
        Once::Done(self.result(
            Status::Skip,
            self.tested.clone(),
            format!("reason: {reason}"),
        ))
    }

    fn bork(&self, message: String, info: &str) -> Once {
        let mut result = self.result(Status::Bork, message, info.to_string());
        result.title = Some(self.tested.clone());
        Once::Done(result)
    }

    /// Directory of the original test file (`{PWD}`).
    fn pwd(&self) -> String {
        self.file
            .parent()
            .unwrap_or(Path::new("."))
            .to_string_lossy()
            .into_owned()
    }

    /// `--CAPTURE_STDIO--`.
    fn capture(&self) -> Capture {
        match self.test.section("CAPTURE_STDIO") {
            Some(c) => {
                let c = lossy(c).to_uppercase();
                Capture {
                    stdin: c.contains("STDIN"),
                    stdout: c.contains("STDOUT"),
                    stderr: c.contains("STDERR"),
                }
            }
            None => Capture::ALL,
        }
    }
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

/// `--repeat` output: keep what was printed before the first execution and the output of the
/// last one, dropping the earlier runs and the SAPI's separators (run-tests.php `run_test()`).
fn strip_repeat_output(out: &[u8]) -> Vec<u8> {
    let Some(first) = find(out, REPEAT_FIRST) else {
        return out.to_vec();
    };
    let rest =
        rfind(out, REPEAT_AGAIN).map_or(first + REPEAT_FIRST.len(), |p| p + REPEAT_AGAIN.len());
    let mut kept = out[..first].to_vec();
    kept.extend_from_slice(&out[rest..]);
    kept
}

/// `strpos()`: offset of the first occurrence of a non-empty `needle`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `strrpos()`: offset of the last occurrence of a non-empty `needle`.
fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

/// Spawn errors worth retrying.
fn is_transient_spawn_error(e: &std::io::Error) -> bool {
    use nix::errno::Errno;
    e.raw_os_error().is_some_and(|code| {
        [Errno::ENOMEM, Errno::EAGAIN, Errno::ETXTBSY]
            .iter()
            .any(|errno| *errno as i32 == code)
    })
}

fn remove_request_vars(env: &Env) -> Env {
    let mut env = env.clone();
    for k in [
        "REQUEST_METHOD",
        "QUERY_STRING",
        "PATH_TRANSLATED",
        "SCRIPT_FILENAME",
    ] {
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

    /// `create_test_command()`: `[php, (--repeat N), <ini args>]`; the caller appends
    /// `-f <file>`.
    fn php_argv(&self, extra: &[String]) -> Vec<OsString> {
        let mut argv = vec![self.config.php.clone().into_os_string()];
        if self.config.repeating() {
            argv.push(os("--repeat"));
            argv.push(os(&self.config.repeat.to_string()));
        }
        argv.extend(extra.iter().map(OsString::from));
        argv
    }

    /// `[php, -q, <ini args>, <extra>, <script>]`, the command line for SKIPIF, CLEAN and
    /// REDIRECTTEST helper scripts.
    fn helper_argv(&self, ini_args: &[String], extra: &[&str], script: &Path) -> Vec<OsString> {
        let mut argv = vec![self.config.php.clone().into_os_string(), os("-q")];
        argv.extend(ini_args.iter().map(OsString::from));
        argv.extend(extra.iter().map(|a| os(a)));
        argv.push(script.as_os_str().to_owned());
        argv
    }

    fn exec(
        &self,
        argv: &[OsString],
        env: &Env,
        stdin: Option<&[u8]>,
        capture: Capture,
    ) -> Vec<u8> {
        let inv = Invocation {
            argv,
            env,
            cwd: &self.config.srcdir,
            stdin,
            capture,
            discard_stderr: true,
            timeout: self.config.timeout,
        };
        let mut result = run(&inv);
        // Spawning can fail transiently on a loaded machine (fork ENOMEM/EAGAIN) or while the
        // binary is being relinked (ETXTBSY): back off and retry rather than recording a bogus
        // failure.
        let mut delay = Duration::from_millis(500);
        for _ in 0..6 {
            match &result {
                Err(e) if is_transient_spawn_error(e) => {
                    std::thread::sleep(delay);
                    delay *= 2;
                    result = run(&inv);
                }
                _ => break,
            }
        }
        match result {
            Ok(out) => out.data,
            Err(e) => format!("phpt-runner: cannot execute {:?}: {e}\n", argv[0]).into_bytes(),
        }
    }

    fn run_once(&self, file: &Path, redirect: Option<&Redirect>, retried: bool) -> Once {
        let mut attempt = match self.load(file, redirect) {
            Ok(a) => a,
            Err(once) => return once,
        };
        let repeating = self.config.repeating();
        if repeating && attempt.test.has_section("FILE_EXTERNAL") {
            return attempt.skip("Test with FILE_EXTERNAL might not be repeatable");
        }
        // Without CGI and phpdbg binaries, their "does not support --repeat" skips are
        // unreachable: the "not available" skips come first in run-tests.php too.
        if attempt.test.is_cgi() {
            return attempt.skip("CGI not available");
        }
        if attempt.test.has_section("PHPDBG") {
            return attempt.skip("phpdbg not available");
        }
        if repeating {
            if let Some(section) = ["CLEAN", "STDIN", "CAPTURE_STDIO"]
                .into_iter()
                .find(|s| attempt.test.has_section(s))
            {
                return attempt.skip(&format!("Test with {section} might not be repeatable"));
            }
        }
        for ext in [
            "diff",
            "log",
            "exp",
            "stdin",
            "out",
            "mem",
            "sh",
            "php",
            "skip.php",
            "post",
            "clean.php",
            "preload.php",
        ] {
            let _ = fs::remove_file(attempt.paths.file(ext));
        }
        let mut prepared = match self.prepare(&attempt) {
            Ok(p) => p,
            Err(once) => return once,
        };
        let skip_info = match self.evaluate_skipif(&mut attempt, &prepared) {
            Ok(info) => info,
            Err(once) => return once,
        };
        if let Some(code) = attempt.test.section("REDIRECTTEST") {
            if redirect.is_some() {
                return attempt.bork(
                    "Redirected test did not contain redirection info".into(),
                    "",
                );
            }
            let tested = attempt.tested.clone();
            return match self.run_redirect(
                file,
                code,
                &tested,
                &prepared.extensions,
                &prepared.orig_ini_args,
            ) {
                Ok(results) => Once::Many(results),
                Err(msg) => attempt.bork(msg, ""),
            };
        }
        let (out, clean_output) = match self.execute(&attempt, &mut prepared) {
            Ok(o) => o,
            Err(once) => return once,
        };
        self.judge(&attempt, &out, &clean_output, retried, skip_info)
    }

    /// Parse the test file and work out where its generated files go.
    fn load<'a>(
        &self,
        file: &'a Path,
        redirect: Option<&'a Redirect>,
    ) -> Result<Attempt<'a>, Once> {
        let base_name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file_dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
        let paths = Paths {
            test_dir: redirect.map_or(file_dir, |r| r.dir.clone()),
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
        match TestFile::load(file, redirect.is_some()) {
            Ok(test) => {
                let mut tested = test.name();
                if let Some(r) = redirect {
                    tested = format!("{} {tested}", r.prefix);
                }
                Ok(Attempt {
                    file,
                    redirect,
                    paths,
                    shortname,
                    test,
                    tested,
                })
            }
            Err(e) => Err(Once::Done(TestResult {
                // run-tests.php names the `JUnit` test case after the unparsable file.
                title: Some(shortname.clone()),
                path: shortname,
                name: e.0,
                result: Status::Bork,
                info: String::new(),
                duration_ms: 0,
                diff: None,
                junit_message: None,
            })),
        }
    }

    /// Build the environment and INI settings (`--ENV--`, `--EXTENSIONS--`, `--INI--`).
    fn prepare(&self, a: &Attempt<'_>) -> Result<Prepared, Once> {
        let cfg = &self.config;
        let mut env = a
            .redirect
            .map_or_else(|| cfg.env.clone(), |r| r.env.clone());
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
        let pwd = a.pwd();
        if a.test.section_not_empty("ENV") {
            let env_str = replace(a.test.section_or_empty("ENV"), b"{PWD}", pwd.as_bytes());
            for line in env_str.split(|&b| b == b'\n') {
                let mut parts = trim(line).splitn(2, |&b| b == b'=');
                if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                    if !php_empty(k) {
                        env.insert(os_from_bytes(k), os_from_bytes(v));
                    }
                }
            }
        }

        let mut extensions: Vec<String> = a
            .test
            .section("EXTENSIONS")
            .map(|s| {
                lossy(trim(s))
                    .split(['\n', '\r'])
                    .filter(|e| !e.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if let Some(r) = a.redirect {
            extensions.extend(r.extensions.iter().cloned());
        }
        let mut ini = self
            .extension_settings(&extensions)
            .map_err(|reason| a.skip(&reason))?;
        ini.apply_lines(cfg.ini_overwrites.iter().map(String::as_str));
        let orig_ini_args = ini.to_arguments();
        if cfg.repeating() {
            // Make sure warnings still show up on the second run.
            ini.set("opcache.record_warnings", "1");
        }

        if let Some(section) = a.test.section("INI") {
            let text = lossy(section)
                .replace("{PWD}", &pwd)
                .replace("{TMP}", &std::env::temp_dir().to_string_lossy());
            let text = replace_placeholders(&text, "MAIL", |f| format!("tee {f} >/dev/null"));
            let mut missing_env = None;
            let text = replace_placeholders(&text, "ENV", |name| {
                std::env::var(name).unwrap_or_else(|_| {
                    missing_env.get_or_insert_with(|| name.to_string());
                    String::new()
                })
            });
            if let Some(name) = missing_env {
                return Err(a.skip(&format!("Environment variable {name} is not set")));
            }
            ini.apply_lines(text.split(['\n', '\r']));
            if cfg.repeating() && ini.get("opcache.opt_debug_level").is_some() {
                return Err(a.skip("opt_debug_level tests are not repeatable"));
            }
        }
        set_env(
            &mut env,
            "TEST_PHP_EXTRA_ARGS",
            format!(" {}", ini.to_params()),
        );
        Ok(Prepared {
            env,
            extensions,
            orig_ini_args,
            test_ini_args: ini.to_arguments(),
        })
    }

    /// `-d extension=...` settings for required extensions that are available as shared
    /// modules, or the skip reason if some are missing.
    fn extension_settings(&self, extensions: &[String]) -> Result<IniSettings, String> {
        let known = &self.config.extensions;
        let mut ini = IniSettings::new();
        let mut missing = Vec::new();
        for ext in extensions {
            if known.loaded.iter().any(|l| l == ext) {
                continue;
            }
            let shared = known
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
        if missing.is_empty() {
            Ok(ini)
        } else {
            let plural = if missing.len() > 1 { "s" } else { "" };
            Err(format!(
                "Required extension{plural} missing: {}",
                missing.join(", ")
            ))
        }
    }

    /// Run `--SKIPIF--` and act on its output.
    fn evaluate_skipif(&self, a: &mut Attempt<'_>, p: &Prepared) -> Result<SkipInfo, Once> {
        let mut info = SkipInfo::default();
        if !a.test.section_not_empty("SKIPIF") {
            return Ok(info);
        }
        let code = a.test.section_or_empty("SKIPIF").to_vec();
        let mut skip_env = remove_request_vars(&p.env);
        set_env(&mut skip_env, "TEST_PHP_EVALUATING_SKIPIF", "1");
        let argv = self.helper_argv(
            &p.orig_ini_args,
            &[
                "-d",
                "opcache.file_cache=",
                "-d",
                "opcache.file_cache_only=0",
                "-d",
                "display_errors=1",
                "-d",
                "display_startup_errors=0",
            ],
            &a.paths.file("skip.php"),
        );
        let output = self.check_skip(&argv, &code, &skip_env);

        if starts_with_ignore_case(&output, b"skip") {
            let reason = prefix_reason(&output[4..], false)
                .map(|r| format!("reason: {r}"))
                .unwrap_or_default();
            return Err(Once::Done(a.result(Status::Skip, a.tested.clone(), reason)));
        }
        if let Some(r) = starts_with_ignore_case(&output, b"info")
            .then(|| prefix_reason(&output[4..], false))
            .flatten()
        {
            info.text = format!(" (info: {r})");
        } else if let Some(r) = starts_with_ignore_case(&output, b"warn")
            .then(|| prefix_reason(&output[4..], true))
            .flatten()
        {
            info.warn = true;
            info.text = format!(" (warn: {r})");
        } else if let Some(section) = ["XFAIL", "XLEAK", "FLAKY"]
            .into_iter()
            .find(|s| starts_with_ignore_case(&output, s.as_bytes()))
        {
            a.test
                .set_section(section, trim_start(&output[5..]).to_vec());
        } else if !output.is_empty() {
            return Err(a.bork(lossy(&output), "reason: invalid output from SKIPIF"));
        }
        Ok(info)
    }

    /// Write the test script, run it, then run `--CLEAN--`. Returns (output, clean output).
    fn execute(&self, a: &Attempt<'_>, p: &mut Prepared) -> Result<(Vec<u8>, Vec<u8>), Once> {
        let Some(code) = a.test.section("FILE") else {
            return Err(a.bork("missing section --FILE--".into(), ""));
        };
        let test_file = a.paths.file("php");
        if let Err(e) = fs::write(&test_file, code) {
            return Err(a.bork(format!("cannot write {}: {e}", test_file.display()), ""));
        }
        let env = &mut p.env;
        let test_file_os = test_file.into_os_string();
        set_env(env, "REDIRECT_STATUS", "1");
        for k in ["PATH_TRANSLATED", "SCRIPT_FILENAME"] {
            if env.get(&os(k)).is_none_or(|v| v.is_empty()) {
                set_env(env, k, test_file_os.clone());
            }
        }
        set_env(env, "HTTP_COOKIE", "");
        set_env(env, "REQUEST_METHOD", "GET");
        set_env(env, "CONTENT_TYPE", "");
        set_env(env, "CONTENT_LENGTH", "");
        if a.test.has_section("XLEAK") {
            set_env(env, "ZEND_ALLOC_PRINT_LEAKS", "0");
        }

        let mut argv = self.php_argv(&p.test_ini_args);
        argv.push(os("-f"));
        argv.push(test_file_os);
        if let Some(args) = a.test.section("ARGS") {
            // run-tests.php hands the raw ARGS section to the shell.
            let mut shell = argv
                .iter()
                .map(|arg| escapeshellarg(&arg.to_string_lossy()))
                .collect::<Vec<_>>()
                .join(" ");
            shell.push_str(" -- ");
            shell.push_str(&lossy(args));
            argv = vec![os("/bin/sh"), os("-c"), os(&format!("exec {shell}"))];
        }
        let mut out = self.exec(&argv, env, a.test.section("STDIN"), a.capture());
        if self.config.repeating() {
            out = strip_repeat_output(&out);
        }

        let mut clean_output = Vec::new();
        if a.test.section_not_empty("CLEAN") {
            let clean_file = a.paths.file("clean.php");
            if fs::write(&clean_file, trim(a.test.section_or_empty("CLEAN"))).is_ok() {
                let argv = self.helper_argv(
                    &p.orig_ini_args,
                    &[
                        "-d",
                        "opcache.file_cache=",
                        "-d",
                        "opcache.file_cache_only=0",
                    ],
                    &clean_file,
                );
                clean_output =
                    self.exec(&argv, &remove_request_vars(env), None, Capture::STDOUT_ONLY);
            }
            let _ = fs::remove_file(&clean_file);
        }
        Ok((out, clean_output))
    }

    /// Compare the output with the expectation and produce the result (writing `.exp`, `.out`
    /// and `.diff` for failures).
    fn judge(
        &self,
        a: &Attempt<'_>,
        out: &[u8],
        clean_output: &[u8],
        retried: bool,
        skip_info: SkipInfo,
    ) -> Once {
        let Some((kind, expected)) = a.test.expectation() else {
            return a.bork(
                "missing section --EXPECT--, --EXPECTF-- or --EXPECTREGEX--".into(),
                "",
            );
        };
        let output = normalize_crlf(trim(out));
        let wanted = normalize_crlf(trim(expected));
        let passed = match kind {
            ExpectKind::Exact => output == wanted,
            ExpectKind::Format => regex_matches(&expectf_to_regex(&wanted), &output),
            ExpectKind::Regex => regex_matches(&wanted, &output),
        };

        // Deviation from run-tests.php: a timeout is not retried even though its marker
        // contains "timed out", so hanging tests cost one timeout instead of two.
        let timed_out = contains(out, TIMEOUT_MARKER);
        if !passed && !retried && !timed_out && (is_flaky_output(&output) || is_flaky(&a.test)) {
            return Once::Retry;
        }

        let test_file = a.paths.file("php");
        if passed {
            let _ = fs::remove_file(&test_file);
            if !php_empty(clean_output) {
                let mut once = a.bork(lossy(&output), "reason: invalid output from CLEAN");
                if let Once::Done(result) = &mut once {
                    result.junit_message = Some(lossy(clean_output));
                }
                return once;
            }
            let (status, info) = if a.test.has_section("XFAIL") {
                (
                    Status::Warn,
                    " (warn: XFAIL section but test passes)".to_string(),
                )
            } else if retried {
                (
                    Status::Warn,
                    " (warn: Test passed on retry attempt)".to_string(),
                )
            } else if skip_info.warn {
                (Status::Warn, skip_info.text)
            } else {
                (Status::Pass, String::new())
            };
            return Once::Done(a.result(status, a.tested.clone(), info));
        }

        let (status, info) = match a.test.section("XFAIL") {
            Some(reason) => (
                Status::Xfail,
                format!("  XFAIL REASON: {}", lossy(trim_end(reason))),
            ),
            None => (Status::Fail, skip_info.text),
        };
        let mut diff = match kind {
            ExpectKind::Exact => generate_diff(&wanted, &output, &|e, o| e == o),
            ExpectKind::Format | ExpectKind::Regex => {
                // A fresh matcher per diff: its cache of compiled line patterns must not
                // outlive the test.
                let matcher = LineMatcher::default();
                generate_diff(&wanted, &output, &|e, o| matcher.matches(e, o))
            }
        };
        if a.redirect.is_some() {
            diff = format!("# original source file: {}\n{diff}", self.shortname(a.file));
        }
        if self.config.write_artifacts {
            let _ = fs::write(a.paths.file("exp"), &wanted);
            let _ = fs::write(a.paths.file("out"), &output);
            let _ = fs::write(a.paths.file("diff"), &diff);
        } else {
            let _ = fs::remove_file(&test_file);
        }
        let mut res = a.result(status, a.tested.clone(), info);
        res.diff = Some(diff);
        Once::Done(res)
    }

    /// Run a SKIPIF section, caching by directory and code (`SkipCache::checkSkip`).
    fn check_skip(&self, argv: &[OsString], code: &[u8], env: &Env) -> Vec<u8> {
        let skip_file = Path::new(argv.last().map_or(OsStr::new(""), OsString::as_os_str));
        let dir = skip_file.parent().unwrap_or(Path::new(".")).to_path_buf();
        let key = (dir, argv.to_vec(), code.to_vec());
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
        let out = self.exec(argv, env, None, Capture::STDOUT_ONLY);
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
        let argv = self.helper_argv(ini_args, &[], &script_file);
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
        assert_eq!(
            replace_placeholders("{ENV:}", "ENV", |_| "!".into()),
            "{ENV:}"
        );
    }

    #[test]
    fn repeat_output_keeps_prologue_and_last_run() {
        let out = b"pre\nExecuting for the first time...\nrun1\n\
                    Finished execution, repeating...\nrun2\n\
                    Finished execution, repeating...\nrun3\n";
        assert_eq!(strip_repeat_output(out), b"pre\nrun3\n");
        // A single separator-free run (e.g. a fatal error before the first repeat).
        let out = b"Executing for the first time...\nonly\n";
        assert_eq!(strip_repeat_output(out), b"only\n");
        // Output of a binary without --repeat support is left alone.
        assert_eq!(strip_repeat_output(b"plain\n"), b"plain\n");
        assert_eq!(find(b"abcabc", b"bc"), Some(1));
        assert_eq!(rfind(b"abcabc", b"bc"), Some(4));
        assert_eq!(rfind(b"ab", b"abc"), None);
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
