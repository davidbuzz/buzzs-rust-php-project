//! The `php` command line interface (`sapi/cli/php_cli.c`).

mod config;
mod extensions;
mod getopt;
mod proctitle;
mod sapi;
mod startup;

use config::Config;
use getopt::{GetOpt, INVALID, OPTIONS};
use php_runtime::{Value, ZStr};
use php_vm::{Throw, Vm, VmOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;

/// `php_build_date` (`__DATE__ " " __TIME__` of the build).
pub(crate) const BUILD_DATE: &str = env!("PHP_CLI_BUILD_DATE");

/// Stack the VM needs for deep recursion in user code.
const VM_STACK: u64 = 256 << 20;

/// `php_cli_usage()`.
fn usage(argv0: &[u8]) -> Vec<u8> {
    let prog: &[u8] = match argv0.iter().rposition(|&c| c == b'/') {
        Some(p) => &argv0[p + 1..],
        None => b"php",
    };
    let prog = String::from_utf8_lossy(prog);
    format!(
        "Usage: {prog} [options] [-f] <file> [--] [args...]
   {prog} [options] -r <code> [--] [args...]
   {prog} [options] [-B <begin_code>] -R <code> [-E <end_code>] [--] [args...]
   {prog} [options] [-B <begin_code>] -F <file> [-E <end_code>] [--] [args...]
   {prog} [options] -S <addr>:<port> [-t docroot] [router]
   {prog} [options] -- [args...]
   {prog} [options] -a

  -a               Run as interactive shell (requires readline extension)
  -c <path>|<file> Look for php.ini file in this directory
  -C               Do not chdir to the script's directory (no-op in CLI)
  -n               No configuration (ini) files will be used
  -d foo[=bar]     Define INI entry foo with value 'bar'
  -e               Generate extended information for debugger/profiler
  -f <file>        Parse and execute <file>.
  -h               This help
  -i               PHP information
  -l               Syntax check only (lint)
  -m               Show compiled in modules
  -q               Do not output HTTP headers (no-op in CLI)
  -r <code>        Run PHP <code> without using script tags <?..?>
  -B <begin_code>  Run PHP <begin_code> before processing input lines
  -R <code>        Run PHP <code> for every input line
  -F <file>        Parse and execute <file> for every input line
  -E <end_code>    Run PHP <end_code> after processing all input lines
  -H               Hide any passed arguments from external tools.
  -S <addr>:<port> Run with built-in web server.
  -t <docroot>     Specify document root <docroot> for built-in web server.
  -s               Output HTML syntax highlighted source.
  -v               Version number
  -w               Output source with stripped comments and whitespace.

  args...          Arguments passed to script. Use -- args when first argument
                   starts with - or script is read from stdin

  --ini            Show configuration file names
  --ini=diff       Show INI entries that differ from the built-in default

  --rf <name>      Show information about function <name>.
  --rc <name>      Show information about class <name>.
  --re <name>      Show information about extension <name>.
  --rz <name>      Show information about Zend extension <name>.
  --ri <name>      Show configuration for extension <name>.

  --repeat <count> Repeat script execution <count> times.
                   For internal purposes only.

"
    )
    .into_bytes()
}

/// `php_get_version()`.
fn version_banner() -> String {
    format!(
        "PHP {} (cli) (built: {BUILD_DATE}) (NTS)\nCopyright \u{a9} The PHP Group and Contributors\nZend Engine v{}, Copyright \u{a9} Zend by Perforce\n",
        php_vm::PHP_VERSION,
        php_vm::ZEND_VERSION
    )
}

/// Caps the address space so that runaway scripts fail instead of
/// exhausting the machine's memory (PHP's `memory_limit` analogue).
fn limit_memory() {
    let limit: u64 = std::env::var("PHPRUST_MAX_AS_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4096)
        * 1024
        * 1024;
    let rl = libc::rlimit {
        rlim_cur: limit,
        rlim_max: limit,
    };
    // SAFETY: setrlimit only reads the struct we pass; failure is harmless.
    unsafe {
        libc::setrlimit(libc::RLIMIT_AS, &raw const rl);
    }
}

/// Makes sure the main thread may grow its stack to [`VM_STACK`], so the
/// VM runs on the main thread: process-directed signals then reach it
/// (pcntl), the process stays single-threaded (`unshare()`, `fork()`
/// children exit with the script's status) — as in PHP.
///
/// The main thread's stack limit is fixed when the program is loaded, so
/// a too small `RLIMIT_STACK` is raised and the binary re-executed once
/// (the raised limit is then inherited, which ends the recursion).
/// Returns false when that is impossible; the VM then runs on a
/// dedicated thread with a large stack instead.
fn ensure_main_stack(argv: &[Vec<u8>]) -> bool {
    let mut rl = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes into the struct we own.
    if unsafe { libc::getrlimit(libc::RLIMIT_STACK, &raw mut rl) } != 0 {
        return false;
    }
    if rl.rlim_cur == libc::RLIM_INFINITY || rl.rlim_cur >= VM_STACK {
        return true;
    }
    if rl.rlim_max != libc::RLIM_INFINITY && rl.rlim_max < VM_STACK {
        return false;
    }
    let raised = libc::rlimit {
        rlim_cur: VM_STACK,
        rlim_max: rl.rlim_max,
    };
    // SAFETY: setrlimit only reads the struct we pass.
    if unsafe { libc::setrlimit(libc::RLIMIT_STACK, &raw const raised) } != 0 {
        return false;
    }
    let mut cmd = std::process::Command::new("/proc/self/exe");
    if let Some(a0) = argv.first() {
        cmd.arg0(std::ffi::OsStr::from_bytes(a0));
    }
    cmd.args(argv.iter().skip(1).map(|a| std::ffi::OsStr::from_bytes(a)));
    // Only returns on failure.
    let _ = cmd.exec();
    // SAFETY: as above; restores the original limit.
    unsafe {
        libc::setrlimit(libc::RLIMIT_STACK, &raw const rl);
    }
    false
}

php_memlimit::install_global_allocator!();

/// An allocation crossed the hard ceiling above `memory_limit`: PHP's
/// message on stderr, then exit 137 without unwinding.
fn hard_limit(e: php_memlimit::HardLimitExceeded) -> ! {
    let _ = std::io::stderr().write_all(format!("{e} in Unknown on line 0\n").as_bytes());
    // SAFETY: `_exit` terminates the process immediately; nothing here
    // relies on destructors or atexit handlers (PHP does the same).
    unsafe { libc::_exit(137) }
}

fn main() {
    let argv: Vec<Vec<u8>> = std::env::args_os().map(OsStringExt::into_vec).collect();
    let on_main_thread = ensure_main_stack(&argv);
    limit_memory();
    php_memlimit::set_hard_limit_handler(hard_limit);
    #[cfg(unix)]
    // SAFETY: ignoring SIGPIPE like php_cli.c, so writes to a closed pipe
    // fail with EPIPE instead of killing the process.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    if on_main_thread {
        finish(do_php_cli(&argv));
    }
    // Fallback: a dedicated VM thread.  Signals are blocked on the main
    // thread so process-directed ones reach the VM thread, and the VM
    // thread ends the process itself — also in a `pcntl_fork()` child,
    // where it is the only thread left.
    let old = block_all_signals();
    let vm_thread = std::thread::Builder::new()
        .stack_size(usize::try_from(VM_STACK).unwrap_or(usize::MAX))
        .spawn(move || {
            restore_signal_mask(&old);
            finish(do_php_cli(&argv))
        });
    match vm_thread {
        Ok(h) => {
            let _ = h.join();
            finish(255)
        }
        Err(_) => finish(255),
    }
}

/// Flushes the standard streams and ends the process with `status`.
fn finish(status: i32) -> ! {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(status)
}

/// Blocks every signal on the calling thread; returns the previous mask.
fn block_all_signals() -> libc::sigset_t {
    // SAFETY: sigset_t is plain data; sigfillset/pthread_sigmask only
    // write the sets we own.
    unsafe {
        let mut all: libc::sigset_t = std::mem::zeroed();
        let mut old: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&raw mut all);
        libc::pthread_sigmask(libc::SIG_BLOCK, &raw const all, &raw mut old);
        old
    }
}

