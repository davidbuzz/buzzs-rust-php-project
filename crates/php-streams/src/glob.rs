//! `glob()` — a port of PHP's bundled OpenBSD glob (`main/php_glob.c`) and
//! the filesystem-independent parts of `PHP_FUNCTION(glob)`
//! (`ext/standard/dir.c`).
//!
//! php-src master uses its own glob implementation unless configured with
//! `--with-system-glob`, so the flag values are PHP's `PHP_GLOB_*` constants
//! (not glibc's). Matching works on bytes; patterns are compiled into 16-bit
//! "Char" units exactly like the C code, so quoting (`\x`), brace expansion,
//! bracket expressions (including `[:class:]`), result sorting (`strcmp`
//! order per expanded brace alternative) and `GLOB_MARK`/`GLOB_NOCHECK`
//! behave identically.
//!
//! Directory access uses `std::fs`. Because `read_dir` hides `.` and `..`,
//! they are synthesised at the start of every directory listing (C `readdir`
//! returns them too); with [`GLOB_NOSORT`] the raw order of the remaining
//! entries is the kernel's `readdir` order, as in PHP.
//!
//! Not supported (not reachable through PHP's `glob()` because they are not
//! in [`GLOB_AVAILABLE_FLAGS`]): `GLOB_TILDE`, `GLOB_ALTDIRFUNC`,
//! `GLOB_KEEPSTAT`, `GLOB_LIMIT`, `GLOB_APPEND`, `GLOB_DOOFFS`.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// `GLOB_ERR`: stop on unreadable directories. (PHP passes no error
/// callback, so in practice this flag has no effect, as in php-src.)
pub const GLOB_ERR: i32 = 0x0004;
/// `GLOB_MARK`: append `/` to matching directories.
pub const GLOB_MARK: i32 = 0x0008;
/// `GLOB_NOCHECK`: return the pattern itself when nothing matches.
pub const GLOB_NOCHECK: i32 = 0x0010;
/// `GLOB_NOSORT`: return entries in directory order.
pub const GLOB_NOSORT: i32 = 0x0020;
/// `GLOB_BRACE`: expand `{a,b,c}` alternatives.
pub const GLOB_BRACE: i32 = 0x0080;
/// `GLOB_NOMAGIC`: like `GLOB_NOCHECK` but only for patterns without
/// wildcards (internal; not a PHP constant).
pub const GLOB_NOMAGIC: i32 = 0x0200;
/// `GLOB_NOESCAPE`: backslash is not a quoting character.
pub const GLOB_NOESCAPE: i32 = 0x1000;
/// `GLOB_ONLYDIR`: only return directories (filtered by `PHP_FUNCTION(glob)`,
/// not by the matcher). PHP defines it as `1 << 30` with its own glob.
pub const GLOB_ONLYDIR: i32 = 1 << 30;
/// All flags accepted by PHP's `glob()`.
pub const GLOB_AVAILABLE_FLAGS: i32 =
    GLOB_BRACE | GLOB_MARK | GLOB_NOSORT | GLOB_NOCHECK | GLOB_NOESCAPE | GLOB_ERR | GLOB_ONLYDIR;

/// Set internally when the pattern contained wildcards.
const GLOB_MAGCHAR: i32 = 0x0100;
/// Mask applied before calling the matcher (`PHP_GLOB_FLAGMASK`).
const GLOB_FLAGMASK: i32 = !GLOB_ONLYDIR;

/// `MAXPATHLEN`/`PATH_MAX` on Linux.
pub const MAXPATHLEN: usize = 4096;
const PATH_MAX: usize = 4096;

/// Failure codes of the low-level matcher ([`php_glob`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobError {
    /// `PHP_GLOB_NOSPACE` (-1).
    NoSpace,
    /// `PHP_GLOB_ABORTED` (-2).
    Aborted,
    /// `PHP_GLOB_NOMATCH` (-3): nothing matched and `GLOB_NOCHECK` unset.
    NoMatch,
    /// A generated path exceeded `PATH_MAX` (the C code returns 1).
    PathTooLong,
}

impl GlobError {
    /// The C return code of `php_glob()` for this error.
    pub fn code(self) -> i32 {
        match self {
            GlobError::NoSpace => -1,
            GlobError::Aborted => -2,
            GlobError::NoMatch => -3,
            GlobError::PathTooLong => 1,
        }
    }
}

