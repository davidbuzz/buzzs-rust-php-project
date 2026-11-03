//! A port of ICU's `MessagePattern` (apostrophe mode `DOUBLE_OPTIONAL`):
//! splits a MessageFormat pattern into parts.  Patterns are validated with
//! `umsg_open()` before they reach this parser, so error reporting here is
//! coarse (exact ICU errors come from ICU itself).

use crate::error::UErrorCode;
use crate::sys::uchar::{has_binary_property, PATTERN_SYNTAX, PATTERN_WHITE_SPACE};

/// `UMessagePatternPartType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartType {
    /// `MSG_START`
    MsgStart,
    /// `MSG_LIMIT`
    MsgLimit,
    /// `SKIP_SYNTAX`
    SkipSyntax,
    /// `INSERT_CHAR`
    InsertChar,
    /// `REPLACE_NUMBER`
    ReplaceNumber,
    /// `ARG_START`
    ArgStart,
    /// `ARG_LIMIT`
    ArgLimit,
    /// `ARG_NUMBER`
    ArgNumber,
    /// `ARG_NAME`
    ArgName,
    /// `ARG_TYPE`
    ArgType,
    /// `ARG_STYLE`
    ArgStyle,
    /// `ARG_SELECTOR`
    ArgSelector,
    /// `ARG_INT`
    ArgInt,
    /// `ARG_DOUBLE`
    ArgDouble,
}

/// `UMessagePatternArgType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    /// `{0}`
    None,
    /// `{0,number,...}` etc.
    Simple,
    /// `choice`
    Choice,
    /// `plural`
    Plural,
    /// `select`
    Select,
    /// `selectordinal`
    SelectOrdinal,
}

impl ArgType {
    /// `UMSGPAT_ARG_TYPE_HAS_PLURAL_STYLE`.
    pub fn has_plural_style(self) -> bool {
        matches!(self, Self::Plural | Self::SelectOrdinal)
    }
}

/// One part of a parsed pattern.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Part {
    /// The part type.
    pub ty: PartType,
    /// Index into the pattern (UTF-16 units).
    pub index: usize,
    /// Length in the pattern.
    pub length: usize,
    /// Numeric value (`ARG_NUMBER`, `ARG_INT`, nesting level, ...).
    pub value: i32,
    /// For `ARG_START`, the argument type.
    pub arg_type: ArgType,
    /// For start parts, the index of the matching limit part.
    pub limit_part: usize,
    /// For `ARG_DOUBLE`.
    pub double: f64,
}

impl Part {
    /// `index + length`.
    pub fn limit(&self) -> usize {
        self.index + self.length
    }
}

/// A parsed MessageFormat pattern.
#[derive(Debug, Clone, Default)]
pub struct MessagePattern {
    msg: Vec<u16>,
    parts: Vec<Part>,
    has_arg_names: bool,
    has_arg_numbers: bool,
}

const APOS: u16 = 0x27;
const LBRACE: u16 = 0x7b;
const RBRACE: u16 = 0x7d;
const PIPE: u16 = 0x7c;
const POUND: u16 = 0x23;
const MAX_VALUE: i32 = 0x7fff;

fn is_white(c: u16) -> bool {
    has_binary_property(u32::from(c), PATTERN_WHITE_SPACE)
}

fn is_identifier(c: u16) -> bool {
    !(has_binary_property(u32::from(c), PATTERN_SYNTAX) || has_binary_property(u32::from(c), PATTERN_WHITE_SPACE))
}

type PResult<T> = Result<T, UErrorCode>;

impl MessagePattern {
    /// Parses a MessageFormat pattern.
    pub fn parse(pattern: &[u16]) -> PResult<Self> {
        let mut p = Self { msg: pattern.to_vec(), ..Self::default() };
        p.parse_message(0, 0, 0, ArgType::None)?;
        Ok(p)
    }

    /// The pattern string.
    pub fn pattern(&self) -> &[u16] {
        &self.msg
    }

    /// The parts.
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// `getPart(i)`.
    pub fn part(&self, i: usize) -> &Part {
        &self.parts[i]
    }

    /// `countParts()`.
    pub fn count_parts(&self) -> usize {
        self.parts.len()
    }