/// Restores a mask saved by [`block_all_signals`] on the calling thread.
fn restore_signal_mask(old: &libc::sigset_t) {
    // SAFETY: pthread_sigmask only reads the set we pass.
    unsafe {
        libc::pthread_sigmask(
            libc::SIG_SETMASK,
            std::ptr::from_ref(old),
            std::ptr::null_mut(),
        );
    }
}

fn out(bytes: &[u8]) {
    let mut o = std::io::stdout();
    let _ = o.write_all(bytes);
    let _ = o.flush();
}

fn err(bytes: &[u8]) {
    let _ = std::io::stderr().write_all(bytes);
}

/// `do_php_cli()`: the options that matter before startup (`-c`, `-n`,
/// `-d`, `-h`), the configuration, then [`do_cli`].
fn do_php_cli(argv: &[Vec<u8>]) -> i32 {
    // `-S <addr>:<port> [-t docroot] [router]`: the built-in web server.
    if let Some(status) =
        php_cli_server::maybe_run(argv.get(1..).unwrap_or_default(), extensions::all)
    {
        return status;
    }
    let argv0 = argv.first().cloned().unwrap_or_else(|| b"php".to_vec());
    let mut opts = config::Options {
        binary: std::env::current_exe().ok(),
        ..config::Options::default()
    };
    let mut g = GetOpt::new(argv, OPTIONS, true);
    while let Some((c, v)) = g.next_opt() {
        match c {
            INVALID => {
                out(&usage(&argv0));
                return 1;
            }
            _ => match u8::try_from(c).unwrap_or(0) {
                b'c' => opts.path_override = v,
                b'n' => opts.ignore = true,
                b'd' => opts
                    .defines
                    .extend(php_vm::parse_cli_define(&v.unwrap_or_default())),
                b'h' | b'?' => {
                    out(&usage(&argv0));
                    return 0;
                }
                b'i' | b'v' | b'm' => break,
                _ => {}
            },
        }
    }
    let cfg = Config::load(&opts);
    for w in &cfg.warnings {
        err(format!("PHP:  {w}\n").as_bytes());
    }
    if let Err(status) = startup::module_startup(&cfg) {
        return status;
    }
    do_cli(argv, &cfg)
}

