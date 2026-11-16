// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `Locale` / `locale_*` (php-src `ext/intl/locale/locale_methods.cpp`).

use crate::common::{self, INTL_MAX_LOCALE_LEN, ULOC_FULLNAME_CAPACITY};
use crate::error::{Error, Result, UErrorCode};
use crate::sys::loc::{self, Display, Part};
use crate::ustr;

/// `Locale::LANG_TAG` etc.
pub mod tags {
    /// `LANG_TAG`
    pub const LANG: &str = "language";
    /// `EXTLANG_TAG`
    pub const EXTLANG: &str = "extlang";
    /// `SCRIPT_TAG`
    pub const SCRIPT: &str = "script";
    /// `REGION_TAG`
    pub const REGION: &str = "region";
    /// `VARIANT_TAG`
    pub const VARIANT: &str = "variant";
    /// `GRANDFATHERED_LANG_TAG`
    pub const GRANDFATHERED: &str = "grandfathered";
    /// `PRIVATE_TAG`
    pub const PRIVATE: &str = "private";
}

const CANONICALIZE: &str = "canonicalize";

/// IANA grandfathered tags (php-src `LOC_GRANDFATHERED`).
const GRANDFATHERED: [&str; 50] = [
    "art-lojban",
    "cel-gaulish",
    "en-GB-oed",
    "i-ami",
    "i-bnn",
    "i-default",
    "i-enochian",
    "i-hak",
    "i-klingon",
    "i-lux",
    "i-mingo",
    "i-navajo",
    "i-pwn",
    "i-tao",
    "i-tay",
    "i-tsu",
    "no-bok",
    "no-nyn",
    "sgn-BE-FR",
    "sgn-BE-NL",
    "sgn-BR",
    "sgn-CH-DE",
    "sgn-CO",
    "sgn-DE",
    "sgn-DK",
    "sgn-ES",
    "sgn-FR",
    "sgn-GB",
    "sgn-GR",
    "sgn-IE",
    "sgn-IT",
    "sgn-JP",
    "sgn-MX",
    "sgn-NI",
    "sgn-NL",
    "sgn-NO",
    "sgn-PT",
    "sgn-SE",
    "sgn-US",
    "sgn-ZA",
    "zh-cmn",
    "zh-cmn-Hans",
    "zh-cmn-Hant",
    "zh-gan",
    "zh-guoyu",
    "zh-hakka",
    "zh-min",
    "zh-min-nan",
    "zh-wuu",
    "zh-xiang",
];

/// Preferred values (php-src `LOC_PREFERRED_GRANDFATHERED`).
const PREFERRED: [Option<&str>; 50] = [
    Some("jbo"),
    None,
    Some("en-GB-oxendict"),
    Some("ami"),
    Some("bnn"),
    None,
    None,
    Some("hak"),
    Some("tlh"),
    Some("lb"),
    None,
    Some("nv"),
    Some("pwn"),
    Some("tao"),
    Some("tay"),
    Some("tsu"),
    Some("nb"),
    Some("nn"),
    Some("sfb"),
    Some("vgt"),
    Some("bzs"),
    Some("sgg"),
    Some("csn"),
    Some("gsg"),
    Some("dsl"),
    Some("ssp"),
    Some("fsl"),
    Some("bfi"),
    Some("gss"),
    Some("isg"),
    Some("ise"),
    Some("jsl"),
    Some("mfs"),
    Some("ncs"),
    Some("dse"),
    Some("nsl"),
    Some("psr"),
    Some("swl"),
    Some("ase"),
    Some("sfs"),
    Some("cmn"),
    Some("cmn-Hans"),
    Some("cmn-Hant"),
    Some("gan"),
    Some("cmn"),
    Some("hak"),
    None,
    Some("nan"),
    Some("wuu"),
    Some("hsn"),
];

fn find_grandfathered(key: &str) -> Option<usize> {
    GRANDFATHERED
        .iter()
        .position(|g| g.eq_ignore_ascii_case(key))
}

fn preferred_tag(tag: &str) -> Option<String> {
    let i = find_grandfathered(tag)?;
    Some(PREFERRED[i].unwrap_or(GRANDFATHERED[i]).to_owned())
}

