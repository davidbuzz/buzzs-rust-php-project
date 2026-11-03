//! `phpt-runner`: run php-src `.phpt` tests against a PHP binary.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;

use phpt_runner::baseline::Baseline;
use phpt_runner::discover::{collect, find_srcdir, ConflictResolver};
use phpt_runner::ini::{
    default_ini_overwrites, escapeshellarg, IniSettings, E_ALL, NO_EXTENSION_DIR,
};
use phpt_runner::phpt::TestFile;
use phpt_runner::process::Env;
use phpt_runner::report::{by_dir, dir_table, summary, totals, JsonReport};
use phpt_runner::result::{Status, TestResult};
use phpt_runner::runner::{probe_e_all, probe_loads_ini_files, Config, Extensions, Runner};
use phpt_runner::schedule::{run_jobs, Job};
use phpt_runner::scope::Scope;

/// Run php-src `.phpt` tests (a Rust port of run-tests.php).
#[derive(Debug, Parser)]
#[command(name = "phpt-runner", version)]
#[allow(clippy::struct_excessive_bools)]
struct Cli {
    /// PHP binary to test [default: `$TEST_PHP_EXECUTABLE`, else `php` next to this
    /// runner, e.g. target/debug/php].
    #[arg(long)]
    php: Option<PathBuf>,
    /// Number of parallel workers [default: number of CPUs].
    #[arg(short = 'j', long)]
    jobs: Option<usize>,
    /// Known-failures file. Only failures outside it (or baseline tests that now pass) make the
    /// run fail.
    #[arg(long)]
    baseline: Option<PathBuf>,
    /// Rewrite the baseline from this run's failures (entries for tests not run are kept).
    #[arg(long, requires = "baseline")]
    update_baseline: bool,
    /// Only run tests whose path contains this substring (repeatable; any match).
    #[arg(long)]
    filter: Vec<String>,
    /// List the selected tests and exit.
    #[arg(long)]
    list: bool,
    /// Print the diff of each failing test.
    #[arg(long)]
    show_diff: bool,
    /// Write a JSON report to this file.
    #[arg(long, value_name = "OUT")]
    json: Option<PathBuf>,
    /// Per-process timeout in seconds.
    #[arg(long, default_value_t = 60)]
    timeout: u64,
    /// php-src root (working directory of tests) [default: nearest ancestor of the first path
    /// containing run-tests.php].
    #[arg(long)]
    srcdir: Option<PathBuf>,
    /// Extra INI setting passed to every test, like run-tests.php -d (repeatable).
    #[arg(short = 'd', value_name = "NAME=VALUE")]
    ini: Vec<String>,
    /// Only print non-passing result lines.
    #[arg(short, long)]
    quiet: bool,
    /// Do not write .out/.exp/.diff files (or keep the generated .php) for failing tests.
    #[arg(long)]
    no_artifacts: bool,
    /// Group the per-directory summary by the first N path components.
    #[arg(long, value_name = "N")]
    summary_depth: Option<usize>,
    /// Set `SKIP_SLOW_TESTS` for the tests.
    #[arg(long)]
    skip_slow: bool,
    /// Disable coloured output.
    #[arg(long)]
    no_color: bool,
    /// Scope file listing out-of-scope path prefixes (tests under them are not run and are
    /// excluded from pass rates and the baseline) [default: `phpt-scope.txt` in the php-src
    /// root or one of its ancestors].
    #[arg(long, value_name = "FILE")]
    scope: Option<PathBuf>,
    /// Ignore the scope file: run and count every selected test.
    #[arg(long, conflicts_with = "scope")]
    no_scope: bool,
    /// Append each result to this JSON-lines file as soon as it completes; tests already
    /// recorded there are not run again (their recorded results are reused). Makes long runs
    /// resumable after the runner is killed.
    #[arg(long, value_name = "FILE")]
    resume: Option<PathBuf>,
    /// Run the binary in place instead of a private snapshot (by default it is hard-linked or
    /// copied into a temporary directory first, so a concurrent rebuild of e.g.
    /// target/debug/php cannot change or break it mid-run).
    #[arg(long)]
    no_snapshot: bool,
    /// Extra environment variable for every test, e.g. `--env MYSQL_TEST_HOST=127.0.0.1`
    /// (repeatable).
    #[arg(long, value_name = "KEY=VALUE")]
    env: Vec<String>,
    /// Test files or directories.
    #[arg(required = true)]
    paths: Vec<PathBuf>,
}

