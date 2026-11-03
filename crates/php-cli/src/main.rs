//! The `php` command line interface (`sapi/cli/php_cli.c`).

mod extensions;

use php_vm::{Vm, VmOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

const USAGE: &str = "Usage: php [options] [-f] <file> [--] [args...]
   php [options] -r <code> [--] [args...]
   php [options] [-B <begin_code>] -R <code> [-E <end_code>] [--] [args...]
   php [options] [-B <begin_code>] -F <file> [-E <end_code>] [--] [args...]
   php [options] -S <addr>:<port> [-t docroot] [router]
   php [options] -- [args...]
   php [options] -a

  -a               Run as interactive shell (requires readline extension)
  -c <path>|<file> Look for php.ini file in this directory
  -n               No configuration (ini) files will be used
  -d foo[=bar]     Define INI entry foo with value 'bar'
  -e               Generate extended information for debugger/profiler
  -f <file>        Parse and execute <file>.
  -h               This help
  -i               PHP information
  -l               Syntax check only (lint)
  -m               Show compiled in modules
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
  -z <file>        Load Zend extension <file>.

  args...          Arguments passed to script. Use -- args when first argument
                   starts with - or script is read from stdin

  --ini            Show configuration file names

  --rf <name>      Show information about function <name>.
  --rc <name>      Show information about class <name>.
  --re <name>      Show information about extension <name>.
  --rz <name>      Show information about Zend extension <name>.
  --ri <name>      Show configuration for extension <name>.

";

enum Mode {
    File(Vec<u8>),
    Code(Vec<u8>),
    Stdin,
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
        libc::setrlimit(libc::RLIMIT_AS, &rl);
    }
}

fn main() {
    limit_memory();
    let raw: Vec<Vec<u8>> = std::env::args_os().skip(1).map(OsStringExt::into_vec).collect();
    std::process::exit(run(&raw));
}

fn version_banner() -> String {
    format!(
        "PHP {} (cli) (built: Jan  1 2026 00:00:00) (NTS)\nCopyright (c) The PHP Group\nZend Engine v{}, Copyright (c) Zend Technologies\n",
        php_vm::PHP_VERSION,
        php_vm::ZEND_VERSION
    )
}

