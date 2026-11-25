// Derived from php-src ext/standard/browscap.c. Similarity metric to original: 26%
//   Copyright © The PHP Group and Contributors. Author: Zeev Suraski <zeev@php.net>
// Rust port: Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// SPDX-License-Identifier: BSD-3-Clause
//! `get_browser()`: the browscap.ini entry table and the user-agent
//! matcher (`ext/standard/browscap.c`).
//!
//! The binding feeds the INI parser's `INI_SCANNER_RAW` callbacks into
//! [`Browscap::section`] / [`Browscap::entry`] and asks
//! [`Browscap::lookup`] for the capabilities of an agent.

use std::collections::HashMap;

/// `BROWSCAP_NUM_CONTAINS`.
const NUM_CONTAINS: usize = 5;

/// `DEFAULT_SECTION_NAME`: the fallback when no pattern matches.
pub const DEFAULT_SECTION_NAME: &[u8] = b"Default Browser Capability Settings";

/// One `[pattern]` section (`browscap_entry`).
#[derive(Debug, Clone)]
struct Entry {
    pattern: Vec<u8>,
    parent: Option<Vec<u8>>,
    kv_start: usize,
    kv_end: usize,
    contains_start: [u16; NUM_CONTAINS],
    contains_len: [u8; NUM_CONTAINS],
    prefix_len: u8,
}

/// A diagnostic raised while loading the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadDiagnostic {
    /// `E_WARNING` "Skipping excessively long pattern of length N".
    LongPattern(usize),
    /// `E_CORE_ERROR`: a section is its own parent (the message names the
    /// section; the binding appends the file).
    SelfParent(Vec<u8>),
}

/// The parsed browscap data (`browser_data`).
#[derive(Debug, Clone, Default)]
pub struct Browscap {
    /// Entries in hash order (`zend_hash_update_ptr` keeps the position of
    /// a redefined section).
    entries: Vec<Entry>,
    /// Exact pattern -> index into `entries`.
    index: HashMap<Vec<u8>, usize>,
    /// Lower-cased keys and values of every entry (`bdata->kv`).
    kv: Vec<(Vec<u8>, Vec<u8>)>,
    /// The section receiving entries (`ctx->current_entry`).
    current: Option<usize>,
    /// Diagnostics raised by the callbacks.
    pub diagnostics: Vec<LoadDiagnostic>,
}

fn is_placeholder(c: u8) -> bool {
    c == b'?' || c == b'*'
}

/// `browscap_compute_prefix_len`.
fn compute_prefix_len(pattern: &[u8]) -> u8 {
    let n = pattern
        .iter()
        .position(|&c| is_placeholder(c))
        .unwrap_or(pattern.len());
    u8::try_from(n).unwrap_or(u8::MAX)
}

/// `browscap_compute_contains`: the next literal run of at least two
/// characters (or the last single one) after `start`.
fn compute_contains(pattern: &[u8], start: usize) -> (u16, u8, usize) {
    let mut i = start;
    while i < pattern.len() {
        if !is_placeholder(pattern[i]) && i + 1 < pattern.len() && !is_placeholder(pattern[i + 1]) {
            break;
        }
        i += 1;
    }
    let contains_start = i;
    while i < pattern.len() && !is_placeholder(pattern[i]) {
        i += 1;
    }
    (
        u16::try_from(contains_start).unwrap_or(u16::MAX),
        u8::try_from(i - contains_start).unwrap_or(u8::MAX),
        i,
    )
}

/// `browscap_convert_pattern`: the `browser_name_regex` value.
#[must_use]
pub fn convert_pattern(pattern: &[u8]) -> Vec<u8> {
    let mut t = Vec::with_capacity(pattern.len() * 2 + 4);
    t.extend_from_slice(b"~^");
    for &c in pattern {
        match c {
            b'?' => t.push(b'.'),
            b'*' => t.extend_from_slice(b".*"),
            b'.' | b'\\' | b'(' | b')' | b'~' | b'+' => {
                t.push(b'\\');
                t.push(c);
            }
            _ => t.push(c.to_ascii_lowercase()),
        }
    }
    t.extend_from_slice(b"$~");
    t
}

