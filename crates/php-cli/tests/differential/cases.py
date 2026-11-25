"""Case list for clidiff.py.  Left out on purpose: -i/-m/--re of whole
extension sets without -n (the two builds load different extension
lists), -S/-t that would start a server, -a with a terminal."""

N = ["-n"]
CASES = []


def case(name, args, **kw):
    kw["name"] = name
    kw["args"] = args
    CASES.append(kw)


S_ARGV = "<?php var_dump($argc, $argv, $_SERVER['argc'], $_SERVER['argv']);"
S_SRV = ("<?php foreach (['SCRIPT_NAME','PHP_SELF','SCRIPT_FILENAME',"
         "'PATH_TRANSLATED','DOCUMENT_ROOT'] as $k) "
         "{ echo $k, '=', var_export($_SERVER[$k] ?? null, true), \"\\n\"; }"
         "var_dump(is_int($_SERVER['REQUEST_TIME']), "
         "is_float($_SERVER['REQUEST_TIME_FLOAT']), PHP_SAPI, php_sapi_name());")

# --- script sources -------------------------------------------------
case("r-basic", N + ["-r", "echo 1+2, \"\\n\";"])
case("r-argv", N + ["-r", "var_dump($argv, $argc);", "a", "b"])
case("r-argv-dashdash", N + ["-r", "var_dump($argv);", "--", "-x", "y"])
case("r-open-tag", N + ["-r", "<?php echo 1;"])
case("r-parse-error", N + ["-r", "echo 1"])
case("r-empty", N + ["-r", ""])
case("f-argv", N + ["-f", "a.php", "x", "y"], files={"a.php": S_ARGV})
case("f-argv-dashdash", N + ["-f", "a.php", "--", "-x"], files={"a.php": S_ARGV})
case("script-argv", N + ["a.php", "-x", "--y", "z"], files={"a.php": S_ARGV})
case("script-server", N + ["a.php"], files={"a.php": S_SRV})
case("script-server-sub", N + ["sub/a.php"], files={"sub/a.php": S_SRV})
case("stdin-script", N, stdin="<?php echo 'hi', PHP_EOL; var_dump($argv);")
case("stdin-dashdash", N + ["--", "a", "b"], stdin="<?php var_dump($argv);")
case("stdin-dash", N + ["-", "a"], stdin="<?php var_dump($argv);")
case("stdin-server", N, stdin=S_SRV)
case("shebang", N + ["a.php"], files={"a.php": "#!/usr/bin/php\n<?php echo __LINE__, \"\\n\";"})
case("shebang-stdin", N, stdin="#!/usr/bin/php\n<?php echo __LINE__;")
case("missing-script", N + ["nope.php"])
case("dir-script", N + ["sub"], files={"sub/x": ""})
case("missing-f", N + ["-f", "nope.php"])
case("f-no-arg", N + ["-f"])
case("r-no-arg", N + ["-r"])
case("d-no-arg", N + ["-d"])
case("c-no-arg", N + ["-c"])
case("unknown-opt", N + ["-Y"])
case("unknown-long", N + ["--bogus"])
case("r-and-f", N + ["-r", "echo 1;", "-f", "a.php"], files={"a.php": "x"})
case("f-and-r", N + ["-f", "a.php", "-r", "echo 1;"], files={"a.php": "x"})
case("r-twice", N + ["-r", "echo 1;", "-r", "echo 2;"])
case("opt-after-script", N + ["a.php", "-n", "-r", "x"], files={"a.php": S_ARGV})
case("text-outside-tags", N + ["a.php"], files={"a.php": "hello\n<?php echo 1 ?>\nbye\n"})

