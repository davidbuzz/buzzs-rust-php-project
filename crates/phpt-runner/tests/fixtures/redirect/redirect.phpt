--TEST--
Redirector
--REDIRECTTEST--
return [
    'ENV' => ['REDIR_VALUE' => 'from-redirect'],
    'TESTS' => dirname(__FILE__) . '/targets',
];
