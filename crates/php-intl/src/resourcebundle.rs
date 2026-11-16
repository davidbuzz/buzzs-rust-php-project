// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `ResourceBundle` / `resourcebundle_*` (php-src `ext/intl/resourcebundle`).

use crate::common;
use crate::error::{Error, Result, Scope, UErrorCode};
use crate::sys::ures::{self, restype, Bundle};
use crate::ustr;

const MAXPATHLEN: usize = 4096;

/// A PHP `ResourceBundle` object.
#[derive(Debug)]
pub struct ResourceBundle {
    me: Bundle,
}

/// A value fetched from a bundle.
#[derive(Debug)]
pub enum ResourceValue {
    /// `URES_STRING`
    Str(String),
    /// `URES_BINARY`
    Binary(Vec<u8>),
    /// `URES_INT`
    Int(i64),
    /// `URES_INT_VECTOR`
    IntVector(Vec<i64>),
    /// `URES_ARRAY`/`URES_TABLE` — a nested `ResourceBundle`.
    Bundle(ResourceBundle),
}

/// An offset (`string|int`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offset<'a> {
    /// A key.
    Key(&'a [u8]),
    /// An index.
    Index(i64),
}

impl ResourceBundle {
    /// `new ResourceBundle($locale, $bundle, $fallback)` /
    /// `resourcebundle_create()`.  On success the (warning) status is
    /// returned: php leaves it in the global error code.
    pub fn new(
        locale: Option<&str>,
        bundle: Option<&str>,
        fallback: bool,
    ) -> Result<(Self, UErrorCode)> {
        let loc = locale.unwrap_or("");
        common::check_locale_len(loc).map_err(|e| crate::timezone::rescope(e, Scope::Global))?;
        let loc = locale.map_or_else(common::default_locale, str::to_owned);
        if bundle.is_some_and(|b| b.len() >= MAXPATHLEN) {
            return Err(Error::arg_value(2, "is too long"));
        }
        let r = if fallback {
            Bundle::open(bundle, &loc)
        } else {
            Bundle::open_direct(bundle, &loc)
        };
        let (me, st) = r.map_err(|c| Error::intl(c, "Cannot load libICU resource bundle"))?;
        if !fallback
            && (st == UErrorCode::USING_FALLBACK_WARNING || st == UErrorCode::USING_DEFAULT_WARNING)
        {
            let actual = me.locale_by_type(0).unwrap_or_default();
            return Err(Error::intl(
                st,
                format!(
                    "Cannot load libICU resource '{}' without fallback from {loc} to {actual}",
                    bundle.unwrap_or("(default data)")
                ),
            ));
        }
        Ok((Self { me }, st))
    }

    /// `count()`.
    pub fn count(&self) -> i64 {
        i64::from(self.me.size())
    }

    /// Whether iteration yields string keys (`URES_TABLE`).
    pub fn is_table(&self) -> bool {
        self.me.res_type() == restype::TABLE
    }

    fn extract(child: Bundle) -> Result<ResourceValue> {
        Ok(match child.res_type() {
            restype::STRING => {
                let s = child
                    .string()
                    .map_err(|c| Error::intl(c, "Failed to retrieve string value"))?;
                ResourceValue::Str(
                    ustr::from_u16(&s)
                        .map_err(|c| Error::intl(c, "Error converting value to UTF-8"))?,
                )
            }
            restype::BINARY => ResourceValue::Binary(
                child
                    .binary()
                    .map_err(|c| Error::intl(c, "Failed to retrieve binary value"))?,
            ),
            restype::INT => ResourceValue::Int(i64::from(
                child
                    .int()
                    .map_err(|c| Error::intl(c, "Failed to retrieve integer value"))?,
            )),
            restype::INT_VECTOR => ResourceValue::IntVector(
                child
                    .int_vector()
                    .map_err(|c| Error::intl(c, "Failed to retrieve vector value"))?
                    .into_iter()
                    .map(i64::from)
                    .collect(),
            ),
            _ => ResourceValue::Bundle(ResourceBundle { me: child }),
        })
    }

