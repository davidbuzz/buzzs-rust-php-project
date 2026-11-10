--TEST--
GET query string and cookies
--GET--
q=hello&n=2
--COOKIE--
c1=v1; c2=v2
--FILE--
<?php
var_dump($_GET, $_COOKIE, $_SERVER['QUERY_STRING'], $_SERVER['REQUEST_METHOD']);
?>
--EXPECT--
array(2) {
  ["q"]=>
  string(5) "hello"
  ["n"]=>
  string(1) "2"
}
array(2) {
  ["c1"]=>
  string(2) "v1"
  ["c2"]=>
  string(2) "v2"
}
string(11) "q=hello&n=2"
string(3) "GET"
