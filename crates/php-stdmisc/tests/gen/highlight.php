<?php
// Fixture generator: highlight_string() / php_strip_whitespace().
// "snip" hex(src) hex(highlight) hex(strip) for hand-written snippets;
// "corpus" path md5(highlight) md5(strip) for FILE sections of every 4th
// php-src .phpt (read from modules/php-src by the test when present).
// "colors" hex(src) hex(highlight) with custom highlight.* settings.
$snips = [
  "", "x", "<?php", "<?php ", "<?php\n", "<?php echo 1;", "<?php echo 'a' . \"b\$c\";", "<?= \$x ?>", "<? echo 1 ?>", "a<?php ?>b<?php\n?>\nc",
  "<?php\n// comment\n# hash\n/* block */\n/** doc */\n\$a = 1;", "<?php /* unterminated", "<?php \"unterminated \$x", "<?php 'unterminated",
  "<?php\n\$s = <<<EOT\nabc \$x {\$y[1]} \${z}\nEOT;\necho \$s;", "<?php\n\$s = <<<'EOT'\nraw \$x\n  EOT;\n", "<?php\n\$s = <<<EOT\n  a\n  EOT . 'x';\n",
  "<?php \"\$a->b \$a[0] \$a[k] {\$a->b->c} \${a}\";", "<?php `ls \$dir`;", "<?php function f(int \$a = 1, ...\$b): ?array { return [\$a, ...\$b]; }",
  "<?php class A extends B implements C { public const X = 1; private ?int \$y = null; public function __construct(protected readonly string \$z) {} }",
  "<?php namespace A\\B; use C\\D as E; \\F\\g(); namespace\\h(); __NAMESPACE__; __CLASS__; __LINE__; __FILE__; __DIR__; __FUNCTION__; __METHOD__; __TRAIT__; __PROPERTY__;",
  "<?php \$a <=> \$b ?? \$c ?->d; \$e **= 2; \$f <<= 1; (int) \$g; (string)\$h; @\$i; \$j instanceof K;",
  "<?php 0x1F; 0b101; 0o17; 017; 1_000; 1.5e3; .5; 1e; 1__0; 0x; 9223372036854775808; 08;",
  "<?php if (\$a): elseif (\$b): else: endif; foreach (\$x as \$k => &\$v) {} match(\$x) { 1, 2 => 'a', default => 'b' };",
  "<?php enum S: string { case A = 'a'; } \$f = fn(\$x) => \$x * 2; #[Attr(1)] function g() {} yield from h();",
  "<?php \$x = \"tab\there\"; ?>\t<b>html & stuff</b>", "<?php \x01 \x80 \xff;", "<?php __halt_compiler(); raw <data> & stuff", "<?php __halt_compiler();",
  "<?php\necho 1;\n?>\n", "<?php\n\n\n   \$a   =   1 ;  \n\n", "<?php\nfunction a() {\n\treturn <<<X\n\tq\n\tX;\n}\n", "<?php echo <<<A\nx\nA\n;", "<?php echo <<<A\nx\nA;",
  "<?php \$a = <<<A\nx\nA\n\n?>", "<?php ?>", "<?php\n?>", "<?php #comment ?> html", "<?php //comment ?> html", "<?php /*c*/?>x", "<script language=\"php\"> echo 1; </script>",
  "<?php \$\$a; \${'b'}; \$c::\$d; \$e::F; G::class; new class {};", "<?php exit; die(1); exit(); list(\$a) = [1]; isset(\$a); empty(\$b); unset(\$c);",
  "<?php \"\\x41\\u{1F600}\\101\\\$\";", "<?php '\\'\\\\';", "<?php \"{\$a}\";\"\${a}\";\"\${a[1]}\";\"\$a[-1]\";\"\$a[0x1]\";", "<?php &\$a; fn(&\$x) => \$x; \$a & \$b;",
];
foreach ($snips as $s) {
  echo "snip ", bin2hex($s), " ", bin2hex(highlight_string($s, true)), " ";
  $f = tempnam(sys_get_temp_dir(), "hl");
  file_put_contents($f, $s);
  echo bin2hex(@php_strip_whitespace($f)), "\n";
  unlink($f);
}
$root = realpath(__DIR__ . "/../../../../modules/php-src");
if ($root) {
  $files = [];
  foreach (["Zend/tests", "tests", "ext/standard/tests", "ext/tokenizer/tests"] as $d) {
    $it = new RecursiveIteratorIterator(new RecursiveDirectoryIterator("$root/$d", FilesystemIterator::SKIP_DOTS));
    foreach ($it as $f) if (substr($f, -5) === ".phpt") $files[] = substr($f, strlen($root) + 1);
  }
  sort($files);
  foreach ($files as $i => $rel) {
    if ($i % 4) continue;
    $src = file_get_contents("$root/$rel");
    if (!preg_match('/^--FILE--\r?\n(.*?)\r?\n--[A-Z_]+--\r?$/ms', $src, $m)) continue;
    $code = $m[1];
    $f = tempnam(sys_get_temp_dir(), "hl");
    file_put_contents($f, $code);
    echo "corpus $rel ", md5(highlight_string($code, true)), " ", md5(@php_strip_whitespace($f)), "\n";
    unlink($f);
  }
}
// Custom colours last: ini_restore() does not reset highlight.* here.
ini_set("highlight.keyword", "#0000BB");
ini_set("highlight.string", "red");
ini_set("highlight.html", "#0000BB");
foreach ([$snips[5], $snips[6], $snips[9], $snips[10]] as $s) {
  echo "colors ", bin2hex($s), " ", bin2hex(highlight_string($s, true)), "\n";
}
