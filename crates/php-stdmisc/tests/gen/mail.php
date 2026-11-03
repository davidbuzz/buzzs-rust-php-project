<?php
// Fixture generator: mail() through sendmail_path="cat > FILE" in child
// processes. Output: xheader hex(serialize(case)) hex(result) hex(warnings) hex(sent)
$h = ["", "KHeaders", "KHeaders\n\n\n\n\n", "HEAD1: a\nHEAD2: b\n", "HEAD1: a\n\nHEAD2: b\r\n", "HEAD1: a\n\rHEAD2: b\r\n", "HEAD1: a\n\r\nHEAD2: b\r\n",
  "HEAD1: a\rHEAD2: b\r", "HEAD1: a\r\nHEAD2: b\n", "HEAD1: a\r\nHEAD2: b\n\n", "HEAD1: a\r\nHEAD2: b\r", "HEAD1: a\r\nHEAD2: b\r\n", "HEAD1: a\r\nHEAD2: b\r\n\n",
  "HEAD1: a\r\n\nHEAD2: b\r\n", "HEAD1: a\r\n\r\nHEAD2: b\r\n", "HEAD1: a\r\rHEAD2: b\r\n", "\nHEAD1: a\nHEAD2: b\n", "\n\nHEAD1: a\r\nHEAD2: b\r\n", "\rHEAD1: a\nHEAD2: b\r",
  "\r\nHEAD1: a\r\nHEAD2: b\r\n", "\r\n\r\nHEAD1: a\r\nHEAD2: b\r\n", "\r\rHEAD1: a\r\nHEAD2: b\r\n", ":bad", " lead", "X-Test: 1  \t\n", "Cc: a@b\r\n\tc@d"];
$cases = [];
foreach ($h as $x) $cases[] = ["user@example.com", "Test Subject", "A Message", $x, null];
$arrays = [
  ["From" => "a@b", "Cc" => ["x@y", "z@w"], "X-Foo" => "bar"], ["from" => ["a"]], ["To" => "x"], ["subject" => "x"], [0 => "x"], ["X-A" => 1], ["X-A" => ["k" => "v"]], ["X-A" => [1]],
  ["X:A" => "v"], ["X A" => "v"], ["X-A" => "a\nb"], ["X-A" => "a\n b"], ["X-A" => "a\rb"], ["X-A" => "a\r\nb"], ["X-A" => "a\r\n\tb"], ["X-A" => "a\0b"], [], ["Reply-To" => ["a", "b"]], ["X-Multi" => ["one", "two"]],
  ["CC" => "x", "BCC" => "y", "Message-ID" => "<1@x>", "References" => "r", "In-Reply-To" => "i", "Sender" => "s", "Orig-Date" => "d"], ["X-ü" => "v"],
];
foreach ($arrays as $a) $cases[] = ["user@example.com", "Test Subject", "A Message", $a, null];
foreach (["user@example.com\n", "a@b,\r\n\tc@d", "a\x01b\x7fc", "a@b \t\r\n", "a\r\nb"] as $to) $cases[] = [$to, "S\x02ub\r\n ject\n", "Msg\r\nline2\n", "", null];
$cases[] = ["a@b", "s", "m", "", "-n"];
$cases[] = ["a@b", "s", "m", "", "-E"];
$cases[] = ["a@b", "s", "", null, null];
$tmp = tempnam(sys_get_temp_dir(), "mailfx");
foreach (["0", "1"] as $xh) {
  $cmd = escapeshellarg(PHP_BINARY) . " -n -d memory_limit=1G -d " . escapeshellarg("sendmail_path=cat > $tmp") . " -d mail.add_x_header=$xh " . escapeshellarg(__DIR__ . "/helpers/mail_child.php") . " " . escapeshellarg($tmp);
  $p = proc_open($cmd, [1 => ["pipe", "w"]], $pipes, __DIR__ . "/helpers", ["MAIL_CASES" => base64_encode(serialize($cases))]);
  foreach (explode("\n", trim(stream_get_contents($pipes[1]))) as $line) echo "$xh $line\n";
  proc_close($p);
}
@unlink($tmp);
