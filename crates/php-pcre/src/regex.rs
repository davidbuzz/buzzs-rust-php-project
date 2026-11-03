//! Pattern parsing (`/body/flags`) and compiled regular expressions
//! (`pcre_get_compiled_regex_cache_ex` in `php_pcre.c`).

use std::sync::{Arc, OnceLock};

use crate::error::CompileError;
use crate::ffi::{self, Code, Tables};

/// Options PHP derives from a pattern string, before calling PCRE2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPattern<'a> {
    /// The pattern body between the delimiters (escaped delimiters keep their backslash).
    pub body: &'a [u8],
    /// `pcre2_compile()` options derived from the modifiers.
    pub compile_options: u32,
    /// `pcre2_set_compile_extra_options()` value derived from the modifiers.
    pub extra_options: u32,
}

/// Whether the linked PCRE2 knows `PCRE2_EXTRA_CASELESS_RESTRICT` (10.43+),
/// which is what makes PHP accept the `r` modifier.
fn supports_caseless_restrict() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        let (major, minor) = crate::version_numbers();
        (major, minor) >= (10, 43)
    })
}

/// C-locale `isspace()`.
fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// Splits `/body/modifiers` the way PHP does and maps the modifiers to
/// PCRE2 options, producing PHP's exact warnings on malformed input.
pub fn parse_pattern(regex: &[u8]) -> Result<ParsedPattern<'_>, CompileError> {
    let end = regex.len();
    let mut p = 0;
    while p < end && is_c_space(regex[p]) {
        p += 1;
    }
    if p >= end {
        return Err(CompileError::EmptyRegex);
    }
    let start_delimiter = regex[p];
    p += 1;
    if start_delimiter.is_ascii_alphanumeric() || start_delimiter == b'\\' || start_delimiter == 0 {
        return Err(CompileError::BadDelimiter);
    }
    let end_delimiter = match start_delimiter {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        b'<' => b'>',
        other => other,
    };

    let mut pp = p;
    if start_delimiter == end_delimiter {
        while pp < end {
            if regex[pp] == b'\\' && pp + 1 < end {
                pp += 1;
            } else if regex[pp] == end_delimiter {
                break;
            }
            pp += 1;
        }
    } else {
        let mut brackets = 1i32;
        while pp < end {
            if regex[pp] == b'\\' && pp + 1 < end {
                pp += 1;
            } else if regex[pp] == end_delimiter {
                brackets -= 1;
                if brackets <= 0 {
                    break;
                }
            } else if regex[pp] == start_delimiter {
                brackets += 1;
            }
            pp += 1;
        }
    }
    if pp >= end {
        return Err(if start_delimiter == end_delimiter {
            CompileError::NoEndingDelimiter(end_delimiter)
        } else {
            CompileError::NoEndingMatchingDelimiter(end_delimiter)
        });
    }
    let body = &regex[p..pp];

    let mut compile_options = 0u32;
    let mut extra_options = 0u32;
    for &m in &regex[pp + 1..] {
        match m {
            b'i' => compile_options |= ffi::PCRE2_CASELESS,
            b'm' => compile_options |= ffi::PCRE2_MULTILINE,
            b'n' => compile_options |= ffi::PCRE2_NO_AUTO_CAPTURE,
            b's' => compile_options |= ffi::PCRE2_DOTALL,
            b'x' => compile_options |= ffi::PCRE2_EXTENDED,
            b'A' => compile_options |= ffi::PCRE2_ANCHORED,
            b'D' => compile_options |= ffi::PCRE2_DOLLAR_ENDONLY,
            b'r' if supports_caseless_restrict() => {
                extra_options |= ffi::PCRE2_EXTRA_CASELESS_RESTRICT;
            }
            b'S' | b'X' | b' ' | b'\n' | b'\r' => {}
            b'U' => compile_options |= ffi::PCRE2_UNGREEDY,
            b'u' => {
                compile_options |= ffi::PCRE2_UTF | ffi::PCRE2_UCP | ffi::PCRE2_NEVER_BACKSLASH_C;
            }
            b'J' => compile_options |= ffi::PCRE2_DUPNAMES,
            0 => return Err(CompileError::NulModifier),
            other => return Err(CompileError::UnknownModifier(other)),
        }
    }
    Ok(ParsedPattern {
        body,
        compile_options,
        extra_options,
    })
}

/// Outcome of the JIT compilation step PHP runs after a successful compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JitOutcome {
    /// JIT not requested (`pcre.jit=0`).
    Disabled,
    /// JIT code is attached (or compilation succeeded with no code).
    Ok,
    /// `PCRE2_ERROR_NOMEMORY`: PHP warns and turns `pcre.jit` off.
    NoMemory,
    /// Any other JIT failure: PHP warns `JIT compilation failed: …`.
    Failed(i32),
}

