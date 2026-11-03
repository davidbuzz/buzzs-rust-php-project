--TEST--
redirected one
--FILE--
<?php
var_dump(getenv('REDIR_VALUE'));
var_dump(getenv('REDIR_TEST_DIR') === __DIR__ . '/targets/');
?>
--EXPECT--
string(13) "from-redirect"
bool(true)
