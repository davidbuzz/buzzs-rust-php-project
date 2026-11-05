//! Script input/output filters used by Zend's multibyte scanner.

use php_mbstring::convert::{fast_convert, ErrorMode};
use php_mbstring::detect::guess_encoding_for_strings;
use php_mbstring::encoding::{get, utf8, Encoding, EncodingNo, ENCTYPE_GL_UNSAFE};
use std::ops::Deref;
use std::sync::Arc;

/// Encoding configuration supplied by the host when `zend.multibyte` is on.
///
/// Resolve INI encoding names with `php_mbstring::encoding::by_name`; invalid
/// INI values and their startup diagnostics remain the host's responsibility.
#[derive(Clone, Copy, Debug)]
pub struct ScriptEncodingOptions<'a> {
    /// `zend.script_encoding`, in configured order; empty means no fallback.
    pub script_encodings: &'a [&'static Encoding],
    /// Effective mbstring internal encoding; `None` preserves script encoding.
    pub internal_encoding: Option<&'static Encoding>,
    /// `zend.detect_unicode`: BOM and NUL-byte detection precede the fallback.
    pub detect_unicode: bool,
    /// Effective `mb_substitute_character()` replacement code point.
    pub replacement_char: u32,
    /// Effective mbstring conversion error mode.
    pub error_mode: ErrorMode,
}

impl Default for ScriptEncodingOptions<'_> {
    fn default() -> Self {
        Self {
            script_encodings: &[],
            internal_encoding: Some(utf8()),
            detect_unicode: true,
            replacement_char: 0x3F,
            error_mode: ErrorMode::Char,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Input<'a> {
    Borrowed(&'a [u8]),
    Filtered(Arc<[u8]>),
}

impl Deref for Input<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Borrowed(src) => src,
            Self::Filtered(src) => src,
        }
    }
}

type Filter = (&'static Encoding, &'static Encoding);

#[derive(Clone, Debug)]
pub(crate) struct ScriptEncoding<'a> {
    original: &'a [u8],
    internal: Option<&'static Encoding>,
    input: Option<Filter>,
    output: Option<Filter>,
    replacement_char: u32,
    error_mode: ErrorMode,
}

impl<'a> ScriptEncoding<'a> {
    pub fn new(src: &'a [u8], opts: &ScriptEncodingOptions<'_>) -> (Self, Input<'a>) {
        let (original, detected) = if opts.detect_unicode {
            detect_unicode(src)
        } else {
            (src, None)
        };
        let selected = detected.or_else(|| {
            guess_encoding_for_strings(&[original], opts.script_encodings, false, false)
        });
        let mut state = Self {
            original,
            internal: opts.internal_encoding,
            input: None,
            output: None,
            replacement_char: opts.replacement_char,
            error_mode: opts.error_mode,
        };
        let mut input = Input::Borrowed(original);
        if let Some(encoding) = selected {
            if let Some(filtered) = state.set_encoding(encoding) {
                input = filtered;
            }
        }
        (state, input)
    }

    pub fn set_encoding(&mut self, script: &'static Encoding) -> Option<Input<'a>> {
        let compatible = |e: &Encoding| e.flag & ENCTYPE_GL_UNSAFE == 0;
        let internal = self.internal.filter(|&internal| internal != script);
        let (input, output) = match internal {
            None => {
                if compatible(script) {
                    (None, None)
                } else {
                    (Some((script, utf8())), Some((utf8(), script)))
                }
            }
            Some(internal) if compatible(internal) => (Some((script, internal)), None),
            Some(internal) if compatible(script) => (None, Some((script, internal))),
            Some(internal) => (Some((script, utf8())), Some((utf8(), internal))),
        };
        self.output = output;
        if self.input == input {
            return None;
        }
        self.input = input;
        Some(match input {
            None => Input::Borrowed(self.original),
            Some(filter) => Input::Filtered(self.convert(self.original, filter).into()),
        })
    }

    fn convert(&self, bytes: &[u8], (from, to): Filter) -> Vec<u8> {
        fast_convert(bytes, from, to, self.replacement_char, self.error_mode).0
    }

    pub fn output(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        self.output.map(|filter| self.convert(bytes, filter))
    }

    pub fn original_offset(&self, cursor: usize) -> usize {
        let Some(filter) = self.input else {
            return cursor;
        };
        let mut offset = cursor.min(self.original.len());
        let mut previous_direction = None;
        loop {
            let length = self.convert(&self.original[..offset], filter).len();
            if length == cursor {
                return offset;
            }
            let decrease = length > cursor;
            if previous_direction.is_some_and(|previous| previous != decrease) {
                return usize::MAX;
            }
            previous_direction = Some(decrease);
            if decrease {
                let Some(next) = offset.checked_sub(1) else {
                    return usize::MAX;
                };
                offset = next;
            } else if offset < self.original.len() {
                offset += 1;
            } else {
                return usize::MAX;
            }
        }
    }
}

fn detect_unicode(src: &[u8]) -> (&[u8], Option<&'static Encoding>) {
    // Zend only attempts Unicode detection when at least four bytes exist.
    if src.len() < 4 {
        return (src, None);
    }
    for (bom, encoding) in [
        (&b"\0\0\xFE\xFF"[..], EncodingNo::Utf32be),
        (&b"\xFF\xFE\0\0"[..], EncodingNo::Utf32le),
        (&b"\xFE\xFF"[..], EncodingNo::Utf16be),
        (&b"\xFF\xFE"[..], EncodingNo::Utf16le),
        (&b"\xEF\xBB\xBF"[..], EncodingNo::Utf8),
    ] {
        if src.starts_with(bom) {
            return (&src[bom.len()..], Some(get(encoding)));
        }
    }
    let Some(nul) = src.iter().position(|&b| b == 0) else {
        return (src, None);
    };
    if halt_before_nul(&src[..nul]) {
        return (src, None);
    }
    let mut width = 2;
    let mut offset = 0;
    while src.len().saturating_sub(offset) > 2 {
        let Some(n) = src[offset..src.len() - 2].iter().position(|&b| b == 0) else {
            break;
        };
        let p = offset + n;
        if src[p + 1] == 0 && src[p + 2] == 0 {
            width = 4;
            break;
        }
        offset = p + 4;
    }
    let little_endian = src
        .chunks_exact(width)
        .find_map(|chunk| {
            if chunk[0] == 0 && chunk[width - 1] != 0 {
                Some(false)
            } else if chunk[0] != 0 && chunk[width - 1] == 0 {
                Some(true)
            } else {
                None
            }
        })
        .unwrap_or(false);
    let encoding = match (width, little_endian) {
        (4, true) => EncodingNo::Utf32le,
        (4, false) => EncodingNo::Utf32be,
        (_, true) => EncodingNo::Utf16le,
        (_, false) => EncodingNo::Utf16be,
    };
    (src, Some(get(encoding)))
}

fn halt_before_nul(prefix: &[u8]) -> bool {
    const HALT: &[u8] = b"__halt_compiler";
    for start in 0..prefix.len().saturating_sub(HALT.len() + 2) {
        if !prefix[start..start + HALT.len()].eq_ignore_ascii_case(HALT) {
            continue;
        }
        let mut rest = &prefix[start + HALT.len()..];
        let mut matched = true;
        for punctuation in b"();" {
            while rest
                .first()
                .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
            {
                rest = &rest[1..];
            }
            if rest.first() != Some(punctuation) {
                matched = false;
                break;
            }
            rest = &rest[1..];
        }
        if matched {
            return true;
        }
    }
    false
}