/// Why PHP's `glob()` returns `false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobCallError {
    /// The pattern is `MAXPATHLEN` bytes or longer (warning, `false`).
    PatternTooLong,
    /// Flags outside [`GLOB_AVAILABLE_FLAGS`] (warning, `false`).
    InvalidFlags,
    /// The matcher failed (no warning, `false`).
    Failed(GlobError),
    /// Every match was rejected by the `open_basedir` check (no warning
    /// here — the caller's check emits none either — `false`).
    BasedirRestricted,
}

impl GlobCallError {
    /// The `E_WARNING` text PHP emits (as `glob(): <text>`), if any.
    pub fn warning(&self) -> Option<String> {
        match self {
            GlobCallError::PatternTooLong => Some(format!(
                "Pattern exceeds the maximum allowed length of {MAXPATHLEN} characters"
            )),
            GlobCallError::InvalidFlags => Some(
                "At least one of the passed flags is invalid or not supported on this platform"
                    .to_string(),
            ),
            GlobCallError::Failed(_) | GlobCallError::BasedirRestricted => None,
        }
    }
}

/// PHP's `glob(pattern, flags)` without `open_basedir` restrictions.
///
/// `Ok(vec![])` is PHP's empty array (including the "no match" case);
/// `Err` means PHP returns `false` (see [`GlobCallError::warning`]).
pub fn glob(pattern: &[u8], flags: i64) -> Result<Vec<Vec<u8>>, GlobCallError> {
    glob_with_basedir(pattern, flags, |_| true)
}

/// PHP's `glob(pattern, flags)` with an `open_basedir` filter.
///
/// `allowed(path)` is called for every match (PHP calls
/// `php_check_open_basedir_ex(path, 0)`, i.e. without warnings, and only when
/// `open_basedir` is set); rejected paths are dropped, and if that leaves no
/// results at all PHP returns `false` ([`GlobCallError::BasedirRestricted`]).
pub fn glob_with_basedir(
    pattern: &[u8],
    flags: i64,
    mut allowed: impl FnMut(&[u8]) -> bool,
) -> Result<Vec<Vec<u8>>, GlobCallError> {
    if pattern.len() >= MAXPATHLEN {
        return Err(GlobCallError::PatternTooLong);
    }
    if (i64::from(GLOB_AVAILABLE_FLAGS) & flags) != flags {
        return Err(GlobCallError::InvalidFlags);
    }
    // The check above guarantees `flags` fits in the low 31 bits.
    let flags = i32::try_from(flags).map_err(|_| GlobCallError::InvalidFlags)?;

    let paths = match php_glob(pattern, flags & GLOB_FLAGMASK) {
        Ok(paths) => paths,
        Err(GlobError::NoMatch) => return Ok(Vec::new()),
        Err(e) => return Err(GlobCallError::Failed(e)),
    };

    let mut basedir_limit = false;
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        if !allowed(&path) {
            basedir_limit = true;
            continue;
        }
        if flags & GLOB_ONLYDIR != 0 {
            match fs::metadata(as_path(&path)) {
                Ok(md) if md.is_dir() => {}
                _ => continue,
            }
        }
        out.push(path);
    }
    if basedir_limit && out.is_empty() {
        return Err(GlobCallError::BasedirRestricted);
    }
    Ok(out)
}

