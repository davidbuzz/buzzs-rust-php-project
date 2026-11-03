//! A port of `php_getopt()` (`main/getopt.c`): short options (clustered
//! `-nq`), values given as `-dfoo`, `-d foo` or `-d=foo`, long options
//! (`--define foo`, `--ini=diff`) and `--` as the end of the options.

use std::io::Write;

/// One entry of the option table (`opt_struct`).
#[derive(Debug, Clone, Copy)]
pub struct Opt {
    /// The option character (or a code above `'~'` for long-only options).
    pub ch: u8,
    /// 0: no value, 1: required value, 2: optional value (`--ini[=diff]`).
    pub need_param: u8,
    /// The long name.
    pub name: &'static str,
}

/// Returned for an unknown option or a missing value
/// (`PHP_GETOPT_INVALID_ARG`).
pub const INVALID: i32 = -2;

/// The CLI option table (`OPTIONS[]` of `php_cli.c`).
pub const OPTIONS: &[Opt] = &[
    Opt {
        ch: b'a',
        need_param: 0,
        name: "interactive",
    },
    Opt {
        ch: b'B',
        need_param: 1,
        name: "process-begin",
    },
    Opt {
        ch: b'C',
        need_param: 0,
        name: "no-chdir",
    },
    Opt {
        ch: b'c',
        need_param: 1,
        name: "php-ini",
    },
    Opt {
        ch: b'd',
        need_param: 1,
        name: "define",
    },
    Opt {
        ch: b'E',
        need_param: 1,
        name: "process-end",
    },
    Opt {
        ch: b'e',
        need_param: 0,
        name: "profile-info",
    },
    Opt {
        ch: b'F',
        need_param: 1,
        name: "process-file",
    },
    Opt {
        ch: b'f',
        need_param: 1,
        name: "file",
    },
    Opt {
        ch: b'h',
        need_param: 0,
        name: "help",
    },
    Opt {
        ch: b'i',
        need_param: 0,
        name: "info",
    },
    Opt {
        ch: b'l',
        need_param: 0,
        name: "syntax-check",
    },
    Opt {
        ch: b'm',
        need_param: 0,
        name: "modules",
    },
    Opt {
        ch: b'n',
        need_param: 0,
        name: "no-php-ini",
    },
    Opt {
        ch: b'q',
        need_param: 0,
        name: "no-header",
    },
    Opt {
        ch: b'R',
        need_param: 1,
        name: "process-code",
    },
    Opt {
        ch: b'H',
        need_param: 0,
        name: "hide-args",
    },
    Opt {
        ch: b'r',
        need_param: 1,
        name: "run",
    },
    Opt {
        ch: b's',
        need_param: 0,
        name: "syntax-highlight",
    },
    Opt {
        ch: b's',
        need_param: 0,
        name: "syntax-highlighting",
    },
    Opt {
        ch: b'S',
        need_param: 1,
        name: "server",
    },
    Opt {
        ch: b't',
        need_param: 1,
        name: "docroot",
    },
    Opt {
        ch: b'w',
        need_param: 0,
        name: "strip",
    },
    Opt {
        ch: b'?',
        need_param: 0,
        name: "usage",
    },
    Opt {
        ch: b'v',
        need_param: 0,
        name: "version",
    },
    Opt {
        ch: 10,
        need_param: 1,
        name: "rf",
    },
    Opt {
        ch: 10,
        need_param: 1,
        name: "rfunction",
    },
    Opt {
        ch: 11,
        need_param: 1,
        name: "rc",
    },
    Opt {
        ch: 11,
        need_param: 1,
        name: "rclass",
    },
    Opt {
        ch: 12,
        need_param: 1,
        name: "re",
    },
    Opt {
        ch: 12,
        need_param: 1,
        name: "rextension",
    },
    Opt {
        ch: 13,
        need_param: 1,
        name: "rz",
    },
    Opt {
        ch: 13,
        need_param: 1,
        name: "rzendextension",
    },
    Opt {
        ch: 14,
        need_param: 1,
        name: "ri",
    },
    Opt {
        ch: 14,
        need_param: 1,
        name: "rextinfo",
    },
    Opt {
        ch: 15,
        need_param: 2,
        name: "ini",
    },
    Opt {
        ch: 16,
        need_param: 1,
        name: "repeat",
    },
];

