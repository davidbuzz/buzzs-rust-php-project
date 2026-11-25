// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! PHP's `ext/ctype`: the eleven `ctype_*` character-class predicates.
//!
//! Reference: `ext/ctype/ctype.c`.
//!
//! # Semantics
//!
//! * A **string** argument is tested byte by byte with the C library's
//!   `isalnum()`/`isalpha()`/… The empty string is always `false`.
//! * Any **other** argument emits `E_DEPRECATED` "Argument of type %s will be
//!   interpreted as string in the future" (`%s` is `zend_zval_type_name`:
//!   `int`, `float`, `bool`, `null`, `array`, `resource` or the class name of
//!   an object) and then:
//!   * an `int` in `-128..=255` is treated as a single character code
//!     (negative values have 256 added, so `-1` is byte `0xFF`);
//!   * any other `int` returns whether the class accepts the digits of its
//!     decimal representation: `ctype_alnum`/`digit`/`graph`/`print`/`xdigit`
//!     return `true` for large positive integers, `ctype_graph`/`print` also
//!     for large negative integers (the `-` sign is printable), everything else
//!     returns `false`;
//!   * every non-`int` non-string value returns `false`.
//!
//! [`ctype_call`] implements exactly this and returns a [`CtypeOutcome`] so
//! the VM can emit the deprecation itself (with the `ctype_xxx(): ` prefix
//! that `php_error_docref` adds; see [`CtypeOutcome::prefixed_deprecation`]).
//!
//! # Locales
//!
//! PHP calls the libc classification functions, which depend on the current
//! `LC_CTYPE` locale. PHP itself never inherits `LC_CTYPE` from the
//! environment (it calls `setlocale(LC_CTYPE, "C")` at startup), so the
//! classification is the C/POSIX one — pure ASCII, no byte `>= 0x80` belongs
//! to any class — unless the script calls `setlocale()`. That default is
//! what the plain functions ([`ctype_alpha`], [`check_bytes`], …) implement.
//!
//! In UTF-8 locales glibc also classifies no byte `>= 0x80`, so the C tables
//! are correct there too. Only single-byte locales such as `de_DE`
//! (ISO-8859-1) differ: there `ctype_lower("\xe4")` is `true`.
//! [`ctype_call_locale`] is what the extension binds: it asks the C
//! library (`isalpha()`/…, under the locale `setlocale()` selected) about
//! every byte `>= 0x80`, as `ext/ctype/ctype.c` does about every byte.
//! Bytes below `0x80` keep the C answer without a call: POSIX fixes the
//! classes of the portable character set, and no glibc locale changes
//! them. A [`CtypeTable`] — [`CtypeTable::latin1`] approximates glibc's
//! ISO-8859-1 locales, and [`CtypeTable::from_fn`] builds a table from
//! any classifier — can be passed to the `*_with` variants instead.

use php_runtime::Value;

/// One of the eleven character classes of `ext/ctype`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CtypeClass {
    /// `ctype_alnum` (`isalnum`).
    Alnum,
    /// `ctype_alpha` (`isalpha`).
    Alpha,
    /// `ctype_cntrl` (`iscntrl`).
    Cntrl,
    /// `ctype_digit` (`isdigit`).
    Digit,
    /// `ctype_graph` (`isgraph`).
    Graph,
    /// `ctype_lower` (`islower`).
    Lower,
    /// `ctype_print` (`isprint`).
    Print,
    /// `ctype_punct` (`ispunct`).
    Punct,
    /// `ctype_space` (`isspace`).
    Space,
    /// `ctype_upper` (`isupper`).
    Upper,
    /// `ctype_xdigit` (`isxdigit`).
    Xdigit,
}

impl CtypeClass {
    /// All classes, in the order `ext/ctype` registers its functions.
    pub const ALL: [CtypeClass; 11] = [
        CtypeClass::Alnum,
        CtypeClass::Alpha,
        CtypeClass::Cntrl,
        CtypeClass::Digit,
        CtypeClass::Lower,
        CtypeClass::Graph,
        CtypeClass::Print,
        CtypeClass::Punct,
        CtypeClass::Space,
        CtypeClass::Upper,
        CtypeClass::Xdigit,
    ];

