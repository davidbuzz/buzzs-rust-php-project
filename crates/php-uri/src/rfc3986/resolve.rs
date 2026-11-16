// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Reference resolution (RFC 3986 §5.2) as implemented by uriparser's
//! `uriAddBaseUriExMmA` in strict mode.

use super::normalize::remove_dot_segments;
use super::Uri;

/// Why a reference could not be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    /// The base URI has no scheme (`URI_ERROR_ADDBASE_REL_BASE`).
    RelativeBase,
}

/// Resolves `reference` against `base`.
pub fn resolve(reference: &Uri, base: &Uri) -> Result<Uri, ResolveError> {
    if base.scheme.is_none() {
        return Err(ResolveError::RelativeBase);
    }
    let mut t = Uri::default();
    if reference.scheme.is_some() {
        t.scheme.clone_from(&reference.scheme);
        copy_authority(&mut t, reference);
        copy_path(&mut t, reference);
        remove_dot_segments(&mut t, false);
        t.query.clone_from(&reference.query);
    } else {
        if reference.has_host() {
            copy_authority(&mut t, reference);
            copy_path(&mut t, reference);
            remove_dot_segments(&mut t, false);
            t.query.clone_from(&reference.query);
        } else {
            copy_authority(&mut t, base);
            if reference.segments.is_empty() && !reference.absolute_path {
                copy_path(&mut t, base);
                t.query = if reference.query.is_some() {
                    reference.query.clone()
                } else {
                    base.query.clone()
                };
            } else {
                if reference.absolute_path {
                    copy_path(&mut t, reference);
                    // ResolveAbsolutePathFlag
                    if t.has_host() && t.absolute_path {
                        if t.segments.is_empty() {
                            t.segments.push(Vec::new());
                        }
                        t.absolute_path = false;
                    }
                    remove_dot_segments(&mut t, false);
                } else {
                    copy_path(&mut t, base);
                    merge_path(&mut t, reference);
                    remove_dot_segments(&mut t, false);
                    fix_ambiguity(&mut t);
                }
                t.query.clone_from(&reference.query);
            }
            t.fix_empty_trail_segment();
        }
        t.scheme.clone_from(&base.scheme);
    }
    t.fragment.clone_from(&reference.fragment);
    Ok(t)
}

fn copy_authority(dest: &mut Uri, source: &Uri) {
    dest.user_info.clone_from(&source.user_info);
    dest.host.clone_from(&source.host);
    dest.port.clone_from(&source.port);
}

fn copy_path(dest: &mut Uri, source: &Uri) {
    dest.segments.clone_from(&source.segments);
    dest.absolute_path = source.absolute_path;
}

/// `MergePath`: replaces the last base segment with the reference's segments.
fn merge_path(abs: &mut Uri, rel: &Uri) {
    let Some((first, rest)) = rel.segments.split_first() else {
        return;
    };
    if abs.segments.is_empty() {
        abs.segments.push(Vec::new());
    }
    if let Some(last) = abs.segments.last_mut() {
        last.clone_from(first);
    }
    abs.segments.extend(rest.iter().cloned());
}

/// `FixAmbiguity`: prepends a `.` segment when the result could be mistaken
/// for an authority.
fn fix_ambiguity(uri: &mut Uri) {
    let s = &uri.segments;
    let ambiguous = (uri.absolute_path && s.first().is_some_and(Vec::is_empty))
        || (!uri.absolute_path && s.len() >= 2 && s[0].is_empty() && s[1].is_empty());
    if ambiguous {
        uri.segments.insert(0, b".".to_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    fn r(base: &str, rel: &str) -> String {
        let b = parse(base.as_bytes()).unwrap();
        let rr = parse(rel.as_bytes()).unwrap();
        String::from_utf8(resolve(&rr, &b).unwrap().to_bytes()).unwrap()
    }

    #[test]
    fn rfc3986_examples() {
        let base = "http://a/b/c/d;p?q";
        let cases = [
            ("g:h", "g:h"),
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g#s", "http://a/b/c/g#s"),
            ("g?y#s", "http://a/b/c/g?y#s"),
            (";x", "http://a/b/c/;x"),
            ("g;x", "http://a/b/c/g;x"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../", "http://a/"),
            ("../../g", "http://a/g"),
            ("../../../g", "http://a/g"),
            ("../../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("/../g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            (".g", "http://a/b/c/.g"),
            ("g..", "http://a/b/c/g.."),
            ("..g", "http://a/b/c/..g"),
            ("./../g", "http://a/b/g"),
            ("./g/.", "http://a/b/c/g/"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
            ("g;x=1/./y", "http://a/b/c/g;x=1/y"),
            ("g;x=1/../y", "http://a/b/c/y"),
            ("g?y/./x", "http://a/b/c/g?y/./x"),
            ("g#s/../x", "http://a/b/c/g#s/../x"),
            ("http:g", "http:g"),
        ];
        for (rel, want) in cases {
            assert_eq!(r(base, rel), want, "{rel}");
        }
    }

    #[test]
    fn relative_base_rejected() {
        let b = parse(b"/a").unwrap();
        let rr = parse(b"b").unwrap();
        assert_eq!(resolve(&rr, &b), Err(ResolveError::RelativeBase));
    }

    #[test]
    fn empty_base_path() {
        assert_eq!(r("http://a", "b"), "http://a/b");
    }
}
