--TEST--
EXPECTF placeholders
--FILE--
<?php
echo 42, " ", -7, " ", 1.5, " ", dechex(255), " ", __FILE__, "\n";
echo "x\n";
trigger_error("boom", E_USER_WARNING);
?>
--EXPECTF--
%d %i %f %x %s
%c

Warning: boom in %s on line %d