/// The low-level matcher, `php_glob(pattern, flags, NULL, &globbuf)`.
///
/// Returns `gl_pathv` on success. Note that with [`GLOB_BRACE`] a pattern
/// whose alternatives all fail to match returns `Ok(vec![])` rather than
/// [`GlobError::NoMatch`], exactly like the C code.
pub fn php_glob(pattern: &[u8], flags: i32) -> Result<Vec<Vec<u8>>, GlobError> {
    let pattern = pattern
        .iter()
        .position(|&b| b == 0)
        .map_or(pattern, |n| &pattern[..n]);
    if pattern.len() >= PATH_MAX {
        return Err(GlobError::NoMatch);
    }

    let mut patbuf: Vec<Char> = Vec::with_capacity(pattern.len());
    if flags & GLOB_NOESCAPE != 0 {
        patbuf.extend(pattern.iter().map(|&b| Char::from(b)));
    } else {
        // Protect the quoted characters.
        let mut i = 0;
        while i < pattern.len() {
            let c = pattern[i];
            i += 1;
            if c == b'\\' {
                let quoted = if i < pattern.len() {
                    i += 1;
                    pattern[i - 1]
                } else {
                    b'\\'
                };
                patbuf.push(Char::from(quoted) | M_PROTECT);
            } else {
                patbuf.push(Char::from(c));
            }
        }
    }

    let mut g = Globber {
        flags: flags & !GLOB_MAGCHAR,
        pathv: Vec::new(),
    };
    let rv = if flags & GLOB_BRACE != 0 {
        g.globexp1(&patbuf)
    } else {
        g.glob0(&patbuf)
    };
    match rv {
        0 => Ok(g.pathv),
        -1 => Err(GlobError::NoSpace),
        -2 => Err(GlobError::Aborted),
        -3 => Err(GlobError::NoMatch),
        _ => Err(GlobError::PathTooLong),
    }
}

type Char = u16;

const M_QUOTE: Char = 0x8000;
const M_PROTECT: Char = 0x4000;
const M_ASCII: Char = 0x00ff;

const fn meta(c: u8) -> Char {
    c as Char | M_QUOTE
}
const fn char_of(c: Char) -> Char {
    c & M_ASCII
}
const fn ismeta(c: Char) -> bool {
    c & M_QUOTE != 0
}

const M_ALL: Char = meta(b'*');
const M_END: Char = meta(b']');
const M_NOT: Char = meta(b'!');
const M_ONE: Char = meta(b'?');
const M_RNG: Char = meta(b'-');
const M_SET: Char = meta(b'[');
const M_CLASS: Char = meta(b':');

const SEP: Char = b'/' as Char;
const LBRACE: Char = b'{' as Char;
const RBRACE: Char = b'}' as Char;
const LBRACKET: Char = b'[' as Char;
const RBRACKET: Char = b']' as Char;
const COMMA: Char = b',' as Char;

/// A character class predicate.
type ClassFn = fn(u8) -> bool;

/// C-locale character classes in `charclass.h` order (the index is stored
/// in the compiled pattern).
const CCLASSES: [(&[u8], ClassFn); 12] = [
    (b"alnum", |c| c.is_ascii_alphanumeric()),
    (b"alpha", |c| c.is_ascii_alphabetic()),
    (b"blank", |c| c == b' ' || c == b'\t'),
    (b"cntrl", |c| c.is_ascii_control()),
    (b"digit", |c| c.is_ascii_digit()),
    (b"graph", |c| c.is_ascii_graphic()),
    (b"lower", |c| c.is_ascii_lowercase()),
    (b"print", |c| c == b' ' || c.is_ascii_graphic()),
    (b"punct", |c| c.is_ascii_punctuation()),
    (b"space", |c| {
        matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
    }),
    (b"upper", |c| c.is_ascii_uppercase()),
    (b"xdigit", |c| c.is_ascii_hexdigit()),
];

/// Element `i` of a NUL-terminated Char string (0 past the end).
fn at(s: &[Char], i: usize) -> Char {
    s.get(i).copied().unwrap_or(0)
}

/// `g_strchr()`: index of the first `ch` at or after `from`.
fn g_strchr(s: &[Char], from: usize, ch: Char) -> Option<usize> {
    s.get(from..)?
        .iter()
        .position(|&c| c == ch)
        .map(|n| from + n)
}

/// `g_Ctoc()`: Chars back to bytes (truncating to the low 8 bits).
fn ctoc(s: &[Char]) -> Vec<u8> {
    s.iter()
        .take_while(|&&c| c != 0)
        // Truncation to `char` is the C behaviour; it drops M_PROTECT.
        .map(|&c| (c & 0xff) as u8)
        .collect()
}

fn as_path(bytes: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(bytes))
}

struct Globber {
    flags: i32,
    pathv: Vec<Vec<u8>>,
}

impl Globber {
    /// Expands a `{}` pattern recursively, then globs.
    fn globexp1(&mut self, pattern: &[Char]) -> i32 {
        // Protect a single {}, for find(1), like csh.
        if pattern == [LBRACE, RBRACE] {
            return self.glob0(pattern);
        }
        if let Some(ptr) = g_strchr(pattern, 0, LBRACE) {
            return self.globexp2(ptr, pattern);
        }
        self.glob0(pattern)
    }