#[allow(clippy::too_many_lines)]
fn run(raw: &[Vec<u8>]) -> i32 {
    let mut ini: Vec<(String, String)> = Vec::new();
    let mut mode: Option<Mode> = None;
    let mut lint = false;
    let mut script_args: Vec<Vec<u8>> = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let a = &raw[i];
        if a.as_slice() == b"--" {
            script_args.extend(raw[i + 1..].iter().cloned());
            break;
        }
        if !a.starts_with(b"-") || a.len() == 1 {
            if mode.is_none() {
                mode = Some(Mode::File(a.clone()));
                script_args.extend(raw[i + 1..].iter().cloned());
            } else {
                script_args.extend(raw[i..].iter().cloned());
            }
            break;
        }
        let opt = String::from_utf8_lossy(a).into_owned();
        let take_val = |i: &mut usize, inline: &str| -> Option<Vec<u8>> {
            if !inline.is_empty() {
                return Some(inline.as_bytes().to_vec());
            }
            *i += 1;
            raw.get(*i).cloned()
        };
        match opt.as_str() {
            "-n" | "-q" | "-e" | "-H" | "-C" => {}
            "-l" | "--syntax-check" => lint = true,
            "-v" | "--version" => {
                print!("{}", version_banner());
                return 0;
            }
            "-h" | "-?" | "--help" | "--usage" => {
                print!("{USAGE}");
                return 0;
            }
            "-m" | "--modules" => {
                println!("[PHP Modules]");
                let mut m: Vec<&str> = php_vm::LOADED_EXTENSIONS.to_vec();
                m.sort_by_key(|x| x.to_ascii_lowercase());
                for e in m {
                    println!("{e}");
                }
                println!("\n[Zend Modules]\n");
                return 0;
            }
            "-i" | "--info" => {
                println!("phpinfo()\nPHP Version => {}\n", php_vm::PHP_VERSION);
                return 0;
            }
            "--ini" => {
                println!("Configuration File (php.ini) Path: \nLoaded Configuration File:         (none)\nScan for additional .ini files in: (none)\nAdditional .ini files parsed:      (none)");
                return 0;
            }
            _ => {
                if let Some(rest) = opt.strip_prefix("-d") {
                    let Some(v) = take_val(&mut i, rest) else {
                        eprintln!("Error in argument {}. Error: Missing argument to -d", i);
                        return 1;
                    };
                    let v = String::from_utf8_lossy(&v).into_owned();
                    match v.split_once('=') {
                        Some((k, val)) => ini.push((k.trim().to_string(), strip_quotes(val))),
                        None => ini.push((v.trim().to_string(), "1".to_string())),
                    }
                } else if let Some(rest) = opt.strip_prefix("-r") {
                    let Some(v) = take_val(&mut i, rest) else {
                        return 1;
                    };
                    mode = Some(Mode::Code(v));
                    script_args.extend(raw[i + 1..].iter().filter(|x| x.as_slice() != b"--").cloned());
                    break;
                } else if let Some(rest) = opt.strip_prefix("-f") {
                    let Some(v) = take_val(&mut i, rest) else {
                        return 1;
                    };
                    mode = Some(Mode::File(v));
                    let rest_args: Vec<Vec<u8>> = raw[i + 1..].to_vec();
                    let rest_args = if rest_args.first().is_some_and(|x| x.as_slice() == b"--") {
                        rest_args[1..].to_vec()
                    } else {
                        rest_args
                    };
                    script_args.extend(rest_args);
                    break;
                } else if let Some(rest) = opt.strip_prefix("-c") {
                    let _ = take_val(&mut i, rest);
                } else if let Some(rest) = opt.strip_prefix("-z") {
                    let _ = take_val(&mut i, rest);
                } else {
                    eprintln!("Error in argument {}, char 2: no argument for option {}", i + 1, &opt[1..]);
                    print!("{USAGE}");
                    return 1;
                }
            }
        }
        i += 1;
    }
    let mode = mode.unwrap_or(Mode::Stdin);
    let (src, path, display): (Vec<u8>, Vec<u8>, Vec<u8>) = match mode {
        Mode::File(p) => {
            let path = std::path::Path::new(std::ffi::OsStr::from_bytes(&p));
            match std::fs::read(path) {
                Ok(s) => (s, p.clone(), p),
                Err(_) => {
                    println!("Could not open input file: {}", String::from_utf8_lossy(&p));
                    return 1;
                }
            }
        }
        Mode::Code(c) => {
            let mut s = b"<?php ".to_vec();
            s.extend_from_slice(&c);
            (s, b"Command line code".to_vec(), b"Standard input code".to_vec())
        }
        Mode::Stdin => {
            let mut s = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut s);
            (s, b"Standard input code".to_vec(), b"Standard input code".to_vec())
        }
    };
    if lint {
        return match Vm::lint(&src) {
            Ok(()) => {
                println!("No syntax errors detected in {}", String::from_utf8_lossy(&path));
                0
            }
            Err((msg, line, parse)) => {
                let label = if parse { "Parse error" } else { "Fatal error" };
                println!(
                    "PHP {label}:  {msg} in {} on line {line}\n{label}: {msg} in {} on line {line}\nErrors parsing {}",
                    String::from_utf8_lossy(&path),
                    String::from_utf8_lossy(&path),
                    String::from_utf8_lossy(&path)
                );
                255
            }
        };
    }
    let opts = VmOptions { ini };
    let script_name = String::from_utf8_lossy(&display).into_owned();
    let p = String::from_utf8_lossy(&path).into_owned();
    // Run on a thread with a large stack (deep recursion in user code).
    let r = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let stdout: Box<dyn Write> =
                Box::new(std::io::BufWriter::with_capacity(1 << 16, std::io::stdout()));
            let exts = extensions::all();
            let refs: Vec<&dyn php_vm::ext::Extension> = exts.iter().map(AsRef::as_ref).collect();
            let mut vm = Vm::with_extensions(&opts, stdout, &refs);
            vm.set_cli_args(&script_name, &script_args);
            vm.run_script(&src, &p)
        })
        .map(|h| h.join().unwrap_or(255));
    r.unwrap_or(255)
}

fn strip_quotes(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        return v[1..v.len() - 1].to_string();
    }
    v.to_string()
}
