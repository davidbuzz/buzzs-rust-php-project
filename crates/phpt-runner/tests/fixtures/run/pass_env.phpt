--TEST--
ENV section with {PWD}
--ENV--
FOO=bar=baz
HERE={PWD}
--FILE--
<?php
var_dump(getenv('FOO'));
var_dump(getenv('HERE') === __DIR__);
var_dump(getenv('REQUEST_METHOD'));
?>
--EXPECT--
string(7) "bar=baz"
bool(true)
string(3) "GET"