fn is_id_sep(c: u8) -> bool {
    c == b'_' || c == b'-'
}

/// `getSingletonPos()`.
fn singleton_pos(s: &[u8]) -> Option<usize> {
    let len = s.len();
    for i in 0..len {
        if is_id_sep(s[i]) {
            if i == 1 {
                return Some(0);
            }
            if i + 2 < len && is_id_sep(s[i + 2]) {
                return Some(i + 1);
            }
        }
    }
    None
}

/// `getStrrtokenPos()`.
fn strr_token_pos(s: &[u8], saved: usize) -> usize {
    let mut result: isize = -1;
    let mut i = saved;
    while i > 0 {
        i -= 1;
        if is_id_sep(s[i]) || s[i] == b'@' {
            #[allow(clippy::cast_possible_wrap)]
            {
                result = if i >= 2 && is_id_sep(s[i - 2]) {
                    i as isize - 2
                } else {
                    i as isize
                };
            }
            break;
        }
    }
    if result < 1 {
        0
    } else {
        usize::try_from(result).unwrap_or(0)
    }
}

/// Result of `get_icu_value_internal()`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IcuValue {
    /// `result == 1`
    Found(String),
    /// A value returned without setting `result` (grandfathered and
    /// `x-`/`i-` prefixed tags): getters return it, `parseLocale` skips it.
    FoundUnset(String),
    /// `result == -1`
    None,
    /// `result == 0`
    Error,
}

/// `get_icu_value_internal(loc_name, tag_name, &result, fromParseLocale)`.
fn icu_value_internal(loc_name: &str, tag: &str, from_parse: bool) -> IcuValue {
    if loc_name.len() > INTL_MAX_LOCALE_LEN {
        return IcuValue::Error;
    }
    let mut mod_loc = loc_name.to_owned();
    if tag != CANONICALIZE {
        if find_grandfathered(loc_name).is_some() {
            return if tag == tags::LANG {
                IcuValue::FoundUnset(loc_name.to_owned())
            } else {
                IcuValue::Error
            };
        }
        if from_parse {
            let b = loc_name.as_bytes();
            if tag == tags::LANG
                && b.len() > 1
                && matches!(b[0], b'x' | b'X' | b'i' | b'I')
                && is_id_sep(b[1])
            {
                return IcuValue::FoundUnset(loc_name.to_owned());
            }
            match singleton_pos(b) {
                Some(0) => return IcuValue::Error,
                Some(p) => mod_loc = String::from_utf8_lossy(&b[..p - 1]).into_owned(),
                None => {}
            }
        }
    }
    let part = match tag {
        tags::SCRIPT => Part::Script,
        tags::LANG => Part::Language,
        tags::REGION => Part::Country,
        tags::VARIANT => Part::Variant,
        _ => Part::Canonicalize,
    };
    let mut cap = 512;
    loop {
        let (v, n, st) = loc::get_part_raw(part, &mod_loc, cap);
        if st == UErrorCode::BUFFER_OVERFLOW_ERROR {
            cap = usize::try_from(n).unwrap_or(cap) + 1;
            continue;
        }
        if st.is_failure() {
            return IcuValue::Error;
        }
        if n == 0 {
            return IcuValue::None;
        }
        return IcuValue::Found(v);
    }
}

/// The "found" grandfathered case of `icu_value_internal` returns `result
/// == 0` for non-language tags, which the getters report as `null` plus an
/// error; we keep that behaviour.
fn get_value(locale: &str, tag: &str) -> Result<Option<String>> {
    let loc = common::locale_or_default(locale);
    common::check_locale_len(&loc)
        .map_err(|e| crate::timezone::rescope(e, crate::Scope::Global))?;
    match icu_value_internal(&loc, tag, false) {
        IcuValue::Found(v) | IcuValue::FoundUnset(v) => Ok(Some(v)),
        IcuValue::None => Ok(Some(String::new())),
        IcuValue::Error => Err(Error::global(
            UErrorCode::ZERO,
            format!("unable to get locale {tag}"),
        )),
    }
}

/// `Locale::getDefault()`.
pub fn get_default() -> String {
    common::default_locale()
}