    /// `hasNamedArguments()`.
    pub fn has_named_arguments(&self) -> bool {
        self.has_arg_names
    }

    /// `hasNumberedArguments()`.
    pub fn has_numbered_arguments(&self) -> bool {
        self.has_arg_numbers
    }

    /// `getLimitPartIndex(i)`.
    pub fn limit_part_index(&self, i: usize) -> usize {
        let l = self.parts[i].limit_part;
        if l < i {
            i
        } else {
            l
        }
    }

    /// `getSubstring(part)`.
    pub fn substring(&self, p: &Part) -> &[u16] {
        &self.msg[p.index..p.limit()]
    }

    /// `partSubstringMatches(part, s)`.
    pub fn part_matches(&self, p: &Part, s: &[u16]) -> bool {
        self.substring(p) == s
    }

    /// `getNumericValue(part)`.
    pub fn numeric_value(&self, p: &Part) -> f64 {
        match p.ty {
            PartType::ArgInt => f64::from(p.value),
            PartType::ArgDouble => p.double,
            _ => f64::NAN,
        }
    }

    /// `getPluralOffset(pluralStart)`.
    pub fn plural_offset(&self, i: usize) -> f64 {
        let p = &self.parts[i];
        if matches!(p.ty, PartType::ArgInt | PartType::ArgDouble) {
            self.numeric_value(p)
        } else {
            0.0
        }
    }

    fn add_part(&mut self, ty: PartType, index: usize, length: usize, value: i32) {
        self.parts.push(Part {
            ty,
            index,
            length,
            value,
            arg_type: ArgType::None,
            limit_part: 0,
            double: 0.0,
        });
    }

    fn add_limit_part(&mut self, start: usize, ty: PartType, index: usize, length: usize, value: i32) {
        let n = self.parts.len();
        self.parts[start].limit_part = n;
        self.add_part(ty, index, length, value);
    }

    fn at(&self, i: usize) -> u16 {
        self.msg.get(i).copied().unwrap_or(0)
    }

    fn index_of(&self, c: u16, from: usize) -> Option<usize> {
        self.msg.get(from..)?.iter().position(|&x| x == c).map(|p| p + from)
    }

    fn skip_white(&self, mut i: usize) -> usize {
        while i < self.msg.len() && is_white(self.msg[i]) {
            i += 1;
        }
        i
    }

    fn skip_identifier(&self, mut i: usize) -> usize {
        while i < self.msg.len() && is_identifier(self.msg[i]) {
            i += 1;
        }
        i
    }

    fn skip_double(&self, mut i: usize) -> usize {
        while i < self.msg.len() {
            let c = self.msg[i];
            if (c < 0x30 && c != u16::from(b'+') && c != u16::from(b'-') && c != u16::from(b'.'))
                || (c > 0x39 && c != u16::from(b'e') && c != u16::from(b'E') && c != 0x221e)
            {
                break;
            }
            i += 1;
        }
        i
    }

