# Contributing to buzzs-rust-php-project

This project re-implements PHP in Rust. Its goal is to pass the php-src `.phpt`
test suite byte for byte, and to run as fast as the C implementation or faster.
Every change is judged against those two things: the output PHP produces, and
the time it takes.

## Reporting bugs

Open an issue with a short PHP script, the output you expected (what the C
implementation prints, with its version) and what this project prints. A
reproducer of a few lines is worth more than a description.

Security vulnerabilities are not reported in issues: see [SECURITY.md](SECURITY.md).

## What a change must do

- **Match PHP exactly.** Output, warnings and error messages, line numbers,
  `var_dump()` formatting and float printing must be byte for byte what PHP
  prints. Behaviour follows php-src master. A PHP release (currently 8.5) can
  serve as an oracle for checking what PHP does: run a small script through it
  rather than guessing. Where the release and master differ, follow master.
- **Port from php-src.** Each crate's doc comments cite the php-src file(s)
  it ports (`ref: Zend/zend_hash.c`). Match that C source's observable
  behaviour, and cite the php-src commit, issue or advisory a change follows.
  Do not copy code from other PHP implementations.
- **Not regress the suite or the speed.** See [Testing](#testing) and
  [Performance](#performance).

## Code rules

- Idiomatic Rust, `cargo clippy --all-targets` clean at the workspace's
  pedantic settings, and formatted with `cargo fmt`.
- No `unsafe` unless there is no safe way, in practice only in FFI modules,
  and every `unsafe` block carries a `// SAFETY:` comment saying why it holds.
- No `unwrap()` in library code paths that can fail.
- Unit tests in every crate, for the change you make.
- Crates are layered: `php-runtime` holds the shared types (`Value`, `ZStr`,
  `ZArray`) and crates coordinate only through them. An extension is two
  crates: `php-<name>` with the logic, independent of the VM, and
  `php-ext-<name>`, a thin binding through `php_vm::ext`. A new extension
  crate is live only once it is listed in `crates/php-cli/src/extensions.rs`.
- New source files start with the two-line copyright and license header every
  existing file carries (copy it from any file in `crates/`).

## Building

Install the latest stable Rust first: see "Installing Rust" in the [README](README.md)
(minimum 1.85, no maximum).

```sh
cargo build -p php-cli -p phpt-runner            # target/debug/php and the runner
target/debug/php -r 'echo 1+2, "\n";'            # 3
```

Use dev builds while working. The release profile (thin LTO, one codegen unit)
is slow to rebuild; keep it for performance measurements.

## Testing

Fetch the php-src test tree first. It is pinned to the php-src revision the
pass rate and the baseline are measured against:

```sh
scripts/fetch-phpt-tests.sh                      # into modules/php-src
```

Before a change is done, for every crate it touches (several `-p` in one
command):

```sh
cargo fmt && cargo clippy --all-targets -p <crate> && cargo test -p <crate>
```

Then run the `.phpt` tests the change affects (a file or a directory):

```sh
env -u LANGUAGE target/debug/phpt-runner --php target/debug/php -j 4 -q \
    --no-artifacts --summary-depth 2 modules/php-src/ext/json
```

- `--show-diff` shows expected against actual output for a failure.
- `--baseline phpt-baseline.txt` fails only on regressions against the known
  failures. The baseline is refreshed by the maintainer from a full run; do
  not edit it in a pull request.
- `phpt-scope.txt` lists the out-of-scope parts of the suite (they report
  `OUT`); `phpt-environment.txt` lists tests that fail for host reasons under
  the C implementation too (they count as passes).
- `env -u LANGUAGE` matters only if your shell exports `LANGUAGE`.

The full run (release build, all crate tests, the whole in-scope suite and the
runtime comparison) is `./run-all-tests.sh --no-commit`.

## Performance

The target is a total runtime of at most 1.10 times the C implementation's on
the same inputs. If a change touches a hot path (the VM executor, or the
value, array and string code of `php-runtime`), run:

```sh
scripts/acceptance.sh
```

It counts the instructions each benchmark workload takes and compares them
with the counts recorded for the C implementation in `perf/bench/goal.tsv`;
every workload must stay within 1.10 times its goal. See `perf/README.md` for
the method. In hot paths, avoid needless allocation, cloning and indirection.

## Commits and pull requests

- One logical change per commit. The subject says what changed; the body
  says why, and names the php-src commit, issue or advisory it follows.
- Update the `%` column of [FEATURE_GAP.md](FEATURE_GAP.md) for a row the
  change advances.
- Do not edit `modules/php-src`: it is a reference copy of upstream.
- A pull request lists the commands you ran and their results: the crate
  checks, the `.phpt` directories, and `scripts/acceptance.sh` when a hot
  path changed.

## License

The project is licensed under the BSD-3-Clause ([LICENSE](LICENSE)). By
contributing you agree that your contribution is licensed under the same
terms, and that you have the right to submit it.
