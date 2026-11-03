<?php
// Fixture generator: getopt() run in child processes with real argv.
// Output: hex(serialize([short, long, argv])) hex(serialize([result, rest_index]))
$specs = [
  ["a", null], ["a:", null], ["a::", null], ["ab:c::", null], ["abc", null], ["a:b-c:d", null], ["1:2", null], ["x", ["long", "req:", "opt::", "1", "2:", "01"]],
  ["", ["a", "ab:", "abc::", "a-b", "x=y"]], ["a:b::c", ["a", "aa:", "bb::", "c"]], [":a", null], ["a:::", null], ["", null], ["ö:a", ["ö:"]],
];
$argvs = [
  [], ["-a"], ["-a", "x"], ["-ax"], ["-a=x"], ["-a", "-b"], ["-abc"], ["-ab", "x"], ["-abx"], ["-b"], ["-b", "y"], ["-by"], ["-b=y"], ["-c"], ["-cz"], ["-c", "z"], ["-c=z"],
  ["-a", "-a", "-a"], ["-ax", "-ay", "-az"], ["-", "-a"], ["--", "-a"], ["-a", "--", "-b"], ["x", "-a"], ["-a", "x", "-b"], ["-z"], ["-zaq"], ["-:"], ["-a:"], ["-:a"],
  ["--long"], ["--long=v"], ["--long", "v"], ["--req"], ["--req", "v"], ["--req=v"], ["--req=", "v"], ["--opt"], ["--opt", "v"], ["--opt=v"], ["--opt="], ["--1"], ["--2", "x"], ["--2=y", "--2=z"], ["--01"],
  ["--lon"], ["--longer"], ["--a"], ["--ab", "q"], ["--ab=q"], ["--abc"], ["--abc=w"], ["--a-b"], ["--x=y"], ["--x=y=z"], ["--aa=1", "--bb=2", "-c", "-a", "q"],
  ["-1", "-2", "x"], ["-12x"], ["-1"], ["--", "--long"], ["-a", "1", "-a", "2", "--long", "--long"], ["-ac", "-b"], ["--=x"], ["---a"], ["--ö=1", "-ö", "2"],
];
foreach ($specs as [$short, $long]) {
  foreach ($argvs as $argv) {
    $env = ["GO_SPEC" => base64_encode(serialize([$short, $long]))];
    $cmd = escapeshellarg(PHP_BINARY) . " -n -d memory_limit=1G " . escapeshellarg(__DIR__ . "/helpers/getopt_child.php") . " " . implode(" ", array_map("escapeshellarg", $argv));
    $p = proc_open($cmd, [1 => ["pipe", "w"]], $pipes, null, $env);
    $out = stream_get_contents($pipes[1]);
    proc_close($p);
    echo bin2hex(serialize([$short, $long, array_merge(["script.php"], $argv)])), " ", $out, "\n";
  }
}
