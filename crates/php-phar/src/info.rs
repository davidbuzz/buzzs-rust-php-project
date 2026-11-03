//! `phpinfo()` output (`PHP_MINFO_FUNCTION(phar)`) and the INI entries.

use crate::registry as reg;

/// The INI entries: (name, default, changeable everywhere).
pub const INI_ENTRIES: &[(&str, &str, bool)] = &[
    ("phar.readonly", "1", true),
    ("phar.require_hash", "1", true),
    ("phar.cache_list", "", false),
];

/// The two-column table rows.
pub fn table() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Phar: PHP Archive support", "enabled"),
        ("Phar API version", crate::consts::API_VERSION_STR),
        ("Phar-based phar archives", "enabled"),
        ("Tar-based phar archives", "enabled"),
        ("ZIP-based phar archives", "enabled"),
        (
            "gzip compression",
            if reg::has_zlib() {
                "enabled"
            } else {
                "disabled (install ext/zlib)"
            },
        ),
        (
            "bzip2 compression",
            if reg::has_bz2() {
                "enabled"
            } else {
                "disabled (install ext/bz2)"
            },
        ),
        (
            "OpenSSL support",
            if crate::signature::openssl_available() {
                "enabled"
            } else {
                "disabled (install ext/openssl)"
            },
        ),
    ]
}

/// The lines of the credits box (joined with `<br />` or `\n`).
pub const BOX_LINES: [&str; 3] = [
    "Phar based on pear/PHP_Archive, original concept by Davey Shafik.",
    "Phar fully realized by Gregory Beaver and Marcus Boerger.",
    "Portions of tar implementation Copyright (c) 2003-2009 Tim Kientzle.",
];

#[cfg(test)]
mod tests {
    #[test]
    fn rows() {
        let t = super::table();
        assert_eq!(t[1], ("Phar API version", "1.1.1"));
        assert_eq!(t.len(), 8);
    }
}