    /// The PHP function name, e.g. `"ctype_alnum"`.
    #[must_use]
    pub fn function_name(self) -> &'static str {
        match self {
            CtypeClass::Alnum => "ctype_alnum",
            CtypeClass::Alpha => "ctype_alpha",
            CtypeClass::Cntrl => "ctype_cntrl",
            CtypeClass::Digit => "ctype_digit",
            CtypeClass::Graph => "ctype_graph",
            CtypeClass::Lower => "ctype_lower",
            CtypeClass::Print => "ctype_print",
            CtypeClass::Punct => "ctype_punct",
            CtypeClass::Space => "ctype_space",
            CtypeClass::Upper => "ctype_upper",
            CtypeClass::Xdigit => "ctype_xdigit",
        }
    }

    /// Looks a class up by its PHP function name (ASCII case-insensitive, as
    /// PHP function names are).
    #[must_use]
    pub fn from_function_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|c| c.function_name().eq_ignore_ascii_case(name))
    }

    /// The result for an integer outside `-128..=255` that is non-negative
    /// (the `allow_digits` argument of `ctype_impl`).
    #[must_use]
    pub fn allow_digits(self) -> bool {
        matches!(
            self,
            CtypeClass::Alnum
                | CtypeClass::Digit
                | CtypeClass::Graph
                | CtypeClass::Print
                | CtypeClass::Xdigit
        )
    }

    /// The result for an integer below `-128` (the `allow_minus` argument of
    /// `ctype_impl`).
    #[must_use]
    pub fn allow_minus(self) -> bool {
        matches!(self, CtypeClass::Graph | CtypeClass::Print)
    }

    /// Whether byte `c` belongs to the class in the C/POSIX locale.
    #[must_use]
    pub fn matches(self, c: u8) -> bool {
        match self {
            CtypeClass::Alnum => c.is_ascii_alphanumeric(),
            CtypeClass::Alpha => c.is_ascii_alphabetic(),
            CtypeClass::Cntrl => c < 0x20 || c == 0x7f,
            CtypeClass::Digit => c.is_ascii_digit(),
            CtypeClass::Graph => (0x21..0x7f).contains(&c),
            CtypeClass::Lower => c.is_ascii_lowercase(),
            CtypeClass::Print => (0x20..0x7f).contains(&c),
            CtypeClass::Punct => c.is_ascii_punctuation(),
            // isspace: space, \t, \n, \v, \f, \r.
            CtypeClass::Space => c == b' ' || (0x09..=0x0d).contains(&c),
            CtypeClass::Upper => c.is_ascii_uppercase(),
            CtypeClass::Xdigit => c.is_ascii_hexdigit(),
        }
    }

    fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

impl CtypeClass {
    /// Whether byte `c` belongs to the class under the C library's current
    /// `LC_CTYPE` locale. Bytes below `0x80` are answered from the C
    /// tables (see the module docs); the others by `isalpha()` and its
    /// siblings, as `ext/ctype/ctype.c` does.
    #[must_use]
    pub fn matches_locale(self, c: u8) -> bool {
        if c < 0x80 {
            return self.matches(c);
        }
        let f: unsafe extern "C" fn(libc::c_int) -> libc::c_int = match self {
            CtypeClass::Alnum => libc::isalnum,
            CtypeClass::Alpha => libc::isalpha,
            CtypeClass::Cntrl => libc::iscntrl,
            CtypeClass::Digit => libc::isdigit,
            CtypeClass::Graph => libc::isgraph,
            CtypeClass::Lower => libc::islower,
            CtypeClass::Print => libc::isprint,
            CtypeClass::Punct => libc::ispunct,
            CtypeClass::Space => libc::isspace,
            CtypeClass::Upper => libc::isupper,
            CtypeClass::Xdigit => libc::isxdigit,
        };
        // SAFETY: the <ctype.h> classifiers are defined for every value
        // representable as `unsigned char` (here 0x80..=0xFF) and only
        // read the locale's classification table.
        unsafe { f(libc::c_int::from(c)) != 0 }
    }
}

/// A 256-entry classification table (the libc `__ctype_b` of a locale).
#[derive(Clone, PartialEq, Eq)]
pub struct CtypeTable {
    bits: [u16; 256],
}

impl std::fmt::Debug for CtypeTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CtypeTable").finish_non_exhaustive()
    }
}

impl Default for CtypeTable {
    fn default() -> Self {
        Self::c_locale()
    }
}

