<?php
$m = $_GET['m'] ?? '';
switch ($m) {
case 'gz': ob_start('ob_gzhandler'); echo str_repeat("hello gz ", 50), "\n"; break;
case 'gz_nested': ob_start('ob_gzhandler'); ob_start(fn($b) => strtoupper($b)); echo "inner\n"; ob_end_flush(); echo "outer\n"; break;
case 'zlib': ini_set('zlib.output_compression', '1'); echo str_repeat("zlib ", 100); break;
case 'zlib_level': ini_set('zlib.output_compression', '1'); ini_set('zlib.output_compression_level', '9'); echo str_repeat("abc ", 100); var_dump(ob_list_handlers()); break;
case 'flush': echo "a"; flush(); echo "b"; header('X-After-Flush: 1'); break;
case 'cl': header('Content-Length: 3'); echo "abc"; break;
case 'cl_ob': ob_start(); echo "12345"; header('Content-Length: ' . ob_get_length()); ob_end_flush(); break;
case 'empty': break;
case 'empty_ob': ob_start(); break;
case 'nested': ob_start(); echo "1"; ob_start(); echo "2"; var_dump(ob_get_level(), ob_get_status(true)[1]['name'] ?? null); ob_end_clean(); echo "3"; break;
case 'cb_header': ob_start(function ($b) { header('X-From-Cb: 1'); return "[$b]"; }); echo "x"; break;
case 'r304': http_response_code(304); echo "body for 304"; break;
case 'r204': http_response_code(204); echo "body for 204"; break;
case 'chunk': ob_start(null, 4); echo "abcdefgh"; echo "ij"; break;
case 'implicit': ob_implicit_flush(); echo "a"; echo "b"; break;
case 'list': var_dump(ob_list_handlers(), ob_get_level()); break;
case 'clean_all': echo "gone"; while (ob_get_level()) ob_end_clean(); echo "kept"; break;
case 'output_add': output_add_rewrite_var('tok', 'v'); echo '<a href="/x.php">x</a><form action="/y"></form>'; break;
}
echo "\nend\n";
