--TEST--
A wrong header fails the test even though the body matches
--CGI--
--FILE--
<?php
header('X-Test: 1');
echo "body\n";
?>
--EXPECTHEADERS--
X-Test: 2
--EXPECT--
body