    /// Tries to expand the brace at `ptr`.
    fn globexp2(&mut self, ptr: usize, pattern: &[Char]) -> i32 {
        // Copy part up to the brace.
        let mut patbuf: Vec<Char> = pattern[..ptr].to_vec();
        let ls = patbuf.len();

        // Find the balanced brace.
        let ptr = ptr + 1;
        let mut depth = 0i32;
        let mut pe = ptr;
        while at(pattern, pe) != 0 {
            let c = pattern[pe];
            if c == LBRACKET {
                // Ignore everything between [].
                let pm = pe;
                pe += 1;
                while at(pattern, pe) != RBRACKET && at(pattern, pe) != 0 {
                    pe += 1;
                }
                if at(pattern, pe) == 0 {
                    // No matching RBRACKET: just look for RBRACE.
                    pe = pm;
                }
            } else if c == LBRACE {
                depth += 1;
            } else if c == RBRACE {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            pe += 1;
        }

        // Non matching braces; just glob the pattern (prefix only, as in C).
        if depth != 0 || at(pattern, pe) == 0 {
            return self.glob0(&patbuf);
        }

        let mut depth = 0i32;
        let mut pl = ptr;
        let mut pm = ptr;
        while pm <= pe {
            match at(pattern, pm) {
                LBRACKET => {
                    let pb = pm;
                    pm += 1;
                    while at(pattern, pm) != RBRACKET && at(pattern, pm) != 0 {
                        pm += 1;
                    }
                    if at(pattern, pm) == 0 {
                        pm = pb;
                    }
                }
                LBRACE => depth += 1,
                RBRACE if depth > 0 => depth -= 1,
                COMMA if depth > 0 => {}
                RBRACE | COMMA => {
                    // Append the current alternative and the rest of the
                    // pattern after the closing brace.
                    patbuf.truncate(ls);
                    patbuf.extend_from_slice(&pattern[pl..pm]);
                    patbuf.extend(pattern[pe + 1..].iter().take_while(|&&c| c != 0));
                    let rv = self.globexp1(&patbuf);
                    if rv != 0 && rv != GlobError::NoMatch.code() {
                        return rv;
                    }
                    pl = pm + 1;
                }
                _ => {}
            }
            pm += 1;
        }
        0
    }

    /// Parses a `[:class:]` at `*pi` (pointing at the `:`). Returns 1 if it
    /// is not a class, -1 for an unknown class, 0 on success.
    fn charclass(pattern: &[Char], pi: &mut usize, out: &mut Vec<Char>) -> i32 {
        let start = *pi + 1;
        let Some(colon) = g_strchr(pattern, start, Char::from(b':')) else {
            return 1;
        };
        if at(pattern, colon + 1) != RBRACKET {
            return 1;
        }
        let name = &pattern[start..colon];
        let Some(idx) = CCLASSES.iter().position(|(cname, _)| {
            cname.len() == name.len() && cname.iter().zip(name).all(|(&a, &b)| Char::from(a) == b)
        }) else {
            return -1;
        };
        out.push(M_CLASS);
        // CCLASSES has 12 entries, so the index always fits.
        out.push(Char::try_from(idx).unwrap_or(0));
        *pi += name.len() + 3;
        0
    }

    /// Compiles the pattern, globs it and sorts the new matches.
    #[allow(clippy::too_many_lines)]
    fn glob0(&mut self, pattern: &[Char]) -> i32 {
        let oldpathc = self.pathv.len();
        let mut buf: Vec<Char> = Vec::with_capacity(pattern.len() + 1);
        let mut q = 0usize;
        loop {
            let c = at(pattern, q);
            q += 1;
            if c == 0 {
                break;
            }
            match c {
                LBRACKET => {
                    let mut c = at(pattern, q);
                    // Like fnmatch(), treat a leading '^' as '!'.
                    if c == Char::from(b'^') {
                        c = Char::from(b'!');
                    }
                    let not = c == Char::from(b'!');
                    if not {
                        q += 1;
                    }
                    if at(pattern, q) == 0 || g_strchr(pattern, q + 1, RBRACKET).is_none() {
                        buf.push(LBRACKET);
                        if not {
                            q -= 1;
                        }
                        continue;
                    }
                    buf.push(M_SET);
                    if not {
                        buf.push(M_NOT);
                    }
                    let mut c = at(pattern, q);
                    q += 1;
                    loop {
                        if c == LBRACKET && at(pattern, q) == Char::from(b':') {
                            let mut err;
                            loop {
                                err = Self::charclass(pattern, &mut q, &mut buf);
                                if err != 0 {
                                    break;
                                }
                                c = at(pattern, q);
                                q += 1;
                                if !(c == LBRACKET && at(pattern, q) == Char::from(b':')) {
                                    break;
                                }
                            }
                            if err == -1 && self.flags & GLOB_NOCHECK == 0 {
                                return GlobError::NoMatch.code();
                            }
                            if c == RBRACKET {
                                break;
                            }
                        }
                        buf.push(char_of(c));
                        if at(pattern, q) == Char::from(b'-') {
                            let end = at(pattern, q + 1);
                            if end != RBRACKET {
                                buf.push(M_RNG);
                                buf.push(char_of(end));
                                q += 2;
                            }
                        }
                        c = at(pattern, q);
                        q += 1;
                        if c == RBRACKET {
                            break;
                        }
                    }
                    self.flags |= GLOB_MAGCHAR;
                    buf.push(M_END);
                }
                c if c == Char::from(b'?') => {
                    self.flags |= GLOB_MAGCHAR;
                    buf.push(M_ONE);
                }
                c if c == Char::from(b'*') => {
                    self.flags |= GLOB_MAGCHAR;
                    // Collapse adjacent stars to one.
                    if buf.last() != Some(&M_ALL) {
                        buf.push(M_ALL);
                    }
                }
                c => buf.push(char_of(c)),
            }
        }

        let err = self.glob1(&buf);
        if err != 0 {
            return err;
        }

        if self.pathv.len() == oldpathc {
            if self.flags & GLOB_NOCHECK != 0
                || (self.flags & GLOB_NOMAGIC != 0 && self.flags & GLOB_MAGCHAR == 0)
            {
                self.pathv.push(ctoc(pattern));
                return 0;
            }
            return GlobError::NoMatch.code();
        }
        if self.flags & GLOB_NOSORT == 0 {
            self.pathv[oldpathc..].sort_unstable();
        }
        0
    }

    fn glob1(&mut self, pattern: &[Char]) -> i32 {
        // A null pathname is invalid -- POSIX 1003.1 sect. 2.4.
        if at(pattern, 0) == 0 {
            return 0;
        }
        let mut pathbuf: Vec<Char> = Vec::with_capacity(PATH_MAX);
        self.glob2(&mut pathbuf, pattern, 0)
    }

    /// Walks pattern segments without wildcards, recursing via `glob3` for
    /// the first segment that has some.
    fn glob2(&mut self, pathbuf: &mut Vec<Char>, pattern: &[Char], mut pi: usize) -> i32 {
        let limit = PATH_MAX - 1;
        let mut anymeta = false;
        loop {
            if at(pattern, pi) == 0 {
                // End of pattern: the path must exist.
                let path = ctoc(pathbuf);
                let Ok(md) = fs::symlink_metadata(as_path(&path)) else {
                    return 0;
                };
                if self.flags & GLOB_MARK != 0
                    && pathbuf.last() != Some(&SEP)
                    && (md.is_dir()
                        || (md.file_type().is_symlink()
                            && fs::metadata(as_path(&path)).is_ok_and(|m| m.is_dir())))
                {
                    if pathbuf.len() + 1 > limit {
                        return 1;
                    }
                    pathbuf.push(SEP);
                }
                self.pathv.push(ctoc(pathbuf));
                return 0;
            }

            // Find end of next segment, copy tentatively to the path.
            let seg_start = pathbuf.len();
            let mut p = pi;
            while at(pattern, p) != 0 && at(pattern, p) != SEP {
                if ismeta(pattern[p]) {
                    anymeta = true;
                }
                if pathbuf.len() + 1 > limit {
                    return 1;
                }
                pathbuf.push(pattern[p]);
                p += 1;
            }

            if anymeta {
                pathbuf.truncate(seg_start);
                return self.glob3(pathbuf, pattern, pi, p);
            }
            // No expansion, do next segment.
            pi = p;
            while at(pattern, pi) == SEP {
                if pathbuf.len() + 1 > limit {
                    return 1;
                }
                pathbuf.push(SEP);
                pi += 1;
            }
        }
    }

    /// Reads the directory in `pathbuf` and matches its entries against the
    /// segment `pattern[pi..rest]`.
    fn glob3(&mut self, pathbuf: &mut Vec<Char>, pattern: &[Char], pi: usize, rest: usize) -> i32 {
        let limit = PATH_MAX - 1;
        if pathbuf.len() > limit {
            return 1;
        }
        let dir = if pathbuf.is_empty() {
            b".".to_vec()
        } else {
            ctoc(pathbuf)
        };
        // PHP passes no error callback, so open failures are ignored
        // (even with GLOB_ERR).
        let Ok(entries) = fs::read_dir(as_path(&dir)) else {
            return 0;
        };
        let names = [b".".to_vec(), b"..".to_vec()].into_iter().chain(
            entries
                .map_while(Result::ok)
                .map(|e| e.file_name().as_bytes().to_vec()),
        );

        let segment = &pattern[pi..rest];
        let pathend = pathbuf.len();
        let mut err = 0;
        for name in names {
            // Initial DOT must be matched literally.
            if name.first() == Some(&b'.') && at(pattern, pi) != Char::from(b'.') {
                continue;
            }
            if pathend + name.len() + 1 >= limit {
                err = 1;
                break;
            }
            pathbuf.extend(name.iter().map(|&b| Char::from(b)));
            if !glob_match(&pathbuf[pathend..], segment) {
                pathbuf.truncate(pathend);
                continue;
            }
            err = self.glob2(pathbuf, pattern, rest);
            pathbuf.truncate(pathend);
            if err != 0 {
                break;
            }
        }
        err
    }
}

/// `match()`: the non-recursive (Russ Cox) wildcard matcher.
fn glob_match(name: &[Char], pat: &[Char]) -> bool {
    let mut n = 0usize;
    let mut p = 0usize;
    let mut next: Option<(usize, usize)> = None;
    let name_at = |i: usize| at(name, i);

    'outer: loop {
        let mut failed = false;
        while p < pat.len() {
            let c = pat[p];
            p += 1;
            match c {
                M_ALL => {
                    while p < pat.len() && pat[p] == M_ALL {
                        p += 1;
                    }
                    if p == pat.len() {
                        return true;
                    }
                    if name_at(n) == 0 {
                        return false;
                    }
                    next = Some((n + 1, p - 1));
                }
                M_ONE => {
                    let k = name_at(n);
                    n += 1;
                    if k == 0 {
                        failed = true;
                        break;
                    }
                }
                M_SET => {
                    let mut ok = false;
                    let k = name_at(n);
                    n += 1;
                    if k == 0 {
                        failed = true;
                        break;
                    }
                    let negate = at(pat, p) == M_NOT;
                    if negate {
                        p += 1;
                    }
                    loop {
                        let c = at(pat, p);
                        p += 1;
                        if c == M_END {
                            break;
                        }
                        if c == M_CLASS {
                            let idx = usize::from(at(pat, p));
                            if let Some((_, class)) = CCLASSES.get(idx) {
                                // Names hold bytes, so `k` fits in a u8.
                                if class((k & 0xff) as u8) {
                                    ok = true;
                                }
                            }
                            p += 1;
                        }
                        if at(pat, p) == M_RNG {
                            if c <= k && k <= at(pat, p + 1) {
                                ok = true;
                            }
                            p += 2;
                        } else if c == k {
                            ok = true;
                        }
                        if p > pat.len() {
                            break;
                        }
                    }
                    if ok == negate {
                        failed = true;
                        break;
                    }
                }
                c => {
                    let k = name_at(n);
                    n += 1;
                    if k != c {
                        failed = true;
                        break;
                    }
                }
            }
        }
        if !failed && name_at(n) == 0 {
            return true;
        }
        // fail:
        if let Some((nn, np)) = next {
            n = nn;
            p = np;
            continue 'outer;
        }
        return false;
    }
}

#[cfg(test)]
mod tests;
