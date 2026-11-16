// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `IntlBreakIterator`, `IntlRuleBasedBreakIterator`,
//! `IntlCodePointBreakIterator` and `IntlPartsIterator` (php-src
//! `ext/intl/breakiterator`).  Offsets are UTF-8 byte offsets, as php-src
//! iterates a UTF-8 `UText`.

use std::sync::Arc;

use crate::common;
use crate::error::{Error, Result, StatusExt, UErrorCode};
use crate::sys::ubrk::{self, BreakIterator as Ubrk};
use crate::sys::utext::Utf8Text;
use crate::ustr;

/// `IntlBreakIterator::DONE`.
pub const DONE: i64 = -1;

/// `UBreakIteratorType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `createCharacterInstance`
    Character = 0,
    /// `createWordInstance`
    Word = 1,
    /// `createLineInstance`
    Line = 2,
    /// `createSentenceInstance`
    Sentence = 3,
    /// `createTitleInstance`
    Title = 4,
}

/// Which PHP class an iterator object belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// `IntlRuleBasedBreakIterator`
    RuleBased,
    /// `IntlCodePointBreakIterator`
    CodePoint,
}

/// php-src's `CodePointBreakIterator`, driven by a UTF-8 `UText`.
#[derive(Debug)]
struct CodePoint {
    text: Utf8Text,
    last: i32,
}

impl CodePoint {
    fn new(bytes: Arc<[u8]>) -> std::result::Result<Self, UErrorCode> {
        Ok(Self {
            text: Utf8Text::open(bytes)?,
            last: -1,
        })
    }
}

#[derive(Debug)]
enum Inner {
    Icu(Ubrk),
    CodePoint(CodePoint),
}

/// A PHP `IntlBreakIterator` object.
#[derive(Debug)]
pub struct IntlBreakIterator {
    inner: Inner,
    text: Option<Arc<[u8]>>,
}

fn i32_arg(v: i64) -> Result<i32> {
    i32::try_from(v)
        .map_err(|_| Error::arg_value(1, format!("must be between {} and {}", i32::MIN, i32::MAX)))
}

const U_SENTINEL: i32 = -1;

impl IntlBreakIterator {
    /// `IntlBreakIterator::create*Instance($locale)` (`null` locale = the
    /// default).  The status (possibly a warning) is returned alongside:
    /// php leaves it in the global error code.
    pub fn create(kind: Kind, locale: Option<&str>) -> Result<(Self, UErrorCode)> {
        let l = locale.map_or_else(common::default_locale, str::to_owned);
        let (bi, st) = Ubrk::open(kind as i32, &l)
            .map_err(|c| Error::global(c, "error creating BreakIterator"))?;
        Ok((
            Self {
                inner: Inner::Icu(bi),
                text: None,
            },
            st,
        ))
    }

    /// `IntlBreakIterator::createCodePointInstance()`.
    pub fn create_code_point() -> Self {
        let cp = CodePoint::new(Arc::from(&[][..])).expect("empty UText");
        Self {
            inner: Inner::CodePoint(cp),
            text: None,
        }
    }

    /// `new IntlRuleBasedBreakIterator($rules, $compiled)`.
    pub fn from_rules(rules: &[u8], compiled: bool) -> Result<Self> {
        let bi = if compiled {
            Ubrk::open_binary_rules(Arc::from(rules)).map_err(|_| {
                Error::IntlException(
                    "IntlRuleBasedBreakIterator::__construct(): unable to create instance from compiled rules".into(),
                )
            })?
        } else {
            let u = ustr::to_u16(rules).map_err(|_| {
                Error::IntlException(
                    "IntlRuleBasedBreakIterator::__construct(): rules were not a valid UTF-8 string".into(),
                )
            })?;
            Ubrk::open_rules(&u).map_err(|(_, pe)| {
                Error::IntlException(format!(
                    "IntlRuleBasedBreakIterator::__construct(): unable to create RuleBasedBreakIterator from rules ({})",
                    crate::error::parse_error_to_string(&pe)
                ))
            })?
        };
        Ok(Self {
            inner: Inner::Icu(bi),
            text: None,
        })
    }

    /// The PHP class of this object.
    pub fn class(&self) -> Class {
        match self.inner {
            Inner::Icu(_) => Class::RuleBased,
            Inner::CodePoint(_) => Class::CodePoint,
        }
    }

