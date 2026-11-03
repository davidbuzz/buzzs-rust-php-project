//! `phpt-runner`: run php-src `.phpt` tests against a PHP binary.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;

use phpt_runner::baseline::Baseline;
use phpt_runner::discover::{collect, find_srcdir, ConflictResolver};
use phpt_runner::ini::{default_ini_overwrites, escapeshellarg, IniSettings};
use phpt_runner::process::Env;
use phpt_runner::report::{by_dir, dir_table, summary, totals, JsonReport};
use phpt_runner::result::{Status, TestResult};
use phpt_runner::runner::{Config, Extensions, Runner};
use phpt_runner::schedule::{run_jobs, Job};

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

/// Locate the PHP binary, build the base environment and probe extensions. The returned probe
/// cache directory must outlive the run.
fn make_runner(cli: &Cli, srcdir: &Path) -> Result<(Runner, Option<SharedProbeCache>), String> {
    let php = cli
        .php
        .clone()
        .or_else(default_php)
        .ok_or("no PHP binary found: build php-cli (target/debug/php) or pass --php <path>")?;
    let php = php
        .canonicalize()
        .map_err(|e| format!("invalid PHP executable {}: {e}", php.display()))?;

    let mut ini_overwrites = default_ini_overwrites();
    ini_overwrites.extend(cli.ini.iter().cloned());
    let mut env = base_env(&php, cli.skip_slow);
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
    Ok((runner, probe_cache))
}

/// Run all tests in parallel, printing result lines as they complete.
fn execute(cli: &Cli, runner: &Runner, tests: &[PathBuf]) -> Vec<TestResult> {
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
    let (tests, srcdir) = select_tests(cli)?;
    if cli.list {
        for t in &tests {
            println!("{}", shortname(t, &srcdir));
        }
        return Ok(ExitCode::SUCCESS);
    }
    if tests.is_empty() {
        println!("No tests found.");
        return Ok(ExitCode::SUCCESS);
    }
    let (runner, _probe_cache) = make_runner(cli, &srcdir)?;

    let start = Instant::now();
    let results = execute(cli, &runner, &tests);
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
