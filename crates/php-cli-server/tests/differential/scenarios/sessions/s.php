<?php
ini_set('session.save_path', __DIR__ . '/sess');
@mkdir(__DIR__ . '/sess');
$m = $_GET['m'] ?? '';
if ($m === 'limiter') session_cache_limiter($_GET['l']);
if ($m === 'strict') ini_set('session.use_strict_mode', '1');
if ($m === 'lax') ini_set('session.use_strict_mode', '0');
if ($m === 'handler') {
    session_set_save_handler(new class implements SessionHandlerInterface, SessionIdInterface, SessionUpdateTimestampHandlerInterface {
        function create_sid(): string { return 'customid' . str_repeat('a', 24); }
        function validateId($id): bool { return true; }
        function updateTimestamp($id, $d): bool { echo "touch\n"; return true; }
        function open($p, $n): bool { $GLOBALS['log'][] = "open"; return true; }
        function close(): bool { echo "close\n"; return true; }
        function read($id): string|false { $GLOBALS['log'][] = "read $id"; return 'n|i:41;'; }
        function write($id, $d): bool { echo "write $d\n"; return true; }
        function destroy($id): bool { return true; }
        function gc($l): int|false { return 0; }
    });
}
$before = [session_status(), session_name(), session_cache_limiter(), session_module_name()];
$started = session_start();
var_dump($before, $started, $GLOBALS['log'] ?? null);
echo "id shape: ", strlen(session_id()), ' ', preg_match('/^[a-v0-9,-]+$/', session_id()), "\n";
$_SESSION['n'] = ($_SESSION['n'] ?? 0) + 1;
var_dump($_SESSION);
switch ($m) {
case 'regen': var_dump(session_regenerate_id(true)); break;
case 'destroy': var_dump(session_destroy()); setcookie(session_name(), '', 1); break;
case 'two': session_write_close(); session_name('SECOND'); session_id('second'); session_start(); $_SESSION['x'] = 1; var_dump($_SESSION); break;
case 'abort': session_abort(); break;
case 'reset': $_SESSION['n'] = 100; session_reset(); var_dump($_SESSION); break;
case 'encode': var_dump(session_encode()); break;
case 'params': session_write_close(); session_set_cookie_params(['lifetime' => 0, 'path' => '/x', 'samesite' => 'Strict']); var_dump(session_get_cookie_params()); break;
}
echo implode("\n", array_filter(headers_list(), fn($h) => !str_starts_with($h, 'X-Powered'))), "\n";
