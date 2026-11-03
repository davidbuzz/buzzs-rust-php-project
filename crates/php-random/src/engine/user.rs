//! Userland engines (`ext/random/engine_user.c`): any object implementing
//! `Random\Engine` whose `generate()` returns a byte string.

use super::{Engine, Generated};
use crate::error::{RandomError, Result};

/// The hook the VM implements to call a userland `generate()` method.
pub trait UserGenerate {
    /// Calls `$engine->generate()` and returns the string it produced. A
    /// thrown exception must be reported as [`RandomError::Propagated`] (the
    /// VM keeps the exception pending).
    fn call_generate(&mut self) -> Result<Vec<u8>>;
}

impl<F: FnMut() -> Result<Vec<u8>>> UserGenerate for F {
    fn call_generate(&mut self) -> Result<Vec<u8>> {
        self()
    }
}

/// Converts the string returned by a userland `generate()` into a
/// [`Generated`]: the first (at most 8) bytes are read little-endian, and
/// an empty string is a `Random\BrokenRandomEngineError`.
pub fn generated_from_bytes(bytes: &[u8]) -> Result<Generated> {
    if bytes.is_empty() {
        return Err(RandomError::BrokenEngine(
            "A random engine must return a non-empty string".into(),
        ));
    }
    let size = bytes.len().min(8);
    Ok(Generated {
        value: crate::state::le_u64(&bytes[..size]),
        size,
    })
}

/// Adapter turning a [`UserGenerate`] into an [`Engine`]
/// (`php_random_algo_user`).
#[derive(Debug, Clone, Default)]
pub struct UserEngine<T>(pub T);

impl<T: UserGenerate> Engine for UserEngine<T> {
    fn generate(&mut self) -> Result<Generated> {
        let bytes = self.0.call_generate()?;
        generated_from_bytes(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion() {
        assert_eq!(
            generated_from_bytes(b"\x01\x02").unwrap(),
            Generated {
                value: 0x0201,
                size: 2
            }
        );
        assert_eq!(
            generated_from_bytes(&[0xff; 12]).unwrap(),
            Generated::u64(u64::MAX)
        );
        assert_eq!(
            generated_from_bytes(b"").unwrap_err(),
            RandomError::BrokenEngine("A random engine must return a non-empty string".into())
        );
    }

    #[test]
    fn closure_engine() {
        let mut n = 0u8;
        let mut e = UserEngine(move || {
            n += 1;
            Ok(vec![n])
        });
        assert_eq!(e.generate().unwrap().value, 1);
        assert_eq!(e.generate().unwrap().value, 2);
        assert_eq!(e.generate_bytes().unwrap(), vec![3]);
    }
}