# --- exit statuses ----------------------------------------------------
case("exit-3", N + ["-r", "exit(3);"])
case("exit-str", N + ["-r", "exit('bye');"])
case("exit-256", N + ["-r", "exit(256);"])
case("exit-neg", N + ["-r", "exit(-1);"])
case("exit-true", N + ["-r", "exit(true);"])
case("exit-float", N + ["-r", "exit(2.5);"])
case("uncaught", N + ["-r", "throw new Exception('boom');"])
case("uncaught-script", N + ["a.php"], files={"a.php": "<?php\nfunction f(){throw new LogicException('x');}\nf();"})
case("fatal", N + ["-r", "undefined_fn();"])
case("fatal-class", N + ["a.php"], files={"a.php": "<?php\nclass A{}\nclass A{}"})
case("parse-error", N + ["a.php"], files={"a.php": "<?php\necho 1\necho 2;"})
case("die-shutdown", N + ["-r", "register_shutdown_function(function(){echo 's'; die(4);}); echo 'm';"])
case("exit-in-shutdown-after-exit", N + ["-r", "register_shutdown_function(function(){exit(5);}); exit(2);"])
case("exception-in-shutdown", N + ["-r", "register_shutdown_function(function(){throw new Exception('s');});"])
case("trigger-error", N + ["-r", "trigger_error('u', E_USER_ERROR);"])
case("memory-exhaust", ["-n", "-d", "memory_limit=4M", "-r", "$a=str_repeat('x', 8<<20);"], nomem=True)

# --- -l ------------------------------------------------------------------
case("lint-good", N + ["-l", "a.php"], files={"a.php": "<?php echo 1;"})
case("lint-bad", N + ["-l", "a.php"], files={"a.php": "<?php echo 1"})
case("lint-many", N + ["-l", "a.php", "b.php", "c.php"],
     files={"a.php": "<?php echo 1;", "b.php": "<?php echo", "c.php": "<?php 2;"})
case("lint-missing", N + ["-l", "nope.php"])
case("lint-stdin", N + ["-l"], stdin="<?php echo 1;")
case("lint-stdin-bad", N + ["-l"], stdin="<?php echo")
case("lint-compile-error", N + ["-l", "a.php"], files={"a.php": "<?php function f(){} function f(){}"})
case("lint-long", N + ["--syntax-check", "a.php"], files={"a.php": "<?php echo 1;"})
case("lint-r", N + ["-l", "-r", "echo 1;"])

# --- -w / -s ------------------------------------------------------------
case("strip", N + ["-w", "a.php"], files={"a.php": "<?php\n// c\n/* d */ echo   1 ;  # e\n$a =\n  2;\n?>\nhtml\n"})
case("strip-long", N + ["--strip", "a.php"], files={"a.php": "<?php /** x */ echo 1;"})
case("strip-stdin", N + ["-w"], stdin="<?php  echo  1;")
case("highlight", N + ["-s", "a.php"], files={"a.php": "<?php\n// c\necho 'x' . \"y$z\";\n?>\nhtml\n"})
case("highlight-long", N + ["--syntax-highlight", "a.php"], files={"a.php": "<?php echo 1;"})
case("highlight-stdin", N + ["-s"], stdin="<?php echo 1;")

# --- -B/-R/-F/-E ---------------------------------------------------------------
LINES = "a\nbb\nccc\n"
case("R", N + ["-R", "echo strtoupper($argn), $argi, \"\\n\";"], stdin=LINES)
case("BRE", N + ["-B", "$n=0;", "-R", "$n+=strlen($argn);", "-E", "echo $n;"], stdin=LINES)
case("F", N + ["-F", "a.php"], stdin=LINES, files={"a.php": "<?php echo '[', $argn, ']';"})
case("R-noeol", N + ["-R", "echo $argn, '|';"], stdin="a\nb")
case("R-empty", N + ["-R", "echo $argn, '|';"], stdin="")
case("B-only", N + ["-B", "echo 'b';"], stdin=LINES)
case("E-only", N + ["-E", "echo 'e';"], stdin=LINES)
case("R-crlf", N + ["-R", "var_dump($argn);"], stdin="a\r\nb\r\n")
case("R-and-r", N + ["-R", "echo 1;", "-r", "echo 2;"])
case("R-twice", N + ["-R", "echo 1;", "-R", "echo 2;"])
case("R-argv", N + ["-R", "var_dump($argv);", "--", "x"], stdin="l\n")
case("long-process", N + ["--process-begin", "echo 'b';", "--process-code", "echo $argn;", "--process-end", "echo 'e';"], stdin="1\n2\n")
case("F-missing", N + ["-F", "nope.php"], stdin="a\n")

