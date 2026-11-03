//! A model of the Zend memory limit for URI operations whose allocations
//! the input controls.
//!
//! The VM does not account memory, so the "Allowed memory size of N bytes
//! exhausted" fatal errors of the `oom_*` tests are reproduced from the
//! memory the C implementation would need: uriparser keeps one 32-byte
//! `UriPathSegment` per path segment (and normalization works on a full
//! copy), lexbor's IDNA processing works on code points, and every WHATWG
//! validation error keeps a copy of the rest of the input as its `context`.
//!
//! The check also protects the process: `php-uri` materializes those
//! contexts eagerly, which is quadratic in the input length, so inputs
//! that would exceed the limit are rejected before parsing.

use php_vm::ext::VmResult;
use php_vm::Vm;

/// Memory a fresh script already uses.
const BASELINE_USAGE: u64 = 512 * 1024;
/// `sizeof(UriPathSegmentA)`.
const SEGMENT_SIZE: u64 = 32;
/// A `UrlValidationError` object with its three properties.
const VALIDATION_ERROR_SIZE: u64 = 128;
/// Bytes per input byte of IDNA work on non-ASCII text (UTF-32 code
/// points, mapped and normalized copies).
const IDNA_FACTOR: u64 = 8;

/// The effective `memory_limit` in bytes (`None` = unlimited).
fn limit(vm: &Vm) -> Option<u64> {
    let raw = vm
        .ini_string("memory_limit")
        .unwrap_or_else(|| b"128M".to_vec());
    let (v, _) = php_support::ini::parse_quantity(&raw);
    u64::try_from(v).ok().filter(|&v| v > 0)
}

/// Raises the out-of-memory fatal error when an operation needing `bytes`
/// (on top of the baseline) exceeds `memory_limit`.
pub(crate) fn check(vm: &mut Vm, bytes: u64) -> VmResult<()> {
    let Some(limit) = limit(vm) else {
        return Ok(());
    };
    if BASELINE_USAGE.saturating_add(bytes) > limit {
        let tried = bytes.clamp(SEGMENT_SIZE, 2 * 1024 * 1024);
        return Err(vm.fatal(&format!(
            "Allowed memory size of {limit} bytes exhausted (tried to allocate {tried} bytes)"
        )));
    }
    Ok(())
}

/// What uriparser holds for a parsed RFC 3986 URI.
#[allow(clippy::naive_bytecount)] // not hot; avoids a dependency
pub(crate) fn rfc3986(uri: &[u8]) -> u64 {
    let segments = uri.iter().filter(|&&c| c == b'/').count() as u64 + 1;
    segments * SEGMENT_SIZE + uri.len() as u64
}

/// Whether `c` at `i` may raise a WHATWG validation error.
fn may_raise(input: &[u8], i: usize) -> bool {
    let c = input[i];
    match c {
        b'%' => {
            !(i + 2 < input.len()
                && input[i + 1].is_ascii_hexdigit()
                && input[i + 2].is_ascii_hexdigit())
        }
        0..=0x20 | 0x7F | b'"' | b'<' | b'>' | b'`' | b'{' | b'}' | b'^' | b'|' | b'\\' => true,
        _ => false,
    }
}

/// What parsing `input` as a WHATWG URL costs: the input, IDNA work on
/// non-ASCII text and (an upper bound of) the validation error contexts.
pub(crate) fn whatwg(input: &[u8]) -> u64 {
    let n = input.len() as u64;
    let non_ascii = input.iter().filter(|c| !c.is_ascii()).count() as u64;
    let mut errors = 0u64;
    for i in 0..input.len() {
        if may_raise(input, i) {
            errors = errors
                .saturating_add(VALIDATION_ERROR_SIZE)
                .saturating_add(n - i as u64);
        }
    }
    n.saturating_add(non_ascii.saturating_mul(IDNA_FACTOR))
        .saturating_add(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3986_counts_segments() {
        assert_eq!(rfc3986(b""), 32);
        assert_eq!(rfc3986(b"/a/b"), 3 * 32 + 4);
    }

    #[test]
    fn whatwg_bounds_error_contexts() {
        assert_eq!(whatwg(b"https://example.com/"), 20);
        assert_eq!(whatwg(b"a%41"), 4);
        // "%" without hex digits raises with the rest of the input.
        assert_eq!(whatwg(b"a%zz"), 4 + 128 + 3);
        let big = vec![b'%'; 100 * 1024];
        assert!(whatwg(&big) > 1 << 32);
        assert_eq!(whatwg("ä".as_bytes()), 2 + 16);
    }
}
