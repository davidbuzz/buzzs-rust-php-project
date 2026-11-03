<?php
// Generates tests/data/unserialize_cases.txt:
//   php -d memory_limit=1G tests/data/gen_unserialize.php > tests/data/unserialize_cases.txt
// Each line: <hex input> <result>, where result is "ok" or the last
// "Error at offset"/"Extra data" warning text.
$cases = [
    'O:8:"stdClass":0:{}', 'O:8:"stdClass":0:{}xyz', 'O:8:"stdClass":1:{s:1:"a";i:1;}trailing',
    'O:8:"stdClass":1:{s:1:"a";a:1:{i:0;i:x;}}', 'O:8:"stdClass":1:{s:1:"a";O:8:"stdClass":0:{}', 
    'a:1:{i:0;O:8:"stdClass":1:{s:1:"b";s:3:"ab";}}', 'a:2:{i:0;i:1;i:1;i:2;}', 'a:2:{i:0;i:1;i:1;i:2;}}',
    'a:1:{i:0;a:1:{i:0;b:3;}}', 'a:1:{s:1:"x";N;', 'a:1:{N;i:1;}', 'a:1:{d:1.5;i:1;}', 'a:-1:{}',
    'a:1:{i:0;R:5;}', 'a:1:{i:0;r:1;}', 'O:8:"stdClass":1:{i:0;i:1;}', 'O:8:"stdClass":2:{s:1:"a";i:1;s:1:"b"}',
    'O:1:"A":0:{}', 'O:0:"":0:{}', 'O:3:"\\AB":0:{}', 'O:8:"stdClass":-1:{}', 'O:8:"stdClass":a:{}',
    's:3:"abc"', 's:3:"abcd";', 'i:1;i:2;', 'N;N;', 'b:1;x', 'd:0.5;y', 'd:;', 'i:;', 'i:-;',
    'a:1:{i:0;s:1:"a"x}', 'a:2:{i:0;i:1;}', 'a:1:{i:0;i:1;i:1;i:2;}',
    'C:3:"Foo":3:{abc}x', 'C:3:"Foo":3:{abcx', 'O:8:"stdClass":1:{s:1:"a";R:1;}',
];
foreach ($cases as $c) {
    error_clear_last();
    $r = @unserialize($c);
    $e = error_get_last();
    $msg = "ok";
    if ($e && (str_contains($e['message'], 'Error at offset') || str_contains($e['message'], 'Extra data'))) {
        $msg = $e['message'];
    }
    echo bin2hex($c), " ", $msg, "\n";
}
