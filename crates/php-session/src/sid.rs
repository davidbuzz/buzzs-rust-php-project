//! Session ID generation and validation (`php_session_create_id`,
//! `bin_to_readable`, `php_session_valid_key`).

/// `PS_MAX_SID_LENGTH`.
pub const PS_MAX_SID_LENGTH: usize = 256;

/// The alphabet of `bin_to_readable` (no `/` or `+`, so IDs are URL safe).
const HEXCONVTAB: &[u8; 64] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ,-";

/// `bin_to_readable()`: encodes `outlen` characters of `nbits` (4, 5 or 6)
/// bits each, consuming input bytes little-endian bit first. Returns `None`
/// when the input is too short (PHP treats that as unreachable).
#[must_use]
pub fn bin_to_readable(input: &[u8], outlen: usize, nbits: u8) -> Option<Vec<u8>> {
    let mask: u32 = (1 << nbits) - 1;
    let mut out = Vec::with_capacity(outlen);
    let mut bytes = input.iter();
    let mut w: u32 = 0;
    let mut have: u32 = 0;
    for _ in 0..outlen {
        if have < u32::from(nbits) {
            let b = bytes.next()?;
            w |= u32::from(*b) << have;
            have += 8;
        }
        out.push(HEXCONVTAB[(w & mask) as usize]);
        w >>= nbits;
        have -= u32::from(nbits);
    }
    Some(out)
}

/// `php_session_create_id()`: reads `sid_length` random bytes and encodes
/// them with [`bin_to_readable`]. The RNG is a closure so callers can fix
/// it (the session module uses the host's CSPRNG).
pub fn create_id<E>(
    sid_length: usize,
    bits_per_character: u8,
    mut rng: impl FnMut(&mut [u8]) -> Result<(), E>,
) -> Result<Vec<u8>, E> {
    let mut buf = vec![0u8; sid_length];
    rng(&mut buf)?;
    // sid_length bytes always cover sid_length * 6 bits.
    Ok(bin_to_readable(&buf, sid_length, bits_per_character).unwrap_or_default())
}

/// `php_session_valid_key()`: 1..=256 characters of `[a-zA-Z0-9,-]`
/// (the whole string, embedded NUL bytes included, is checked).
#[must_use]
pub fn valid_key(key: &[u8]) -> bool {
    !key.is_empty()
        && key.len() <= PS_MAX_SID_LENGTH
        && key
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || c == b',' || c == b'-')
}

/// The characters that make `php_session_start()` discard a proposed ID
/// (`"\r\n\t <>'\"\\"`, checked with `strpbrk`, i.e. up to the first NUL).
#[must_use]
pub fn has_dangerous_chars(id: &[u8]) -> bool {
    id.iter()
        .take_while(|&&c| c != 0)
        .any(|c| b"\r\n\t <>'\"\\".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_encoding() {
        // 4 bits: low nibble first.
        assert_eq!(bin_to_readable(&[0xab, 0x01], 4, 4).unwrap(), b"ba10");
        // 5 bits over 0xff 0xff: 31, 31, 31 ...
        assert_eq!(bin_to_readable(&[0xff, 0xff], 3, 5).unwrap(), b"vvv");
        // 6 bits: 63 -> '-', 62 -> ','.
        assert_eq!(bin_to_readable(&[0xff, 0xff, 0xff], 4, 6).unwrap(), b"----");
        assert_eq!(bin_to_readable(&[0b0011_1110], 1, 6).unwrap(), b",");
        assert!(bin_to_readable(&[0], 3, 4).is_none());
    }

    #[test]
    fn create_with_fixed_rng() {
        let id = create_id::<()>(32, 4, |b| {
            for (i, x) in b.iter_mut().enumerate() {
                *x = u8::try_from(i).unwrap();
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(id, b"00102030405060708090a0b0c0d0e0f0");
        let id = create_id::<()>(26, 5, |b| {
            b.fill(0);
            Ok(())
        })
        .unwrap();
        assert_eq!(id.len(), 26);
        assert!(id.iter().all(|&c| c == b'0'));
        assert!(create_id(32, 4, |_| Err("no randomness")).is_err());
    }

    #[test]
    fn key_validation() {
        assert!(valid_key(b"abc-DEF,123"));
        assert!(!valid_key(b""));
        assert!(!valid_key(b"a b"));
        assert!(!valid_key(b"nul\0x"));
        assert!(valid_key(&[b'a'; 256]));
        assert!(!valid_key(&[b'a'; 257]));
        assert!(has_dangerous_chars(b"a<b"));
        assert!(!has_dangerous_chars(b"ab\0<"));
    }
}