/// `context.mode` (`php_cli.h`; the order matters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Mode {
    Standard,
    Highlight,
    Lint,
    Strip,
    CliDirect,
    ProcessStdin,
    ReflectionFunction,
    ReflectionClass,
    ReflectionExtension,
    ReflectionZendExtension,
    ReflectionExtInfo,
    ShowIniConfig,
    ShowIniDiff,
}

const PARAM_MODE_CONFLICT: &str = "Either execute direct code, process stdin or use a file.\n";

/// The parsed request options of `do_cli()`.
#[derive(Debug, Default)]
struct Request {
    mode: Option<Mode>,
    script_file: Option<Vec<u8>>,
    exec_direct: Option<Vec<u8>>,
    exec_run: Option<Vec<u8>>,
    exec_begin: Option<Vec<u8>>,
    exec_end: Option<Vec<u8>>,
    reflection_what: Vec<u8>,
    repeats: i64,
    hide_argv: bool,
}

impl Request {
    fn mode(&self) -> Mode {
        self.mode.unwrap_or(Mode::Standard)
    }
}

/// Parses the mode options (`do_cli()`'s second `php_getopt()` loop):
/// the request, the index of the first non-option argument and the
/// parameter error, if any.
#[allow(clippy::too_many_lines)]
fn parse_request(argv: &[Vec<u8>]) -> (Request, usize, Option<&'static str>) {
    let mut r = Request {
        repeats: 1,
        ..Request::default()
    };
    let mut interactive = false;
    let mut param_error: Option<&'static str> = None;
    let mut g = GetOpt::new(argv, OPTIONS, false);
    while let Some((c, v)) = g.next_opt() {
        let v = v.unwrap_or_default();
        let mode = r.mode();
        match c {
            10 => {
                r.mode = Some(Mode::ReflectionFunction);
                r.reflection_what = v;
            }
            11 => {
                r.mode = Some(Mode::ReflectionClass);
                r.reflection_what = v;
            }
            12 => {
                r.mode = Some(Mode::ReflectionExtension);
                r.reflection_what = v;
            }
            13 => {
                r.mode = Some(Mode::ReflectionZendExtension);
                r.reflection_what = v;
            }
            14 => {
                r.mode = Some(Mode::ReflectionExtInfo);
                r.reflection_what = v;
            }
            15 => {
                if v.is_empty() {
                    r.mode = Some(Mode::ShowIniConfig);
                } else if v == b"diff" {
                    r.mode = Some(Mode::ShowIniDiff);
                } else {
                    param_error = Some("Unknown argument for --ini\n");
                }
            }
            16 => {
                r.repeats = String::from_utf8_lossy(&v).trim().parse().unwrap_or(0);
            }
            _ => match u8::try_from(c).unwrap_or(0) {
                b'a' => {
                    // Mode conflicts are checked as with a shell present;
                    // without readline the shell itself is unavailable.
                    if !interactive {
                        if mode != Mode::Standard {
                            param_error = Some(PARAM_MODE_CONFLICT);
                            continue;
                        }
                        interactive = true;
                    }
                    param_error = Some("Interactive shell (-a) requires the readline extension.\n");
                }
                b'F' => {
                    if mode == Mode::ProcessStdin {
                        if r.exec_run.is_some() || r.script_file.is_some() {
                            param_error = Some("You can use -R or -F only once.\n");
                            continue;
                        }
                    } else if mode != Mode::Standard {
                        param_error = Some(PARAM_MODE_CONFLICT);
                        continue;
                    }
                    r.mode = Some(Mode::ProcessStdin);
                    r.script_file = Some(v);
                }
                b'f' => {
                    if mode == Mode::CliDirect || mode == Mode::ProcessStdin {
                        param_error = Some(PARAM_MODE_CONFLICT);
                    } else if r.script_file.is_some() {
                        param_error = Some("You can use -f only once.\n");
                    } else {
                        r.script_file = Some(v);
                    }
                }
                b'l' if mode == Mode::Standard => r.mode = Some(Mode::Lint),
                b'r' => {
                    if mode == Mode::CliDirect {
                        if r.exec_direct.is_some() || r.script_file.is_some() {
                            param_error = Some("You can use -r only once.\n");
                            continue;
                        }
                    } else if mode != Mode::Standard || interactive {
                        param_error = Some(PARAM_MODE_CONFLICT);
                        continue;
                    }
                    r.mode = Some(Mode::CliDirect);
                    r.exec_direct = Some(v);
                }
                b'R' => {
                    if mode == Mode::ProcessStdin {
                        if r.exec_run.is_some() || r.script_file.is_some() {
                            param_error = Some("You can use -R or -F only once.\n");
                            continue;
                        }
                    } else if mode != Mode::Standard {
                        param_error = Some(PARAM_MODE_CONFLICT);
                        continue;
                    }
                    r.mode = Some(Mode::ProcessStdin);
                    r.exec_run = Some(v);
                }
                b'B' | b'E' => {
                    let begin = c == i32::from(b'B');
                    let slot_used = if begin {
                        r.exec_begin.is_some()
                    } else {
                        r.exec_end.is_some()
                    };
                    if mode == Mode::ProcessStdin {
                        if slot_used {
                            param_error = Some(if begin {
                                "You can use -B only once.\n"
                            } else {
                                "You can use -E only once.\n"
                            });
                            continue;
                        }
                    } else if mode != Mode::Standard || interactive {
                        param_error = Some(PARAM_MODE_CONFLICT);
                        continue;
                    }
                    r.mode = Some(Mode::ProcessStdin);
                    if begin {
                        r.exec_begin = Some(v);
                    } else {
                        r.exec_end = Some(v);
                    }
                }
                b's' => {
                    if mode == Mode::CliDirect || mode == Mode::ProcessStdin {
                        param_error = Some("Source highlighting only works for files.\n");
                    } else {
                        r.mode = Some(Mode::Highlight);
                    }
                }
                b'H' => r.hide_argv = true,
                b'w' => {
                    if mode == Mode::CliDirect || mode == Mode::ProcessStdin {
                        param_error = Some("Source stripping only works for files.\n");
                    } else {
                        r.mode = Some(Mode::Strip);
                    }
                }
                _ => {}
            },
        }
    }
    (r, g.optind, param_error)
}

