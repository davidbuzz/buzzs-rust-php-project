// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Port of `zend_sort` / `zend_insert_sort` (`Zend/zend_sort.c`).
//!
//! PHP's sort is a hybrid insertion sort / quicksort derived from libc++. It
//! is not stable by itself; PHP makes it stable by breaking comparison ties on
//! the original position. Reproducing the exact algorithm matters because user
//! comparison callbacks are observable (they may print, count calls, or be
//! inconsistent), so the sequence of comparisons must match PHP's.
//!
//! The comparison function returns an `i32` where only `> 0` is inspected,
//! exactly like the C code.
//!
//! The algorithm is written against [`SortAccess`], the `(base, cmp, swp)`
//! triple the C functions take: [`zend_sort`] / [`zend_insert_sort`] sort a
//! slice with an element comparator, while [`zend_sort_by`] lets a caller
//! whose comparisons must see the whole sequence as it stands mid-sort (a
//! hash table sorted in place while PHP code can reach it) drive the same
//! comparisons and swaps through indices.

/// Index-based access to the sequence being sorted, the `cmp` / `swp`
/// callbacks of `zend_sort(base, nmemb, siz, cmp, swp)`.
pub trait SortAccess {
    /// `cmp(base + a * siz, base + b * siz)`: only `> 0` is inspected.
    fn compare(&mut self, a: usize, b: usize) -> i32;
    /// `swp(base + a * siz, base + b * siz)`.
    fn swap(&mut self, a: usize, b: usize);
}

/// A slice with an element comparator.
struct SliceAccess<'a, T, F: ?Sized> {
    v: &'a mut [T],
    cmp: &'a mut F,
}

impl<T, F: FnMut(&T, &T) -> i32 + ?Sized> SortAccess for SliceAccess<'_, T, F> {
    #[inline]
    fn compare(&mut self, a: usize, b: usize) -> i32 {
        let Self { v, cmp } = self;
        cmp(&v[a], &v[b])
    }

    #[inline]
    fn swap(&mut self, a: usize, b: usize) {
        self.v.swap(a, b);
    }
}

/// Sorts `v` with PHP's hybrid insertion sort (used for up to 16 elements).
pub fn zend_insert_sort<T, F>(v: &mut [T], cmp: &mut F)
where
    F: FnMut(&T, &T) -> i32 + ?Sized,
{
    let n = v.len();
    insert_sort_range(&mut SliceAccess { v, cmp }, 0, n);
}

fn sort2<S: SortAccess + ?Sized>(s: &mut S, a: usize, b: usize) {
    if s.compare(a, b) > 0 {
        s.swap(a, b);
    }
}

fn sort3<S: SortAccess + ?Sized>(s: &mut S, a: usize, b: usize, c: usize) {
    if s.compare(a, b) <= 0 {
        if s.compare(b, c) <= 0 {
            return;
        }
        s.swap(b, c);
        if s.compare(a, b) > 0 {
            s.swap(a, b);
        }
        return;
    }
    if s.compare(c, b) <= 0 {
        s.swap(a, c);
        return;
    }
    s.swap(a, b);
    if s.compare(b, c) > 0 {
        s.swap(b, c);
    }
}

#[allow(clippy::many_single_char_names)]
fn sort4<S: SortAccess + ?Sized>(s: &mut S, idx: [usize; 4]) {
    let [a, b, c, d] = idx;
    sort3(s, a, b, c);
    if s.compare(c, d) > 0 {
        s.swap(c, d);
        if s.compare(b, c) > 0 {
            s.swap(b, c);
            if s.compare(a, b) > 0 {
                s.swap(a, b);
            }
        }
    }
}

#[allow(clippy::many_single_char_names)]
fn sort5<S: SortAccess + ?Sized>(s: &mut S, idx: [usize; 5]) {
    let [a, b, c, d, e] = idx;
    sort4(s, [a, b, c, d]);
    if s.compare(d, e) > 0 {
        s.swap(d, e);
        if s.compare(c, d) > 0 {
            s.swap(c, d);
            if s.compare(b, c) > 0 {
                s.swap(b, c);
                if s.compare(a, b) > 0 {
                    s.swap(a, b);
                }
            }
        }
    }
}

