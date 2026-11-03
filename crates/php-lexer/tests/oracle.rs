//! Compares `token_get_all()` output with `/usr/bin/php` on a corpus of
//! tricky snippets. Skipped when PHP (with ext/tokenizer) is unavailable.
//!
//! Snippets avoid syntax newer than the installed PHP (8.3): `public(set)`,
//! `__PROPERTY__`, `|>`, `(void)` and comments after `->`.

use std::fmt::Write as _;
use std::process::Command;

const SNIPPETS: &[&str] = &[
    "<?php echo $a + 1;",
    "hello <?php echo 1 ?>\nworld<?= 2 ?>\r\nx",
    "<?php\n$a = 0x1A + 0b101 + 0o17 + 017 + 1_000 + 1.5e3 + .5 + 1. + 9223372036854775807 + 9223372036854775808;",
    "<?php 0x7FFFFFFFFFFFFFFF; 0x8000000000000000; 0b1111111111111111111111111111111111111111111111111111111111111111; 08; 0_7; 1__2; 1_; 0x; 0b2; 1e; 1e+; 1.e5; 1..2; 1...2;",
    "<?php $x = 'a\\'b\\\\c\\n'; $y = \"a\\n$b {$c->d} ${e} ${f['g']} $h[0] $h[-1] $h[k] $h[$k] $i->j $i?->k $l->1 {$m}\";",
    "<?php \"$a[0x1] $a[01] $a[ 1] $a[1 ] $a[9223372036854775808] $a[-0]\";",
    "<?php `ls $dir {$x} -la`;",
    "<?php $a = <<<EOT\n  foo $bar\n  baz {$x[1]}\n  EOT;\n$b = <<<'NOW'\n    raw $x\n  NOW;\n",
    "<?php <<<A\nA;\n<<<\"B\"\n\nB;\n<<<C\n  x\nC\n;",
    "<?php echo <<<END\n\tfoo\n\tEND . <<<X\n x\n X;",
    "<?php <<<A\n${a}\n{$b}\n$c\nA;",
    "<?php <<<A\n  a\n  ${a}\n  A;\n",
    "<?php <<<A\nAB\nA\n;",
    "<?php <<<A\n  a\n b\n  A;",
    "<?php <<<A\n\\\nA;\n",
    "<?php (int)$a; ( INTEGER )$b; (float)(double)(real)(string)(binary)(array)(object)(bool)(boolean)(unset); ( int\t) (in t)",
    "<?php $a?->b?->c; $a->b->c; $a::b; A\\B\\c(); \\A\\b; namespace\\foo; namespace \\foo; $a->class; $a-> foo; $a ->\nfoo;",
    "<?php function &f(&$a, &...$b) {} $a & $b; $a &$b; $a && $b; $a &= $b; $a & /* x */ $b; $a & // x\n $b; $a & # x\n ...$b;",
    "<?php enum Foo {} enum extends {} enum implements X; enum/**/Bar {} enum\nBaz {} $enum; enum(1); enum\\foo;",
    "<?php yield from $a; yield\nfrom $b; yield/* c */from $c; yield fromx; YIELD FROM $d; yield # x\nfrom $e;",
    "<?php #[Attr] function f() {} # comment\n// comment ?> after\n<?php /** doc */ /**/ /* c */ /***/ /**\n*/",
    "<?php /* unterminated",
    "<?php /** unterminated doc",
    "<?php 'unterminated",
    "<?php \"unterminated $a",
    "<?php `unterminated",
    "<?php <<<A\nunterminated",
    "<?php <<<'A'\nunterminated\n  ",
    "<?php __halt_compiler(); garbage <?php ?> \"",
    "<?php __HALT_COMPILER() ?>\nraw data",
    "<?php __halt_compiler\n/* c */(\n)\n;rest",
    "<?php\n\n$a = \"line\r\nbreak\rhere\";\n$b = 1;",
    "<?php $a\x01 \x7f $b",
    "<?php OR AND XOR or and xor <=> ** **= ?? ??= ... :: => -> ++ -- <<= >>= << >> <> != !== === == <= >= += -= *= /= .= %= |= ^= || ",
    "<?php __CLASS__ __TRAIT__ __FUNCTION__ __METHOD__ __LINE__ __FILE__ __DIR__ __NAMESPACE__ __halt_compiler",
    "<?php die exit fn function const return try catch finally throw if elseif endif else while endwhile do for endfor foreach endforeach declare enddeclare instanceof as switch match endswitch case default break continue goto echo print class interface trait extends implements new clone var eval include include_once require require_once namespace use insteadof global isset empty static abstract final private protected public readonly unset list array callable",
    "<?php b'x' B\"y $z\" b<<<A\nx\nA;\n",
    "<?php $$a ${'b'} $ {c} $",
    "<?php \"\\u{1F600} \\u{} \\x41 \\101 \\400 \\e \\$a {\\$b} \\{$c}\";",
    "<?php \"$a->b->c $a->$b ${a[1]} ${a}[1] {$a[1][2]} $a[b][c]\";",
    "<?php \"$a[\" ; \"$a[]\"; \"$a[\\\"x\\\"]\"; \"$a[ \";",
    "<?php ?>",
    "<?php",
    "<?php ",
    "<?phpx",
    "<?",
    "<? echo 1;",
    "text only",
    "",
    "<?php\r\n$a\r\n=\r1;\r\n?>\r\nx\r\n",
    "<?php { ( [ ] ) } ) ] }",
    "<?php $a = [1, 2, ...$b]; [$a, [$b]] = $c; list('k' => $v) = $d;",
    "<?php static fn() => 1; static function() {}; new class {}; new readonly class {};",
    "<?php match($a) { 1, 2 => 'x', default => 'y' };",
    "<?php $a = \"{$a['b']}{$c->d()}{$e::$f}\";",
    "<?php echo \"${a->b}\";",
    "<?php <<<A\n  $a\n  A . <<<B\n    $b\n    B;\n  A;\n",
    "<?php $x = <<<EOT\n  a\n  {$y[<<<Z\n  q\n  Z]}\n  EOT;\n",
    "<?php\n#!/usr/bin/env php\n",
    "#!/usr/bin/env php\n<?php echo 1;",
    "<?php $a->/* c */b; $a?->c;",
    "<?php 1 <<<A\nfoo\nA\n",
];

