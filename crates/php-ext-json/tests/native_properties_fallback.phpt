--TEST--
Native handlers declining JSON retain ordinary object recursion guards
--EXTENSIONS--
intl
json
--INI--
error_reporting=0
--FILE--
<?php
$o = IntlTimeZone::createTimeZone('UTC');
$o->self = $o;
var_dump(json_encode($o), json_last_error());
var_dump(json_encode($o, JSON_PARTIAL_OUTPUT_ON_ERROR));
?>
--EXPECT--
bool(false)
int(6)
string(13) "{"self":null}"