/// `zend_insert_sort` on the elements `start..start + nmemb`.
fn insert_sort_range<S: SortAccess + ?Sized>(s: &mut S, start: usize, nmemb: usize) {
    match nmemb {
        0 | 1 => {}
        2 => sort2(s, start, start + 1),
        3 => sort3(s, start, start + 1, start + 2),
        4 => sort4(s, [start, start + 1, start + 2, start + 3]),
        5 => sort5(s, [start, start + 1, start + 2, start + 3, start + 4]),
        _ => {
            let end = start + nmemb;
            let sentry = start + 6;
            // Plain insertion sort for the first 6 elements.
            let mut i = start + 1;
            while i < sentry {
                let mut j = i - 1;
                if s.compare(j, i) <= 0 {
                    i += 1;
                    continue;
                }
                while j != start {
                    j -= 1;
                    if s.compare(j, i) <= 0 {
                        j += 1;
                        break;
                    }
                }
                let mut k = i;
                while k > j {
                    s.swap(k, k - 1);
                    k -= 1;
                }
                i += 1;
            }
            // Insertion with a two-step probe for the rest.
            let mut i = sentry;
            while i < end {
                let mut j = i - 1;
                if s.compare(j, i) <= 0 {
                    i += 1;
                    continue;
                }
                loop {
                    j -= 2;
                    if s.compare(j, i) <= 0 {
                        j += 1;
                        if s.compare(j, i) <= 0 {
                            j += 1;
                        }
                        break;
                    }
                    if j == start {
                        break;
                    }
                    if j == start + 1 {
                        j -= 1;
                        if s.compare(i, j) > 0 {
                            j += 1;
                        }
                        break;
                    }
                }
                let mut k = i;
                while k > j {
                    s.swap(k, k - 1);
                    k -= 1;
                }
                i += 1;
            }
        }
    }
}

/// Sorts `v` exactly like PHP's `zend_sort` (same comparisons, same swaps).
pub fn zend_sort<T, F>(v: &mut [T], cmp: &mut F)
where
    F: FnMut(&T, &T) -> i32 + ?Sized,
{
    let n = v.len();
    sort_range(&mut SliceAccess { v, cmp }, 0, n);
}

/// `zend_sort` over the elements `0..nmemb` of an index-based sequence:
/// the same comparisons and swaps as [`zend_sort`], issued through `s`.
pub fn zend_sort_by<S: SortAccess + ?Sized>(s: &mut S, nmemb: usize) {
    sort_range(s, 0, nmemb);
}