    fn parse_message(&mut self, index: usize, start_len: usize, nesting: i32, parent: ArgType) -> PResult<usize> {
        if nesting > MAX_VALUE {
            return Err(UErrorCode::INDEX_OUTOFBOUNDS_ERROR);
        }
        let msg_start = self.parts.len();
        self.add_part(PartType::MsgStart, index, start_len, nesting);
        let mut index = index + start_len;
        let len = self.msg.len();
        while index < len {
            let mut c = self.msg[index];
            index += 1;
            if c == APOS {
                if index == len {
                    self.add_part(PartType::InsertChar, index, 0, i32::from(APOS));
                } else {
                    c = self.msg[index];
                    if c == APOS {
                        self.add_part(PartType::SkipSyntax, index, 1, 0);
                        index += 1;
                    } else if c == LBRACE
                        || c == RBRACE
                        || (parent == ArgType::Choice && c == PIPE)
                        || (parent.has_plural_style() && c == POUND)
                    {
                        self.add_part(PartType::SkipSyntax, index - 1, 1, 0);
                        loop {
                            match self.index_of(APOS, index + 1) {
                                Some(i) => {
                                    index = i;
                                    if self.at(index + 1) == APOS {
                                        index += 1;
                                        self.add_part(PartType::SkipSyntax, index, 1, 0);
                                    } else {
                                        self.add_part(PartType::SkipSyntax, index, 1, 0);
                                        index += 1;
                                        break;
                                    }
                                }
                                None => {
                                    index = len;
                                    self.add_part(PartType::InsertChar, index, 0, i32::from(APOS));
                                    break;
                                }
                            }
                        }
                    } else {
                        self.add_part(PartType::InsertChar, index, 0, i32::from(APOS));
                    }
                }
            } else if parent.has_plural_style() && c == POUND {
                self.add_part(PartType::ReplaceNumber, index - 1, 1, 0);
            } else if c == LBRACE {
                index = self.parse_arg(index - 1, 1, nesting)?;
            } else if (nesting > 0 && c == RBRACE) || (parent == ArgType::Choice && c == PIPE) {
                let limit_len = usize::from(!(parent == ArgType::Choice && c == RBRACE));
                self.add_limit_part(msg_start, PartType::MsgLimit, index - 1, limit_len, nesting);
                if parent == ArgType::Choice {
                    return Ok(index - 1);
                }
                return Ok(index);
            }
        }
        if nesting > 0 {
            return Err(UErrorCode::UNMATCHED_BRACES);
        }
        self.add_limit_part(msg_start, PartType::MsgLimit, index, 0, nesting);
        Ok(index)
    }

    fn parse_arg_number(&self, start: usize, limit: usize) -> i32 {
        const NOT_NUMBER: i32 = -1;
        const NOT_VALID: i32 = -2;
        if start >= limit {
            return NOT_VALID;
        }
        let mut i = start;
        let c = self.msg[i];
        i += 1;
        let (mut number, mut bad) = if c == 0x30 {
            if i == limit {
                return 0;
            }
            (0i32, true)
        } else if (0x31..=0x39).contains(&c) {
            (i32::from(c - 0x30), false)
        } else {
            return NOT_NUMBER;
        };
        while i < limit {
            let c = self.msg[i];
            i += 1;
            if (0x30..=0x39).contains(&c) {
                if number >= i32::MAX / 10 {
                    bad = true;
                }
                number = number.wrapping_mul(10).wrapping_add(i32::from(c - 0x30));
            } else {
                return NOT_NUMBER;
            }
        }
        if bad {
            NOT_VALID
        } else {
            number
        }
    }

    fn eq_ignore_case(&self, at: usize, word: &str) -> bool {
        word.bytes().enumerate().all(|(k, b)| {
            let c = self.at(at + k);
            c < 0x80 && u8::try_from(c).is_ok_and(|c| c.to_ascii_lowercase() == b)
        })
    }

