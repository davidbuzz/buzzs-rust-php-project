a rust re-implementation of php that passes all its test suite.

the original php is written in C.

mandatory:   all best practice rust must be followed.

## reference source

modules/php-src is READONLY reference material that will be deleted after the port is done.  https://github.com/php/php-src

it is a plain clone of upstream `master`, currently at commit `0c7255dc521` ("Merge branch 'PHP-8.6'").

to recreate it:

```sh
mkdir -p modules
git clone https://github.com/php/php-src modules/php-src
```
