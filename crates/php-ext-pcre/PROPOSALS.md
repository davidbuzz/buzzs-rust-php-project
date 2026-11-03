# Proposals for other crates (from php-ext-pcre)

## php-runtime: a cached "valid UTF-8" flag on `ZStr` (PHP's `IS_STR_VALID_UTF8`)

`preg_match()` with `/u` validates the whole subject on every call. PHP avoids
that by flagging a `zend_string` once a match from offset 0 succeeded
(`GC_ADD_FLAGS(subject, IS_STR_VALID_UTF8)`) and passing `PCRE2_NO_UTF_CHECK`
afterwards (`bug72685.phpt`: 64K `preg_match('/\G\w/u', $s, $m, 0, $pos)` calls
on a 64 KB string must stay well under a second).

Until `ZStr` has such a flag, `php-ext-pcre` keeps a one-entry identity cache
(`state::VALID_UTF8`, strings ≥ 4 KB only) that holds a clone of the last
validated subject. That keeps one large string alive a little longer than PHP
would (visible only via `memory_get_usage()` / refcount dumps right after a
`/u` match on a ≥ 4 KB string). A real flag removes that compromise:

```diff
 struct StrInner {
     hash: Cell<u64>,
     interned: bool,
+    /// `IS_STR_VALID_UTF8`: the bytes are known to be valid UTF-8.
+    /// Cleared whenever the bytes are mutated in place.
+    valid_utf8: Cell<bool>,
     bytes: Vec<u8>,
 }

 impl ZStr {
+    /// Whether the string is known to be valid UTF-8 (`ZSTR_IS_VALID_UTF8`).
+    #[must_use]
+    pub fn is_known_valid_utf8(&self) -> bool {
+        self.0.valid_utf8.get()
+    }
+
+    /// Records that the string is valid UTF-8 (`GC_ADD_FLAGS(s, IS_STR_VALID_UTF8)`).
+    /// Callers must only set it after an actual validation.
+    pub fn mark_valid_utf8(&self) {
+        self.0.valid_utf8.set(true);
+    }
```

(plus `valid_utf8: Cell::new(false)` in the constructors and a reset in every
in-place append/mutation path). The binding would then replace
`state::known_valid_utf8(s)` / `state::remember_valid_utf8(s)` with
`s.is_known_valid_utf8()` / `s.mark_valid_utf8()`.
