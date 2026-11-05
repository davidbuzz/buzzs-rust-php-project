--TEST--
JSON guards rebuilt DateInterval property tables, not the object (GH-20503)
--EXTENSIONS--
date
json
--INI--
zend.max_allowed_stack_size=512K
--FILE--
<?php
$obj = new DateInterval('P1W');
$obj->prop3 = $obj;
var_dump(json_encode($obj));
var_dump(json_last_error_msg());

$obj2 = new DateInterval('P1D');
$obj2->prop = [$obj2];
var_dump(json_encode($obj2));
var_dump(json_last_error_msg());
?>
--EXPECTF--
Deprecated: Creation of dynamic property DateInterval::$prop3 is deprecated in %s on line %d
bool(false)
string(28) "Maximum stack depth exceeded"

Deprecated: Creation of dynamic property DateInterval::$prop is deprecated in %s on line %d
bool(false)
string(18) "Recursion detected"