fn sort_range<S: SortAccess + ?Sized>(s: &mut S, mut base: usize, mut nmemb: usize) {
    loop {
        if nmemb <= 16 {
            insert_sort_range(s, base, nmemb);
            return;
        }
        let start = base;
        let end = start + nmemb;
        let offset = nmemb >> 1;
        let pivot0 = start + offset;
        if nmemb >> 10 != 0 {
            let delta = offset >> 1;
            sort5(s, [start, start + delta, pivot0, pivot0 + delta, end - 1]);
        } else {
            sort3(s, start, pivot0, end - 1);
        }
        s.swap(start + 1, pivot0);
        let pivot = start + 1;
        let mut i = pivot + 1;
        let mut j = end - 1;
        'outer: loop {
            while s.compare(pivot, i) > 0 {
                i += 1;
                if i == j {
                    break 'outer;
                }
            }
            j -= 1;
            if j == i {
                break 'outer;
            }
            while s.compare(j, pivot) > 0 {
                j -= 1;
                if j == i {
                    break 'outer;
                }
            }
            s.swap(i, j);
            i += 1;
            if i == j {
                break 'outer;
            }
        }
        s.swap(pivot, i - 1);
        if (i - 1) - start < end - i {
            sort_range(s, start, (i - start) - 1);
            base = i;
            nmemb = end - i;
        } else {
            sort_range(s, i, end - i);
            nmemb = (i - start) - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    #[test]
    fn sorts_all_sizes() {
        let mut rng = Rng(0x1234_5678_9abc_def1);
        for n in 0..200 {
            for _ in 0..5 {
                let mut v: Vec<u64> = (0..n).map(|_| rng.next() % 50).collect();
                let mut expected = v.clone();
                expected.sort_unstable();
                zend_sort(&mut v, &mut |a: &u64, b: &u64| match a.cmp(b) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                });
                assert_eq!(v, expected, "n={n}");
            }
        }
    }

    #[test]
    fn large_uses_sort5_pivot() {
        let mut rng = Rng(42);
        let mut v: Vec<u64> = (0..5000).map(|_| rng.next() % 1000).collect();
        let mut expected = v.clone();
        expected.sort_unstable();
        zend_sort(&mut v, &mut |a: &u64, b: &u64| {
            i32::from(a > b) - i32::from(a < b)
        });
        assert_eq!(v, expected);
    }

    #[test]
    fn stable_with_order_tiebreak() {
        let mut rng = Rng(7);
        let mut v: Vec<(u64, usize)> = (0..300).map(|i| (rng.next() % 5, i)).collect();
        zend_sort(&mut v, &mut |a: &(u64, usize), b: &(u64, usize)| {
            let r = i32::from(a.0 > b.0) - i32::from(a.0 < b.0);
            if r != 0 {
                r
            } else {
                i32::from(a.1 > b.1) - i32::from(a.1 < b.1)
            }
        });
        for w in v.windows(2) {
            assert!(w[0].0 < w[1].0 || (w[0].0 == w[1].0 && w[0].1 < w[1].1));
        }
    }

    fn trace(input: &[i32]) -> String {
        let mut v = input.to_vec();
        let mut calls = Vec::new();
        zend_sort(&mut v, &mut |a: &i32, b: &i32| {
            calls.push(format!("{a},{b}"));
            a - b
        });
        let mut sorted = input.to_vec();
        sorted.sort_unstable();
        assert_eq!(v, sorted);
        calls.join(" ")
    }

    /// An index-based sequence that logs its comparisons and swaps.
    struct Logged {
        v: Vec<i32>,
        log: Vec<String>,
    }

    impl SortAccess for Logged {
        fn compare(&mut self, a: usize, b: usize) -> i32 {
            self.log.push(format!("{},{}", self.v[a], self.v[b]));
            self.v[a] - self.v[b]
        }

        fn swap(&mut self, a: usize, b: usize) {
            self.v.swap(a, b);
        }
    }

    #[test]
    fn index_access_matches_slice_sort() {
        let mut rng = Rng(99);
        for n in [0usize, 1, 2, 5, 6, 7, 16, 17, 40, 1100] {
            let input: Vec<i32> = (0..n)
                .map(|_| i32::try_from(rng.next() % 30).unwrap_or(0))
                .collect();
            let mut by_slice = input.clone();
            let mut slice_log = Vec::new();
            zend_sort(&mut by_slice, &mut |a: &i32, b: &i32| {
                slice_log.push(format!("{a},{b}"));
                a - b
            });
            let mut logged = Logged {
                v: input,
                log: Vec::new(),
            };
            zend_sort_by(&mut logged, n);
            assert_eq!(logged.v, by_slice, "n={n}");
            assert_eq!(logged.log, slice_log, "n={n}");
        }
    }

    #[test]
    fn comparison_sequences_match_php() {
        // Expected strings produced by usort() with an echoing callback in PHP.
        assert_eq!(trace(&[3, 1, 2]), "3,1 2,1 3,2");
        assert_eq!(
            trace(&[5, 3, 8, 1, 9, 2, 7]),
            "5,3 5,8 8,1 5,1 3,1 8,9 9,2 8,2 5,2 3,2 1,2 9,7 5,7 8,7"
        );
        let desc: Vec<i32> = (1..=20).rev().collect();
        assert_eq!(
            trace(&desc),
            "20,10 1,10 10,18 2,10 10,17 3,10 10,16 4,10 10,15 5,10 10,14 6,10 10,13 7,10 \
             10,12 8,10 10,11 9,10 10,19 1,9 9,2 1,2 9,3 2,3 9,4 3,4 9,5 4,5 9,6 4,6 5,6 9,7 \
             5,7 6,7 9,8 6,8 7,8 19,11 19,12 11,12 19,13 12,13 19,14 13,14 19,15 14,15 19,16 \
             14,16 15,16 19,17 15,17 16,17 19,18 16,18 17,18 19,20"
        );
    }
}
