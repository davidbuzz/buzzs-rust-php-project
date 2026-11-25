<?php
$s = $_SERVER;
foreach (['REQUEST_TIME', 'REQUEST_TIME_FLOAT', 'REMOTE_PORT', 'DOCUMENT_ROOT', 'SCRIPT_FILENAME', 'PATH_TRANSLATED', 'SERVER_PORT', 'HTTP_HOST', 'SERVER_SOFTWARE', 'PATH', 'SHLVL', '_', 'PWD', 'OLDPWD', 'TERM', 'HOME', 'USER', 'LANG', 'LANGUAGE', 'LOGNAME', 'MAIL', 'SHELL', 'SSH_CLIENT', 'SSH_CONNECTION', 'SSH_TTY', 'XDG_RUNTIME_DIR', 'XDG_SESSION_ID', 'DBUS_SESSION_BUS_ADDRESS', 'PHP_CLI_SERVER_WORKERS', 'SERVER_NAME', 'SERVER_ADDR'] as $k) unset($s[$k]);
foreach ($s as $k => $v) if (!preg_match('/^(HTTP_|REQUEST_|QUERY_|SCRIPT_|PHP_|PATH_INFO|CONTENT_|GATEWAY|SERVER_PROTOCOL|REDIRECT_|ORIG_|argv|argc|REMOTE_ADDR|AUTH_TYPE)/', $k)) unset($s[$k]);
echo "SERVER: "; var_dump($s);
echo "GET: "; var_dump($_GET);
echo "POST: "; var_dump($_POST);
echo "COOKIE: "; var_dump($_COOKIE);
echo "REQUEST: "; var_dump($_REQUEST);
echo "FILES: "; var_dump($_FILES);
echo "input: "; var_dump(file_get_contents('php://input'), file_get_contents('php://input'));
var_dump(error_get_last());
