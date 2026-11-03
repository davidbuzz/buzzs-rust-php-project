--TEST--
POST requires CGI
--POST--
a=1
--FILE--
<?php var_dump($_POST); ?>
--EXPECT--
x
