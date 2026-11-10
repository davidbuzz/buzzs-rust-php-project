--TEST--
POST body and request variables reach php-cgi
--POST--
a=1&b[]=x
--FILE--
<?php
var_dump($_POST, $_SERVER['REQUEST_METHOD'], $_SERVER['CONTENT_TYPE'], $_SERVER['CONTENT_LENGTH'], PHP_SAPI);
?>
--EXPECTF--
array(2) {
  ["a"]=>
  string(1) "1"
  ["b"]=>
  array(1) {
    [0]=>
    string(1) "x"
  }
}
string(4) "POST"
string(33) "application/x-www-form-urlencoded"
string(1) "9"
string(8) "cgi-fcgi"