/// `do_cli()`.
#[allow(clippy::too_many_lines)]
fn do_cli(argv: &[Vec<u8>], cfg: &Config) -> i32 {
    // First pass: -i, -v and -m end processing right away.
    let mut g = GetOpt::new(argv, OPTIONS, false);
    while let Some((c, _)) = g.next_opt() {
        match u8::try_from(c).unwrap_or(0) {
            b'i' => {
                return run_snippet(cfg, b"<?php phpinfo(INFO_ALL & ~INFO_CREDITS);", &[], false)
                    .min(0);
            }
            b'v' => {
                out(version_banner().as_bytes());
                return 0;
            }
            b'm' => {
                return run_snippet(cfg, MODULES_SNIPPET, &[], false).min(0);
            }
            _ => {}
        }
    }
    let (mut req, mut optind, param_error) = parse_request(argv);
    if let Some(e) = param_error {
        out(e.as_bytes());
        return 1;
    }
    if req.hide_argv {
        proctitle::hide_args();
    }
    let mut repeats = req.repeats;
    if repeats > 1 {
        out(b"Executing for the first time...\n");
    }
    let mut exit_status = 0;
    loop {
        // Only set script_file if not set already, not in direct/stdin
        // mode and not after `--`.
        let mode = req.mode();
        if argv.len() > optind
            && req.script_file.is_none()
            && mode != Mode::CliDirect
            && mode != Mode::ProcessStdin
            && argv[optind - 1] != b"--"
        {
            req.script_file = Some(argv[optind].clone());
            optind += 1;
        }
        let status = run_request(argv, optind, &req, cfg);
        if mode == Mode::Lint {
            // Failures stick: the last failing status wins.
            if status != 0 {
                exit_status = status;
            }
        } else {
            exit_status = status;
        }
        if mode == Mode::Lint && argv.len() > optind && argv[optind] != b"--" {
            req.script_file = None;
            continue;
        }
        repeats -= 1;
        if repeats > 0 {
            out(b"Finished execution, repeating...\n");
            continue;
        }
        break;
    }
    exit_status
}