#[derive(Debug)]
struct Inner {
    // Field order matters: `code` must drop before `tables`, which it references.
    code: Code,
    _tables: Option<Arc<Tables>>,
    compile_options: u32,
    jit: bool,
    capture_count: u32,
    name_count: u32,
    /// Group number → name (length `capture_count + 1`).
    group_names: Vec<Option<Arc<[u8]>>>,
    /// The name table in PCRE2 order, `(number, name)`.
    name_table: Vec<(u16, Arc<[u8]>)>,
}

/// A compiled PHP regular expression (a `pcre_cache_entry`).
///
/// Cloning is cheap (reference counted); a clone is what the cache hands out,
/// and "in use" entries are protected from cache eviction exactly like PHP's
/// `refcount`.
#[derive(Debug, Clone)]
pub struct Regex {
    inner: Arc<Inner>,
}

impl Regex {
    /// Parses and compiles a full PHP pattern string (`"/body/flags"`) without
    /// the cache, with JIT enabled.  Prefer [`crate::Pcre::compile`] in the VM.
    pub fn compile(pattern: &[u8]) -> Result<Self, CompileError> {
        Self::compile_with(pattern, true, None).map(|(re, _)| re)
    }

    /// Compiles with explicit JIT and character-table choices; also reports how
    /// JIT compilation went so the caller can emit PHP's warnings.
    pub(crate) fn compile_with(
        pattern: &[u8],
        jit: bool,
        tables: Option<Arc<Tables>>,
    ) -> Result<(Self, JitOutcome), CompileError> {
        let parsed = parse_pattern(pattern)?;
        let mut code = Code::compile(
            parsed.body,
            parsed.compile_options,
            parsed.extra_options,
            tables.as_deref(),
        )
        .map_err(|e| {
            let message =
                if u32::try_from(e.code) == Ok(ffi::PCRE2_ERROR_BACKSLASH_C_CALLER_DISABLED) {
                    b"using \\C is incompatible with the 'u' modifier".to_vec()
                } else {
                    ffi::error_message(e.code)
                };
            CompileError::Compilation {
                message,
                offset: e.offset,
            }
        })?;

        let mut has_jit = false;
        let jit_outcome = if jit {
            let rc = code.jit_compile();
            if rc >= 0 {
                has_jit = code.jit_size() > 0;
                JitOutcome::Ok
            } else if rc == ffi::PCRE2_ERROR_NOMEMORY {
                JitOutcome::NoMemory
            } else {
                JitOutcome::Failed(rc)
            }
        } else {
            JitOutcome::Disabled
        };

        let capture_count = code.capture_count().map_err(CompileError::PatternInfo)?;
        let name_count = code.name_count().map_err(CompileError::PatternInfo)?;
        let raw_table = code.name_table().map_err(CompileError::PatternInfo)?;
        let mut group_names = vec![None; capture_count as usize + 1];
        let mut name_table = Vec::with_capacity(raw_table.len());
        for (number, name) in raw_table {
            let name: Arc<[u8]> = Arc::from(name);
            if let Some(slot) = group_names.get_mut(usize::from(number)) {
                *slot = Some(Arc::clone(&name));
            }
            name_table.push((number, name));
        }

        Ok((
            Self {
                inner: Arc::new(Inner {
                    code,
                    _tables: tables,
                    compile_options: parsed.compile_options,
                    jit: has_jit,
                    capture_count,
                    name_count,
                    group_names,
                    name_table,
                }),
            },
            jit_outcome,
        ))
    }

    /// Number of capturing groups (`PCRE2_INFO_CAPTURECOUNT`).
    pub fn capture_count(&self) -> usize {
        self.inner.capture_count as usize
    }

    /// Number of named groups (`PCRE2_INFO_NAMECOUNT`).
    pub fn name_count(&self) -> usize {
        self.inner.name_count as usize
    }

    /// Group number → name table PHP builds (`make_subpats_table`); index 0 is
    /// the whole match and is always `None`.  In result arrays a named group
    /// produces its name key immediately *before* its numeric key.
    pub fn group_names(&self) -> &[Option<Arc<[u8]>>] {
        &self.inner.group_names
    }

    /// The raw PCRE2 name table, `(group number, name)` in PCRE2's order.
    pub fn name_table(&self) -> &[(u16, Arc<[u8]>)] {
        &self.inner.name_table
    }

    /// The `pcre2_compile()` options PHP derived from the modifiers.
    pub fn compile_options(&self) -> u32 {
        self.inner.compile_options
    }

    /// Whether the `u` modifier (UTF-8 mode) is on.
    pub fn is_utf(&self) -> bool {
        self.inner.compile_options & ffi::PCRE2_UTF != 0
    }

    /// Whether JIT code is attached (`PREG_JIT` in `preg_options`).
    pub fn is_jit(&self) -> bool {
        self.inner.jit
    }