/// The value stored for an INI value: booleans become `"1"` / `""`.
fn normalize_value(v: &[u8]) -> Vec<u8> {
    let eq = |s: &str| v.eq_ignore_ascii_case(s.as_bytes());
    if eq("on") || eq("yes") || eq("true") {
        b"1".to_vec()
    } else if eq("no") || eq("off") || eq("none") || eq("false") {
        Vec::new()
    } else {
        v.to_vec()
    }
}

/// `browscap_match_string_wildcard`: `*` matches any run, `?` any byte.
fn match_wildcard(s: &[u8], pattern: &[u8]) -> bool {
    let (mut p, mut si) = (0usize, 0usize);
    // (pattern position after the `*`, subject position it skipped to).
    let mut restore: Option<(usize, usize)> = None;
    while si < s.len() {
        let pc = pattern.get(p).copied();
        if pc == Some(b'*') {
            p += 1;
            while p < pattern.len() && pattern[p] == b'*' {
                p += 1;
            }
            if p == pattern.len() {
                return true;
            }
            if pattern[p] != b'?' {
                while si < s.len() && s[si] != pattern[p] {
                    si += 1;
                }
            }
            restore = Some((p, si));
            continue;
        } else if pc == Some(s[si]) || pc == Some(b'?') {
            p += 1;
            si += 1;
            if p != pattern.len() {
                continue;
            }
            if si == s.len() {
                return true;
            }
        }
        // Mismatch: let the last `*` swallow one more byte.
        match restore.as_mut() {
            Some((rp, rs)) => {
                p = *rp;
                *rs += 1;
                si = *rs;
            }
            None => return false,
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

/// `zend_memnstr` from `from`: the position of `needle` in `hay`.
fn find_from(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    let rest = hay.get(from..)?;
    if needle.is_empty() {
        return Some(from);
    }
    rest.windows(needle.len())
        .position(|w| w == needle)
        .map(|i| from + i)
}

impl Entry {
    /// `browscap_get_minimum_length`.
    fn minimum_length(&self) -> usize {
        usize::from(self.prefix_len)
            + self
                .contains_len
                .iter()
                .map(|&l| usize::from(l))
                .sum::<usize>()
    }
}

/// The outcome of [`Browscap::compare`].
enum Compare {
    NoMatch,
    /// A wildcard match keeping `len` literal characters.
    Wildcard(usize),
    /// The lower-cased pattern equals the agent.
    Exact,
}

impl Browscap {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of sections.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no section was read.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `ZEND_INI_PARSER_SECTION`: starts the entry for `pattern`.
    pub fn section(&mut self, pattern: &[u8]) {
        if pattern.len() > usize::from(u16::MAX) {
            self.diagnostics
                .push(LoadDiagnostic::LongPattern(pattern.len()));
            return;
        }
        let prefix_len = compute_prefix_len(pattern);
        let mut pos = usize::from(prefix_len);
        let mut contains_start = [0u16; NUM_CONTAINS];
        let mut contains_len = [0u8; NUM_CONTAINS];
        for (start, len) in contains_start.iter_mut().zip(contains_len.iter_mut()) {
            let (s, l, next) = compute_contains(pattern, pos);
            *start = s;
            *len = l;
            pos = next;
        }
        let entry = Entry {
            pattern: pattern.to_vec(),
            parent: None,
            kv_start: self.kv.len(),
            kv_end: self.kv.len(),
            contains_start,
            contains_len,
            prefix_len,
        };
        let idx = if let Some(&i) = self.index.get(pattern) {
            self.entries[i] = entry;
            i
        } else {
            self.index.insert(pattern.to_vec(), self.entries.len());
            self.entries.push(entry);
            self.entries.len() - 1
        };
        self.current = Some(idx);
    }

    /// `ZEND_INI_PARSER_ENTRY` with a value: a capability (key lower-cased)
    /// or the `Parent` of the current section.
    pub fn entry(&mut self, key: &[u8], value: &[u8]) {
        let Some(cur) = self.current else {
            return;
        };
        let new_value = normalize_value(value);
        if key.eq_ignore_ascii_case(b"parent") {
            // A section that is its own parent would loop forever.
            if self.entries[cur].pattern.eq_ignore_ascii_case(value) {
                let name = self.entries[cur].pattern.clone();
                self.diagnostics.push(LoadDiagnostic::SelfParent(name));
                return;
            }
            self.entries[cur].parent = Some(new_value);
        } else {
            self.kv.push((key.to_ascii_lowercase(), new_value));
            self.entries[cur].kv_end = self.kv.len();
        }
    }

    /// `browser_reg_compare` for one entry (`agent` is lower-cased).
    fn compare(entry: &Entry, agent: &[u8]) -> Compare {
        let pattern_lc = entry.pattern.to_ascii_lowercase();
        let prefix = usize::from(entry.prefix_len);
        let mut cur = prefix;
        for (&start, &len) in entry.contains_start.iter().zip(&entry.contains_len) {
            let (start, len) = (usize::from(start), usize::from(len));
            if len == 0 {
                continue;
            }
            let Some(needle) = pattern_lc.get(start..start + len) else {
                return Compare::NoMatch;
            };
            match find_from(agent, cur, needle) {
                Some(at) => cur = at + len,
                None => return Compare::NoMatch,
            }
        }
        if agent == pattern_lc.as_slice() {
            return Compare::Exact;
        }
        let (Some(s), Some(p)) = (agent.get(prefix..), pattern_lc.get(prefix..)) else {
            return Compare::NoMatch;
        };
        if !match_wildcard(s, p) {
            return Compare::NoMatch;
        }
        // Characters the pattern keeps literally (wildcards excluded).
        let literal = entry.pattern[prefix..]
            .iter()
            .filter(|&&c| !is_placeholder(c))
            .count();
        Compare::Wildcard(prefix + literal)
    }

    /// `get_browser()` for `agent`: the entry's `browser_name_regex`,
    /// `browser_name_pattern`, `parent` and capabilities, then those of
    /// its parents (first definition wins). `None` when nothing matches
    /// and there is no default section.
    #[must_use]
    pub fn lookup(&self, agent: &[u8]) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
        let lookup = agent.to_ascii_lowercase();
        let found = match self.index.get(&lookup) {
            Some(&i) => Some(i),
            None => self.search(&lookup),
        };
        let found = match found {
            Some(i) => i,
            None => *self.index.get(DEFAULT_SECTION_NAME)?,
        };
        let entry = &self.entries[found];
        let mut out: Vec<(Vec<u8>, Vec<u8>)> = vec![
            (
                b"browser_name_regex".to_vec(),
                convert_pattern(&entry.pattern),
            ),
            (b"browser_name_pattern".to_vec(), entry.pattern.clone()),
        ];
        if let Some(p) = &entry.parent {
            out.push((b"parent".to_vec(), p.clone()));
        }
        self.add_kv(entry, &mut out);
        let mut entry = entry;
        // At most one visit per section: a parent cycle ends the walk.
        for _ in 0..self.entries.len() {
            let Some(parent) = &entry.parent else {
                break;
            };
            let Some(&i) = self.index.get(parent) else {
                break;
            };
            entry = &self.entries[i];
            self.add_kv(entry, &mut out);
        }
        Some(out)
    }

    /// The pattern search over all entries (hash order).
    fn search(&self, lookup: &[u8]) -> Option<usize> {
        let mut found: Option<usize> = None;
        let mut prev_len = 0usize;
        for (i, entry) in self.entries.iter().enumerate() {
            if lookup.len() < entry.minimum_length() {
                continue;
            }
            let prefix = usize::from(entry.prefix_len);
            let prefix_matches = entry.pattern[..prefix]
                .iter()
                .zip(lookup)
                .all(|(p, l)| p.to_ascii_lowercase() == *l);
            if !prefix_matches {
                continue;
            }
            match Self::compare(entry, lookup) {
                Compare::NoMatch => {}
                Compare::Exact => return Some(i),
                Compare::Wildcard(len) => {
                    // Prefer the pattern replacing the fewest characters.
                    if found.is_none() || prev_len < len {
                        found = Some(i);
                        prev_len = len;
                    }
                }
            }
        }
        found
    }

    /// `browscap_entry_add_kv_to_existing_array` (`zend_hash_add`).
    fn add_kv(&self, entry: &Entry, out: &mut Vec<(Vec<u8>, Vec<u8>)>) {
        for (k, v) in &self.kv[entry.kv_start..entry.kv_end] {
            if !out.iter().any(|(ok, _)| ok == k) {
                out.push((k.clone(), v.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Browscap {
        let mut b = Browscap::new();
        b.section(b"DefaultProperties");
        b.entry(b"Browser", b"DefaultProperties");
        b.entry(b"Frames", b"false");
        b.entry(b"Tables", b"true");
        b.section(b"Opera 7.1");
        b.entry(b"Parent", b"DefaultProperties");
        b.entry(b"Browser", b"Opera");
        b.entry(b"Frames", b"on");
        b.section(b"Opera/7.1* (Windows NT 5.1; ?)*");
        b.entry(b"Parent", b"Opera 7.1");
        b.entry(b"Platform", b"WinXP");
        b.section(b"*");
        b.entry(b"Browser", b"Default Browser");
        b
    }

    fn get<'a>(r: &'a [(Vec<u8>, Vec<u8>)], k: &str) -> Option<&'a [u8]> {
        r.iter()
            .find(|(key, _)| key == k.as_bytes())
            .map(|(_, v)| v.as_slice())
    }

    #[test]
    fn pattern_match_and_parent_chain() {
        let b = table();
        let r = b
            .lookup(b"Opera/7.11 (Windows NT 5.1; U) [en]")
            .expect("match");
        assert_eq!(
            get(&r, "browser_name_regex"),
            Some(&b"~^opera/7\\.1.* \\(windows nt 5\\.1; .\\).*$~"[..])
        );
        assert_eq!(get(&r, "parent"), Some(&b"Opera 7.1"[..]));
        assert_eq!(get(&r, "platform"), Some(&b"WinXP"[..]));
        assert_eq!(get(&r, "browser"), Some(&b"Opera"[..]));
        // The nearer definition wins; booleans are normalized.
        assert_eq!(get(&r, "frames"), Some(&b"1"[..]));
        assert_eq!(get(&r, "tables"), Some(&b"1"[..]));
        let keys: Vec<&[u8]> = r.iter().map(|(k, _)| k.as_slice()).collect();
        assert_eq!(
            keys,
            [
                &b"browser_name_regex"[..],
                b"browser_name_pattern",
                b"parent",
                b"platform",
                b"browser",
                b"frames",
                b"tables"
            ]
        );
    }

    #[test]
    fn catch_all_and_default_section() {
        let b = table();
        let r = b
            .lookup(b"")
            .expect("the * section matches the empty agent");
        assert_eq!(get(&r, "browser_name_pattern"), Some(&b"*"[..]));
        assert_eq!(get(&r, "browser_name_regex"), Some(&b"~^.*$~"[..]));
        let mut d = Browscap::new();
        d.section(b"Firefox");
        d.entry(b"Browser", b"Firefox");
        assert!(d.lookup(b"").is_none());
        d.section(DEFAULT_SECTION_NAME);
        d.entry(b"Browser", b"Default");
        let r = d.lookup(b"x").expect("default section");
        assert_eq!(get(&r, "browser"), Some(&b"Default"[..]));
    }

    #[test]
    fn wildcards() {
        assert!(match_wildcard(b"abc", b"a*c"));
        assert!(match_wildcard(b"abbbc", b"a*bc"));
        assert!(match_wildcard(b"abc", b"a?c"));
        assert!(!match_wildcard(b"abd", b"a*c"));
        assert!(match_wildcard(b"", b"**"));
        assert!(!match_wildcard(b"x", b""));
        assert!(match_wildcard(b"mozilla/5.0 x", b"*x"));
    }

    #[test]
    fn self_parent_is_rejected() {
        let mut b = Browscap::new();
        b.section(b"Loop");
        b.entry(b"Parent", b"loop");
        assert_eq!(
            b.diagnostics,
            [LoadDiagnostic::SelfParent(b"Loop".to_vec())]
        );
        assert!(b
            .lookup(b"Loop")
            .is_some_and(|r| get(&r, "parent").is_none()));
    }
}