impl CtypeTable {
    /// Builds a table from a classifier `f(class, byte)`.
    pub fn from_fn(mut f: impl FnMut(CtypeClass, u8) -> bool) -> Self {
        let mut bits = [0u16; 256];
        for (b, slot) in (0u8..=255).zip(bits.iter_mut()) {
            for class in CtypeClass::ALL {
                if f(class, b) {
                    *slot |= class.bit();
                }
            }
        }
        Self { bits }
    }

    /// The C/POSIX locale (ASCII only). Also correct for glibc's UTF-8
    /// locales, which classify no byte `>= 0x80`.
    #[must_use]
    pub fn c_locale() -> Self {
        Self::from_fn(CtypeClass::matches)
    }

    /// An approximation of glibc's single-byte ISO-8859-1 locales (such as
    /// `de_DE`): the C classes plus, for `0x80..=0xFF`, the classification
    /// glibc derives from the Unicode properties of U+0080..U+00FF
    /// (`0x80..=0x9F` are controls; letters are `0xAA`, `0xB5`, `0xBA`,
    /// `0xC0..=0xFF` except `0xD7`/`0xF7`; everything else from `0xA0` on is
    /// punctuation; no extra digits or spaces).
    #[must_use]
    pub fn latin1() -> Self {
        Self::from_fn(|class, c| {
            if c < 0x80 {
                return class.matches(c);
            }
            let upper = matches!(c, 0xc0..=0xd6 | 0xd8..=0xde);
            let lower = matches!(c, 0xb5 | 0xdf..=0xf6 | 0xf8..=0xff);
            let alpha = upper || lower || c == 0xaa || c == 0xba;
            let cntrl = c < 0xa0;
            match class {
                CtypeClass::Alnum | CtypeClass::Alpha => alpha,
                CtypeClass::Cntrl => cntrl,
                CtypeClass::Digit | CtypeClass::Space | CtypeClass::Xdigit => false,
                CtypeClass::Graph | CtypeClass::Print => !cntrl,
                CtypeClass::Lower => lower,
                CtypeClass::Upper => upper,
                CtypeClass::Punct => !cntrl && !alpha,
            }
        })
    }

    /// Whether byte `c` belongs to `class` in this table.
    #[must_use]
    pub fn matches(&self, class: CtypeClass, c: u8) -> bool {
        self.bits[usize::from(c)] & class.bit() != 0
    }
}

/// Tests a string in the C locale: `true` iff it is non-empty and every
/// byte belongs to `class`.
#[must_use]
pub fn check_bytes(class: CtypeClass, s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|&c| class.matches(c))
}

/// [`check_bytes`] with an explicit locale table.
#[must_use]
pub fn check_bytes_with(table: &CtypeTable, class: CtypeClass, s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(|&c| table.matches(class, c))
}

/// The deprecation text for a non-string argument of type `type_name`
/// (without the `ctype_xxx(): ` prefix).
#[must_use]
pub fn deprecation_message(type_name: &str) -> String {
    format!("Argument of type {type_name} will be interpreted as string in the future")
}

/// The result of a `ctype_*` call on an arbitrary value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtypeOutcome {
    /// The function's return value.
    pub result: bool,
    /// The `E_DEPRECATED` message to emit before returning, without the
    /// `ctype_xxx(): ` prefix (`None` for string arguments).
    pub deprecation: Option<String>,
}

impl CtypeOutcome {
    /// The deprecation as `php_error_docref` prints it, prefixed with the
    /// function name (e.g. `"ctype_digit(): Argument of type int will be
    /// interpreted as string in the future"`).
    #[must_use]
    pub fn prefixed_deprecation(&self, class: CtypeClass) -> Option<String> {
        self.deprecation
            .as_ref()
            .map(|m| format!("{}(): {m}", class.function_name()))
    }
}

/// `ctype_xxx(mixed $text): bool` in the C locale.
#[must_use]
pub fn ctype_call(class: CtypeClass, arg: &Value) -> CtypeOutcome {
    ctype_call_impl(class, arg, &|c| class.matches(c))
}

/// [`ctype_call`] under the current `LC_CTYPE` locale of the C library:
/// what `ext/ctype/ctype.c` computes after a script's `setlocale()`.
#[must_use]
pub fn ctype_call_locale(class: CtypeClass, arg: &Value) -> CtypeOutcome {
    ctype_call_impl(class, arg, &|c| class.matches_locale(c))
}