# --- info options ---------------------------------------------------------------
case("v", N + ["-v"])
case("version", N + ["--version"])
case("h", N + ["-h"])
case("help", N + ["--help"])
case("usage-q", N + ["-?"])
case("ini", N + ["--ini"], env={"PHP_INI_SCAN_DIR": ""})
case("ini-c", ["-c", "{d}/php.ini", "--ini"], files={"php.ini": "precision=3\n"}, env={"PHP_INI_SCAN_DIR": ""}, nomem=True)
case("rf", N + ["--rf", "strlen"])
case("rf-missing", N + ["--rf", "nope_fn"])
case("rf-long", N + ["--rfunction", "array_map"])
case("rf-method", N + ["--rf", "ArrayObject::count"])
case("rc", N + ["--rc", "Countable"])
case("rc-missing", N + ["--rc", "NoSuch"])
case("rc-long", N + ["--rclass", "Stringable"])
case("re-core", N + ["--re", "Core"])
case("re-missing", N + ["--re", "nope"])
case("re-json", N + ["--re", "json"])
case("rz-missing", N + ["--rz", "nope"])
case("rz-long", N + ["--rzendextension", "nope"])
case("ri-core", N + ["--ri", "core"])
case("ri-standard", N + ["--ri", "standard"])
case("ri-json", N + ["--ri", "json"])
case("ri-date", ["-n", "-d", "date.timezone=UTC", "--ri", "date"])
case("ri-missing", N + ["--ri", "nope"])
case("ri-pcre", N + ["--ri", "pcre"])
case("rf-noarg", N + ["--rf"])
case("z-missing", N + ["-z", "nope.so", "-r", "echo 1;"])
case("q", N + ["-q", "-r", "echo 1;"])
case("C", N + ["-C", "-r", "echo getcwd() === realpath('.') ? 'y' : 'n';"])
case("H", N + ["-H", "-r", "var_dump($argv);", "--", "secret"])
case("e", N + ["-e", "-r", "echo 1;"])
case("S-noarg", N + ["-S"])
case("t-noarg", N + ["-t"])
case("t-without-S", N + ["-t", ".", "-r", "echo 1;"])
case("a-nontty", N + ["-a"], stdin="echo 1;\n")
case("m-dash-f", N + ["-m", "-f", "x"])

# --- STDIN/STDOUT/STDERR ----------------------------------------------------------------
case("stdin-fgets", N + ["-r", "var_dump(fgets(STDIN), fgets(STDIN), feof(STDIN), fgets(STDIN), feof(STDIN));"], stdin="l1\nl2")
case("stdin-empty", N + ["-r", "var_dump(fgets(STDIN), feof(STDIN), stream_get_contents(STDIN));"])
case("stdin-contents", N + ["-r", "var_dump(stream_get_contents(STDIN));"], stdin="abc\ndef")
case("stdin-meta", N + ["-r", "$m=stream_get_meta_data(STDIN); unset($m['unread_bytes']); var_dump($m);"], stdin="x")
case("stdout-meta", N + ["-r", "var_dump(stream_get_meta_data(STDOUT)['mode'], stream_get_meta_data(STDERR)['mode']);"])
case("stderr-write", N + ["-r", "fwrite(STDERR, 'e'); fwrite(STDOUT, 'o'); echo 'x';"])
case("php-stdin", N + ["-r", "var_dump(file_get_contents('php://stdin'));"], stdin="pp")
case("stdin-consts-from-stdin", N, stdin="<?php var_dump(defined('STDIN'), defined('STDOUT'), defined('STDERR'));")
case("stdin-consts-r", N + ["-r", "var_dump(STDIN, STDOUT, STDERR);"])
case("fclose-stdout", N + ["-r", "fclose(STDOUT); echo 'x'; fwrite(STDERR, 'y');"])

