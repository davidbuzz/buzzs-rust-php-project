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

/// Sorts `v` with PHP's hybrid insertion sort (used for up to 16 elements).
pub fn zend_insert_sort<T, F>(v: &mut [T], cmp: &mut F)
where
    F: FnMut(&T, &T) -> i32 + ?Sized,
{
    insert_sort_range(v, 0, v.len(), cmp);
}

fn sort2<T, F: FnMut(&T, &T) -> i32 + ?Sized>(v: &mut [T], a: usize, b: usize, cmp: &mut F) {
    if cmp(&v[a], &v[b]) > 0 {
        v.swap(a, b);
    }
}

fn sort3<T, F: FnMut(&T, &T) -> i32 + ?Sized>(
    v: &mut [T],
    a: usize,
    b: usize,
    c: usize,
    cmp: &mut F,
) {
    if cmp(&v[a], &v[b]) <= 0 {
        if cmp(&v[b], &v[c]) <= 0 {
            return;
        }
        v.swap(b, c);
        if cmp(&v[a], &v[b]) > 0 {
            v.swap(a, b);
        }
        return;
    }
    if cmp(&v[c], &v[b]) <= 0 {
        v.swap(a, c);
        return;
    }
    v.swap(a, b);
    if cmp(&v[b], &v[c]) > 0 {
        v.swap(b, c);
    }
}

#[allow(clippy::many_single_char_names)]
fn sort4<T, F: FnMut(&T, &T) -> i32 + ?Sized>(v: &mut [T], idx: [usize; 4], cmp: &mut F) {
    let [a, b, c, d] = idx;
    sort3(v, a, b, c, cmp);
    if cmp(&v[c], &v[d]) > 0 {
        v.swap(c, d);
        if cmp(&v[b], &v[c]) > 0 {
            v.swap(b, c);
            if cmp(&v[a], &v[b]) > 0 {
                v.swap(a, b);
            }
        }
    }
}

#[allow(clippy::many_single_char_names)]
fn sort5<T, F: FnMut(&T, &T) -> i32 + ?Sized>(v: &mut [T], idx: [usize; 5], cmp: &mut F) {
    let [a, b, c, d, e] = idx;
    sort4(v, [a, b, c, d], cmp);
    if cmp(&v[d], &v[e]) > 0 {
        v.swap(d, e);
        if cmp(&v[c], &v[d]) > 0 {
            v.swap(c, d);
            if cmp(&v[b], &v[c]) > 0 {
                v.swap(b, c);
                if cmp(&v[a], &v[b]) > 0 {
                    v.swap(a, b);
                }
            }
        }
    }
}

/// `zend_insert_sort` on `v[start..start + nmemb]`.
fn insert_sort_range<T, F: FnMut(&T, &T) -> i32 + ?Sized>(
    v: &mut [T],
    start: usize,
    nmemb: usize,
    cmp: &mut F,
) {
    match nmemb {
        0 | 1 => {}
        2 => sort2(v, start, start + 1, cmp),
        3 => sort3(v, start, start + 1, start + 2, cmp),
        4 => sort4(v, [start, start + 1, start + 2, start + 3], cmp),
        5 => sort5(v, [start, start + 1, start + 2, start + 3, start + 4], cmp),
        _ => {
            let end = start + nmemb;
            let sentry = start + 6;
            // Plain insertion sort for the first 6 elements.
            let mut i = start + 1;
            while i < sentry {
                let mut j = i - 1;
                if cmp(&v[j], &v[i]) <= 0 {
                    i += 1;
                    continue;
                }
                while j != start {
                    j -= 1;
                    if cmp(&v[j], &v[i]) <= 0 {
                        j += 1;
                        break;
                    }
                }
                let mut k = i;
                while k > j {
                    v.swap(k, k - 1);
                    k -= 1;
                }
                i += 1;
            }
            // Insertion with a two-step probe for the rest.
            let mut i = sentry;
            while i < end {
                let mut j = i - 1;
                if cmp(&v[j], &v[i]) <= 0 {
                    i += 1;
                    continue;
                }
                loop {
                    j -= 2;
                    if cmp(&v[j], &v[i]) <= 0 {
                        j += 1;
                        if cmp(&v[j], &v[i]) <= 0 {
                            j += 1;
                        }
                        break;
                    }
                    if j == start {
                        break;
                    }
                    if j == start + 1 {
                        j -= 1;
                        if cmp(&v[i], &v[j]) > 0 {
                            j += 1;
                        }
                        break;
                    }
                }
                let mut k = i;
                while k > j {
                    v.swap(k, k - 1);
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
    sort_range(v, 0, v.len(), cmp);
}

fn sort_range<T, F: FnMut(&T, &T) -> i32 + ?Sized>(
    v: &mut [T],
    mut base: usize,
    mut nmemb: usize,
    cmp: &mut F,
) {
    loop {
        if nmemb <= 16 {
            insert_sort_range(v, base, nmemb, cmp);
            return;
        }
        let start = base;
        let end = start + nmemb;
        let offset = nmemb >> 1;
        let pivot0 = start + offset;
        if nmemb >> 10 != 0 {
            let delta = offset >> 1;
            sort5(
                v,
                [start, start + delta, pivot0, pivot0 + delta, end - 1],
                cmp,
            );
        } else {
            sort3(v, start, pivot0, end - 1, cmp);
        }
        v.swap(start + 1, pivot0);
        let pivot = start + 1;
        let mut i = pivot + 1;
        let mut j = end - 1;
        'outer: loop {
            while cmp(&v[pivot], &v[i]) > 0 {
                i += 1;
                if i == j {
                    break 'outer;
                }
            }
            j -= 1;
            if j == i {
                break 'outer;
            }
            while cmp(&v[j], &v[pivot]) > 0 {
                j -= 1;
                if j == i {
                    break 'outer;
                }
            }
            v.swap(i, j);
            i += 1;
            if i == j {
                break 'outer;
            }
        }
        v.swap(pivot, i - 1);
        if (i - 1) - start < end - i {
            sort_range(v, start, (i - start) - 1, cmp);
            base = i;
            nmemb = end - i;
        } else {
            sort_range(v, i, end - i, cmp);
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