fn php_dump(snippets: &[&str]) -> Option<Vec<String>> {
    let dir = std::env::temp_dir().join(format!("php-lexer-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let script = dir.join("dump.php");
    std::fs::write(
        &script,
        r#"<?php
array_shift($argv);
foreach ($argv as $path) {
    $out = '';
    foreach (token_get_all(file_get_contents($path)) as $t) {
        if (is_array($t)) {
            $out .= token_name($t[0]) . "\t" . $t[2] . "\t" . bin2hex($t[1]) . "\n";
        } else {
            $out .= $t . "\t-\t" . bin2hex($t) . "\n";
        }
    }
    echo bin2hex($out), "\n";
}
"#,
    )
    .ok()?;
    let mut paths = Vec::new();
    for (i, s) in snippets.iter().enumerate() {
        let p = dir.join(format!("{i}.php"));
        std::fs::write(&p, s).ok()?;
        paths.push(p);
    }
    let out = Command::new("php")
        .arg("-d")
        .arg("short_open_tag=0")
        .arg("-d")
        .arg("error_reporting=0")
        .arg(&script)
        .args(&paths)
        .output()
        .ok()?;
    let _ = std::fs::remove_dir_all(&dir);
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let lines: Vec<String> = text
        .lines()
        .map(|l| {
            let bytes: Vec<u8> = (0..l.len())
                .step_by(2)
                .filter_map(|i| u8::from_str_radix(&l[i..i + 2], 16).ok())
                .collect();
            String::from_utf8_lossy(&bytes).into_owned()
        })
        .collect();
    (lines.len() == snippets.len()).then_some(lines)
}

fn rust_dump(src: &str) -> String {
    let mut out = String::new();
    for t in php_lexer::token_get_all(src.as_bytes()) {
        let mut hex = String::new();
        for b in t.text {
            let _ = write!(hex, "{b:02x}");
        }
        if t.is_char() {
            let _ = writeln!(out, "{}\t-\t{hex}", String::from_utf8_lossy(t.text));
        } else {
            let _ = writeln!(out, "{}\t{}\t{hex}", t.kind.name().unwrap_or("?"), t.line);
        }
    }
    out
}

#[test]
fn matches_php_token_get_all() {
    let Some(expected) = php_dump(SNIPPETS) else {
        eprintln!("php with ext/tokenizer not available; skipping");
        return;
    };
    let mut failures = Vec::new();
    for (snippet, exp) in SNIPPETS.iter().zip(&expected) {
        let got = rust_dump(snippet);
        if &got != exp {
            failures.push(format!(
                "snippet {snippet:?}\n--- php\n{exp}--- rust\n{got}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
