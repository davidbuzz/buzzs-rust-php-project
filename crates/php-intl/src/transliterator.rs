// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `Transliterator` / `transliterator_*` (php-src `ext/intl/transliterator`).

use crate::error::{Error, Result, UErrorCode};
use crate::sys::utrans::{self, Transliterator as Utrans};
use crate::ustr;

/// `Transliterator::FORWARD`.
pub const FORWARD: i64 = 0;
/// `Transliterator::REVERSE`.
pub const REVERSE: i64 = 1;

/// A PHP `Transliterator` object.
#[derive(Debug)]
pub struct Transliterator {
    t: Utrans,
    id: String,
}

fn check_direction(d: i64) -> Result<i32> {
    match d {
        FORWARD => Ok(0),
        REVERSE => Ok(1),
        _ => Err(Error::arg_value(
            2,
            "must be either Transliterator::FORWARD or Transliterator::REVERSE",
        )),
    }
}

impl Transliterator {
    fn wrap(t: Utrans) -> Result<Self> {
        let id = ustr::from_u16(&t.id())
            .map_err(|c| Error::global(c, "internal constructor call failed"))?;
        Ok(Self { t, id })
    }

    /// `Transliterator::create($id, $direction)` (errors are global; the
    /// method returns `null`).
    pub fn create(id: &[u8], direction: i64) -> Result<Self> {
        let dir = check_direction(direction)?;
        let uid = ustr::to_u16(id)
            .map_err(|c| Error::global(c, "String conversion of id to UTF-16 failed"))?;
        let t = Utrans::open(&uid, dir, None).map_err(|(c, _)| {
            Error::global(
                c,
                format!(
                    "unable to open ICU transliterator with id \"{}\"",
                    String::from_utf8_lossy(id)
                ),
            )
        })?;
        Self::wrap(t)
    }

    /// `Transliterator::createFromRules($rules, $direction)`.
    pub fn create_from_rules(rules: &[u8], direction: i64) -> Result<Self> {
        let dir = check_direction(direction)?;
        let ur = ustr::to_u16(rules)
            .map_err(|c| Error::global(c, "String conversion of rules to UTF-16 failed"))?;
        let id = ustr::str_u16("RulesTransPHP");
        let t = Utrans::open(&id, dir, Some(&ur)).map_err(|(c, pe)| {
            Error::global(
                c,
                format!(
                    "unable to create ICU transliterator from rules ({})",
                    crate::error::parse_error_to_string(&pe)
                ),
            )
        })?;
        Self::wrap(t)
    }

    /// `createInverse()`.
    pub fn create_inverse(&self) -> Result<Self> {
        let t = self
            .t
            .inverse()
            .map_err(|c| Error::global(c, "could not create inverse ICU transliterator"))?;
        Self::wrap(t)
    }

    /// Clone (`clone $t`).
    pub fn try_clone(&self) -> Result<Self> {
        let t = self
            .t
            .try_clone()
            .map_err(|_| Error::Thrown("Could not clone transliterator".into()))?;
        Ok(Self {
            t,
            id: self.id.clone(),
        })
    }

    /// The readonly `$id` property.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// `toRules($escapeUnprintable)`.
    pub fn to_rules(&self, escape_unprintable: bool) -> Result<String> {
        let r = self
            .t
            .to_rules(escape_unprintable)
            .map_err(|c| Error::intl(c, "Failed to export transliterator rules"))?;
        ustr::from_u16(&r).map_err(|c| Error::intl(c, "Failed to export transliterator rules"))
    }

    /// `transliterate($string, $start, $end)`.  `procedural` selects the
    /// argument numbering of `transliterator_transliterate()` (which has the
    /// transliterator as argument #1).
    pub fn transliterate(
        &self,
        s: &[u8],
        start: i64,
        end: i64,
        procedural: bool,
    ) -> Result<String> {
        let (a_start, a_end) = if procedural { (3, 4) } else { (2, 3) };
        if end < -1 {
            return Err(Error::arg_value(
                a_end,
                "must be greater than or equal to -1",
            ));
        }
        if start < 0 {
            return Err(Error::arg_value(
                a_start,
                "must be greater than or equal to 0",
            ));
        }
        if end != -1 && start > end {
            return Err(Error::arg_value(
                a_start,
                format!("must be less than or equal to argument #{a_end} ($end)"),
            ));
        }
        let u = ustr::to_u16(s)
            .map_err(|c| Error::intl(c, "String conversion of string to UTF-16 failed"))?;
        let ulen = i64::try_from(u.len()).unwrap_or(i64::MAX);
        if start > ulen || (end != -1 && end > ulen) {
            return Err(Error::intl(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                format!(
                    "Neither \"start\" nor the \"end\" arguments can exceed the number of UTF-16 code units (in this case, {ulen})"
                ),
            ));
        }
        let limit = if end == -1 { ulen } else { end };
        #[allow(clippy::cast_possible_truncation)]
        let r = self
            .t
            .trans(&u, start as i32, limit as i32)
            .map_err(|c| Error::object_code(c, "transliteration failed"))?;
        ustr::from_u16(&r).map_err(|c| Error::intl(c, "Error converting value to UTF-8"))
    }
}

/// `Transliterator::listIDs()`.
pub fn list_ids() -> Result<Vec<String>> {
    let ids = utrans::ids()
        .map_err(|c| Error::global(c, "Failed to obtain registered transliterators"))?;
    ids.iter()
        .map(|i| ustr::from_u16(i))
        .collect::<std::result::Result<_, _>>()
        .map_err(|c| Error::global(c, "Failed to build array of registered transliterators"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translit() {
        let t = Transliterator::create(b"Any-Latin; Latin-ASCII; Lower()", FORWARD).unwrap();
        assert_eq!(
            t.transliterate("Ἀριστοτέλης".as_bytes(), 0, -1, false)
                .unwrap(),
            "aristoteles"
        );
        assert_eq!(t.id(), "Any-Latin;Latin-ASCII;Lower()");
        let r = Transliterator::create_from_rules(b"a > b; b > a;", FORWARD).unwrap();
        assert_eq!(r.transliterate(b"abc", 0, -1, false).unwrap(), "bac");
        assert_eq!(r.transliterate(b"abc", 1, 2, false).unwrap(), "aac");
        assert!(Transliterator::create(b"Nonexistent", FORWARD).is_err());
        let inv = Transliterator::create(b"Latin-Greek", FORWARD)
            .unwrap()
            .create_inverse()
            .unwrap();
        assert_eq!(inv.id(), "Greek-Latin");
        assert!(list_ids().unwrap().len() > 100);
        let e = Transliterator::create_from_rules(b"a > ;!", FORWARD).unwrap_err();
        assert!(e
            .intl_message()
            .unwrap()
            .starts_with("unable to create ICU transliterator from rules (parse error"));
    }
}