# --- errors ------------------------------------------------------------
ERR = "echo $u; $a=[]; echo $a['k']; strlen(); "
case("err-default", N + ["-r", "echo $u; echo 'after';"])
case("err-stderr", N + ["-d", "display_errors=stderr", "-r", "echo $u; echo 'a';"])
case("err-off", N + ["-d", "display_errors=0", "-r", "echo $u; echo 'a';"])
case("err-log-stderr", N + ["-d", "display_errors=0", "-d", "log_errors=1", "-r", "echo $u;"])
case("err-log-both", N + ["-d", "log_errors=1", "-r", "echo $u;"])
case("err-log-file", N + ["-d", "log_errors=1", "-d", "error_log={d}/e.log", "-r",
                          "echo $u; echo preg_replace('/^\\[[^]]*\\] /m', '', file_get_contents(ini_get('error_log')));"])
case("err-html", N + ["-d", "html_errors=1", "-r", "echo $u;"])
case("err-html-fatal", N + ["-d", "html_errors=1", "-r", "f();"])
case("err-html-exc", N + ["-d", "html_errors=1", "-r", "throw new Exception('<b>');"])
case("err-prepend", N + ["-d", "error_prepend_string=PRE", "-d", "error_append_string=APP", "-r", "echo $u;"])
case("err-reporting-0", N + ["-d", "error_reporting=0", "-r", "echo $u; f();"])
case("err-deprecated", N + ["-r", "$a = null; $a[] = 1; echo strlen(null);"])
case("err-warning", N + ["-r", "echo 1/0;"])
case("err-warning-fopen", N + ["-r", "fopen('/nonexistent/x', 'r');"])
case("err-script-file", N + ["a.php"], files={"a.php": "<?php\necho $u;\nf();\n"})
case("err-startup-ini", N + ["-d", "error_reporting=E_ALL", "-d", "precision=abc", "-r", "echo 1.5;"])
case("err-display-startup", N + ["-d", "display_startup_errors=0", "-d", "extension=nope.so", "-r", "echo 1;"])
case("err-extension-missing", N + ["-d", "extension=nope", "-r", "echo 1;"])
case("err-xmlrpc", N + ["-d", "xmlrpc_errors=1", "-r", "echo $u;"])
case("err-docref", N + ["-d", "html_errors=1", "-d", "docref_root=http://x/", "-r", "strlen();"])
case("err-ignore-repeated", N + ["-d", "ignore_repeated_errors=1", "-r", "for($i=0;$i<3;$i++) echo $u;"])
case("register-argc-argv-off", N + ["-d", "register_argc_argv=0", "-r", "var_dump(isset($argv), isset($_SERVER['argv']));", "x"])
case("prepend-append", N + ["-d", "auto_prepend_file={d}/p.php", "-d", "auto_append_file={d}/q.php", "a.php"],
     files={"a.php": "<?php echo 'main';", "p.php": "<?php echo 'pre';", "q.php": "<?php echo 'post';"})
case("prepend-r", N + ["-d", "auto_prepend_file={d}/p.php", "-r", "echo 'main';"], files={"p.php": "<?php echo 'pre';"})
case("prepend-missing", N + ["-d", "auto_prepend_file=nope.php", "-r", "echo 1;"])
case("prepend-missing-script", N + ["-d", "auto_prepend_file=nope.php", "a.php"], files={"a.php": "<?php echo 1;"})
case("append-after-exit", N + ["-d", "auto_append_file={d}/q.php", "a.php"], files={"a.php": "<?php exit('x');", "q.php": "<?php echo 'post';"})
case("cli-defaults", N + ["-r", "var_dump(ini_get('max_execution_time'), ini_get('output_buffering'), ini_get('implicit_flush'), ini_get('html_errors'), ini_get('display_errors'), ini_get('register_argc_argv'), ob_get_level());"])

