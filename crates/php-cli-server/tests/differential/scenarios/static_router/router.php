<?php
$u = parse_url($_SERVER['REQUEST_URI'], PHP_URL_PATH);
if (str_starts_with($u, '/false')) return false;
if (str_starts_with($u, '/static')) return false;
if ($u === '/str') return "a string";
if ($u === '/none') return;
if ($u === '/a.css') return false;
if ($u === '/a.txt') { echo "router took a.txt\n"; return true; }
if ($u === '/hdr') { header('X-Router: 1'); return false; }
if ($u === '/withindex/' || $u === '/withhtml/' || $u === '/sub/x.php' || $u === '/sub/x.php/pi') return false;
echo "router: $u ", $_SERVER['SCRIPT_NAME'], " ", $_SERVER['SCRIPT_FILENAME'] === __FILE__ ? 'self' : 'other', "\n";