const MODULES_SNIPPET: &[u8] = b"<?php
$m = get_loaded_extensions();
usort($m, 'strcasecmp');
echo \"[PHP Modules]\\n\";
foreach ($m as $e) echo $e, \"\\n\";
echo \"\\n[Zend Modules]\\n\";
$z = get_loaded_extensions(true);
sort($z);
foreach ($z as $e) echo $e, \"\\n\";
echo \"\\n\";
";

/// Creates a VM with the configuration and the CLI symbols.
fn new_vm(cfg: &Config) -> Vm {
    let opts = VmOptions {
        ini: cfg.entries.clone(),
    };
    let stdout: Box<dyn Write> = Box::new(std::io::BufWriter::with_capacity(
        1 << 16,
        std::io::stdout(),
    ));
    let exts = extensions::all();
    let refs: Vec<&dyn php_vm::ext::Extension> = exts.iter().map(AsRef::as_ref).collect();
    let mut vm = Vm::with_extensions(&opts, stdout, &refs);
    vm.set_native_stack_size(usize::try_from(VM_STACK).unwrap_or(usize::MAX));
    sapi::register(&mut vm, cfg);
    vm
}

/// `php_request_startup()` (what `Vm::run_script` does before running).
fn request_startup(vm: &mut Vm) {
    let ob = vm.ini_long("output_buffering");
    let flush = vm.ini_bool("implicit_flush");
    php_support::output::request_startup(vm, None, ob, flush);
    for hook in vm.startup_hooks.clone() {
        hook(vm);
    }
    let limit = u64::try_from(vm.ini_long("memory_limit")).unwrap_or(u64::MAX);
    if php_memlimit::request_start(php_memlimit::limit_from_ini(limit)).is_err() {
        php_memlimit::GLOBAL.force_limit(Some(php_memlimit::DEFAULT_LIMIT));
    }
}

/// Runs PHP code for an informational mode (`-i`, `-m`, `--r*`); `args`
/// become `$argv[1..]`.
fn run_snippet(cfg: &Config, code: &[u8], args: &[Vec<u8>], with_handles: bool) -> i32 {
    let _ = with_handles;
    let mut vm = new_vm(cfg);
    let mut request_argv = vec![b"Standard input code".to_vec()];
    request_argv.extend_from_slice(args);
    vm.set_cli_args("Standard input code", args);
    sapi::register_variables(
        &mut vm,
        &sapi::Request {
            php_self: b"Standard input code",
            script_filename: b"",
            argv: &request_argv,
        },
    );
    request_startup(&mut vm);
    let r = vm.run_main(code, &ZStr::from("Command line code"));
    vm.shutdown(r)
}

/// `cli_seek_file_begin()` and reading the script: a directory opens
/// (as with `fopen()`) but reads as empty.
fn open_script(file: &[u8]) -> Option<Vec<u8>> {
    let p = std::path::Path::new(std::ffi::OsStr::from_bytes(file));
    if std::fs::File::open(p).is_err() {
        could_not_open(file);
        return None;
    }
    Some(std::fs::read(p).unwrap_or_default())
}

fn could_not_open(file: &[u8]) {
    err(format!(
        "Could not open input file: {}\n",
        String::from_utf8_lossy(file)
    )
    .as_bytes());
}