enum OptErr {
    Colon,
    NotFound,
    Arg,
}

/// The parser state (`php_getopt()`'s statics plus `optind`).
pub struct GetOpt<'a> {
    argv: &'a [Vec<u8>],
    opts: &'a [Opt],
    /// Index of the next argument to look at (`php_optind`).
    pub optind: usize,
    optchr: usize,
    dash: bool,
    show_err: bool,
}

impl<'a> GetOpt<'a> {
    /// Starts parsing `argv` (which includes `argv[0]`) at index 1.
    #[must_use]
    pub fn new(argv: &'a [Vec<u8>], opts: &'a [Opt], show_err: bool) -> Self {
        GetOpt {
            argv,
            opts,
            optind: 1,
            optchr: 0,
            dash: false,
            show_err,
        }
    }

    fn byte(&self, ind: usize, pos: usize) -> u8 {
        self.argv[ind].get(pos).copied().unwrap_or(0)
    }

    fn error(&self, optint: usize, optchr: usize, err: &OptErr) -> (i32, Option<Vec<u8>>) {
        if self.show_err {
            let mut msg = format!("Error in argument {optint}, char {}: ", optchr + 1).into_bytes();
            match err {
                OptErr::Colon => msg.extend_from_slice(b": in flags\n"),
                OptErr::NotFound => {
                    msg.extend_from_slice(b"option not found ");
                    msg.push(self.byte(optint, optchr));
                    msg.push(b'\n');
                }
                OptErr::Arg => {
                    msg.extend_from_slice(b"no argument for option ");
                    msg.push(self.byte(optint, optchr));
                    msg.push(b'\n');
                }
            }
            let _ = std::io::stderr().write_all(&msg);
        }
        (INVALID, None)
    }