    /// Whether two handles refer to the same compiled pattern (cache identity).
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Arc::ptr_eq(&a.inner, &b.inner)
    }

    /// True when another clone of this regex is alive (PHP's `refcount > 0`).
    pub(crate) fn in_use(&self) -> bool {
        Arc::strong_count(&self.inner) > 1
    }

    pub(crate) fn code(&self) -> &Code {
        &self.inner.code
    }

    /// Byte length of the character starting at `pos` (PHP's
    /// `calculate_unit_length`): 1 outside UTF mode, else the lead byte plus
    /// its continuation bytes.
    pub(crate) fn unit_length(&self, subject: &[u8], pos: usize) -> usize {
        if !self.is_utf() {
            return 1;
        }
        let mut end = pos + 1;
        while end < subject.len() && subject[end] & 0xC0 == 0x80 {
            end += 1;
        }
        end - pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(p: &[u8]) -> String {
        parse_pattern(p).unwrap_err().to_string()
    }

    #[test]
    fn delimiter_errors() {
        assert_eq!(err(b""), "Empty regular expression");
        assert_eq!(err(b"      "), "Empty regular expression");
        assert_eq!(
            err(b"12"),
            "Delimiter must not be alphanumeric, backslash, or NUL byte"
        );
        assert_eq!(
            err(b"\\a\\"),
            "Delimiter must not be alphanumeric, backslash, or NUL byte"
        );
        assert_eq!(
            err(b"\0\0"),
            "Delimiter must not be alphanumeric, backslash, or NUL byte"
        );
        assert_eq!(err(b"~a"), "No ending delimiter '~' found");
        assert_eq!(err(b"{"), "No ending matching delimiter '}' found");
        assert_eq!(err(b"(a(b)"), "No ending matching delimiter ')' found");
        assert_eq!(err(b"//z"), "Unknown modifier 'z'");
        assert_eq!(err(b"/a/e"), "Unknown modifier 'e'");
        assert_eq!(err(b"/a/\0"), "NUL byte is not a valid modifier");
        assert_eq!(err(b"/a\\/"), "No ending delimiter '/' found");
    }

    #[test]
    fn bodies_and_options() {
        let p = parse_pattern(b"  @\\@\\@@").unwrap();
        assert_eq!(p.body, b"\\@\\@");
        let p = parse_pattern(b"{a{1,2}b}i").unwrap();
        assert_eq!(p.body, b"a{1,2}b");
        assert_eq!(p.compile_options, ffi::PCRE2_CASELESS);
        let p = parse_pattern(b"<>").unwrap();
        assert_eq!(p.body, b"");
        let p = parse_pattern(b"/x/imsxADSUXJun \n\r").unwrap();
        let expected = ffi::PCRE2_CASELESS
            | ffi::PCRE2_MULTILINE
            | ffi::PCRE2_DOTALL
            | ffi::PCRE2_EXTENDED
            | ffi::PCRE2_ANCHORED
            | ffi::PCRE2_DOLLAR_ENDONLY
            | ffi::PCRE2_UNGREEDY
            | ffi::PCRE2_DUPNAMES
            | ffi::PCRE2_UTF
            | ffi::PCRE2_UCP
            | ffi::PCRE2_NEVER_BACKSLASH_C
            | ffi::PCRE2_NO_AUTO_CAPTURE;
        assert_eq!(p.compile_options, expected);
        // A trailing backslash right before the end is not an escape.
        assert_eq!(
            parse_pattern(b"/a\\").unwrap_err(),
            CompileError::NoEndingDelimiter(b'/')
        );
    }

    #[test]
    fn compile_reports_pcre_errors() {
        let e = Regex::compile(b"/(/").unwrap_err();
        assert_eq!(
            e.to_string(),
            "Compilation failed: missing closing parenthesis at offset 1"
        );
        let e = Regex::compile(b"/(.*)\\C/u").unwrap_err();
        assert_eq!(
            e.to_string(),
            "Compilation failed: using \\C is incompatible with the 'u' modifier at offset 6"
        );
    }

    #[test]
    fn group_tables() {
        let re = Regex::compile(b"/(?<b>x)(y)(?<a>z)/").unwrap();
        assert_eq!(re.capture_count(), 3);
        assert_eq!(re.name_count(), 2);
        let names: Vec<_> = re
            .group_names()
            .iter()
            .map(|n| n.as_deref().map(<[u8]>::to_vec))
            .collect();
        assert_eq!(
            names,
            vec![None, Some(b"b".to_vec()), None, Some(b"a".to_vec())]
        );
        assert_eq!(re.name_table()[0].0, 3);
        assert!(!re.is_utf());
        assert!(Regex::compile(b"/./u").unwrap().is_utf());
    }

    #[test]
    fn unit_length() {
        let re = Regex::compile(b"/x/u").unwrap();
        let s = "aé€😀".as_bytes();
        assert_eq!(re.unit_length(s, 0), 1);
        assert_eq!(re.unit_length(s, 1), 2);
        assert_eq!(re.unit_length(s, 3), 3);
        assert_eq!(re.unit_length(s, 6), 4);
        let re = Regex::compile(b"/x/").unwrap();
        assert_eq!(re.unit_length(s, 1), 1);
    }
}
