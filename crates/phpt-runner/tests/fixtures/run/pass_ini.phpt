--TEST--
INI section and defaults
--INI--
precision=5
arg_separator.output==
--FILE--
<?php
echo M_PI, "\n", ini_get('arg_separator.output'), "\n", ini_get('display_errors'), "\n", ini_get('date.timezone'), "\n";
?>
--EXPECT--
3.1416
=
1
UTC