    /// `get($index, $fallback)` / `$bundle[$index]`.  `arg_num` is the
    /// offset's argument number for the `ValueError`s (`None` for array
    /// access, which raises plain `ValueError`s).  On success the status
    /// (possibly a warning) is returned for the global error code.
    pub fn get(
        &self,
        offset: Offset<'_>,
        fallback: bool,
        arg_num: Option<u32>,
    ) -> Result<(ResourceValue, UErrorCode)> {
        let (r, desc_num, desc_key) = match offset {
            Offset::Key(k) => {
                if k.is_empty() {
                    return Err(match arg_num {
                        Some(a) => Error::arg_value(a, "must not be empty"),
                        None => Error::Value("Offset must not be empty".into()),
                    });
                }
                let key = String::from_utf8_lossy(k).into_owned();
                (self.me.by_key(&key), None, Some(key))
            }
            Offset::Index(i) => {
                let Ok(idx) = i32::try_from(i) else {
                    let m = format!("{} must be between {} and {}", "index", i32::MIN, i32::MAX);
                    return Err(match arg_num {
                        Some(a) => Error::arg_value(a, m),
                        None => Error::Value(format!(
                            "Index must be between {} and {}",
                            i32::MIN,
                            i32::MAX
                        )),
                    });
                };
                (self.me.by_index(idx), Some(idx), None)
            }
        };
        let elem = || {
            desc_num.map_or_else(
                || format!("'{}'", desc_key.clone().unwrap_or_default()),
                |n| n.to_string(),
            )
        };
        let (child, st) =
            r.map_err(|c| Error::intl(c, format!("Cannot load resource element {}", elem())))?;
        if !fallback
            && (st == UErrorCode::USING_FALLBACK_WARNING || st == UErrorCode::USING_DEFAULT_WARNING)
        {
            let locale = self.me.locale_by_type(0).unwrap_or_default();
            return Err(Error::intl(
                st,
                format!(
                    "Cannot load element {} without fallback to {locale}",
                    elem()
                ),
            ));
        }
        Ok((Self::extract(child)?, st))
    }

    /// Iteration: the `i`-th element and, for tables, its key.
    pub fn entry(&self, i: i64) -> Option<(Option<String>, ResourceValue)> {
        let (child, _) = self.me.by_index(i32::try_from(i).ok()?).ok()?;
        let key = if self.is_table() { child.key() } else { None };
        Some((key, Self::extract(child).ok()?))
    }
}

/// `ResourceBundle::getLocales($bundle)` (`""` = ICU's default data).
pub fn get_locales(bundle: &str) -> Result<Vec<String>> {
    if bundle.len() >= MAXPATHLEN {
        return Err(Error::arg_value(1, "is too long"));
    }
    let b = if bundle.is_empty() {
        None
    } else {
        Some(bundle)
    };
    ures::available_locales(b).map_err(|c| Error::global(c, "Cannot fetch locales list"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles() {
        let (rb, _) = ResourceBundle::new(Some("es"), None, true).unwrap();
        assert_eq!(rb.count(), 14);
        assert!(rb.is_table());
        let (v, _) = rb.get(Offset::Key(b"Version"), true, Some(1)).unwrap();
        assert!(matches!(v, ResourceValue::Str(_)));
        let e = rb
            .get(Offset::Key(b"nonexistent"), true, Some(1))
            .unwrap_err();
        assert_eq!(e.intl_code(), UErrorCode::MISSING_RESOURCE_ERROR);
        assert_eq!(
            e.intl_message(),
            Some("Cannot load resource element 'nonexistent'")
        );
        let e = ResourceBundle::new(Some("en_US"), Some("/nonexistent/bundle"), true).unwrap_err();
        assert_eq!(e.intl_message(), Some("Cannot load libICU resource bundle"));
        assert!(!get_locales("").unwrap().is_empty());
    }
}