/// `Locale::setDefault($locale)`: the value the VM stores into
/// `intl.default_locale` (an empty locale means ICU's default).
pub fn set_default_value(locale: &str) -> String {
    if locale.is_empty() {
        common::icu_default_locale()
    } else {
        locale.to_owned()
    }
}

/// `Locale::getPrimaryLanguage()`; `Ok(None)` never happens (errors are
/// `null` + intl error).
pub fn get_primary_language(locale: &str) -> Result<Option<String>> {
    get_value(locale, tags::LANG)
}

/// `Locale::getScript()`.
pub fn get_script(locale: &str) -> Result<Option<String>> {
    get_value(locale, tags::SCRIPT)
}

/// `Locale::getRegion()`.
pub fn get_region(locale: &str) -> Result<Option<String>> {
    get_value(locale, tags::REGION)
}

/// `Locale::canonicalize()`.
pub fn canonicalize(locale: &str) -> Result<Option<String>> {
    get_value(locale, CANONICALIZE)
}

/// `Locale::getKeywords()` (`Ok(None)` → `null`: no keywords).
pub fn get_keywords(locale: &str) -> Result<Option<Vec<(String, String)>>> {
    common::check_locale_len(locale)
        .map_err(|e| crate::timezone::rescope(e, crate::Scope::Global))?;
    let loc = common::locale_or_default(locale);
    match loc::keywords(&loc) {
        Ok(v) => Ok(v),
        Err(_) => Err(Error::global(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "Error encountered while getting the keyword value for the keyword",
        )),
    }
}

fn display_value(
    kind: Display,
    tag: &str,
    locale: &str,
    in_locale: Option<&str>,
) -> Result<Option<String>> {
    if locale.len() > ULOC_FULLNAME_CAPACITY {
        return Err(Error::global(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "name too long",
        ));
    }
    let loc = common::locale_or_default(locale);
    let mut mod_loc = loc.clone();
    if kind != Display::Name && find_grandfathered(&loc).is_some() {
        if kind == Display::Language {
            mod_loc = preferred_tag(&loc).unwrap_or(loc);
        } else {
            return Ok(None);
        }
    }
    let disp = in_locale.map_or_else(common::default_locale, str::to_owned);
    let (v, st) = loc::display(kind, &mod_loc, &disp);
    if st.is_failure() {
        return Err(Error::global(st, format!("unable to get locale {tag}")));
    }
    ustr::from_u16(&v).map(Some).map_err(|c| {
        Error::global(
            c,
            format!("error converting display name for {tag} to UTF-8"),
        )
    })
}

/// `Locale::getDisplayName()` (`Ok(None)` → `false` without error).
pub fn get_display_name(locale: &str, in_locale: Option<&str>) -> Result<Option<String>> {
    display_value(Display::Name, "name", locale, in_locale)
}

/// `Locale::getDisplayLanguage()`.
pub fn get_display_language(locale: &str, in_locale: Option<&str>) -> Result<Option<String>> {
    display_value(Display::Language, tags::LANG, locale, in_locale)
}

/// `Locale::getDisplayScript()`.
pub fn get_display_script(locale: &str, in_locale: Option<&str>) -> Result<Option<String>> {
    display_value(Display::Script, tags::SCRIPT, locale, in_locale)
}

/// `Locale::getDisplayRegion()`.
pub fn get_display_region(locale: &str, in_locale: Option<&str>) -> Result<Option<String>> {
    display_value(Display::Region, tags::REGION, locale, in_locale)
}

/// `Locale::getDisplayVariant()`.
pub fn get_display_variant(locale: &str, in_locale: Option<&str>) -> Result<Option<String>> {
    display_value(Display::Variant, tags::VARIANT, locale, in_locale)
}

/// `Locale::getDisplayKeyword($keyword, $displayLocale)`.
pub fn get_display_keyword(keyword: &str, in_locale: Option<&str>) -> Result<String> {
    let disp = in_locale.map_or_else(common::default_locale, str::to_owned);
    let v = loc::display_keyword(keyword, &disp)
        .map_err(|c| Error::global(c, "unable to get locale keyword"))?;
    ustr::from_u16(&v)
        .map_err(|c| Error::global(c, "error converting display name for keyword to UTF-8"))
}

