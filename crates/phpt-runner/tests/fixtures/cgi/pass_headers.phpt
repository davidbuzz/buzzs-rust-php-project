--TEST--
Response headers are split off and checked against EXPECTHEADERS
--CGI--
--FILE--
<?php
header('X-Test: 1');
http_response_code(404);
echo "body\n";
?>
--EXPECTHEADERS--
Status: 404 Not Found
X-Test: 1
--EXPECT--
body
