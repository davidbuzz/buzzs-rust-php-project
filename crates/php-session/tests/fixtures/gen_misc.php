<?php
// Oracle generator for cookie/cache dates and session ID alphabets.
// Run: php -d memory_limit=1G gen_misc.php > misc.tsv
// Lines: "date\t<timestamp>\t<gmdate>" and "sidchars\t<bits>\t<sorted chars>".
// ID alphabets first: any output marks the headers as sent.
$sid = [];
foreach ([4, 5, 6] as $bits) {
    ini_set('session.sid_bits_per_character', $bits);
    $chars = [];
    for ($i = 0; $i < 400; $i++) { foreach (str_split(session_create_id()) as $c) { $chars[$c] = 1; } }
    ksort($chars, SORT_STRING);
    $sid[] = "sidchars\t$bits\t" . implode('', array_keys($chars)) . "\n";
}
echo implode('', $sid);
mt_srand(42);
$ts = [0, 1, -1, 59, 86399, 86400, 375007920, 951782400, 951868800, 1700000000, 2147483647, 2147483648,
       4102444800, 253402300799, -86400, -2208988800];
for ($i = 0; $i < 200; $i++) { $ts[] = mt_rand(0, 4102444800); }
foreach ($ts as $t) { echo "date\t$t\t", gmdate("D, d M Y H:i:s \\G\\M\\T", $t), "\n"; }