/// `Locale::getDisplayKeywordValue($locale, $keyword, $displayLocale)`.
pub fn get_display_keyword_value(
    locale: &str,
    keyword: &str,
    in_locale: Option<&str>,
) -> Result<String> {
    if locale.len() > ULOC_FULLNAME_CAPACITY {
        return Err(Error::global(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "name too long",
        ));
    }
    let loc = common::locale_or_default(locale);
    let disp = in_locale.map_or_else(common::default_locale, str::to_owned);
    let v = loc::display_keyword_value(&loc, keyword, &disp)
        .map_err(|c| Error::global(c, "unable to get locale keyword_value"))?;
    ustr::from_u16(&v).map_err(|c| {
        Error::global(
            c,
            "error converting display name for keyword_value to UTF-8",
        )
    })
}

/// One element of the `composeLocale()` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subtag<'a> {
    /// A string.
    Str(&'a str),
    /// An array of values (`None` = a non-string element).
    List(Vec<Option<&'a str>>),
    /// Any other type.
    Other,
}

/// `Locale::composeLocale($subtags)`.  `get` looks a key up in the PHP
/// array; `len` is its element count.  An empty array yields
/// `Error::code(U_ZERO_ERROR)` (php returns `false` silently).
pub fn compose<'a>(len: usize, get: &dyn Fn(&str) -> Option<Subtag<'a>>) -> Result<String> {
    if len == 0 {
        return Err(Error::code(UErrorCode::ZERO));
    }
    let not_string = || {
        Error::global(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "parameter array element is not a string",
        )
    };
    let mut out = String::new();
    // grandfathered
    match get(tags::GRANDFATHERED) {
        Some(Subtag::Str(s)) => return Ok(s.to_owned()),
        Some(_) => return Err(not_string()),
        None => {}
    }
    match get(tags::LANG) {
        None => return Err(Error::arg_value(1, "must contain a \"language\" key")),
        Some(Subtag::Str(s)) => out.push_str(s),
        Some(_) => return Err(not_string()),
    }
    let multiple = |out: &mut String, key: &str, max: usize| -> Result<()> {
        let prefix = |out: &mut String, k: &str| {
            if k.starts_with(tags::PRIVATE) {
                out.push_str("_x");
            }
        };
        match get(key) {
            Some(Subtag::Str(s)) => {
                prefix(out, key);
                out.push('_');
                out.push_str(s);
            }
            Some(Subtag::List(items)) => {
                for (n, it) in items.iter().enumerate() {
                    let Some(s) = it else {
                        return Err(not_string());
                    };
                    if n == 0 {
                        prefix(out, key);
                    }
                    out.push('_');
                    out.push_str(s);
                }
            }
            Some(Subtag::Other) => return Err(not_string()),
            None => {
                let mut first = true;
                for i in 0..max {
                    let k = format!("{key}{i}");
                    match get(&k) {
                        Some(Subtag::Str(s)) => {
                            if first {
                                prefix(out, &k);
                                first = false;
                            }
                            out.push('_');
                            out.push_str(s);
                        }
                        Some(_) => return Err(not_string()),
                        None => {}
                    }
                }
            }
        }
        Ok(())
    };
    multiple(&mut out, tags::EXTLANG, 3)?;
    for key in [tags::SCRIPT, tags::REGION] {
        match get(key) {
            Some(Subtag::Str(s)) => {
                out.push('_');
                out.push_str(s);
            }
            Some(_) => return Err(not_string()),
            None => {}
        }
    }
    multiple(&mut out, tags::VARIANT, 15)?;
    multiple(&mut out, tags::PRIVATE, 15)?;
    Ok(out)
}

/// `get_private_subtags()`.
fn private_subtags(loc: &str) -> Option<String> {
    let mut s = loc.as_bytes();
    while let Some(p) = singleton_pos(s) {
        if s[p] == b'x' || s[p] == b'X' {
            if p + 2 == s.len() {
                return None;
            }
            return Some(String::from_utf8_lossy(s.get(p + 2..).unwrap_or(&[])).into_owned());
        }
        if p + 1 >= s.len() {
            break;
        }
        s = &s[p + 1..];
    }
    None
}

