//! Port of run-tests.php's `Differ` / `DiffOutputBuilder`, producing the same `.diff` format:
//! `001- expected` / `001+ actual` with three lines of context.

use std::fmt::Write as _;

/// Number of unchanged lines shown around each change.
const CONTEXT_LINES: usize = 3;

/// Above this many line-pair comparisons the LCS is skipped (all old lines removed, all new
/// lines added) to keep pathological outputs from taking minutes.
const MAX_LCS_WORK: usize = 4_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Old,
    Added,
    Removed,
}

struct Differ<'f> {
    is_equal: &'f dyn Fn(&[u8], &[u8]) -> bool,
}

impl Differ<'_> {
    fn eq(&self, a: &[u8], b: &[u8]) -> bool {
        (self.is_equal)(a, b)
    }

    fn diff_to_array<'a>(
        &self,
        from: &[&'a [u8]],
        to: &[&'a [u8]],
    ) -> Vec<(&'a [u8], Kind, usize)> {
        // Common prefix.
        let mut start = 0;
        while start < from.len() && start < to.len() && self.eq(from[start], to[start]) {
            start += 1;
        }
        // Common suffix (never overlapping the prefix).
        let mut end = 0;
        while from.len() - end > start
            && to.len() - end > start
            && self.eq(from[from.len() - 1 - end], to[to.len() - 1 - end])
        {
            end += 1;
        }
        let from_mid = &from[start..from.len() - end];
        let to_mid = &to[start..to.len() - end];

        let mut diff = Vec::with_capacity(from.len() + to.len());
        let mut from_line = 1 + start;
        let mut to_line = 1 + start;
        for token in &from[..start] {
            diff.push((*token, Kind::Old, 0));
        }

        let common = if from_mid.len().saturating_mul(to_mid.len()) > MAX_LCS_WORK {
            Vec::new()
        } else {
            self.common_subsequence(from_mid, to_mid)
        };
        let mut fi = 0;
        let mut ti = 0;
        for token in common {
            while fi < from_mid.len() && !self.eq(from_mid[fi], token) {
                diff.push((from_mid[fi], Kind::Removed, from_line));
                from_line += 1;
                fi += 1;
            }
            while ti < to_mid.len() && !self.eq(token, to_mid[ti]) {
                diff.push((to_mid[ti], Kind::Added, to_line));
                to_line += 1;
                ti += 1;
            }
            diff.push((token, Kind::Old, 0));
            from_line += 1;
            to_line += 1;
            fi += 1;
            ti += 1;
        }
        for token in from_mid.iter().skip(fi) {
            diff.push((*token, Kind::Removed, from_line));
            from_line += 1;
        }
        for token in to_mid.iter().skip(ti) {
            diff.push((*token, Kind::Added, to_line));
            to_line += 1;
        }
        for token in &from[from.len() - end..] {
            diff.push((*token, Kind::Old, 0));
        }
        diff
    }

    fn common_subsequence<'a>(&self, from: &[&'a [u8]], to: &[&'a [u8]]) -> Vec<&'a [u8]> {
        if from.is_empty() {
            return Vec::new();
        }
        if from.len() == 1 {
            return to
                .iter()
                .find(|t| self.eq(from[0], t))
                .map(|t| vec![*t])
                .unwrap_or_default();
        }
        let i = from.len() / 2;
        let (from_start, from_end) = from.split_at(i);
        let ll_b = self.subsequence_length(from_start.iter().copied(), to.iter().copied());
        let ll_e =
            self.subsequence_length(from_end.iter().rev().copied(), to.iter().rev().copied());
        let mut j_max = 0;
        let mut max = 0;
        for j in 0..=to.len() {
            let m = ll_b[j] + ll_e[to.len() - j];
            if m >= max {
                max = m;
                j_max = j;
            }
        }
        let (to_start, to_end) = to.split_at(j_max);
        let mut result = self.common_subsequence(from_start, to_start);
        result.extend(self.common_subsequence(from_end, to_end));
        result
    }

    fn subsequence_length<'a>(
        &self,
        from: impl Iterator<Item = &'a [u8]>,
        to: impl Iterator<Item = &'a [u8]> + Clone,
    ) -> Vec<usize> {
        let to: Vec<&[u8]> = to.collect();
        let mut current = vec![0usize; to.len() + 1];
        for f in from {
            let prev = current.clone();
            for (j, t) in to.iter().enumerate() {
                current[j + 1] = if self.eq(f, t) {
                    prev[j] + 1
                } else {
                    current[j].max(prev[j + 1])
                };
            }
        }
        current
    }
}