# --- INI ------------------------------------------------------------------
case("ini-get-all-keys", N + ["-r", "$a = ini_get_all(null, true); ksort($a); foreach ($a as $k => $v) echo $k, \"\\n\";"])
case("ini-get-all-core", N + ["-r", "var_dump(ini_get_all('core'));"])
case("ini-get-all-core-nodetails", N + ["-r", "var_dump(ini_get_all('core', false));"])
case("ini-get-all-standard", N + ["-r", "var_dump(ini_get_all('standard'));"])
case("ini-get-all-date", N + ["-r", "var_dump(ini_get_all('date'));"])
case("ini-get-all-json", N + ["-r", "var_dump(ini_get_all('json'));"])
case("ini-get-all-pcre", N + ["-r", "var_dump(ini_get_all('pcre'));"])
case("ini-get-all-session", N + ["-r", "var_dump(ini_get_all('session'));"])
case("ini-get-all-missing", N + ["-r", "var_dump(ini_get_all('nope'));"])
case("ini-get-all-CORE", N + ["-r", "var_dump(count(ini_get_all('CORE')) > 0);"])
case("ini-get-missing", N + ["-r", "var_dump(ini_get('nope'), ini_set('nope', 1), ini_restore('nope'));"])
for name, vals in {
    "precision": ["17", "-1", "abc", "", "1k", "100"],
    "memory_limit": ["2G", "256m", "-1", "abc", "", "1x", "0x10", "99999999999999999999"],
    "display_errors": ["On", "yes", "true", "stderr", "stdout", "2", "garbage", ""],
    "error_reporting": ["E_ALL", "32767", "-1", "abc", ""],
    "max_execution_time": ["10", "-5", "abc"],
    "short_open_tag": ["Off", "on", "1", "none"],
    "upload_max_filesize": ["2G", "1g", "512", "10K"],
    "output_buffering": ["4096", "On", "Off", "1"],
    "serialize_precision": ["-1", "17", "abc"],
    "zend.assertions": ["1", "0", "-1", "5"],
    "default_charset": ["", "ISO-8859-1"],
    "date.timezone": ["UTC", "Nope/Zone", ""],
    "pcre.backtrack_limit": ["10", "1k", "abc"],
    "session.gc_probability": ["-1", "5"],
    "assert.active": ["0", "off"],
    "arg_separator.output": ["&amp;", ""],
    "user_agent": ["x y"],
    "auto_detect_line_endings": ["1"],
    "zend.enable_gc": ["0", "off"],
    "realpath_cache_size": ["1M", "abc"],
    "max_input_time": ["-1", "x"],
}.items():
    for i, v in enumerate(vals):
        case(f"d-{name}-{i}", N + ["-d", f"{name}={v}", "-r", f"var_dump(ini_get('{name}'));"])
case("d-flag-only", N + ["-d", "display_errors", "-r", "var_dump(ini_get('display_errors'));"])
case("d-quoted", N + ["-d", "error_log=\"a b\"", "-r", "var_dump(ini_get('error_log'));"])
case("d-const-expr", N + ["-d", "error_reporting=E_ALL & ~E_NOTICE", "-r", "var_dump(ini_get('error_reporting'), error_reporting());"])
case("d-unknown", N + ["-d", "foo.bar=baz", "-r", "var_dump(ini_get('foo.bar'), get_cfg_var('foo.bar'));"])
case("d-spaces", N + ["-d", " precision = 5 ", "-r", "var_dump(ini_get('precision'));"])
case("d-no-equals-unknown", N + ["-d", "foo", "-r", "var_dump(get_cfg_var('foo'));"])
case("d-twice", N + ["-d", "precision=3", "-d", "precision=4", "-r", "var_dump(ini_get('precision'));"])
case("d-extension-ini-later", N + ["-d", "date.timezone=Europe/Paris", "-r", "echo date_default_timezone_get();"])
case("get-cfg-var", N + ["-d", "precision=3", "-r", "var_dump(get_cfg_var('precision'), get_cfg_var('nope'), get_cfg_var('cfg_file_path'));"])
case("get-cfg-var-n-empty", N + ["-r", "var_dump(get_cfg_var('precision'));"])
case("loaded-file-n", N + ["-r", "var_dump(php_ini_loaded_file(), php_ini_scanned_files());"])