/// `php_strtok_r` over `"-_"` with php-src's "stop at singleton" rule.
fn tokenize_until_singleton(v: &str) -> Vec<String> {
    let mut toks = v.split(['-', '_']).filter(|t| !t.is_empty());
    let mut out = Vec::new();
    if let Some(first) = toks.next() {
        out.push(first.to_owned());
        for t in toks {
            if t.len() <= 1 {
                break;
            }
            out.push(t.to_owned());
        }
    }
    out
}

/// `Locale::parseLocale($locale)` → ordered `(key, value)` pairs.
pub fn parse(locale: &str) -> Result<Vec<(String, String)>> {
    common::check_locale_len(locale)
        .map_err(|e| crate::timezone::rescope(e, crate::Scope::Global))?;
    let loc = common::locale_or_default(locale);
    let mut out = Vec::new();
    if find_grandfathered(&loc).is_some() {
        out.push((tags::GRANDFATHERED.to_owned(), loc));
        return Ok(out);
    }
    for tag in [
        tags::LANG,
        tags::SCRIPT,
        tags::REGION,
        tags::VARIANT,
        tags::PRIVATE,
    ] {
        let value = if tag == tags::PRIVATE {
            private_subtags(&loc)
        } else {
            match icu_value_internal(&loc, tag, true) {
                IcuValue::Found(v) => Some(v),
                _ => None,
            }
        };
        let Some(v) = value else { continue };
        if tag == tags::PRIVATE || tag == tags::VARIANT {
            for (i, t) in tokenize_until_singleton(&v).into_iter().enumerate() {
                out.push((format!("{tag}{i}"), t));
            }
        } else {
            out.push((tag.to_owned(), v));
        }
    }
    Ok(out)
}

/// `Locale::getAllVariants($locale)`.
pub fn get_all_variants(locale: &str) -> Result<Vec<String>> {
    let loc = common::locale_or_default(locale);
    common::check_locale_len(&loc)
        .map_err(|e| crate::timezone::rescope(e, crate::Scope::Global))?;
    if find_grandfathered(&loc).is_some() {
        return Ok(Vec::new());
    }
    match icu_value_internal(&loc, tags::VARIANT, false) {
        IcuValue::Found(v) => Ok(tokenize_until_singleton(&v)),
        _ => Ok(Vec::new()),
    }
}

/// `strToMatch()`: lowercase, `-` → `_`.
fn str_to_match(s: &str) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    Some(
        s.bytes()
            .map(|b| {
                if b == b'-' {
                    '_'
                } else {
                    char::from(b.to_ascii_lowercase())
                }
            })
            .collect(),
    )
}

/// `Locale::filterMatches($languageTag, $locale, $canonicalize)`.
pub fn filter_matches(lang_tag: &str, loc_range: &str, canonical: bool) -> Result<bool> {
    let loc_range = common::locale_or_default(loc_range);
    if loc_range == "*" {
        return Ok(true);
    }
    let rescope = |e| crate::timezone::rescope(e, crate::Scope::Global);
    common::check_locale_len(&loc_range).map_err(rescope)?;
    common::check_locale_len(lang_tag).map_err(rescope)?;
    let (tag, range) = if canonical {
        let IcuValue::Found(r) = icu_value_internal(&loc_range, CANONICALIZE, false) else {
            return Err(Error::global(
                UErrorCode::ZERO,
                "unable to canonicalize loc_range",
            ));
        };
        let IcuValue::Found(t) = icu_value_internal(lang_tag, CANONICALIZE, false) else {
            return Err(Error::global(
                UErrorCode::ZERO,
                "unable to canonicalize lang_tag",
            ));
        };
        (t, r)
    } else {
        (lang_tag.to_owned(), loc_range)
    };
    let (Some(t), Some(r)) = (str_to_match(&tag), str_to_match(&range)) else {
        return Ok(false);
    };
    if let Some(rest) = t.strip_prefix(r.as_str()) {
        let next = rest.bytes().next();
        let ok = match next {
            None => true,
            Some(c) => is_id_sep(c) || (canonical && c == b'@'),
        };
        return Ok(ok);
    }
    Ok(false)
}

