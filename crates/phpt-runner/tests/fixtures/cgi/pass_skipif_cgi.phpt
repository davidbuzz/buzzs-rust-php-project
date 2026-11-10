--TEST--
SKIPIF runs under php-cgi -C too
--GET--
a=b
--SKIPIF--
<?php if (PHP_SAPI !== 'cgi-fcgi') die('skip SKIPIF not run by php-cgi'); ?>
--FILE--
<?php echo $_GET['a'], "\n"; ?>
--EXPECT--
b
