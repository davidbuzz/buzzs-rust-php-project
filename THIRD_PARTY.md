# Third-party code

buzzs-rust-php-project ports php-src to Rust. Its files are derived from
php-src and from libraries that php-src bundles. This file lists where each
part comes from, who holds the copyright, and which license covers it. The
license texts are in [LICENSES/](LICENSES/), copied unchanged from php-src.

The repository's own [LICENSE](LICENSE) (BSD-3-Clause) covers the port
**only where the upstream license allows it**. Code derived from the
components below stays under the upstream license, and its notices must be
kept.

## php-src itself

| | |
|---|---|
| Upstream | https://github.com/php/php-src (`Zend/`, `main/`, `ext/`, `sapi/`, `TSRM/`, `run-tests.php`) |
| Copyright | © 1999–2026 The PHP Group and Contributors; © 1999–2026 Zend Technologies Ltd., a subsidiary company of Perforce Software, Inc.; plus the individual authors named in the `Authors:` block of each C file |
| License | BSD-3-Clause ([LICENSES/php-src-BSD-3-Clause.txt](LICENSES/php-src-BSD-3-Clause.txt)) |
| Used by | nearly every crate: each file's `ref:` doc comment names the C file(s) it ports |

When a Rust file ports a php-src C file, the copyright of that C file's
authors carries over into the Rust file. The `ref:` comment names that C
file, and its header names the authors.

## Libraries bundled with php-src

php-src's [`README.REDIST.BINS`](LICENSES/php-src-README.REDIST.BINS.txt)
reproduces the notices of the code it bundles. The parts this repository
ports are listed here.

| Component (php-src path) | Copyright | License | License text | Crates derived from it |
|---|---|---|---|---|
| libmbfl (`ext/mbstring/libmbfl`) | © 1998–2001 HappySize, Inc., and contributors | **LGPL-2.1-only** | [LGPL-2.1.txt](LICENSES/LGPL-2.1.txt) | `php-mbstring` (and the `php-ext-mbstring` binding) |
| libbcmath (`ext/bcmath/libbcmath`) | © 1991–1997 Free Software Foundation, Inc.; © 2000 Philip A. Nelson | **LGPL-2.0-or-later** (shipped with the 2.1 text) | [LGPL-2.1.txt](LICENSES/LGPL-2.1.txt) | `php-bcmath` |
| timelib (`ext/date/lib`) | © 2015–2021 Derick Rethans; © 2017–2019, 2021 MongoDB, Inc. | MIT | [timelib-MIT.txt](LICENSES/timelib-MIT.txt) | `php-date` |
| lexbor (`ext/lexbor`) | © 2018–2020 Alexander Borisov | Apache-2.0 | [lexbor-Apache-2.0.txt](LICENSES/lexbor-Apache-2.0.txt), [lexbor-NOTICE.txt](LICENSES/lexbor-NOTICE.txt) | `php-dom` (vendored C sources with their own notices in `crates/php-dom/vendor/`), `php-uri` (WHATWG URL) |
| uriparser (`ext/uri/uriparser`) | © 2007 Weijia Song, Sebastian Pipping, and contributors | BSD-3-Clause | [uriparser-BSD-3-Clause.txt](LICENSES/uriparser-BSD-3-Clause.txt) | `php-uri` (RFC 3986) |
| libavifinfo (`ext/standard/libavifinfo`) | © 2021 Alliance for Open Media | BSD-2-Clause | [libavifinfo-BSD-2-Clause.txt](LICENSES/libavifinfo-BSD-2-Clause.txt) | `php-exif` |
| libmagic CDF reader (`ext/fileinfo/libmagic/cdf.c`) | © Ian F. Darwin; © 2008 Christos Zoulas | BSD-2-Clause | [libmagic-BSD-2-Clause.txt](LICENSES/libmagic-BSD-2-Clause.txt) | `php-ext-fileinfo` |
| xxHash (`ext/hash/xxhash`) | © 2012–2021 Yann Collet | BSD-2-Clause | [xxHash-BSD-2-Clause.txt](LICENSES/xxHash-BSD-2-Clause.txt) | `php-hash` |
| PMurHash (`ext/hash/murmur`) | Austin Appleby, Shane Day | public domain | — | `php-hash` |
| Mersenne Twister (`ext/random/engine_mt19937.c`) | © 1997–2002 Makoto Matsumoto and Takuji Nishimura; © 2000–2003 Richard J. Wagner | BSD-3-Clause | README.REDIST.BINS §6 | `php-random` |
| FreeSec crypt (`ext/standard/crypt_freesec.c`) | © 1994 David Burren | BSD-3-Clause | README.REDIST.BINS §4 | `php-stdmisc` |
| crypt_blowfish (`ext/standard/crypt_blowfish.c`) | Solar Designer | public domain (fallback: permissive) | README.REDIST.BINS §5 | `php-stdmisc` |
| scanf (`ext/standard/scanf.c`, from Tcl 8.3) | © The Regents of the University of California, Sun Microsystems, Inc., Scriptics Corporation, and others | Tcl license | README.REDIST.BINS §7 | `php-stdstr` |
| strnatcmp (`ext/standard/strnatcmp.c`) | © 2000 Martin Pool | zlib | README.REDIST.BINS §8 | `php-stdstr` |
| uuencode (`ext/standard/uuencode.c`) | © 1983, 1993 The Regents of the University of California | BSD-4-Clause (UC) | README.REDIST.BINS §9 | `php-stdstr` |
| snprintf / spprintf (`main/snprintf.c`, `main/spprintf.c`) | © 2002, 2006 Todd C. Miller; © 1995–1998 The Apache Group | ISC; Apache-1.0 | README.REDIST.BINS §10 | `php-support` |
| phar tar code (`ext/phar/tar.c`) | © 2003–2009 Tim Kientzle | BSD-2-Clause | README.REDIST.BINS §14 | `php-phar` |

Notes:

- **LGPL-2.1 (libmbfl, libbcmath).** Code derived from LGPL-2.1 code can
  only be distributed under the LGPL-2.1 (or the GPL). The BSD-3-Clause
  grant in [LICENSE](LICENSE) does not cover those crates.
- **zlib (strnatcmp).** Altered versions must be marked plainly as altered.
  The Rust port is one, and it is not the original software.
- **BSD-4-Clause (uuencode) and Apache-1.0 (spprintf)** come with
  advertising and acknowledgement clauses. Keep the acknowledgements from
  README.REDIST.BINS §9 and §10 in documentation that ships binaries.

## Not ported, only used

The oracle and test tree under `modules/php-src` (gitignored, fetched by
`scripts/fetch-phpt-tests.sh`) keeps php-src's own license. `tools/lcov` is
GPL-2.0-or-later. System libraries linked at build or run time (libxml2,
libxslt, OpenSSL, ICU, PCRE2, zlib, libbz2, libcurl, libzip, libgd,
libmagic, libsodium, libsqlite3, and the rest) keep their own licenses. This
repository contains none of their code.

## Keeping this file current

A new port of a bundled library, or of a php-src file that carries its own
third-party notice, gets a row here. If its license text is not already in
`LICENSES/`, copy it there unchanged from php-src.