/// One element of the `lookup()` array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupEntry<'a> {
    /// A string.
    Str(&'a [u8]),
    /// Anything else (`TypeError`).
    NonString,
}

/// `Locale::lookup($languageTag, $locale, $canonicalize, $defaultLocale)`.
pub fn lookup(
    tags_list: &[LookupEntry<'_>],
    loc_range: &str,
    canonical: bool,
    fallback: Option<&str>,
) -> Result<Option<String>> {
    let range = if loc_range.is_empty() {
        fallback.map_or_else(common::default_locale, str::to_owned)
    } else {
        loc_range.to_owned()
    };
    common::check_locale_len(&range)
        .map_err(|e| crate::timezone::rescope(e, crate::Scope::Global))?;
    if tags_list.is_empty() {
        return Ok(Some(String::new()));
    }
    let mut cur: Vec<(String, String)> = Vec::with_capacity(tags_list.len());
    for t in tags_list {
        let LookupEntry::Str(s) = t else {
            return Err(Error::arg_type(2, "must only contain string values"));
        };
        if s.contains(&0) {
            return Err(Error::arg_value(2, "must not contain any null bytes"));
        }
        let orig = String::from_utf8_lossy(s).into_owned();
        let m = str_to_match(&orig).ok_or_else(|| {
            Error::global(
                UErrorCode::ILLEGAL_ARGUMENT_ERROR,
                "unable to canonicalize lang_tag",
            )
        })?;
        cur.push((m, orig));
    }
    let canon_fail = |m: &'static str| Error::global(UErrorCode::ILLEGAL_ARGUMENT_ERROR, m);
    if canonical {
        for e in &mut cur {
            match icu_value_internal(&e.0, CANONICALIZE, false) {
                IcuValue::Found(v) if !v.is_empty() => {
                    e.0 = str_to_match(&v)
                        .ok_or_else(|| canon_fail("unable to canonicalize lang_tag"))?;
                }
                _ => return Err(canon_fail("unable to canonicalize lang_tag")),
            }
        }
    }
    let range = if canonical {
        match icu_value_internal(&range, CANONICALIZE, false) {
            IcuValue::Found(v) if !v.is_empty() => v,
            _ => return Err(canon_fail("unable to canonicalize loc_range")),
        }
    } else {
        range
    };
    let cur_range =
        str_to_match(&range).ok_or_else(|| canon_fail("unable to canonicalize lang_tag"))?;
    let rb = cur_range.as_bytes();
    let mut saved = rb.len();
    let mut found = None;
    'outer: while saved > 0 {
        for (m, orig) in &cur {
            if m.len() == saved && rb[..saved] == m.as_bytes()[..saved] {
                found = Some(if canonical { m.clone() } else { orig.clone() });
                break 'outer;
            }
        }
        saved = strr_token_pos(rb, saved);
    }
    match found {
        Some(f) if !f.is_empty() => Ok(Some(f)),
        _ => Ok(Some(fallback.map_or_else(String::new, str::to_owned))),
    }
}

/// One element of `acceptFromHttp()`'s `$availableLocales`.
pub type AvailableEntry<'a> = LookupEntry<'a>;

