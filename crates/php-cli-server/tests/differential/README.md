# Web SAPI differential tests

`difftest.py` plays each scenario under `scenarios/` against our `php -S`,
`php-cgi` and `php-cgi -b` (FastCGI) and against the C implementation's
(`/usr/bin/php8.5`, `/usr/bin/php-cgi8.5`), and prints the first difference
in each scenario. A scenario is a document root plus `scenario.json` (see
the script's docstring). Every scenario sends several requests to the same
server process, so state one request leaves behind shows up in the next.

```sh
# our binaries from target/debug; one core; every server is killed afterwards
python3 crates/php-cli-server/tests/differential/difftest.py --ours target/debug
python3 .../difftest.py --ours target/debug --every scenarios/resp_headers   # all differing requests
python3 .../difftest.py --ours target/debug -v --mode cgi scenarios/sessions # full responses
```

`DIFFTEST_CPU` picks the core (default 12). The C side gets Ubuntu's shared
extensions with `-d extension=...` (it runs with `-n`). Only what must
differ is normalised: `Date`, the port, the `X-Powered-By` version (and the
lengths of var_dump()ed strings holding it), session ids and temporary file
names (mapped to their order of first appearance, with their length), and
the `Host` header php 8.5's server echoes (master does not).

Where PHP 8.5 and php-src master differ (new defaults, deprecations,
messages), master is the specification: scenarios avoid those spots or pin
them with `-d`. `open/` holds scenarios that still differ; its README says
why.