    /// The next option: `Some((code, value))`, or `None` at the end of
    /// the options (`EOF`).  `code` is [`INVALID`] for errors.
    #[allow(clippy::too_many_lines)]
    pub fn next_opt(&mut self) -> Option<(i32, Option<Vec<u8>>)> {
        let argc = self.argv.len();
        if self.optind >= argc {
            return None;
        }
        let ind = self.optind;
        if !self.dash {
            if self.byte(ind, 0) != b'-' {
                return None;
            }
            if self.byte(ind, 1) == 0 {
                // A lone `-` means stdin: left to the caller.
                return None;
            }
        }
        let mut idx: Option<usize> = None;
        let mut arg_start;
        if self.byte(ind, 0) == b'-' && self.byte(ind, 1) == b'-' {
            if self.byte(ind, 2) == 0 {
                self.optind += 1;
                return None;
            }
            let arg = &self.argv[ind];
            arg_start = 2;
            let mut arg_end = arg.len() - 1;
            // `--name=value`
            let eq = arg[arg_start..arg_end.max(arg_start)]
                .iter()
                .position(|&c| c == b'=');
            if let Some(p) = eq {
                arg_end = p;
                arg_start += 1;
            } else {
                arg_end -= 1;
            }
            let name = &arg[2..];
            let found = self.opts.iter().position(|o| {
                name.len() >= arg_end
                    && o.name.len() == arg_end
                    && &name[..arg_end] == o.name.as_bytes()
            });
            let Some(i) = found else {
                self.optind += 1;
                return Some(self.error(self.optind - 1, self.optchr, &OptErr::Arg));
            };
            idx = Some(i);
            self.optchr = 0;
            self.dash = false;
            arg_start += self.opts[i].name.len();
        } else {
            if !self.dash {
                self.dash = true;
                self.optchr = 1;
            }
            if self.byte(ind, self.optchr) == b':' {
                self.dash = false;
                self.optind += 1;
                return Some(self.error(self.optind - 1, self.optchr, &OptErr::Colon));
            }
            arg_start = 1 + self.optchr;
        }
        let i = if let Some(i) = idx {
            i
        } else {
            let c = self.byte(ind, self.optchr);
            if let Some(i) = self.opts.iter().position(|o| o.ch == c) {
                i
            } else {
                let errind = self.optind;
                let errchr = self.optchr;
                if self.byte(ind, self.optchr + 1) == 0 {
                    self.dash = false;
                    self.optind += 1;
                } else {
                    self.optchr += 1;
                }
                return Some(self.error(errind, errchr, &OptErr::NotFound));
            }
        };
        let opt = self.opts[i];
        let code = i32::from(opt.ch);
        if opt.need_param > 0 {
            self.dash = false;
            if self.byte(ind, arg_start) == 0 {
                self.optind += 1;
                if self.optind == argc {
                    if opt.need_param == 1 {
                        return Some(self.error(self.optind - 1, self.optchr, &OptErr::Arg));
                    }
                } else if opt.need_param == 1 {
                    let v = self.argv[self.optind].clone();
                    self.optind += 1;
                    return Some((code, Some(v)));
                }
                return Some((code, None));
            }
            let start = if self.byte(ind, arg_start) == b'=' {
                arg_start + 1
            } else {
                arg_start
            };
            let v = self.argv[ind][start..].to_vec();
            self.optind += 1;
            return Some((code, Some(v)));
        }
        // Several short options in one argument (`-nq`).
        let is_long = self.byte(ind, 0) == b'-' && self.byte(ind, 1) == b'-';
        if arg_start >= 2 && !is_long {
            if self.byte(ind, self.optchr + 1) == 0 {
                self.dash = false;
                self.optind += 1;
            } else {
                self.optchr += 1;
            }
        } else {
            self.optind += 1;
        }
        Some((code, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> (Vec<(i32, Option<String>)>, usize) {
        let full: Vec<Vec<u8>> = std::iter::once("php")
            .chain(args.iter().copied())
            .map(|s| s.as_bytes().to_vec())
            .collect();
        let mut g = GetOpt::new(&full, OPTIONS, false);
        let mut out = Vec::new();
        while let Some((c, v)) = g.next_opt() {
            out.push((c, v.map(|v| String::from_utf8_lossy(&v).into_owned())));
        }
        (out, g.optind)
    }

    #[test]
    fn short_and_clustered() {
        let (o, ind) = parse(&["-n", "-dfoo=1", "-d", "bar", "-nq", "file.php", "-x"]);
        assert_eq!(
            o,
            vec![
                (i32::from(b'n'), None),
                (i32::from(b'd'), Some("foo=1".into())),
                (i32::from(b'd'), Some("bar".into())),
                (i32::from(b'n'), None),
                (i32::from(b'q'), None),
            ]
        );
        assert_eq!(ind, 6);
    }

    #[test]
    fn long_options() {
        let (o, _) = parse(&["--define", "a=b", "--ini=diff", "--ini", "--rf=strlen"]);
        assert_eq!(
            o,
            vec![
                (i32::from(b'd'), Some("a=b".into())),
                (15, Some("diff".into())),
                (15, None),
                (10, Some("strlen".into())),
            ]
        );
    }

    #[test]
    fn double_dash_and_errors() {
        let (o, ind) = parse(&["--", "-n"]);
        assert!(o.is_empty());
        assert_eq!(ind, 2);
        let (o, _) = parse(&["--notexisting"]);
        assert_eq!(o, vec![(INVALID, None)]);
        let (o, _) = parse(&["-d"]);
        assert_eq!(o, vec![(INVALID, None)]);
        let (o, _) = parse(&["-r=echo 1;"]);
        assert_eq!(o, vec![(i32::from(b'r'), Some("echo 1;".into()))]);
    }
}
