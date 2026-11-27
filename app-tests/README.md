# app-tests

Real PHP projects, run under our `php` and under the C implementation. A test that fails here
and passes there, or a page that comes out differently, is a bug in this code base.

## Nothing third-party is committed in this repository

**This directory contains no code, tests, data or documentation of any other project, only the
tools that fetch them when they are missing.**

- What git tracks here is exactly three files, all written for this project and under its
  licence (BSD-3-Clause): this `README.md`, `run-all-app-tests.sh` and `.gitignore`.
- The script knows, for each project, where its upstream is and which revision to take: a
  repository address and a commit, or the address of a release file and its SHA-256. That is
  all it holds about them.
- A project is downloaded from its own upstream, onto your disk only, into `app-tests/apps/`.
  Composer installs its dependencies there too. Both `app-tests/apps/` and `app-tests/logs/`
  are ignored by git (`.gitignore` in this directory).
- Each fetched project stays under its own licence, which is in its own checkout
  (`app-tests/apps/<name>/LICENSE` or similar). Those licences differ from ours and from each
  other; none of them applies to this repository, because none of that code is in it.

Rules for anyone changing this directory:

1. Never `git add` anything under `app-tests/apps/` or `app-tests/logs/`, and never copy a
   file, or part of a file, of a fetched project into a tracked file.
2. A change a fetched project needs before it can run (a test to leave out, a configuration
   file) is made by a command in the script, not kept as a patch file: a patch contains the
   project's own lines.
3. No credentials in tracked files. What needs a database takes its settings from the
   environment.

`app-tests/run-all-app-tests.sh --verify` checks rule 1 (it also runs before every fetch, and
the script refuses to fetch if `apps/` is not ignored):

```sh
app-tests/run-all-app-tests.sh --verify
git ls-files app-tests        # must list README.md, run-all-app-tests.sh and .gitignore only
```

## Running

```sh
cargo build --release -p php-cli                    # the binary under test
app-tests/run-all-app-tests.sh --list               # everything it knows, and what is on disk
app-tests/run-all-app-tests.sh reactphp-promise     # one suite or site (or several)
app-tests/run-all-app-tests.sh                      # all of them, one after the other
app-tests/run-all-app-tests.sh --fetch-only         # fetch and install what is missing; run nothing
```

Environment:

| Variable | Meaning |
|---|---|
| `PHP_BIN` | our binary (default `target/release/php`, else `target/debug/php`) |
| `REF_BIN` | the C implementation (default `/usr/bin/php8.5`); empty runs under ours only |
| `APP_TIMEOUT` | seconds one suite may take under one binary (default 2700) |
| `MYSQL_TEST_HOST`, `MYSQL_TEST_USER`, `MYSQL_TEST_PASSWD` | a MySQL or MariaDB server, the variables the PHPT runner uses |

What needs a database is left out, with a line saying so, when the `MYSQL_TEST_*` variables are
not set. It creates and drops databases whose names start with `apptests_`, and touches no
other, so the account needs the right to create databases.

**Nothing is downloaded twice.** A checkout that is on disk at its pinned commit is used as it
is, without touching the network; a release file that is on disk with the pinned SHA-256 is
not downloaded again, nor unpacked again; dependencies are installed only when they are
missing. Something is fetched again only when its pin in the script changes. The one exception
is the `composer` site, whose test is that each binary downloads and installs packages itself.

It is not part of `cargo test`. It runs one thing at a time, each a single process.

## What is compared, and under which settings

Both binaries run with the same settings: php-src's own defaults, written out in the script
(`settings_ini`). A distribution's `php.ini` changes several of them (which errors are shown,
whether assertions run, whether `$_ENV` is filled), and under those C would be compared with
ours on unequal terms. The settings go into an ini directory for each binary, so the processes
a suite starts get them too. C keeps a memory limit of 1G, ours 2G.

**Suites** (`suites` in the script) are PHPUnit runs. For each, one line: the tests run and
failed under ours, the same under C, the CPU time of each run and their ratio, and how many
tests fail only here, with their names. The console logs, the JUnit reports and the full list
(`<name>.fails-only-here.txt`) are in `app-tests/logs/`.

**Sites** (`sites` in the script) are applications and tools that are installed and exercised:
a tool's commands are run, or an application is installed into a fresh database, served by
`php -S` under each binary, and a fixed list of its pages fetched. Every command's output and
every page is one item. Items are compared after what must differ is taken out (times, tokens,
session ids, the two version numbers); the script lists the items that still differ, and
`<name>.differences.txt` shows how. Each binary gets a fresh copy of the application and a
fresh database, at the same path and under the same name, one after the other.

Two kinds of difference are taken out on purpose, each by a rule with its reason next to it in
the script:

- notices php-src master raises and 8.5 does not ("deprecated since 8.6"): ours follows master,
  the reference binary is 8.5;
- differences of the host, not of the engines (C's GD claiming an image format the system's
  libgd cannot write).

The exit status is 0 when nothing fails or differs only here and everything produced a result.

## The projects

Fetched from their upstreams at the revisions pinned in the script. A pin is moved on purpose,
by editing its line: results are comparable from run to run only against one revision.

| What | Kind | Upstream | Licence, as upstream declares it |
|---|---|---|---|
| `amphp-*` (14) | suites | https://github.com/amphp | MIT |
| `reactphp-*` (10) | suites | https://github.com/reactphp | MIT |
| `revolt-event-loop` | suite | https://github.com/revoltphp/event-loop | MIT |
| `laravel-framework` | suite | https://github.com/laravel/framework | MIT |
| `phpunit` (main), `phpunit-12-unit`, `phpunit-12-end-to-end` | suites | https://github.com/sebastianbergmann/phpunit | BSD-3-Clause |
| `doctrine-dbal`, `doctrine-dbal-mysqli` | suites | https://github.com/doctrine/dbal | MIT |
| `doctrine-orm` | suite | https://github.com/doctrine/orm | MIT |
| `symfony` (every component), `symfony-http-foundation`, `symfony-http-kernel` | suites | https://github.com/symfony | MIT |
| `symfony-demo` (its tests), `symfony-demo-pages` | suite, site | https://github.com/symfony/demo | MIT |
| `wordpress-develop-formatting`, `wordpress-develop-query` | suites | https://github.com/WordPress/wordpress-develop | GPL-2.0-or-later |
| `wordpress` (installed with WP-CLI) | site | https://wordpress.org, https://github.com/wp-cli/wp-cli | GPL-2.0-or-later; WP-CLI MIT |
| `mediawiki` | site | https://releases.wikimedia.org/mediawiki | GPL-2.0-or-later |
| `matomo` | site | https://builds.matomo.org | GPL-3.0-or-later |
| `composer` (run as a tool; it installs Monolog and PHPUnit) | site | https://getcomposer.org | MIT |

The licence column is for orientation; the licence file in each fetched copy is what counts.

## What it needs

`git`, `composer`, `curl`, `tar`, `sha256sum`, `python3`, `perl`, the `mysql` client for what
needs a database, and network access for whatever is not on disk yet. The reference binary
needs the extensions the projects use (on Debian or Ubuntu the `php8.5-*` packages for cli,
curl, dom/xml, gd, intl, mbstring, mysql, sqlite3, zip, bcmath, gmp).

## Limits of this version

- The dependencies of a library are not pinned: Composer resolves them on the day they are
  installed (a library has no lock file). They stay as installed until `vendor/` is removed.
- There is no list of known failures yet: every run reports all that fails only here.
- MediaWiki's own PHPUnit and parser suites are not run yet, only its installation and pages.
