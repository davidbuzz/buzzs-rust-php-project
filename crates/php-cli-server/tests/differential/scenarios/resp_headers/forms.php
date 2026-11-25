<?php
header('X-A: 1');
header('X-A: 2', false);
header('x-a: 3', false);
header('X-B: 1');
header('X-B: 2');
header('Set-Cookie: a=1');
header('Set-Cookie: b=2', false);
setcookie('c', 'v w;', ['expires' => 1, 'path' => '/p', 'domain' => 'example.com', 'secure' => true, 'httponly' => true, 'samesite' => 'Lax']);
setrawcookie('d', 'raw%20v', 0, '', '', false, false);
setcookie('e', '', ['partitioned' => true, 'secure' => true]);
setcookie('f');
header('X-Gone: 1');
header_remove('X-Gone');
header('X-Empty:');
header('X-Spaces:    lead');
switch ($_GET['m'] ?? '') {
case 'loc': header('Location: /elsewhere'); break;
case 'loc301': header('Location: /elsewhere', true, 301); break;
case 'loc201': http_response_code(201); header('Location: /elsewhere'); break;
case 'status': header('HTTP/1.1 404 x'); break;
case 'status10': header('HTTP/1.0 418 Teapot Thing'); break;
case 'statushdr': header('Status: 403 Nope'); break;
case 'code': var_dump(http_response_code(299), http_response_code()); break;
case 'code0': header('X: y', true, 503); break;
case 'odd': http_response_code(999); break;
case 'neg': var_dump(http_response_code(-1)); break;
case 'ct': header('Content-Type: application/json'); header('Content-Type: text/plain'); break;
case 'ctnocs': ini_set('default_charset', ''); header('Content-Type: text/plain'); break;
case 'cl': header('Content-Length: 5'); echo "123456789"; exit;
case 'date': header('Date: Mon, 01 Jan 2001 00:00:00 GMT'); break;
case 'conn': header('Connection: keep-alive'); break;
case 'xpb': header_remove('X-Powered-By'); break;
case 'remall': header_remove(); break;
}
echo implode("\n", headers_list()), "\n";
var_dump(http_response_code());
if (isset($_GET['bad'])) { header("X-Bad: a\r\nInjected: 1"); header('NoColon'); header("X-Nul: a\0b"); }
echo "done\n";