/// The absolute, symlink-resolved path of the primary script
/// (`php_execute_script` expands it for `__FILE__` and messages).
fn real_path(file: &[u8]) -> Vec<u8> {
    std::fs::canonicalize(std::ffi::OsStr::from_bytes(file))
        .map_or_else(|_| file.to_vec(), |c| c.into_os_string().into_vec())
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// One request (`do_repeat:` to `out:` of `do_cli()`).
#[allow(clippy::too_many_lines)]
fn run_request(argv: &[Vec<u8>], optind: usize, req: &Request, cfg: &Config) -> i32 {
    let mode = req.mode();
    // The script source (from the file, or stdin for file modes).
    let mut src: Option<Vec<u8>> = None;
    let php_self: Vec<u8>;
    let mut script_filename: Vec<u8> = Vec::new();
    if let Some(f) = &req.script_file {
        match open_script(f) {
            Some(s) if mode != Mode::ProcessStdin => src = Some(s),
            Some(_) => {}
            None => return 1,
        }
        php_self = f.clone();
        script_filename.clone_from(f);
    } else {
        php_self = b"Standard input code".to_vec();
        if mode < Mode::CliDirect {
            let mut s = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut s);
            src = Some(s);
        }
    }
    // $argv: php_self, then the arguments after the options.
    let mut request_argv: Vec<Vec<u8>> = vec![php_self.clone()];
    request_argv.extend(argv.iter().skip(optind).cloned());

    match mode {
        Mode::Lint => {
            let src = src.unwrap_or_default();
            lint(cfg, &src, &php_self)
        }
        Mode::Strip | Mode::Highlight => {
            let func = if mode == Mode::Strip {
                "php_strip_whitespace"
            } else {
                "highlight_file"
            };
            let echo = if mode == Mode::Strip { "echo " } else { "" };
            let file = if req.script_file.is_some() {
                php_self.clone()
            } else {
                // Source from stdin: hand it over in a temporary file.
                let tmp = std::env::temp_dir().join(format!("php-cli-src-{}", std::process::id()));
                if std::fs::write(&tmp, src.unwrap_or_default()).is_err() {
                    return 1;
                }
                tmp.into_os_string().into_vec()
            };
            let code = format!("<?php {echo}{func}($argv[1]);").into_bytes();
            let r = run_snippet(cfg, &code, std::slice::from_ref(&file), false);
            if req.script_file.is_none() {
                let _ = std::fs::remove_file(std::ffi::OsStr::from_bytes(&file));
            }
            r
        }
        Mode::Standard | Mode::CliDirect | Mode::ProcessStdin => {
            let mut vm = new_vm(cfg);
            vm.set_cli_args(&lossy(&php_self), &request_argv[1..]);
            sapi::register_variables(
                &mut vm,
                &sapi::Request {
                    php_self: &php_self,
                    script_filename: &script_filename,
                    argv: &request_argv,
                },
            );
            match mode {
                Mode::Standard => {
                    let src = src.unwrap_or_default();
                    let path = if req.script_file.is_some() {
                        real_path(&php_self)
                    } else {
                        php_self.clone()
                    };
                    execute_script(&mut vm, &src, &path)
                }
                Mode::CliDirect => {
                    request_startup(&mut vm);
                    let r = eval_code(
                        &mut vm,
                        req.exec_direct.as_deref().unwrap_or_default(),
                        "Command line code",
                    );
                    vm.shutdown(r)
                }
                _ => process_stdin(&mut vm, req),
            }
        }
        Mode::ReflectionFunction
        | Mode::ReflectionClass
        | Mode::ReflectionExtension
        | Mode::ReflectionZendExtension => {
            let what = &req.reflection_what;
            let ctor = match mode {
                Mode::ReflectionFunction if what.windows(2).any(|w| w == b"::") => {
                    "ReflectionMethod::createFromMethodName($argv[1])"
                }
                Mode::ReflectionFunction => "new ReflectionFunction($argv[1])",
                Mode::ReflectionClass => "new ReflectionClass($argv[1])",
                Mode::ReflectionExtension => "new ReflectionExtension($argv[1])",
                _ => "new ReflectionZendExtension($argv[1])",
            };
            let code = format!(
                "<?php try {{ $r = {ctor}; }} catch (Throwable $e) {{ echo 'Exception: ', $e->getMessage(), \"\\n\"; exit(1); }} echo $r, \"\\n\";"
            );
            run_snippet(cfg, code.as_bytes(), std::slice::from_ref(what), false)
        }
        Mode::ReflectionExtInfo => {
            let code = b"<?php $n = $argv[1];
if (extension_loaded($n)) { (new ReflectionExtension($n))->info(); }
elseif ($n === 'main') { $all = ini_get_all(null, true); }
else { echo \"Extension '$n' not present.\\n\"; exit(1); }";
            run_snippet(cfg, code, std::slice::from_ref(&req.reflection_what), false)
        }
        Mode::ShowIniConfig => {
            out(show_ini_config(cfg).as_bytes());
            0
        }
        Mode::ShowIniDiff => run_snippet(cfg, INI_DIFF_SNIPPET, &[], false),
    }
}

/// `--ini`.
fn show_ini_config(cfg: &Config) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "Configuration File (php.ini) Path: \"{}\"",
        config::CONFIG_FILE_PATH
    );
    let _ = match &cfg.opened_path {
        Some(p) => writeln!(s, "Loaded Configuration File:         \"{}\"", lossy(p)),
        None => writeln!(s, "Loaded Configuration File:         (none)"),
    };
    let _ = match &cfg.scanned_path {
        Some(p) => writeln!(s, "Scan for additional .ini files in: \"{}\"", lossy(p)),
        None => writeln!(s, "Scan for additional .ini files in: (none)"),
    };
    let _ = writeln!(
        s,
        "Additional .ini files parsed:      {}",
        cfg.scanned_files
            .as_deref()
            .map_or_else(|| "(none)".to_string(), lossy)
    );
    s
}

