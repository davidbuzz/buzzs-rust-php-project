<?php
// Runs mail() cases; sendmail_path writes to $argv[1].
$out = $argv[1];
$cases = unserialize(base64_decode(getenv("MAIL_CASES")));
foreach ($cases as $c) {
  @unlink($out);
  $err = "";
  set_error_handler(function ($no, $str) use (&$err) { $err .= $str . "\n"; return true; });
  try {
    $r = var_export(mail($c[0], $c[1], $c[2], $c[3], ...($c[4] === null ? [] : [$c[4]])), true);
  } catch (Throwable $e) {
    $r = get_class($e) . ": " . $e->getMessage();
  }
  restore_error_handler();
  $body = file_exists($out) ? file_get_contents($out) : "";
  echo bin2hex(serialize($c)), " ", bin2hex($r), " ", bin2hex($err), " ", bin2hex($body), "\n";
}