SETS = [
    ("precision", "5"), ("precision", "abc"), ("memory_limit", "64M"), ("memory_limit", "1"),
    ("display_errors", "stderr"), ("error_reporting", "E_ALL"), ("max_execution_time", "5"),
    ("include_path", ".:/tmp"), ("open_basedir", "/tmp"), ("short_open_tag", "0"),
    ("upload_max_filesize", "1M"), ("register_argc_argv", "0"), ("date.timezone", "Bad/Zone"),
    ("date.timezone", "UTC"), ("session.save_handler", "nope"), ("output_buffering", "1"),
    ("zend.assertions", "-1"), ("zend.assertions", "0"), ("assert.callback", "strlen"),
    ("default_charset", "latin1"), ("error_log", "{d}/x.log"), ("serialize_precision", "5"),
    ("pcre.jit", "0"), ("mbstring.language", "nope"), ("session.name", "1abc"),
    ("allow_url_fopen", "0"), ("expose_php", "0"), ("zend.enable_gc", "0"),
    ("arg_separator.output", ""), ("auto_prepend_file", "x"), ("max_input_time", "5"),
]
for i, (k, v) in enumerate(SETS):
    case(f"set-{k}-{i}", N + ["-r", f"var_dump(ini_set('{k}', '{v}'), ini_get('{k}')); ini_restore('{k}'); var_dump(ini_get('{k}'));"])
case("set-int-arg", N + ["-r", "var_dump(ini_set('precision', 5), ini_set('precision', 5.5), ini_set('precision', true), ini_set('precision', null), ini_get('precision'));"])
case("set-array-arg", N + ["-r", "try { ini_set('precision', []); } catch (TypeError $e) { echo $e->getMessage(); }"])
case("ini-alter", N + ["-r", "var_dump(ini_alter('precision', '3'), ini_get('precision'));"])
case("set-system-level", N + ["-r", "var_dump(ini_set('extension_dir', '/x'), ini_set('disable_functions', 'x'), ini_set('max_file_uploads', 3), ini_set('memory_limit', '1G'));"])
case("ini-parse-quantity", N + ["-r", "foreach (['1k','1M','1G','0x10','010','0b11','1x','',' 2 ','-1k','99999999999999999999G'] as $q) var_dump(ini_parse_quantity($q));"])
case("ini-get-bool-normal", N + ["-d", "short_open_tag=yes", "-d", "html_errors=On", "-r", "var_dump(ini_get('short_open_tag'), ini_get('html_errors'));"])

# --- php.ini via -c ------------------------------------------------------
INI = """; comment
[PHP]
precision = 7
error_reporting = E_ALL & ~E_NOTICE
foo.str = "quoted value"
foo.env = ${HOME_X}
foo.concat = "a"${HOME_X}"b"
foo.const = PHP_VERSION_ID
foo.bool = On
foo.none = none
foo.null = null
foo.num = 0x10
foo.arr[] = 1
foo.arr[] = 2
foo.arr[k] = v
[PATH=/nonexistent]
precision = 3
[HOST=localhost]
precision = 4
"""
CFG = ("var_dump(ini_get('precision'), ini_get('error_reporting'), get_cfg_var('foo.str'), "
       "get_cfg_var('foo.env'), get_cfg_var('foo.concat'), get_cfg_var('foo.bool'), get_cfg_var('foo.none'), "
       "get_cfg_var('foo.null'), get_cfg_var('foo.num'), get_cfg_var('foo.arr'), "
       "basename(php_ini_loaded_file()), php_ini_scanned_files());")