    fn parse_arg(&mut self, index: usize, start_len: usize, nesting: i32) -> PResult<usize> {
        let arg_start = self.parts.len();
        self.add_part(PartType::ArgStart, index, start_len, 0);
        let name_index = self.skip_white(index + start_len);
        let len = self.msg.len();
        if name_index == len {
            return Err(UErrorCode::UNMATCHED_BRACES);
        }
        let mut index = self.skip_identifier(name_index);
        let number = self.parse_arg_number(name_index, index);
        if number >= 0 {
            self.has_arg_numbers = true;
            self.add_part(PartType::ArgNumber, name_index, index - name_index, number);
        } else if number == -1 {
            self.has_arg_names = true;
            self.add_part(PartType::ArgName, name_index, index - name_index, 0);
        } else {
            return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
        }
        index = self.skip_white(index);
        if index == len {
            return Err(UErrorCode::UNMATCHED_BRACES);
        }
        let mut c = self.msg[index];
        let mut arg_type = ArgType::None;
        if c == RBRACE {
            // done
        } else if c != u16::from(b',') {
            return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
        } else {
            let type_index = self.skip_white(index + 1);
            index = type_index;
            while index < len && self.msg[index] < 0x80 && (self.msg[index] as u8).is_ascii_alphabetic() {
                index += 1;
            }
            let length = index - type_index;
            index = self.skip_white(index);
            if index == len {
                return Err(UErrorCode::UNMATCHED_BRACES);
            }
            c = self.msg[index];
            if length == 0 || (c != u16::from(b',') && c != RBRACE) {
                return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
            }
            arg_type = ArgType::Simple;
            if length == 6 {
                if self.eq_ignore_case(type_index, "choice") {
                    arg_type = ArgType::Choice;
                } else if self.eq_ignore_case(type_index, "plural") {
                    arg_type = ArgType::Plural;
                } else if self.eq_ignore_case(type_index, "select") {
                    arg_type = ArgType::Select;
                }
            } else if length == 13
                && self.eq_ignore_case(type_index, "select")
                && self.eq_ignore_case(type_index + 6, "ordinal")
            {
                arg_type = ArgType::SelectOrdinal;
            }
            self.parts[arg_start].arg_type = arg_type;
            if arg_type == ArgType::Simple {
                self.add_part(PartType::ArgType, type_index, length, 0);
            }
            if c == RBRACE {
                if arg_type != ArgType::Simple {
                    return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
                }
            } else {
                index += 1;
                index = match arg_type {
                    ArgType::Simple => self.parse_simple_style(index)?,
                    ArgType::Choice => self.parse_choice_style(index, nesting)?,
                    _ => self.parse_plural_or_select_style(arg_type, index, nesting)?,
                };
            }
        }
        self.add_limit_part(arg_start, PartType::ArgLimit, index, 1, 0);
        self.parts.last_mut().map_or((), |p| p.arg_type = arg_type);
        Ok(index + 1)
    }

    fn parse_simple_style(&mut self, index: usize) -> PResult<usize> {
        let start = index;
        let mut index = index;
        let mut nested = 0;
        while index < self.msg.len() {
            let c = self.msg[index];
            index += 1;
            if c == APOS {
                index = self.index_of(APOS, index).ok_or(UErrorCode::PATTERN_SYNTAX_ERROR)? + 1;
            } else if c == LBRACE {
                nested += 1;
            } else if c == RBRACE {
                if nested > 0 {
                    nested -= 1;
                } else {
                    index -= 1;
                    self.add_part(PartType::ArgStyle, start, index - start, 0);
                    return Ok(index);
                }
            }
        }
        Err(UErrorCode::UNMATCHED_BRACES)
    }

    fn parse_choice_style(&mut self, index: usize, nesting: i32) -> PResult<usize> {
        let mut index = self.skip_white(index);
        let len = self.msg.len();
        if index == len || self.msg[index] == RBRACE {
            return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
        }
        loop {
            let number_index = index;
            index = self.skip_double(index);
            if index == number_index {
                return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
            }
            self.parse_double(number_index, index, true)?;
            index = self.skip_white(index);
            if index == len {
                return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
            }
            let c = self.msg[index];
            if !(c == POUND || c == u16::from(b'<') || c == 0x2264) {
                return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
            }
            self.add_part(PartType::ArgSelector, index, 1, 0);
            index = self.parse_message(index + 1, 0, nesting + 1, ArgType::Choice)?;
            if index == len {
                return Ok(index);
            }
            if self.msg[index] == RBRACE {
                return Ok(index);
            }
            index = self.skip_white(index + 1);
        }
    }

