<?php
header_register_callback(function () { header('X-Callback: ' . ini_get('default_charset')); echo "[cb]"; });
ini_set('default_charset', $_GET['cs'] ?? 'UTF-8');
if (isset($_GET['ct'])) header('Content-Type: ' . $_GET['ct']);
if (isset($_GET['mime'])) ini_set('default_mimetype', $_GET['mime']);
echo "body\n";
header('X-After: 1');
echo "end\n";
