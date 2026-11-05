//! Locale sorting runs in its own test process: LC_COLLATE is process-global.

use php_runtime::{ArrayKey, CollectingContext, PhpRef, Value, ZArray};
use php_stdarr::{compare, setops, sort, SORT_DESC, SORT_FLAG_CASE, SORT_LOCALE_STRING};

fn strings(a: &ZArray) -> Vec<String> {
    a.values()
        .map(|v| v.as_str().unwrap().to_string_lossy())
        .collect()
}

#[test]
fn locale_sort_uses_current_collation() {
    let mut ctx = CollectingContext::new();
    for (locale, expected) in [
        (c"C", ["A", "a", "b", "z", "ä"]),
        (c"en_US.utf8", ["a", "A", "ä", "b", "z"]),
    ] {
        // SAFETY: the locale name is a static NUL-terminated string.
        let available = unsafe { libc::setlocale(libc::LC_COLLATE, locale.as_ptr()) };
        if available.is_null() {
            eprintln!("locale {} unavailable", locale.to_string_lossy());
            continue;
        }
        let input = ZArray::from_values(["b", "A", "a", "ä", "z"].map(Value::from));
        for flags in [SORT_LOCALE_STRING, SORT_LOCALE_STRING | SORT_FLAG_CASE] {
            let a = PhpRef::new(Value::Array(input.clone()));
            sort::sort(&mut ctx, &a, flags).unwrap();
            let Value::Array(sorted) = a.get() else {
                panic!("not an array")
            };
            assert_eq!(strings(&sorted), expected);
            sort::rsort(&mut ctx, &a, flags).unwrap();
            let Value::Array(sorted) = a.get() else {
                panic!("not an array")
            };
            assert_eq!(
                strings(&sorted),
                expected.into_iter().rev().collect::<Vec<_>>()
            );

            let keyed =
                ZArray::from_pairs(["b", "A", "a", "ä", "z"].map(|s| (s, Value::from(s))));
            let a = PhpRef::new(Value::Array(keyed));
            sort::ksort(&mut ctx, &a, flags).unwrap();
            let Value::Array(sorted) = a.get() else {
                panic!("not an array")
            };
            let keys: Vec<_> = sorted.keys().map(ArrayKey::to_zstr).collect();
            assert_eq!(
                keys.iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>(),
                expected
            );
        }
        let a = PhpRef::new(Value::Array(input));
        sort::array_multisort(
            &mut ctx,
            &[
                Value::Reference(a.clone()),
                Value::Int(SORT_DESC),
                Value::Int(SORT_LOCALE_STRING),
            ],
        )
        .unwrap();
        let Value::Array(sorted) = a.get() else {
            panic!("not an array")
        };
        assert_eq!(
            strings(&sorted),
            expected.into_iter().rev().collect::<Vec<_>>()
        );

        let nul = ZArray::from_values(["a\0second", "a\0first", "b"].map(Value::from));
        let unique = setops::array_unique(&mut ctx, &nul, SORT_LOCALE_STRING).unwrap();
        assert_eq!(strings(&unique), ["a\0second", "b"]);
        assert_eq!(compare::strcoll(b"a\0second", b"a\0first"), 0);
        assert_eq!(
            compare::key_compare_locale(&ArrayKey::Int(12), &ArrayKey::from("12")),
            0
        );
    }
}