case("c-file", ["-c", "{d}/my.ini", "-r", CFG], files={"my.ini": INI}, env={"HOME_X": "hx"}, nomem=True)
case("c-dir", ["-c", "{d}", "-r", CFG], files={"php.ini": INI}, env={"HOME_X": "hx"}, nomem=True)
case("c-missing", ["-c", "{d}/nope.ini", "-r", "var_dump(php_ini_loaded_file());"], nomem=True)
case("c-and-n", ["-c", "{d}/my.ini", "-n", "-r", "var_dump(php_ini_loaded_file(), ini_get('precision'));"], files={"my.ini": "precision=3\n"}, nomem=True)
case("c-d-override", ["-c", "{d}/my.ini", "-d", "precision=9", "-r", "var_dump(ini_get('precision'));"], files={"my.ini": "precision=3\n"}, nomem=True)
case("c-syntax-error", ["-c", "{d}/my.ini", "-r", "echo ini_get('precision');"], files={"my.ini": "precision=3\n[x\nfoo=\"bar\n"}, nomem=True)
case("c-syntax-error2", ["-c", "{d}/my.ini", "-r", "echo ini_get('precision');"], files={"my.ini": "precision=(3\n"}, nomem=True)
case("c-memory-limit", ["-c", "{d}/my.ini", "-r", "echo ini_get('memory_limit');"], files={"my.ini": "memory_limit=33M\n"}, nomem=True)
case("phprc-file", ["-r", "var_dump(basename(php_ini_loaded_file()), ini_get('precision'));"],
     files={"my.ini": "precision=3\n"}, env={"PHPRC": "{d}/my.ini", "PHP_INI_SCAN_DIR": ""}, nomem=True)
case("phprc-dir", ["-r", "var_dump(basename(php_ini_loaded_file()), ini_get('precision'));"],
     files={"php.ini": "precision=3\n"}, env={"PHPRC": "{d}", "PHP_INI_SCAN_DIR": ""}, nomem=True)
case("scan-dir", ["-c", "{d}/my.ini", "-r", "var_dump(ini_get('precision'), ini_get('serialize_precision'), str_replace(getenv('D'), '', php_ini_scanned_files()));"],
     files={"my.ini": "precision=3\n", "conf.d/10-a.ini": "precision=5\n", "conf.d/20-b.ini": "serialize_precision=6\n", "conf.d/x.txt": "precision=9\n"},
     env={"PHP_INI_SCAN_DIR": "{d}/conf.d", "D": "{d}"}, nomem=True)
case("scan-dir-ini", ["-c", "{d}/my.ini", "--ini"],
     files={"my.ini": "precision=3\n", "conf.d/10-a.ini": "precision=5\n"},
     env={"PHP_INI_SCAN_DIR": "{d}/conf.d"}, nomem=True)
case("scan-dir-multi", ["-c", "{d}/my.ini", "-r", "var_dump(ini_get('precision'));"],
     files={"my.ini": "", "a/1.ini": "precision=5\n", "b/1.ini": "precision=6\n"},
     env={"PHP_INI_SCAN_DIR": "{d}/a:{d}/b"}, nomem=True)
case("script-dir-ini", ["a.php"], files={"a.php": "<?php var_dump(ini_get('precision'), php_ini_loaded_file() !== false);", "php.ini": "precision=3\n"},
     env={"PHPRC": "{d}/none", "PHP_INI_SCAN_DIR": ""}, nomem=True)
case("c-extension-section", ["-c", "{d}/my.ini", "-r", "var_dump(ini_get('date.timezone'));"], files={"my.ini": "[Date]\ndate.timezone=UTC\n"}, nomem=True)
case("c-include-path-dollar", ["-c", "{d}/my.ini", "-r", "var_dump(ini_get('include_path'));"], files={"my.ini": "include_path=\".:${PATHX}/lib\"\n"}, env={"PATHX": "/p"}, nomem=True)
case("c-raw-ini-chars", ["-c", "{d}/my.ini", "-r", "var_dump(get_cfg_var('a.b'), get_cfg_var('a.c'), get_cfg_var('a.d'));"],
     files={"my.ini": "a.b = foo ; trailing\na.c = 'single'\na.d = 1 | 2\n"}, nomem=True)
case("c-ini-constants", ["-c", "{d}/my.ini", "-r", "var_dump(get_cfg_var('a'), get_cfg_var('b'), get_cfg_var('c'));"],
     files={"my.ini": "a = E_ALL\nb = ~E_DEPRECATED\nc = E_WARNING|E_NOTICE\n"}, nomem=True)
case("c-memory-limit-dump", ["-c", "{d}/my.ini", "-r", "var_dump(ini_get_all('core')['memory_limit']);"], files={"my.ini": "memory_limit=33M\n"}, nomem=True)
case("i-n-keys", N + ["-i"])