/// [`ctype_call`] with an explicit locale table.
#[must_use]
pub fn ctype_call_with(table: &CtypeTable, class: CtypeClass, arg: &Value) -> CtypeOutcome {
    ctype_call_impl(class, arg, &|c| table.matches(class, c))
}

fn ctype_call_impl(class: CtypeClass, arg: &Value, is: &dyn Fn(u8) -> bool) -> CtypeOutcome {
    match arg {
        Value::Reference(r) => ctype_call_impl(class, &r.get(), is),
        Value::Str(s) => CtypeOutcome {
            result: !s.is_empty() && s.as_bytes().iter().all(|&c| is(c)),
            deprecation: None,
        },
        other => {
            let result = match *other {
                Value::Int(n) => int_result(class, n, is),
                _ => false,
            };
            CtypeOutcome {
                result,
                deprecation: Some(deprecation_message(&other.type_name())),
            }
        }
    }
}

fn int_result(class: CtypeClass, n: i64, is: &dyn Fn(u8) -> bool) -> bool {
    if let Ok(c) = u8::try_from(n) {
        is(c)
    } else if (-128..0).contains(&n) {
        // -128..=-1 map to bytes 0x80..=0xFF.
        is(u8::try_from(n + 256).unwrap_or(0))
    } else if n >= 0 {
        class.allow_digits()
    } else {
        class.allow_minus()
    }
}