    /// Clone (`clone $bi`).
    pub fn try_clone(&self) -> Result<Self> {
        let inner = match &self.inner {
            Inner::Icu(b) => Inner::Icu(
                b.try_clone()
                    .map_err(|_| Error::Thrown("Could not clone BreakIterator".into()))?,
            ),
            Inner::CodePoint(c) => {
                let mut n = CodePoint::new(c.text.bytes().clone())
                    .map_err(|_| Error::Thrown("Could not clone BreakIterator".into()))?;
                n.text.set_native_index(c.text.native_index());
                n.last = c.last;
                Inner::CodePoint(n)
            }
        };
        Ok(Self {
            inner,
            text: self.text.clone(),
        })
    }

    /// `getText()` (`None` → `null`).
    pub fn text(&self) -> Option<&[u8]> {
        self.text.as_deref()
    }

    /// `setText($text)`.
    pub fn set_text(&mut self, text: &[u8]) -> Result<()> {
        let bytes: Arc<[u8]> = Arc::from(text);
        match &mut self.inner {
            Inner::Icu(b) => b.set_text(bytes.clone()).map_err(|(c, opening)| {
                Error::intl(
                    c,
                    if opening {
                        "error opening UText"
                    } else {
                        "error calling BreakIterator::setText()"
                    },
                )
            })?,
            Inner::CodePoint(c) => {
                *c = CodePoint::new(bytes.clone()).or_intl("error opening UText")?;
            }
        }
        self.text = Some(bytes);
        Ok(())
    }

    /// `first()`.
    pub fn first(&mut self) -> i64 {
        i64::from(match &mut self.inner {
            Inner::Icu(b) => b.first(),
            Inner::CodePoint(c) => {
                c.text.set_native_index(0);
                c.last = U_SENTINEL;
                0
            }
        })
    }

    /// `last()`.
    pub fn last(&mut self) -> i64 {
        i64::from(match &mut self.inner {
            Inner::Icu(b) => b.last(),
            Inner::CodePoint(c) => {
                let pos = c.text.native_length();
                c.text.set_native_index(pos);
                c.last = U_SENTINEL;
                i32::try_from(pos).unwrap_or(i32::MAX)
            }
        })
    }

    fn cp_index(c: &CodePoint) -> i32 {
        i32::try_from(c.text.native_index()).unwrap_or(i32::MAX)
    }

    /// `previous()`.
    pub fn previous(&mut self) -> i64 {
        i64::from(match &mut self.inner {
            Inner::Icu(b) => b.previous(),
            Inner::CodePoint(c) => {
                c.last = c.text.previous32();
                if c.last == U_SENTINEL {
                    ubrk::DONE
                } else {
                    Self::cp_index(c)
                }
            }
        })
    }

    fn next0(&mut self) -> i32 {
        match &mut self.inner {
            Inner::Icu(b) => b.next_boundary(),
            Inner::CodePoint(c) => {
                c.last = c.text.next32();
                if c.last == U_SENTINEL {
                    ubrk::DONE
                } else {
                    Self::cp_index(c)
                }
            }
        }
    }

    /// `next($offset = null)`.
    pub fn next(&mut self, n: Option<i64>) -> Result<i64> {
        let Some(n) = n else {
            return Ok(i64::from(self.next0()));
        };
        let n = i32_arg(n)?;
        Ok(i64::from(match &mut self.inner {
            Inner::Icu(b) => {
                let mut result = 0;
                match n.cmp(&0) {
                    std::cmp::Ordering::Greater => {
                        let mut k = n;
                        while k > 0 && result != ubrk::DONE {
                            result = b.next_boundary();
                            k -= 1;
                        }
                    }
                    std::cmp::Ordering::Less => {
                        let mut k = n;
                        while k < 0 && result != ubrk::DONE {
                            result = b.previous();
                            k += 1;
                        }
                    }
                    std::cmp::Ordering::Equal => result = b.current(),
                }
                result
            }
            Inner::CodePoint(c) => {
                if c.text.move_index32(n) {
                    c.last = c.text.current32();
                    Self::cp_index(c)
                } else {
                    c.last = U_SENTINEL;
                    ubrk::DONE
                }
            }
        }))
    }

    /// `current()`.
    pub fn current(&self) -> i64 {
        i64::from(match &self.inner {
            Inner::Icu(b) => b.current(),
            Inner::CodePoint(c) => Self::cp_index(c),
        })
    }

    /// `following($offset)`.
    pub fn following(&mut self, o: i64) -> Result<i64> {
        let o = i32_arg(o)?;
        Ok(i64::from(match &mut self.inner {
            Inner::Icu(b) => b.following(o),
            Inner::CodePoint(c) => {
                c.last = c.text.next32_from(i64::from(o));
                if c.last == U_SENTINEL {
                    ubrk::DONE
                } else {
                    Self::cp_index(c)
                }
            }
        }))
    }