const INI_DIFF_SNIPPET: &[u8] = b"<?php
echo \"Non-default INI settings:\\n\";
$all = ini_get_all(null, true);
uksort($all, 'strcasecmp');
foreach ($all as $k => $v) {
    $d = $v['global_value'];
    $l = $v['local_value'];
    if ($l === $d) continue;
    printf(\"%s: %s -> %s\\n\", $k, $d === null ? '(none)' : \"\\\"$d\\\"\", $l === null ? '(none)' : \"\\\"$l\\\"\");
}
";

/// `php_resolve_path()` for `auto_prepend_file`/`auto_append_file`:
/// relative names are looked up in `include_path`, then the working
/// directory.
fn resolve_path(vm: &Vm, name: &[u8]) -> Option<Vec<u8>> {
    let is_file = |p: &[u8]| std::path::Path::new(std::ffi::OsStr::from_bytes(p)).is_file();
    if name.starts_with(b"/") || name.starts_with(b"./") || name.starts_with(b"../") {
        return is_file(name).then(|| name.to_vec());
    }
    let include_path = vm.ini_string("include_path").unwrap_or_default();
    for dir in include_path.split(|&c| c == b':').filter(|d| !d.is_empty()) {
        let mut cand = dir.to_vec();
        if !cand.ends_with(b"/") {
            cand.push(b'/');
        }
        cand.extend_from_slice(name);
        if is_file(&cand) {
            return Some(cand);
        }
    }
    is_file(name).then(|| name.to_vec())
}

/// `php_execute_script()`: `auto_prepend_file`, the primary script, then
/// `auto_append_file`, and request shutdown.
fn execute_script(vm: &mut Vm, src: &[u8], path: &[u8]) -> i32 {
    request_startup(vm);
    let extra = |vm: &mut Vm, directive: &str| -> Result<(), Throw> {
        let name = vm.ini_string(directive).unwrap_or_default();
        if name.is_empty() {
            return Ok(());
        }
        let Some(full) = resolve_path(vm, &name) else {
            return Ok(());
        };
        // Recorded like any `require`d file (`get_included_files()`,
        // `*_once`).
        let real = ZStr::from(real_path(&full));
        if vm.included_set.insert(real.clone()) {
            vm.included.push(real);
        }
        vm.include_file(&lossy(&full))
    };
    let mut r = extra(vm, "auto_prepend_file");
    if r.is_ok() {
        r = vm.run_main(src, &ZStr::from(path));
    }
    if r.is_ok() {
        r = extra(vm, "auto_append_file");
    }
    vm.shutdown(r)
}

/// `zend_eval_string_ex(code, NULL, name, 1)`.
fn eval_code(vm: &mut Vm, code: &[u8], name: &str) -> Result<(), Throw> {
    let mut src = b"<?php ".to_vec();
    src.extend_from_slice(code);
    vm.run_main(&src, &ZStr::from(name))
}

/// `PHP_CLI_MODE_PROCESS_STDIN`: `-B`, then `-R`/`-F` for every line
/// of `STDIN` (with `$argn`/`$argi`), then `-E`.
fn process_stdin(vm: &mut Vm, req: &Request) -> i32 {
    request_startup(vm);
    let mut r = Ok(());
    if let Some(b) = &req.exec_begin {
        r = eval_code(vm, b, "Command line begin code");
    }
    let stdin = vm.constants.get(b"STDIN".as_slice()).cloned();
    let script_src: Option<(Vec<u8>, Vec<u8>)> = match (&req.exec_run, &req.script_file) {
        (None, Some(f)) => Some((f.clone(), real_path(f))),
        _ => None,
    };
    let mut index: i64 = 0;
    while r.is_ok() && vm.exit_status == 0 {
        let Some(stdin) = &stdin else { break };
        let line = match vm.call_value(&Value::Str(ZStr::from("fgets")), vec![stdin.clone()]) {
            Ok(Value::Str(s)) => s,
            Ok(_) => break,
            Err(t) => {
                r = Err(t);
                break;
            }
        };
        let mut l = line.as_bytes().to_vec();
        while l.last().is_some_and(|&c| c == b'\n' || c == b'\r') {
            l.pop();
        }
        index += 1;
        sapi::set_global(vm, "argn", Value::Str(ZStr::from(l)));
        sapi::set_global(vm, "argi", Value::Int(index));
        if let Some(code) = &req.exec_run {
            r = eval_code(vm, code, "Command line run code");
        } else if let Some((given, path)) = &script_src {
            if let Ok(s) = std::fs::read(std::ffi::OsStr::from_bytes(given)) {
                r = vm.run_main(&s, &ZStr::from(path.as_slice()));
            } else {
                could_not_open(given);
                vm.exit_status = 1;
            }
        }
    }
    if r.is_ok() {
        if let Some(e) = &req.exec_end {
            r = eval_code(vm, e, "Command line end code");
        }
    }
    vm.shutdown(r)
}

/// `-l`: `php_lint_script()` then the CLI's verdict line.
fn lint(cfg: &Config, src: &[u8], php_self: &[u8]) -> i32 {
    let name = lossy(php_self);
    match Vm::lint(src) {
        Ok(()) => {
            out(format!("No syntax errors detected in {name}\n").as_bytes());
            0
        }
        Err((msg, line, parse)) => {
            let label = if parse { "Parse error" } else { "Fatal error" };
            let display = cfg.get("display_errors").unwrap_or("1");
            let log = cfg
                .get("log_errors")
                .is_some_and(|v| php_support::ini::parse_bool(v.as_bytes()));
            let text = format!("{msg} in {name} on line {line}");
            if log {
                err(format!("PHP {label}:  {text}\n").as_bytes());
            }
            if display.eq_ignore_ascii_case("stderr") {
                err(format!("PHP {label}:  {text}\n").as_bytes());
            } else if php_support::ini::parse_bool(display.as_bytes()) {
                out(format!("\n{label}: {text}\n").as_bytes());
            }
            out(format!("Errors parsing {name}\n").as_bytes());
            255
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_uses_program_basename() {
        let u = String::from_utf8(usage(b"/usr/bin/php8")).unwrap();
        assert!(u.starts_with("Usage: php8 [options] [-f] <file> [--] [args...]\n"));
        let u = String::from_utf8(usage(b"weird")).unwrap();
        assert!(u.starts_with("Usage: php [options]"));
        assert!(u.contains("  -C               Do not chdir"));
    }

    #[test]
    fn version_banner_format() {
        let v = version_banner();
        assert!(v.starts_with(&format!("PHP {} (cli) (built: ", php_vm::PHP_VERSION)));
        assert!(
            v.contains(") (NTS)\nCopyright \u{a9} The PHP Group and Contributors\nZend Engine v")
        );
    }

    fn args(a: &[&str]) -> Vec<Vec<u8>> {
        std::iter::once("php")
            .chain(a.iter().copied())
            .map(|s| s.as_bytes().to_vec())
            .collect()
    }

    #[test]
    fn mode_conflicts() {
        let (_, _, e) = parse_request(&args(&["-r", "echo 1;", "-F", "x"]));
        assert_eq!(e, Some(PARAM_MODE_CONFLICT));
        let (_, _, e) = parse_request(&args(&["-F", "a", "-R", "b"]));
        assert_eq!(e, Some("You can use -R or -F only once.\n"));
        let (_, _, e) = parse_request(&args(&["-r", "1", "-s"]));
        assert_eq!(e, Some("Source highlighting only works for files.\n"));
        let (r, _, e) = parse_request(&args(&["-r", "echo 3;", "-l"]));
        assert_eq!(e, None);
        assert_eq!(r.mode(), Mode::CliDirect);
        let (r, ind, e) = parse_request(&args(&["-l", "a.php", "b.php"]));
        assert_eq!(e, None);
        assert_eq!(r.mode(), Mode::Lint);
        assert_eq!(ind, 2);
    }

    #[test]
    fn builtin_date_shape() {
        assert_eq!(BUILD_DATE.len(), 20);
    }
}