macro_rules! string_fns {
    ($($(#[$m:meta])* $name:ident => $class:ident;)*) => {
        $(
            $(#[$m])*
            #[must_use]
            pub fn $name(s: &[u8]) -> bool {
                check_bytes(CtypeClass::$class, s)
            }
        )*
    };
}

string_fns! {
    /// `ctype_alnum()` on a string (C locale).
    ctype_alnum => Alnum;
    /// `ctype_alpha()` on a string (C locale).
    ctype_alpha => Alpha;
    /// `ctype_cntrl()` on a string (C locale).
    ctype_cntrl => Cntrl;
    /// `ctype_digit()` on a string (C locale).
    ctype_digit => Digit;
    /// `ctype_graph()` on a string (C locale).
    ctype_graph => Graph;
    /// `ctype_lower()` on a string (C locale).
    ctype_lower => Lower;
    /// `ctype_print()` on a string (C locale).
    ctype_print => Print;
    /// `ctype_punct()` on a string (C locale).
    ctype_punct => Punct;
    /// `ctype_space()` on a string (C locale).
    ctype_space => Space;
    /// `ctype_upper()` on a string (C locale).
    ctype_upper => Upper;
    /// `ctype_xdigit()` on a string (C locale).
    ctype_xdigit => Xdigit;
}

#[cfg(test)]
mod oracle_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn count(class: CtypeClass) -> usize {
        (0u8..=255).filter(|&c| class.matches(c)).count()
    }

    #[test]
    fn c_locale_counts_match_001_phpt() {
        // ext/ctype/tests/001.phpt
        let expected = [
            (CtypeClass::Lower, 26),
            (CtypeClass::Upper, 26),
            (CtypeClass::Alpha, 52),
            (CtypeClass::Digit, 10),
            (CtypeClass::Alnum, 62),
            (CtypeClass::Cntrl, 33),
            (CtypeClass::Graph, 94),
            (CtypeClass::Print, 95),
            (CtypeClass::Punct, 32),
            (CtypeClass::Space, 6),
            (CtypeClass::Xdigit, 22),
        ];
        for (class, n) in expected {
            assert_eq!(count(class), n, "{class:?}");
        }
    }

    #[test]
    fn strings() {
        assert!(!ctype_digit(b""));
        assert!(ctype_digit(b"0123"));
        assert!(!ctype_digit(b"12a"));
        assert!(ctype_space(b" \t\n\r\x0b\x0c"));
        assert!(!ctype_space(b" \0"));
        assert!(ctype_xdigit(b"09afAF"));
        assert!(!ctype_alpha(b"\xe4"));
        assert!(ctype_punct(b"!@#~"));
        assert!(ctype_cntrl(b"\x7f\0"));
        assert!(ctype_print(b" a~"));
        assert!(!ctype_graph(b" a~"));
        assert!(ctype_upper(b"AZ"));
        assert!(ctype_lower(b"az"));
        assert!(ctype_alnum(b"a1Z"));
    }

    /// Without a `setlocale()` the process runs in the C locale (test
    /// binaries never call it), where no byte `>= 0x80` is classified;
    /// php8.5 -r 'var_dump(ctype_lower("\xe4"), ctype_alpha("abc"));'
    /// gives false, true. Under de_DE (ISO-8859-1) C PHP answers true
    /// for "\xe4" (ext/ctype/tests/lc_ctype_inheritance.phpt), which
    /// this crate gets from `islower()`; that needs a locale this box
    /// does not have, so the CI run of that .phpt is its test.
    #[test]
    fn current_locale_classifies_high_bytes_through_libc() {
        assert!(!CtypeClass::Lower.matches_locale(0xe4));
        assert!(CtypeClass::Lower.matches_locale(b'a'));
        assert!(!ctype_call_locale(CtypeClass::Lower, &Value::from(&b"\xe4"[..])).result);
        for c in 0u8..=255 {
            for class in CtypeClass::ALL {
                assert_eq!(class.matches_locale(c), class.matches(c), "{class:?} {c}");
            }
        }
    }

    #[test]
    fn integers() {
        let o = ctype_call(CtypeClass::Digit, &Value::Int(53));
        assert!(o.result);
        assert_eq!(
            o.prefixed_deprecation(CtypeClass::Digit).as_deref(),
            Some("ctype_digit(): Argument of type int will be interpreted as string in the future")
        );
        assert!(!ctype_call(CtypeClass::Digit, &Value::Int(5)).result);
        assert!(ctype_call(CtypeClass::Digit, &Value::Int(256)).result);
        assert!(!ctype_call(CtypeClass::Digit, &Value::Int(-129)).result);
        assert!(ctype_call(CtypeClass::Graph, &Value::Int(-129)).result);
        assert!(ctype_call(CtypeClass::Print, &Value::Int(-1000)).result);
        assert!(!ctype_call(CtypeClass::Alpha, &Value::Int(1000)).result);
        // -128..-1 map to 0x80..0xFF: nothing in the C locale.
        assert!(!ctype_call(CtypeClass::Print, &Value::Int(-1)).result);
        let latin1 = CtypeTable::latin1();
        assert!(ctype_call_with(&latin1, CtypeClass::Lower, &Value::Int(-28)).result);
    }

    #[test]
    fn other_types() {
        let o = ctype_call(CtypeClass::Alnum, &Value::Float(10.0));
        assert!(!o.result);
        assert_eq!(
            o.deprecation.as_deref(),
            Some("Argument of type float will be interpreted as string in the future")
        );
        for (v, name) in [
            (Value::Null, "null"),
            (Value::Bool(true), "bool"),
            (Value::empty_array(), "array"),
        ] {
            let o = ctype_call(CtypeClass::Print, &v);
            assert!(!o.result);
            assert_eq!(o.deprecation, Some(deprecation_message(name)));
        }
        let s = ctype_call(CtypeClass::Alpha, &Value::from("abc"));
        assert!(s.result && s.deprecation.is_none());
        let mut v = Value::Int(65);
        let _ = v.make_ref();
        assert!(ctype_call(CtypeClass::Upper, &v).result);
    }

    #[test]
    fn latin1_table() {
        let t = CtypeTable::latin1();
        assert!(check_bytes_with(&t, CtypeClass::Lower, b"\xe4\xdf"));
        assert!(check_bytes_with(&t, CtypeClass::Upper, b"\xc4"));
        assert!(!check_bytes_with(&t, CtypeClass::Alpha, b"\xd7"));
        assert!(check_bytes_with(&t, CtypeClass::Punct, b"\xd7\xf7"));
        assert!(check_bytes_with(&t, CtypeClass::Cntrl, b"\x80\x9f"));
        assert!(!check_bytes_with(&t, CtypeClass::Digit, b"\xb2"));
        assert_eq!(CtypeTable::default(), CtypeTable::c_locale());
    }

    #[test]
    fn names() {
        for c in CtypeClass::ALL {
            assert_eq!(CtypeClass::from_function_name(c.function_name()), Some(c));
        }
        assert_eq!(
            CtypeClass::from_function_name("CTYPE_ALPHA"),
            Some(CtypeClass::Alpha)
        );
        assert_eq!(CtypeClass::from_function_name("ctype_foo"), None);
    }
}