    /// `preceding($offset)`.
    pub fn preceding(&mut self, o: i64) -> Result<i64> {
        let o = i32_arg(o)?;
        Ok(i64::from(match &mut self.inner {
            Inner::Icu(b) => b.preceding(o),
            Inner::CodePoint(c) => {
                c.last = c.text.previous32_from(i64::from(o));
                if c.last == U_SENTINEL {
                    ubrk::DONE
                } else {
                    Self::cp_index(c)
                }
            }
        }))
    }

    /// `isBoundary($offset)`.
    pub fn is_boundary(&mut self, o: i64) -> Result<bool> {
        let o = i32_arg(o)?;
        Ok(match &mut self.inner {
            Inner::Icu(b) => b.is_boundary(o),
            Inner::CodePoint(c) => {
                c.text.set_native_index(i64::from(o));
                i64::from(o) == c.text.native_index()
            }
        })
    }

    /// `getLocale($type)`.
    pub fn locale(&self, ty: i64) -> Result<String> {
        if ty != 0 && ty != 1 {
            return Err(Error::arg_value(
                1,
                "must be either Locale::ACTUAL_LOCALE or Locale::VALID_LOCALE",
            ));
        }
        match &self.inner {
            #[allow(clippy::cast_possible_truncation)]
            Inner::Icu(b) => b
                .locale_by_type(ty as i32)
                .map(|l| common::icu_locale_name(&l))
                .or_intl("Call to ICU method has failed"),
            Inner::CodePoint(_) => Ok(String::new()),
        }
    }

    /// `IntlCodePointBreakIterator::getLastCodePoint()`.
    pub fn last_code_point(&self) -> i64 {
        match &self.inner {
            Inner::CodePoint(c) => i64::from(c.last),
            Inner::Icu(_) => -1,
        }
    }

    /// `IntlRuleBasedBreakIterator::getRules()` (the stripped rule source
    /// stored in the compiled rules).
    pub fn rules(&mut self) -> Result<String> {
        let bin = self.binary_rules()?;
        let rd = |o: usize| -> usize {
            bin.get(o..o + 4)
                .map_or(0, |b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as usize)
        };
        let (off, len) = (rd(40), rd(44));
        let src = bin.get(off..off + len).unwrap_or(&[]);
        let s = std::str::from_utf8(src).map_err(|_| {
            Error::intl(
                UErrorCode::INVALID_CHAR_FOUND,
                "Error converting result to UTF-8 string",
            )
        })?;
        Ok(s.trim_end_matches('\0').to_owned())
    }

    /// `IntlRuleBasedBreakIterator::getRuleStatus()`.
    pub fn rule_status(&mut self) -> i64 {
        match &mut self.inner {
            Inner::Icu(b) => i64::from(b.rule_status()),
            Inner::CodePoint(_) => 0,
        }
    }

    /// `IntlRuleBasedBreakIterator::getRuleStatusVec()`.
    pub fn rule_status_vec(&mut self) -> Result<Vec<i64>> {
        match &mut self.inner {
            Inner::Icu(b) => Ok(b
                .rule_status_vec()
                .or_intl("failed obtaining the status values")?
                .into_iter()
                .map(i64::from)
                .collect()),
            Inner::CodePoint(_) => Ok(vec![0]),
        }
    }

    /// `IntlRuleBasedBreakIterator::getBinaryRules()`.
    pub fn binary_rules(&mut self) -> Result<Vec<u8>> {
        match &mut self.inner {
            Inner::Icu(b) => b.binary_rules().or_intl("the rules are too large"),
            Inner::CodePoint(_) => Ok(Vec::new()),
        }
    }

    /// `getPartsIterator($type)` argument check.
    pub fn check_parts_key_type(key_type: i64) -> Result<PartsKey> {
        match key_type {
            0 => Ok(PartsKey::Sequential),
            1 => Ok(PartsKey::Left),
            2 => Ok(PartsKey::Right),
            _ => Err(Error::arg_value(
                1,
                "must be one of IntlPartsIterator::KEY_SEQUENTIAL, IntlPartsIterator::KEY_LEFT, or IntlPartsIterator::KEY_RIGHT",
            )),
        }
    }
}

/// `IntlPartsIterator::KEY_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartsKey {
    /// `KEY_SEQUENTIAL`
    Sequential = 0,
    /// `KEY_LEFT`
    Left = 1,
    /// `KEY_RIGHT`
    Right = 2,
}