fn render(diffs: &[(&[u8], Kind, usize)]) -> String {
    let number_len = 3.max(diffs.len().to_string().len());
    let mut out = String::new();
    let pad = " ".repeat(number_len + 2);
    let mut i = 0;
    while i < diffs.len() {
        let mut next = i;
        while next < diffs.len() && diffs[next].1 == Kind::Old {
            next += 1;
        }
        if next == diffs.len() {
            out.push_str("--\n");
            break;
        }
        if i + CONTEXT_LINES < next {
            out.push_str("--\n");
            i = next - CONTEXT_LINES;
        }
        while i < next {
            let _ = writeln!(out, "{pad}{}", String::from_utf8_lossy(diffs[i].0));
            i += 1;
        }
        while i < diffs.len() && diffs[i].1 != Kind::Old {
            let sign = if diffs[i].1 == Kind::Added { '+' } else { '-' };
            let _ = writeln!(
                out,
                "{:0width$}{sign} {}",
                diffs[i].2,
                String::from_utf8_lossy(diffs[i].0),
                width = number_len
            );
            i += 1;
        }
        let after = (i + CONTEXT_LINES).min(diffs.len());
        while i < after && diffs[i].1 == Kind::Old {
            let _ = writeln!(out, "{pad}{}", String::from_utf8_lossy(diffs[i].0));
            i += 1;
        }
    }
    out
}

/// Produce a run-tests.php style diff between `wanted` and `output` (both already trimmed),
/// comparing lines with `is_equal(expected_line, actual_line)`.
pub fn generate_diff(
    wanted: &[u8],
    output: &[u8],
    is_equal: &dyn Fn(&[u8], &[u8]) -> bool,
) -> String {
    let w = crate::bytes::explode_lines(wanted);
    let o = crate::bytes::explode_lines(output);
    let differ = Differ { is_equal };
    render(&differ.diff_to_array(&w, &o))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact(a: &[u8], b: &[u8]) -> bool {
        a == b
    }

    #[test]
    fn simple_change() {
        let d = generate_diff(b"a\nb\nc", b"a\nx\nc", &exact);
        assert_eq!(d, "     a\n002- b\n002+ x\n     c\n");
        let d = generate_diff(b"a\nb\nc\nd\ne\nf\ng\nh", b"a\nX\nc\nd\ne\nf\ng\nh", &exact);
        assert_eq!(d, "     a\n002- b\n002+ X\n     c\n     d\n     e\n--\n");
    }

    #[test]
    fn identical_has_trailer_only() {
        assert_eq!(generate_diff(b"a\nb", b"a\nb", &exact), "--\n");
    }

    #[test]
    fn context_is_limited() {
        let d = generate_diff(b"1\n2\n3\n4\n5\n6\nx", b"1\n2\n3\n4\n5\n6\ny", &exact);
        assert_eq!(d, "--\n     4\n     5\n     6\n007- x\n007+ y\n");
    }

    #[test]
    fn insertion_in_middle() {
        let d = generate_diff(b"a\nc", b"a\nb\nc", &exact);
        assert_eq!(d, "     a\n002+ b\n     c\n");
    }

    #[test]
    fn custom_equality() {
        let m = crate::expectf::LineMatcher::default();
        let eq = |e: &[u8], a: &[u8]| m.matches(e, a);
        let d = generate_diff(b"int(%d)\nfoo", b"int(3)\nbar", &eq);
        // Prefix/suffix context lines come from the expectation, like in run-tests.php.
        assert_eq!(d, "     int(%d)\n002- foo\n002+ bar\n");
    }
}