/// `Locale::acceptFromHttp($header, $availableLocales)`.  `Ok(None)` →
/// `false` without error.
pub fn accept_from_http(
    header: &[u8],
    available: Option<&[AvailableEntry<'_>]>,
) -> Result<Option<String>> {
    let mut list: Vec<String> = Vec::new();
    let mut canonical: Vec<String> = Vec::new();
    if let Some(av) = available {
        if av.is_empty() {
            return Err(Error::arg_value(2, "must not be empty"));
        }
        for e in av {
            let LookupEntry::Str(s) = e else {
                return Err(Error::arg_type(2, "must only contain string values"));
            };
            if s.contains(&0) {
                return Err(Error::arg_value(2, "must not contain any null bytes"));
            }
            if s.len() > INTL_MAX_LOCALE_LEN {
                return Err(Error::arg_value(
                    2,
                    format!(
                        "must not contain locales longer than {INTL_MAX_LOCALE_LEN} characters"
                    ),
                ));
            }
            let st = String::from_utf8_lossy(s).into_owned();
            let canon = loc::name(&st)
                .map_err(|_| Error::arg_value(2, "must only contain valid locales"))?;
            list.push(st);
            canonical.push(canon);
        }
    }
    if header.len() > ULOC_FULLNAME_CAPACITY
        && header
            .split(|&b| b == b',')
            .any(|f| f.len() > ULOC_FULLNAME_CAPACITY)
    {
        return Err(Error::global(
            UErrorCode::ILLEGAL_ARGUMENT_ERROR,
            "locale string too long",
        ));
    }
    let h = String::from_utf8_lossy(header);
    let refs: Vec<&str> = list.iter().map(String::as_str).collect();
    let (result, outres, st) =
        loc::accept_language_from_http(&h, available.map(|_| refs.as_slice()), INTL_MAX_LOCALE_LEN);
    if outres == -1 {
        return Err(Error::global(st, "failed to retrieve locale list"));
    }
    if st.is_failure() {
        return Err(Error::global(st, "failed to find acceptable locale"));
    }
    if outres == -2 || outres == 0 {
        return Ok(None);
    }
    for (i, l) in list.iter().enumerate() {
        if *l == result || canonical[i] == result {
            return Ok(Some(l.clone()));
        }
    }
    Ok(Some(result))
}

/// `Locale::isRightToLeft($locale)`.
pub fn is_right_to_left(locale: &str) -> bool {
    loc::is_right_to_left(&common::locale_or_default(locale))
}

fn fixed_subtags(locale: &str, add: bool) -> Result<String> {
    let loc = common::locale_or_default(locale);
    let r = if add {
        loc::add_likely_subtags(&loc)
    } else {
        loc::minimize_subtags(&loc)
    };
    match r {
        Ok(v) if v.len() < ULOC_FULLNAME_CAPACITY => Ok(v),
        Ok(_) => Err(Error::global(
            UErrorCode::BUFFER_OVERFLOW_ERROR,
            "invalid locale",
        )),
        Err(c) => Err(Error::global(c, "invalid locale")),
    }
}

/// `Locale::addLikelySubtags($locale)`.
pub fn add_likely_subtags(locale: &str) -> Result<String> {
    fixed_subtags(locale, true)
}

/// `Locale::minimizeSubtags($locale)`.
pub fn minimize_subtags(locale: &str) -> Result<String> {
    fixed_subtags(locale, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getters() {
        assert_eq!(get_primary_language("de_DE").unwrap().unwrap(), "de");
        assert_eq!(get_script("zh-Hant-TW").unwrap().unwrap(), "Hant");
        assert_eq!(get_region("en_US").unwrap().unwrap(), "US");
        assert_eq!(get_region("en").unwrap().unwrap(), "");
        assert_eq!(
            get_primary_language("i-klingon").unwrap().unwrap(),
            "i-klingon"
        );
        assert_eq!(canonicalize("EN-us").unwrap().unwrap(), "en_US");
    }

    #[test]
    fn parse_and_compose() {
        let p = parse("sl_Latn_IT_nedis-x-prv1-prv2").unwrap();
        let keys: Vec<_> = p.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            ["language", "script", "region", "variant0", "private0", "private1"]
        );
        let get = |k: &str| -> Option<Subtag<'static>> {
            match k {
                "language" => Some(Subtag::Str("en")),
                "region" => Some(Subtag::Str("US")),
                "variant" => Some(Subtag::List(vec![Some("a"), Some("b")])),
                _ => None,
            }
        };
        assert_eq!(compose(3, &get).unwrap(), "en_US_a_b");
    }

    #[test]
    fn matching() {
        assert!(!filter_matches("de-DEVA", "de-DE", false).unwrap());
        assert!(filter_matches("de-DE-1996", "de-DE", false).unwrap());
        let tags = [
            LookupEntry::Str(b"de-DEVA"),
            LookupEntry::Str(b"de-DE-1996"),
            LookupEntry::Str(b"de"),
        ];
        assert_eq!(
            lookup(&tags, "de-DE-1996-x-prv1", false, None)
                .unwrap()
                .unwrap(),
            "de-DE-1996"
        );
    }
}