/// The state of an `IntlPartsIterator` (php-src's `zoi_break_iter_parts`).
/// The VM drives it with the engine's iterator protocol: on `rewind` it
/// calls [`PartsCursor::rewind`] then resets its index to -1, on each
/// subsequent fetch it increments its index and, when that is > 0, calls
/// [`PartsCursor::move_forward`] (assigning the returned index if any).
#[derive(Debug, Clone)]
pub struct PartsCursor {
    key: PartsKey,
    current: Option<Vec<u8>>,
    index_right: i64,
}

impl PartsCursor {
    /// A cursor for `getPartsIterator($type)`.
    pub fn new(key: PartsKey) -> Self {
        Self {
            key,
            current: None,
            index_right: 0,
        }
    }

    /// `rewind`.
    pub fn rewind(&mut self, bi: &mut IntlBreakIterator) -> Option<i64> {
        self.current = None;
        bi.first();
        self.move_forward(bi)
    }

    /// `move_forward`: returns the new engine index for `KEY_LEFT` and
    /// `KEY_RIGHT`.
    pub fn move_forward(&mut self, bi: &mut IntlBreakIterator) -> Option<i64> {
        self.current = None;
        let cur = bi.current();
        if cur == DONE {
            return None;
        }
        let next = bi.next0();
        if next == ubrk::DONE {
            return None;
        }
        let next = i64::from(next);
        let new_index = match self.key {
            PartsKey::Left => Some(cur),
            PartsKey::Right => {
                self.index_right = next;
                Some(next)
            }
            PartsKey::Sequential => None,
        };
        let text = bi.text.as_deref().unwrap_or(&[]);
        let (a, b) = (
            usize::try_from(cur).unwrap_or(0),
            usize::try_from(next).unwrap_or(0),
        );
        self.current = Some(text.get(a..b).unwrap_or(&[]).to_vec());
        new_index
    }

    /// `valid`.
    pub fn valid(&self) -> bool {
        self.current.is_some()
    }

    /// `get_current_data`.
    pub fn current(&self) -> Option<&[u8]> {
        self.current.as_deref()
    }

    /// `get_current_key` given the engine's index.
    pub fn key(&self, engine_index: i64) -> i64 {
        if self.key == PartsKey::Right && engine_index == 0 {
            self.index_right
        } else {
            engine_index
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boundaries(bi: &mut IntlBreakIterator) -> Vec<i64> {
        let mut v = vec![bi.first()];
        loop {
            let n = bi.next(None).unwrap();
            if n == DONE {
                break;
            }
            v.push(n);
        }
        v
    }

    #[test]
    fn words_utf8_offsets() {
        let (mut bi, _) = IntlBreakIterator::create(Kind::Word, Some("en")).unwrap();
        bi.set_text("Ação é boa".as_bytes()).unwrap();
        assert_eq!(boundaries(&mut bi), vec![0, 6, 7, 9, 10, 13]);
        assert_eq!(bi.class(), Class::RuleBased);
        let mut cp = IntlBreakIterator::create_code_point();
        cp.set_text("aé😀".as_bytes()).unwrap();
        assert_eq!(boundaries(&mut cp), vec![0, 1, 3, 7]);
        assert_eq!(cp.last_code_point(), -1);
        cp.first();
        cp.next(None).unwrap();
        assert_eq!(cp.last_code_point(), 0x61);
    }

    #[test]
    fn parts() {
        let (mut bi, _) = IntlBreakIterator::create(Kind::Word, Some("en")).unwrap();
        bi.set_text(b"foo bar").unwrap();
        let mut pc = PartsCursor::new(PartsKey::Sequential);
        pc.rewind(&mut bi);
        let mut parts = Vec::new();
        while pc.valid() {
            parts.push(String::from_utf8(pc.current().unwrap().to_vec()).unwrap());
            pc.move_forward(&mut bi);
        }
        assert_eq!(parts, ["foo", " ", "bar"]);
    }

    #[test]
    fn rules() {
        let mut bi = IntlBreakIterator::from_rules(
            b"$LN = [[:letter:] [:number:]];$S = [.;,:];!!forward;$LN+ {1};$S+ {42};",
            false,
        )
        .unwrap();
        bi.set_text(b"sdfkjsdf88\xc3\xa1.... ,;").unwrap();
        bi.first();
        assert_eq!(bi.next(None).unwrap(), 12);
        assert_eq!(bi.rule_status(), 1);
        assert!(bi.rules().unwrap().contains("$LN"));
        let bin = bi.binary_rules().unwrap();
        let mut b2 = IntlBreakIterator::from_rules(&bin, true).unwrap();
        b2.set_text(b"abc ...").unwrap();
        b2.first();
        assert_eq!(b2.next(None).unwrap(), 3);
        assert!(IntlBreakIterator::from_rules(b"[[", false).is_err());
    }
}
