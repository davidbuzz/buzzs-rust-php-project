<?php
// Builds the fixture tree (must match make_tree() in src/glob/tests.rs) and
// prints "pattern-hex \t flagnames \t result" lines; result is "false" or a
// comma-separated list of hex entries (possibly empty).
$root = $argv[1];
@mkdir($root);
chdir($root);
foreach (['dir1', 'dir1/sub', 'dir2', 'emptydir', '.hiddendir'] as $d) @mkdir($d);
foreach (['a.txt', 'b.txt', 'c.log', '.hidden', 'dir1/x.txt', 'dir1/y.php', 'dir1/sub/z.txt', 'dir2/w.txt',
          '[x].txt', 'q?.txt', 'star*file', 'brace{a}', 'Upper.TXT', '1num', 'back\\slash', '.hiddendir/h.txt'] as $f) {
    touch($f);
}
@symlink('dir1', 'link_dir');
@symlink('a.txt', 'link_file');
@symlink('nonexistent', 'dangling');

$cases = [
 ['*', []], ['*.txt', []], ['?.txt', []], ['[ab].txt', []], ['[!a].txt', []], ['[^a].txt', []],
 ['*/', []], ['*/*', []], ['*/*.txt', []], ['dir*', []], ['dir1/*', []], ['dir1/sub/*', []], ['*/sub', []],
 ['.*', []], ['*hidden', []], ['nomatch*', []], ['nomatch', []], ['a.txt', []], ['dir1', []],
 ['{a,b,c}.*', ['GLOB_BRACE']], ['{dir1,dir2}/*', ['GLOB_BRACE']], ['{*.log,*.txt}', ['GLOB_BRACE']],
 ['{a,b}{x,.txt}', ['GLOB_BRACE']], ['{a,{b,c}}.*', ['GLOB_BRACE']], ['*.{txt,log}', ['GLOB_BRACE']],
 ['{c,a,b}.*', ['GLOB_BRACE']], ['{nomatch,a}.txt', ['GLOB_BRACE']], ['{nomatch,zz}.txt', ['GLOB_BRACE']],
 ['{a,b}.txt', []], ['brace{a}', []], ['brace{a}', ['GLOB_BRACE']], ['brace\\{a\\}', ['GLOB_BRACE']],
 ['\\[x\\].txt', []], ['[[]x].txt', []], ['[[:alpha:]].txt', []], ['[[:digit:]]*', []], ['[[:upper:]]*', []],
 ['[[:alpha:][:digit:]]*', []], ['[a-c]*', []], ['[!a-c]*', []], ['q\\?.txt', []], ['q?.txt', []],
 ['star\\*file', []], ['star*', []], ['back\\\\slash', []], ['back\\slash', ['GLOB_NOESCAPE']], ['link*', []], ['dangling', []],
 ['*', ['GLOB_MARK']], ['*/', ['GLOB_MARK']], ['link*', ['GLOB_MARK']], ['*', ['GLOB_ONLYDIR']], ['*', ['GLOB_ONLYDIR', 'GLOB_MARK']],
 ['zz*', ['GLOB_NOCHECK']], ['zz\\*', ['GLOB_NOCHECK']], ['a.txt', ['GLOB_NOCHECK']], ['zz', ['GLOB_NOCHECK']],
 ['dir1//x.txt', []], ['./a.txt', []], ['./*.log', []], ['dir1/../a.txt', []], ['', []], ['{}', ['GLOB_BRACE']],
 ['*.TXT', []], ['dir1/sub/../*.php', []], ['*/*/*', []], ['emptydir/*', []], ['nodir/*', []], ['a.txt/*', []],
 ['.hiddendir/*', []], ['*/h.txt', []], ['.*/h.txt', []], ['[a', []], ['a]', []], ['*[', []],
 ['*', ['GLOB_ERR']], ['dir1/*', ['GLOB_NOSORT', 'GLOB_ONLYDIR']], ['*', ['GLOB_BRACE', 'GLOB_MARK']],
 ['{dir1,dir2}', ['GLOB_BRACE', 'GLOB_ONLYDIR']], ['{a.txt,dir1}', ['GLOB_BRACE', 'GLOB_MARK']],
];
foreach ($cases as [$p, $flags]) {
    $f = 0;
    foreach ($flags as $name) $f |= constant($name);
    $r = glob($p, $f);
    if ($r === false) {
        $out = 'false';
    } else {
        $out = implode(',', array_map('bin2hex', $r));
    }
    echo bin2hex($p), "\t", implode('|', $flags), "\t", $out, "\n";
}
