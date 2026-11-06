# Proposals for other crates (from php-ext-pcre)

## Done: php-runtime `ZStr` carries PHP's `IS_STR_VALID_UTF8` flag

`ZStr::is_valid_utf8()` / `ZStr::mark_valid_utf8()` (ws27/strings2) replace
the one-entry identity cache (`state::VALID_UTF8`, strings >= 4 KB) that used
to stand in for the flag: `state::known_valid_utf8(s)` reads the flag and
`state::remember_valid_utf8(s)` sets it after a `/u` match from offset 0
without error, as `php_pcre_match_impl` does, for subjects of any size and
without holding a handle to them.
