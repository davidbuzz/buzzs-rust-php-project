//! `IntlDatePatternGenerator` (php-src `ext/intl/dateformat/datepatterngenerator_*`).

use crate::common;
use crate::error::{Error, Result, Scope, StatusExt};
use crate::sys::udatpg::{self, PatternGenerator};
use crate::ustr;

/// A PHP `IntlDatePatternGenerator` object.
#[derive(Debug)]
pub struct IntlDatePatternGenerator {
    gen: PatternGenerator,
}

impl IntlDatePatternGenerator {
    /// `new IntlDatePatternGenerator($locale)` / `::create($locale)`.
    pub fn new(locale: Option<&str>) -> Result<Self> {
        let l = locale.unwrap_or("");
        common::check_locale_len(l).map_err(|e| crate::timezone::rescope(e, Scope::Global))?;
        let l = common::locale_or_default(l);
        let gen = PatternGenerator::open(&l)
            .map_err(|c| Error::global(c, "Error creating DateTimePatternGenerator"))?;
        Ok(Self { gen })
    }

    /// Clone (`clone $gen`).
    pub fn try_clone(&self) -> Result<Self> {
        let gen = self
            .gen
            .try_clone()
            .map_err(|_| Error::Thrown("Failed to clone IntlDatePatternGenerator".into()))?;
        Ok(Self { gen })
    }

    /// `getBestPattern($skeleton)`.
    pub fn best_pattern(&mut self, skeleton: &[u8]) -> Result<String> {
        let s = ustr::to_u16(skeleton).or_intl("Skeleton is not a valid UTF-8 string")?;
        let clean = udatpg::skeleton(&s).or_intl("Error getting cleaned skeleton")?;
        let r = self
            .gen
            .best_pattern(&clean)
            .or_intl("Error retrieving pattern")?;
        ustr::from_u16(&r).or_intl("Error converting result to UTF-8")
    }
}

/// `IntlDatePatternGenerator::getSkeleton($pattern)` (global errors).
pub fn skeleton(pattern: &[u8]) -> Result<String> {
    let p = ustr::to_u16(pattern)
        .map_err(|c| Error::global(c, "Pattern is not a valid UTF-8 string"))?;
    let r = udatpg::skeleton(&p).map_err(|c| Error::global(c, "Error getting skeleton"))?;
    ustr::from_u16(&r).map_err(|c| Error::global(c, "Error converting result to UTF-8"))
}

/// `IntlDatePatternGenerator::getBaseSkeleton($pattern)` (global errors).
pub fn base_skeleton(pattern: &[u8]) -> Result<String> {
    let p = ustr::to_u16(pattern)
        .map_err(|c| Error::global(c, "Pattern is not a valid UTF-8 string"))?;
    let r =
        udatpg::base_skeleton(&p).map_err(|c| Error::global(c, "Error getting base skeleton"))?;
    ustr::from_u16(&r).map_err(|c| Error::global(c, "Error converting result to UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_patterns() {
        let mut g = IntlDatePatternGenerator::new(Some("en_US")).unwrap();
        assert_eq!(g.best_pattern(b"YYYYMMMdd").unwrap(), "MMM dd, YYYY");
        let mut g = IntlDatePatternGenerator::new(Some("de_DE")).unwrap();
        assert_eq!(g.best_pattern(b"YYYYMMMdd").unwrap(), "dd. MMM YYYY");
        assert_eq!(skeleton(b"dd.MM.yyyy").unwrap(), "yyyyMMdd");
        assert_eq!(base_skeleton(b"dd.MM.yyyy").unwrap(), "yMd");
        assert!(g.best_pattern(b"\xff").is_err());
    }
}
