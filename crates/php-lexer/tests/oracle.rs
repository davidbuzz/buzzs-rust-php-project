// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Compares `token_get_all()` output with `/usr/bin/php` on a corpus of
//! tricky snippets. Skipped when PHP (with ext/tokenizer) is unavailable.
//!
//! Snippets avoid syntax newer than the installed PHP (8.3): `public(set)`,
//! `__PROPERTY__`, `|>`, `(void)` and comments after `->`.

mod common;

use php_lexer::TokenKind;
use std::fmt::Write as _;
use std::path::PathBuf;
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
    "<?php 0o777777777777777777777777; 0O17; 0X1f; 0B11; 0x00000000000000000001; 0x0000_0000_7FFF_FFFF_FFFF_FFFF; 0xFFFFFFFFFFFFFFFF;",
    "<?php 0b1000000000000000000000000000000000000000000000000000000000000000; 0b0111111111111111111111111111111111111111111111111111111111111111;",
    "<?php 0777777777777777777777; 01000000000000000000000; 00000000000000000000000001; 99999999999999999999; 1_9_2_2_3_3_7_2_0_3_6_8_5_4_7_7_5_8_0_8;",
    "<?php .5e-3; 1_000.000_1; 1e1_0; 1E+5; 1e-5_0; 1._5; 1_.5; ._5; 0.0; 00.0; 1e; 1e_1; 0x1_; 0b_1; 0o_7; 0o8; 0_0;",
    "<?php $a = 1+-2; $b = $c--->d; $e = $f-->$g; $h = -.5; $i = 1.5.2.5;",
    "<?php \"{$a}{$b[1]}{$c->d}{$e::$f}{$g()}{$h->i()}{$j['k']['l']}\";",
    "<?php \"${a}${b[1]}${c['d']}${e . f}${g(1)}\";",
    "<?php \"\\{$a} {\\$b} $\\{c} {$ d} { $e} $ f $1 ${1} {$}\";",
    "<?php \"$a?->b $a?-> $a?->1 $a->b->c $a-> b\";",
    "<?php \"$a[-1] $a[-0] $a[- 1] $a[--1] $a[1.5] $a[1e3] $a[0b11] $a[0o7] $a[007] $a[_x] $a[\\$x]\";",
    "<?php <<<EOT\n  $a[1] {$b} ${c}\n    $d->e\n  EOT;\n",
    "<?php <<<EOT\nEOT;\n<<<EOT\n\nEOT;\n<<<EOT\n  \n  EOT;\n<<<EOT\n\tx\n\tEOT;\n",
    "<?php <<<EOT\nfoo\nEOTX\nEOT;\n<<<'EOT'\nEOT1\nEOT\n;",
    "<?php <<<  EOT\nx\nEOT;\n<<<\t\"EOT\"\nx\nEOT;\n<<<'EOT\nx\nEOT;\n",
    "<?php <<<EOT\r\n  a\r\n  EOT;\r\n<<<'N'\r\n  b\r\n  N;\r\n",
    "<?php <<<EOT\n  a\n EOT;\n",
    "<?php <<<EOT\n  a\n\t\tEOT;\n",
    "<?php <<<A\n  {$x}\n A;\n",
    "<?php <<<A\n${\n",
    "<?php <<<A\n{$a\nA;\n",
    "<?php $x = <<<A\n  a\n  A\n  ;",
    "<?php echo <<<A\n  a\n  A, 1;",
    "<?php `a\\`b $c[0] {$d}`;",
    "<?php #[A] #[B(1, 2)] #[\\C\\D] #[E, F] class X {} #[\n  G\n] fn() => 1;",
    "<?php #comment ?>x<?php // comment ?>\n#x\n<?php # c\r\n?>",
    "<?php function f(): static|null&X {} fn&($x) => $x; function &g() {}",
    "<?php $a = &$b; $c = & $d; $e = &/**/$f; foo(&...$g); $h &= 1; $i & 1;",
    "<?php __halt_compiler",
    "<?php __halt_compiler(",
    "<?php __halt_compiler();",
    "<?php __halt_compiler() ?>",
    "<?php __halt_compiler /* a */ ( /* b */ ) /* c */ ;",
    "<?php function __halt_compiler() {} $a->__halt_compiler(); __HALT_COMPILER();x",
    "x<?php\n?>\n\n<?php\n?>\r\n<?php ?>\r<?php ?>",
    "<?php ?><?php echo 1?>",
    "<?php\0 $a\0;",
    "\u{feff}<?php echo 1;",
    "<?php $été = €; class Ünïcode {} $a->é;",
    "<?php \"\\400 \\777 \\0 \\00 \\000 \\8 \\x \\xg \\u \\u{ \\u{41\";",
    "<?php 'a\\\nb'; \"a\\\nb\"; 'a\nb'; \"\r\";",
];

fn php_dump<S: AsRef<[u8]>>(snippets: &[S]) -> Option<Vec<String>> {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("php-lexer-oracle-{}-{n}", std::process::id()));
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
        std::fs::write(&p, s.as_ref()).ok()?;
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

fn rust_dump(src: &[u8]) -> String {
    let mut out = String::new();
    for t in php_lexer::token_get_all(src) {
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
        let got = rust_dump(snippet.as_bytes());
        if &got != exp {
            failures.push(format!(
                "snippet {snippet:?}\n--- php\n{exp}--- rust\n{got}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Whether `src` uses tokens or scanner fixes that only exist in php-src
/// master and therefore cannot be compared with an older `/usr/bin/php`.
fn uses_master_only_lexing(src: &[u8]) -> bool {
    use TokenKind as T;
    let toks = php_lexer::token_get_all(src);
    toks.iter().enumerate().any(|(i, t)| {
        matches!(
            t.kind,
            T::PublicSet | T::ProtectedSet | T::PrivateSet | T::PropertyC | T::PipeOp | T::VoidCast
        ) || (matches!(t.kind, T::ObjectOperator | T::NullsafeObjectOperator)
            && toks
                .get(i + 1)
                .is_some_and(|n| matches!(n.kind, T::Comment | T::DocComment)))
    })
}

/// Compares every `.phpt` `--FILE--` section in php-src with
/// `/usr/bin/php`. Slow (~15s); run with `cargo test -- --ignored`.
#[test]
#[ignore = "shells out to php for ~23k files"]
fn full_corpus_matches_php() {
    let corpus: Vec<(PathBuf, Vec<u8>)> = common::corpus()
        .into_iter()
        .filter(|(_, src)| !uses_master_only_lexing(src))
        .collect();
    if corpus.is_empty() {
        return;
    }
    let mut failures = Vec::new();
    for chunk in corpus.chunks(1000) {
        let srcs: Vec<&[u8]> = chunk.iter().map(|(_, s)| s.as_slice()).collect();
        let Some(expected) = php_dump(&srcs) else {
            eprintln!("php with ext/tokenizer not available; skipping");
            return;
        };
        for ((path, src), exp) in chunk.iter().zip(&expected) {
            if &rust_dump(src) != exp {
                failures.push(path.display().to_string());
            }
        }
    }
    eprintln!("compared {} files", corpus.len());
    assert!(failures.is_empty(), "mismatches:\n{}", failures.join("\n"));
}