    fn parse_plural_or_select_style(&mut self, arg_type: ArgType, index: usize, nesting: i32) -> PResult<usize> {
        let mut index = index;
        let mut is_empty = true;
        let mut has_other = false;
        let len = self.msg.len();
        let other: Vec<u16> = "other".encode_utf16().collect();
        let offset_colon: Vec<u16> = "offset:".encode_utf16().collect();
        loop {
            index = self.skip_white(index);
            let eos = index == len;
            if eos || self.msg[index] == RBRACE {
                if eos || !has_other {
                    return Err(if has_other {
                        UErrorCode::PATTERN_SYNTAX_ERROR
                    } else {
                        UErrorCode::DEFAULT_KEYWORD_MISSING
                    });
                }
                return Ok(index);
            }
            let selector_index = index;
            if arg_type.has_plural_style() && self.msg[selector_index] == u16::from(b'=') {
                index = self.skip_double(index + 1);
                let length = index - selector_index;
                if length == 1 {
                    return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
                }
                self.add_part(PartType::ArgSelector, selector_index, length, 0);
                self.parse_double(selector_index + 1, index, false)?;
            } else {
                index = self.skip_identifier(index);
                let length = index - selector_index;
                if length == 0 {
                    return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
                }
                if arg_type.has_plural_style()
                    && length == 6
                    && index < len
                    && self.msg.get(selector_index..selector_index + 7) == Some(&offset_colon[..])
                {
                    if !is_empty {
                        return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
                    }
                    let value_index = self.skip_white(index + 1);
                    index = self.skip_double(value_index);
                    if index == value_index {
                        return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
                    }
                    self.parse_double(value_index, index, false)?;
                    is_empty = false;
                    continue;
                }
                self.add_part(PartType::ArgSelector, selector_index, length, 0);
                if self.msg[selector_index..index] == other[..] {
                    has_other = true;
                }
            }
            index = self.skip_white(index);
            if index == len || self.msg[index] != LBRACE {
                return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
            }
            index = self.parse_message(index, 1, nesting + 1, arg_type)?;
            is_empty = false;
        }
    }

    fn parse_double(&mut self, start: usize, limit: usize, allow_infinity: bool) -> PResult<()> {
        let mut index = start;
        let mut neg = false;
        let mut c = self.msg[index];
        index += 1;
        if c == u16::from(b'-') || c == u16::from(b'+') {
            neg = c == u16::from(b'-');
            if index == limit {
                return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
            }
            c = self.msg[index];
            index += 1;
        }
        if c == 0x221e {
            if allow_infinity && index == limit {
                let v = if neg { f64::NEG_INFINITY } else { f64::INFINITY };
                self.add_part(PartType::ArgDouble, start, limit - start, 0);
                if let Some(p) = self.parts.last_mut() {
                    p.double = v;
                }
                return Ok(());
            }
            return Err(UErrorCode::PATTERN_SYNTAX_ERROR);
        }
        let mut value: i32 = 0;
        while (0x30..=0x39).contains(&c) {
            value = value * 10 + i32::from(c - 0x30);
            if value > MAX_VALUE + i32::from(neg) {
                break;
            }
            if index == limit {
                self.add_part(PartType::ArgInt, start, limit - start, if neg { -value } else { value });
                return Ok(());
            }
            c = self.msg[index];
            index += 1;
        }
        let text: String = String::from_utf16_lossy(&self.msg[start..limit]);
        let v = c_strtod_full(&text).ok_or(UErrorCode::PATTERN_SYNTAX_ERROR)?;
        self.add_part(PartType::ArgDouble, start, limit - start, 0);
        if let Some(p) = self.parts.last_mut() {
            p.double = v;
        }
        Ok(())
    }
}

/// `strtod()` that must consume the whole string.
fn c_strtod_full(s: &str) -> Option<f64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E')) {
        return None;
    }
    s.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> MessagePattern {
        MessagePattern::parse(&s.encode_utf16().collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn simple() {
        let p = parse("Hello {0}, you have {1,number,integer} items");
        let types: Vec<_> = p.parts().iter().map(|p| p.ty).collect();
        assert_eq!(
            types,
            vec![
                PartType::MsgStart,
                PartType::ArgStart,
                PartType::ArgNumber,
                PartType::ArgLimit,
                PartType::ArgStart,
                PartType::ArgNumber,
                PartType::ArgType,
                PartType::ArgStyle,
                PartType::ArgLimit,
                PartType::MsgLimit
            ]
        );
        assert_eq!(p.part(0).limit_part, 9);
    }

    #[test]
    fn plural_and_quotes() {
        let p = parse("{n, plural, offset:1 =0{none} one{# '{'x'}' it''s} other{# items}}");
        assert!(p.has_named_arguments());
        assert!(p.parts().iter().any(|x| x.ty == PartType::ReplaceNumber));
        assert!((p.plural_offset(3) - 1.0).abs() < f64::EPSILON);
        let c = parse("{0,choice,0#none|1#one|1<many}");
        assert!(c.parts().iter().filter(|x| x.ty == PartType::ArgSelector).count() == 3);
    }
}