fn default_php() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TEST_PHP_EXECUTABLE").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    ["php", "php-cli"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}

/// The shared probe-cache directory run-tests.php exports as `TEST_PHP_SHARED_CACHE_DIR`
/// (used by `tests/probe_cache.inc`); removed when dropped.
struct SharedProbeCache(PathBuf);

impl SharedProbeCache {
    fn create() -> Option<Self> {
        if std::env::var_os("TEST_PHP_SHARED_CACHE").is_some_and(|v| v == "0") {
            return None;
        }
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() ^ (u64::from(d.subsec_nanos()) << 30));
        for attempt in 0..3u64 {
            let tag = seed ^ (u64::from(std::process::id()) << 40) ^ attempt;
            let dir = std::env::temp_dir().join(format!("php-run-tests-{tag:016x}"));
            let mut builder = std::fs::DirBuilder::new();
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            if builder.create(&dir).is_ok() {
                return Some(SharedProbeCache(dir));
            }
        }
        None
    }
}

impl Drop for SharedProbeCache {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A private hard link (or copy) of the binary under test; removed when dropped.
struct BinarySnapshot {
    dir: PathBuf,
    path: PathBuf,
}

impl BinarySnapshot {
    const PREFIX: &'static str = "phpt-runner-bin-";

    /// Remove snapshot directories left behind by runners that were killed (their `Drop` never
    /// ran), identified by the owner pid in the directory name no longer existing.
    fn remove_stale() {
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(Self::PREFIX)) else {
                continue;
            };
            let Some(pid) = rest.split('-').next().and_then(|p| p.parse::<i32>().ok()) else {
                continue;
            };
            let gone = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None)
                == Err(nix::errno::Errno::ESRCH);
            if gone {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    fn create(php: &Path) -> Result<Self, String> {
        Self::remove_stale();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir().join(format!(
            "{}{}-{nanos:08x}",
            Self::PREFIX,
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let snapshot = BinarySnapshot {
            path: dir.join(php.file_name().unwrap_or_else(|| "php".as_ref())),
            dir,
        };
        if std::fs::hard_link(php, &snapshot.path).is_err() {
            std::fs::copy(php, &snapshot.path)
                .map_err(|e| format!("cannot snapshot {}: {e}", php.display()))?;
        }
        Ok(snapshot)
    }
}

impl Drop for BinarySnapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Things that must stay alive for the whole run.
struct RunGuards {
    _probe_cache: Option<SharedProbeCache>,
    _snapshot: Option<BinarySnapshot>,
}

fn base_env(php: &Path, skip_slow: bool) -> Env {
    let mut env: Env = std::env::vars_os().collect();
    let mut set = |k: &str, v: OsString| {
        env.insert(OsString::from(k), v);
    };
    if std::env::var_os("TEMP").is_none_or(|t| t.is_empty()) {
        set("TEMP", std::env::temp_dir().into_os_string());
    }
    for k in ["SSH_CLIENT", "SSH_AUTH_SOCK", "SSH_TTY", "SSH_CONNECTION"] {
        set(k, "deleted".into());
    }
    let php_str = php.to_string_lossy();
    set("TEST_PHP_EXECUTABLE", php.as_os_str().to_owned());
    set(
        "TEST_PHP_EXECUTABLE_ESCAPED",
        escapeshellarg(&php_str).into(),
    );
    set("TEST_PHP_CGI_EXECUTABLE", OsString::new());
    set("TEST_PHP_CGI_EXECUTABLE_ESCAPED", "''".into());
    set("TEST_PHPDBG_EXECUTABLE", OsString::new());
    set("TEST_PHPDBG_EXECUTABLE_ESCAPED", "''".into());
    if std::env::var_os("SKIP_ONLINE_TESTS").is_none() {
        set("SKIP_ONLINE_TESTS", "1".into());
    }
    if !(std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal())
    {
        set("SKIP_IO_CAPTURE_TESTS", "1".into());
    }
    if skip_slow {
        set("SKIP_SLOW_TESTS", "1".into());
    }
    env
}

fn colorize(status: Status, enabled: bool) -> String {
    if !enabled {
        return status.label().to_string();
    }
    let color = match status {
        Status::Pass => "1;32",
        Status::Fail | Status::Bork => "1;31",
        _ => "1;33",
    };
    format!("\x1b[{color}m{}\x1b[0m", status.label())
}

fn print_result(r: &TestResult, cli: &Cli, color: bool, show_diff: bool) {
    if cli.quiet && matches!(r.result, Status::Pass | Status::Skip) {
        return;
    }
    let mut out = std::io::stdout().lock();
    let _ = writeln!(
        out,
        "{} {} [{}] {}",
        colorize(r.result, color),
        r.name,
        r.path,
        r.info
    );
    if show_diff && r.result == Status::Fail {
        if let Some(diff) = &r.diff {
            let _ = writeln!(
                out,
                "\n========DIFF========\n{}\n========DONE========",
                diff.trim_end()
            );
        }
    }
}

/// Resolve the selected tests and the php-src root.
fn select_tests(cli: &Cli) -> Result<(Vec<PathBuf>, PathBuf), String> {
    let mut tests = collect(&cli.paths)?;
    let srcdir = match &cli.srcdir {
        Some(s) => s.canonicalize().map_err(|e| format!("--srcdir: {e}"))?,
        None => tests
            .first()
            .and_then(|t| find_srcdir(t))
            .or_else(|| std::env::current_dir().ok())
            .ok_or("cannot determine php-src root; pass --srcdir")?,
    };
    if !cli.filter.is_empty() {
        tests.retain(|t| {
            let s = shortname(t, &srcdir);
            cli.filter.iter().any(|f| s.contains(f.as_str()))
        });
    }
    Ok((tests, srcdir))
}

fn shortname(path: &Path, srcdir: &Path) -> String {
    path.strip_prefix(srcdir)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Load the scope file (explicit `--scope`, else the default one if present).
fn load_scope(cli: &Cli, srcdir: &Path) -> Result<Option<(Scope, PathBuf)>, String> {
    if cli.no_scope {
        return Ok(None);
    }
    let Some(path) = cli.scope.clone().or_else(|| Scope::find_default(srcdir)) else {
        return Ok(None);
    };
    let scope = Scope::load(&path)
        .map_err(|e| format!("cannot read scope file {}: {e}", path.display()))?;
    Ok(Some((scope, path)))
}

/// Split off out-of-scope tests, returning their (not run) results.
fn apply_scope(
    tests: &mut Vec<PathBuf>,
    srcdir: &Path,
    scope: Option<&(Scope, PathBuf)>,
) -> Vec<TestResult> {
    let Some((scope, scope_path)) = scope else {
        return Vec::new();
    };
    let (out, keep): (Vec<PathBuf>, Vec<PathBuf>) = std::mem::take(tests)
        .into_iter()
        .partition(|t| scope.is_out_of_scope(&shortname(t, srcdir)));
    *tests = keep;
    let file_name = scope_path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    out.iter()
        .map(|t| TestResult {
            path: shortname(t, srcdir),
            name: TestFile::load(t, false)
                .map(|f| f.name())
                .unwrap_or_default(),
            result: Status::OutOfScope,
            info: format!("reason: out of scope ({file_name})"),
            duration_ms: 0,
            diff: None,
        })
        .collect()
}

/// Locate the PHP binary, build the base environment and probe extensions. The returned probe
/// cache directory must outlive the run.
fn make_runner(cli: &Cli, srcdir: &Path) -> Result<(Runner, RunGuards), String> {
    let php = cli
        .php
        .clone()
        .or_else(default_php)
        .ok_or("no PHP binary found: build php-cli (target/debug/php) or pass --php <path>")?;
    let php = php
        .canonicalize()
        .map_err(|e| format!("invalid PHP executable {}: {e}", php.display()))?;
    let snapshot = if cli.no_snapshot {
        None
    } else {
        Some(BinarySnapshot::create(&php)?)
    };
    let php = snapshot.as_ref().map_or(php, |s| s.path.clone());

    let mut env = base_env(&php, cli.skip_slow);
    for kv in &cli.env {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| format!("--env {kv}: expected KEY=VALUE"))?;
        env.insert(k.into(), v.into());
    }
    let e_all = probe_e_all(&php, &env, srcdir).unwrap_or(E_ALL);
    let mut ini_overwrites = default_ini_overwrites(e_all);
    if !probe_loads_ini_files(&php, &env, srcdir) {
        ini_overwrites.push(format!("extension_dir={NO_EXTENSION_DIR}"));
    }
    ini_overwrites.extend(cli.ini.iter().cloned());
    env.remove(&OsString::from("TEST_PHP_SHARED_CACHE_DIR"));
    let probe_cache = SharedProbeCache::create();
    if let Some(cache) = &probe_cache {
        env.insert(
            "TEST_PHP_SHARED_CACHE_DIR".into(),
            cache.0.clone().into_os_string(),
        );
    }
    let mut probe_ini = IniSettings::new();
    probe_ini.apply_lines(ini_overwrites.iter().map(String::as_str));
    let extensions = Extensions::probe(&php, &probe_ini.to_arguments(), &env, srcdir)
        .unwrap_or_else(|| {
            eprintln!(
                "warning: could not query loaded extensions from {}; tests with --EXTENSIONS-- \
                 will be skipped",
                php.display()
            );
            Extensions::default()
        });
    let runner = Runner::new(Config {
        php,
        srcdir: srcdir.to_path_buf(),
        timeout: Duration::from_secs(cli.timeout),
        env,
        ini_overwrites,
        write_artifacts: !cli.no_artifacts,
        extensions,
    });
    Ok((
        runner,
        RunGuards {
            _probe_cache: probe_cache,
            _snapshot: snapshot,
        },
    ))
}

/// Run all tests in parallel, printing result lines as they complete.
/// Results already recorded in the `--resume` log (unparsable lines are ignored).
fn load_resume_log(path: &Path) -> Result<Vec<TestResult>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn execute(
    cli: &Cli,
    runner: &Runner,
    tests: &[PathBuf],
    mut log: Option<&mut std::fs::File>,
) -> Vec<TestResult> {
    let mut resolver = ConflictResolver::default();
    let jobs: Vec<Job> = tests
        .iter()
        .map(|t| Job {
            path: t.clone(),
            conflicts: resolver.conflicts_for(t),
        })
        .collect();
    let workers = cli.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    });
    let color =
        !cli.no_color && std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();
    let show_diff = cli.show_diff || tests.len() == 1;
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    println!(
        "Running {} test{} with {} using {} worker{}",
        tests.len(),
        plural(tests.len()),
        runner.config().php.display(),
        workers,
        plural(workers)
    );
    let mut results: Vec<TestResult> = Vec::with_capacity(tests.len());
    run_jobs(
        jobs,
        workers,
        |job| runner.run_file(&job.path),
        |batch| {
            for r in batch {
                print_result(&r, cli, color, show_diff);
                // Diffs can be large; they were printed (and written to .diff) already.
                let mut r = r;
                r.diff = None;
                if let Some(log) = log.as_deref_mut() {
                    if let Ok(line) = serde_json::to_string(&r) {
                        let _ = writeln!(log, "{line}");
                    }
                }
                results.push(r);
            }
        },
    );
    results
}

/// Compare with (or update) the baseline and choose the exit code.
fn check_baseline(cli: &Cli, results: &[TestResult]) -> Result<ExitCode, String> {
    let Some(baseline_path) = &cli.baseline else {
        let failed = results.iter().any(|r| r.result.is_failure());
        return Ok(if failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        });
    };
    let baseline = Baseline::load(baseline_path)
        .map_err(|e| format!("cannot read baseline {}: {e}", baseline_path.display()))?;
    if cli.update_baseline {
        let updated = baseline.updated(results);
        updated
            .save(baseline_path)
            .map_err(|e| format!("cannot write baseline {}: {e}", baseline_path.display()))?;
        println!(
            "Baseline {} updated: {} known failures (was {}).",
            baseline_path.display(),
            updated.len(),
            baseline.len()
        );
        return Ok(ExitCode::SUCCESS);
    }
    let cmp = baseline.compare(results);
    for p in &cmp.new_failures {
        println!("NEW FAILURE {p}");
    }
    for p in &cmp.unexpected_passes {
        println!("UNEXPECTED PASS {p} (remove it from the baseline)");
    }
    println!(
        "Baseline: {} new failure(s), {} unexpected pass(es).",
        cmp.new_failures.len(),
        cmp.unexpected_passes.len()
    );
    Ok(if cmp.is_regression() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn run(cli: &Cli) -> Result<ExitCode, String> {
    let (mut tests, srcdir) = select_tests(cli)?;
    let scope = load_scope(cli, &srcdir)?;
    let out_of_scope = apply_scope(&mut tests, &srcdir, scope.as_ref());
    if cli.list {
        for t in &tests {
            println!("{}", shortname(t, &srcdir));
        }
        return Ok(ExitCode::SUCCESS);
    }
    if tests.is_empty() && out_of_scope.is_empty() {
        println!("No tests found.");
        return Ok(ExitCode::SUCCESS);
    }
    if let Some((scope_obj, path)) = &scope {
        if !out_of_scope.is_empty() {
            println!(
                "{} test(s) out of scope per {} ({} prefixes); not run",
                out_of_scope.len(),
                path.display(),
                scope_obj.prefixes().len()
            );
        }
    }
    let (runner, _guards) = make_runner(cli, &srcdir)?;

    let mut previous = Vec::new();
    let mut log = None;
    if let Some(path) = &cli.resume {
        previous = load_resume_log(path)?;
        let selected: std::collections::HashSet<String> =
            tests.iter().map(|t| shortname(t, &srcdir)).collect();
        previous.retain(|r| selected.contains(&r.path));
        let done: std::collections::HashSet<&str> =
            previous.iter().map(|r| r.path.as_str()).collect();
        tests.retain(|t| !done.contains(shortname(t, &srcdir).as_str()));
        let reused = previous.len();
        if reused > 0 {
            println!(
                "Resuming: {reused} result(s) reused from {}",
                path.display()
            );
        }
        log = Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| format!("cannot open {}: {e}", path.display()))?,
        );
    }

    let start = Instant::now();
    let mut results = execute(cli, &runner, &tests, log.as_mut());
    results.append(&mut previous);
    results.extend(out_of_scope);
    let elapsed = start.elapsed().as_secs_f64();

    let total = totals(&results);
    println!();
    print!(
        "{}",
        dir_table(&by_dir(&results, cli.summary_depth), &total)
    );
    print!("{}", summary(&total, elapsed));

    if let Some(path) = &cli.json {
        let report = JsonReport::new(
            runner.config().php.to_string_lossy().into_owned(),
            srcdir.to_string_lossy().into_owned(),
            &results,
        );
        let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    check_baseline(cli, &results)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("ERROR: {msg}");
            ExitCode::from(2)
        }
    }
}
