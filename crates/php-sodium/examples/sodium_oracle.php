<?php
// Deterministic ext/sodium results, one "label: value" line each, mirrored
// by tests/oracle.rs.  Run: php -d memory_limit=1G sodium_oracle.php
function show($v) {
    if (is_bool($v)) return $v ? 'true' : 'false';
    if (is_int($v)) return "int($v)";
    if (is_null($v)) return 'null';
    if (is_array($v)) return '[' . implode(',', array_map('show', $v)) . ']';
    return bin2hex($v);
}
function t($label, $f) {
    $warnings = [];
    set_error_handler(function ($no, $str) use (&$warnings) { $warnings[] = $str; return true; });
    try {
        $r = show($f());
    } catch (Throwable $e) {
        $r = 'EXC ' . get_class($e) . ': ' . $e->getMessage();
    }
    restore_error_handler();
    foreach ($warnings as $w) $r .= " [W: $w]";
    echo "$label: $r\n";
}
$k32 = ''; for ($i = 0; $i < 32; $i++) $k32 .= chr($i);
$k64 = $k32 . strrev($k32);
$k16 = substr($k32, 0, 16);
$n24 = str_repeat("\x24", 24);
$n12 = str_repeat("\x12", 12);
$n8 = str_repeat("\x08", 8);
$msg = "The quick brown fox jumps over the lazy dog";
$ad = "additional";

// utils
t('bin2hex', fn() => sodium_bin2hex("\x00\x01\xfe\xff"));
t('hex2bin', fn() => sodium_hex2bin("00:01:fe:ff", ":"));
t('hex2bin_bad', fn() => sodium_hex2bin("0g"));
t('hex2bin_odd', fn() => sodium_hex2bin("abc"));
t('hex2bin_ignore_nul', fn() => sodium_hex2bin("ab cd", " \0x"));
foreach ([1, 3, 5, 7] as $v) {
    t("bin2base64_$v", fn() => sodium_bin2base64("\xfb\xff\xfe" . $msg, $v));
    t("bin2base64_short_$v", fn() => sodium_bin2base64("\xfb\xff", $v));
    t("base642bin_$v", fn() => sodium_base642bin(sodium_bin2base64("\xfb\xff" . $msg, $v), $v));
}
t('bin2base64_badvariant', fn() => sodium_bin2base64("x", 2));
t('base642bin_badvariant', fn() => sodium_base642bin("x", 0));
t('base642bin_bad', fn() => sodium_base642bin("Zm9v!", 1));
t('base642bin_ignore', fn() => sodium_base642bin(" Zm9v\nYmFy ", 1, " \n"));
t('base642bin_nopad_given_pad', fn() => sodium_base642bin("Zm8=", 3));
t('base642bin_pad_missing', fn() => sodium_base642bin("Zm8", 1));
t('pad_xyz_16', fn() => sodium_pad("xyz", 16));
t('pad_empty_1', fn() => sodium_pad("", 1));
t('pad_5_5', fn() => sodium_pad("abcde", 5));
t('pad_7_3', fn() => sodium_pad("abcdefg", 3));
t('pad_bad', fn() => sodium_pad("x", 0));
t('unpad', fn() => sodium_unpad(sodium_pad("abcdefg", 3), 3));
t('unpad_short', fn() => sodium_unpad("x", 2));
t('unpad_bad', fn() => sodium_unpad("abcd", 4));
t('unpad_neg', fn() => sodium_unpad("abcd", -4));
t('compare', fn() => [sodium_compare("\x01\x02", "\x02\x01"), sodium_compare("\x02\x01", "\x01\x02"), sodium_compare("ab", "ab")]);
t('compare_len', fn() => sodium_compare("a", "ab"));
t('memcmp', fn() => [sodium_memcmp("ab", "ab"), sodium_memcmp("ab", "ac")]);
t('increment', function () { $s = "\xff\xfe\x00"; sodium_increment($s); return $s; });
t('add', function () { $s = "\xff\xff\x01"; sodium_add($s, "\x02\x00\x00"); return $s; });
t('add_len', function () { $s = "ab"; sodium_add($s, "a"); return $s; });
t('memzero', function () { $s = "abc" . chr(1); sodium_memzero($s); return $s; });

// aead
foreach (['chacha20poly1305' => $n8, 'chacha20poly1305_ietf' => $n12, 'xchacha20poly1305_ietf' => $n24, 'aes256gcm' => $n12] as $alg => $nonce) {
    $enc = "sodium_crypto_aead_{$alg}_encrypt";
    $dec = "sodium_crypto_aead_{$alg}_decrypt";
    t("aead_{$alg}_enc", fn() => $enc($msg, $ad, $nonce, $k32));
    t("aead_{$alg}_enc_empty", fn() => $enc('', '', $nonce, $k32));
    t("aead_{$alg}_dec", fn() => $dec($enc($msg, $ad, $nonce, $k32), $ad, $nonce, $k32));
    t("aead_{$alg}_dec_badad", fn() => $dec($enc($msg, $ad, $nonce, $k32), 'x', $nonce, $k32));
    t("aead_{$alg}_dec_short", fn() => $dec('short', $ad, $nonce, $k32));
    t("aead_{$alg}_badnonce", fn() => $enc($msg, $ad, 'x', $k32));
    t("aead_{$alg}_badkey", fn() => $enc($msg, $ad, $nonce, 'x'));
    t("aead_{$alg}_dec_badnonce", fn() => $dec($msg, $ad, 'x', $k32));
    t("aead_{$alg}_dec_badkey", fn() => $dec($msg, $ad, $nonce, 'x'));
    t("aead_{$alg}_keygen_len", fn() => strlen(("sodium_crypto_aead_{$alg}_keygen")()));
}
t('aes256gcm_available', fn() => sodium_crypto_aead_aes256gcm_is_available());

// auth / shorthash / generichash / kdf
t('auth', fn() => sodium_crypto_auth($msg, $k32));
t('auth_badkey', fn() => sodium_crypto_auth($msg, $k16));
t('auth_verify', fn() => [sodium_crypto_auth_verify(sodium_crypto_auth($msg, $k32), $msg, $k32), sodium_crypto_auth_verify(str_repeat("\0", 32), $msg, $k32)]);
t('auth_verify_badmac', fn() => sodium_crypto_auth_verify('x', $msg, $k32));
t('auth_verify_badkey', fn() => sodium_crypto_auth_verify('x', $msg, 'y'));
t('shorthash', fn() => sodium_crypto_shorthash($msg, $k16));
t('shorthash_badkey', fn() => sodium_crypto_shorthash($msg, $k32));
t('generichash', fn() => sodium_crypto_generichash($msg));
t('generichash_key', fn() => sodium_crypto_generichash($msg, $k32, 64));
t('generichash_key16_len16', fn() => sodium_crypto_generichash($msg, $k16, 16));
t('generichash_badlen', fn() => sodium_crypto_generichash($msg, '', 15));
t('generichash_badlen2', fn() => sodium_crypto_generichash($msg, '', 65));
t('generichash_badkey', fn() => sodium_crypto_generichash($msg, 'short'));
t('generichash_stream', function () use ($msg, $k32) {
    $st = sodium_crypto_generichash_init($k32, 48);
    sodium_crypto_generichash_update($st, substr($msg, 0, 10));
    sodium_crypto_generichash_update($st, substr($msg, 10));
    $copy = $st;
    $h = sodium_crypto_generichash_final($st, 48);
    return [$h, sodium_crypto_generichash_final($copy, 48), $copy, $st];
});
t('generichash_init_len', fn() => strlen(sodium_crypto_generichash_init()));
t('generichash_update_badstate', function () { $st = 'abc'; return sodium_crypto_generichash_update($st, 'x'); });
t('generichash_final_badstate', function () { $st = 'abc'; return sodium_crypto_generichash_final($st); });
t('generichash_final_badlen', function () { $st = sodium_crypto_generichash_init(); return sodium_crypto_generichash_final($st, 8); });
t('generichash_init_badlen', fn() => sodium_crypto_generichash_init('', 100));
t('kdf', fn() => sodium_crypto_kdf_derive_from_key(32, 7, 'context!', $k32));
t('kdf_64_max_id', fn() => sodium_crypto_kdf_derive_from_key(64, PHP_INT_MAX, 'ABCDEFGH', $k32));
t('kdf_short', fn() => sodium_crypto_kdf_derive_from_key(15, 1, 'context!', $k32));
t('kdf_long', fn() => sodium_crypto_kdf_derive_from_key(65, 1, 'context!', $k32));
t('kdf_negid', fn() => sodium_crypto_kdf_derive_from_key(16, -1, 'context!', $k32));
t('kdf_badctx', fn() => sodium_crypto_kdf_derive_from_key(16, 1, 'ctx', $k32));
t('kdf_badkey', fn() => sodium_crypto_kdf_derive_from_key(16, 1, 'context!', $k16));

// box / kx / scalarmult
$bkp1 = sodium_crypto_box_seed_keypair($k32);
$bkp2 = sodium_crypto_box_seed_keypair(strrev($k32));
t('box_seed_keypair', fn() => $bkp1);
t('box_seed_keypair_bad', fn() => sodium_crypto_box_seed_keypair('x'));
t('box_publickey_from_secretkey', fn() => sodium_crypto_box_publickey_from_secretkey(sodium_crypto_box_secretkey($bkp1)));
t('scalarmult_base', fn() => sodium_crypto_scalarmult_base($k32));
t('scalarmult_base_bad', fn() => sodium_crypto_scalarmult_base('x'));
t('box_publickey_from_secretkey_bad', fn() => sodium_crypto_box_publickey_from_secretkey('x'));
t('box_publickey', fn() => sodium_crypto_box_publickey($bkp1));
t('box_secretkey_bad', fn() => sodium_crypto_box_secretkey('x'));
t('box_publickey_bad', fn() => sodium_crypto_box_publickey('x'));
$alice_to_bob = sodium_crypto_box_keypair_from_secretkey_and_publickey(sodium_crypto_box_secretkey($bkp1), sodium_crypto_box_publickey($bkp2));
$bob_from_alice = sodium_crypto_box_keypair_from_secretkey_and_publickey(sodium_crypto_box_secretkey($bkp2), sodium_crypto_box_publickey($bkp1));
t('box_kp_from_bad1', fn() => sodium_crypto_box_keypair_from_secretkey_and_publickey('x', $k32));
t('box_kp_from_bad2', fn() => sodium_crypto_box_keypair_from_secretkey_and_publickey($k32, 'x'));
t('box', fn() => sodium_crypto_box($msg, $n24, $alice_to_bob));
t('box_open', fn() => sodium_crypto_box_open(sodium_crypto_box($msg, $n24, $alice_to_bob), $n24, $bob_from_alice));
t('box_open_fail', fn() => sodium_crypto_box_open(str_repeat('x', 40), $n24, $bob_from_alice));
t('box_open_short', fn() => sodium_crypto_box_open('x', $n24, $bob_from_alice));
t('box_badnonce', fn() => sodium_crypto_box($msg, 'x', $alice_to_bob));
t('box_badkp', fn() => sodium_crypto_box($msg, $n24, 'x'));
t('box_open_badnonce', fn() => sodium_crypto_box_open($msg, 'x', $alice_to_bob));
t('box_seal_len', fn() => strlen(sodium_crypto_box_seal($msg, sodium_crypto_box_publickey($bkp1))));
t('box_seal_roundtrip', fn() => sodium_crypto_box_seal_open(sodium_crypto_box_seal($msg, sodium_crypto_box_publickey($bkp1)), $bkp1));
t('box_seal_open_fail', fn() => sodium_crypto_box_seal_open(str_repeat('y', 60), $bkp1));
t('box_seal_open_short', fn() => sodium_crypto_box_seal_open('y', $bkp1));
t('box_seal_badpk', fn() => sodium_crypto_box_seal($msg, 'x'));
t('box_seal_open_badkp', fn() => sodium_crypto_box_seal_open($msg, 'x'));
t('scalarmult', fn() => sodium_crypto_scalarmult($k32, sodium_crypto_box_publickey($bkp2)));
t('scalarmult_zero', fn() => sodium_crypto_scalarmult($k32, str_repeat("\0", 32)));
t('scalarmult_badn', fn() => sodium_crypto_scalarmult('x', $k32));
t('scalarmult_badp', fn() => sodium_crypto_scalarmult($k32, 'x'));
$kx1 = sodium_crypto_kx_seed_keypair($k32);
$kx2 = sodium_crypto_kx_seed_keypair(strrev($k32));
t('kx_seed_keypair', fn() => $kx1);
t('kx_seed_keypair_bad', fn() => sodium_crypto_kx_seed_keypair('x'));
t('kx_secretkey', fn() => sodium_crypto_kx_secretkey($kx1));
t('kx_publickey', fn() => sodium_crypto_kx_publickey($kx1));
t('kx_secretkey_bad', fn() => sodium_crypto_kx_secretkey('x'));
t('kx_publickey_bad', fn() => sodium_crypto_kx_publickey('x'));
t('kx_client', fn() => sodium_crypto_kx_client_session_keys($kx1, sodium_crypto_kx_publickey($kx2)));
t('kx_server', fn() => sodium_crypto_kx_server_session_keys($kx2, sodium_crypto_kx_publickey($kx1)));
t('kx_client_bad1', fn() => sodium_crypto_kx_client_session_keys('x', $k32));
t('kx_client_bad2', fn() => sodium_crypto_kx_client_session_keys($kx1, 'x'));
t('kx_server_bad1', fn() => sodium_crypto_kx_server_session_keys('x', $k32));
t('kx_server_bad2', fn() => sodium_crypto_kx_server_session_keys($kx1, 'x'));
t('kx_keypair_len', fn() => strlen(sodium_crypto_kx_keypair()));

// secretbox / stream
t('secretbox', fn() => sodium_crypto_secretbox($msg, $n24, $k32));
t('secretbox_open', fn() => sodium_crypto_secretbox_open(sodium_crypto_secretbox($msg, $n24, $k32), $n24, $k32));
t('secretbox_open_fail', fn() => sodium_crypto_secretbox_open(str_repeat('z', 20), $n24, $k32));
t('secretbox_open_short', fn() => sodium_crypto_secretbox_open('z', $n24, $k32));
t('secretbox_badnonce', fn() => sodium_crypto_secretbox($msg, 'x', $k32));
t('secretbox_badkey', fn() => sodium_crypto_secretbox($msg, $n24, 'x'));
t('secretbox_open_badnonce', fn() => sodium_crypto_secretbox_open($msg, 'x', $k32));
t('secretbox_open_badkey', fn() => sodium_crypto_secretbox_open($msg, $n24, 'x'));
t('stream', fn() => sodium_crypto_stream(40, $n24, $k32));
t('stream_zero', fn() => sodium_crypto_stream(0, $n24, $k32));
t('stream_badnonce', fn() => sodium_crypto_stream(4, 'x', $k32));
t('stream_badkey', fn() => sodium_crypto_stream(4, $n24, 'x'));
t('stream_xor', fn() => sodium_crypto_stream_xor($msg, $n24, $k32));
t('stream_xor_badnonce', fn() => sodium_crypto_stream_xor($msg, 'x', $k32));
t('stream_xchacha20', fn() => sodium_crypto_stream_xchacha20(40, $n24, $k32));
t('stream_xchacha20_neg', fn() => sodium_crypto_stream_xchacha20(-1, $n24, $k32));
t('stream_xchacha20_badnonce', fn() => sodium_crypto_stream_xchacha20(1, 'x', $k32));
t('stream_xchacha20_badkey', fn() => sodium_crypto_stream_xchacha20(1, $n24, 'x'));
t('stream_xchacha20_xor', fn() => sodium_crypto_stream_xchacha20_xor($msg, $n24, $k32));
t('stream_xchacha20_xor_badkey', fn() => sodium_crypto_stream_xchacha20_xor($msg, $n24, 'x'));
t('stream_xchacha20_xor_ic', fn() => sodium_crypto_stream_xchacha20_xor_ic($msg, $n24, 5, $k32));
t('stream_xchacha20_xor_ic_neg', fn() => sodium_crypto_stream_xchacha20_xor_ic($msg, $n24, -1, $k32));
t('stream_xchacha20_xor_ic_badnonce', fn() => sodium_crypto_stream_xchacha20_xor_ic($msg, 'x', 0, $k32));
t('stream_xchacha20_xor_ic_badkey', fn() => sodium_crypto_stream_xchacha20_xor_ic($msg, $n24, 0, 'x'));

// sign
$skp = sodium_crypto_sign_seed_keypair($k32);
$ssk = sodium_crypto_sign_secretkey($skp);
$spk = sodium_crypto_sign_publickey($skp);
t('sign_seed_keypair', fn() => $skp);
t('sign_seed_keypair_bad', fn() => sodium_crypto_sign_seed_keypair('x'));
t('sign_secretkey_bad', fn() => sodium_crypto_sign_secretkey('x'));
t('sign_publickey_bad', fn() => sodium_crypto_sign_publickey('x'));
t('sign_publickey_from_secretkey', fn() => sodium_crypto_sign_publickey_from_secretkey($ssk));
t('sign_publickey_from_secretkey_bad', fn() => sodium_crypto_sign_publickey_from_secretkey('x'));
t('sign_kp_from', fn() => sodium_crypto_sign_keypair_from_secretkey_and_publickey($ssk, $spk));
t('sign_kp_from_bad1', fn() => sodium_crypto_sign_keypair_from_secretkey_and_publickey('x', $spk));
t('sign_kp_from_bad2', fn() => sodium_crypto_sign_keypair_from_secretkey_and_publickey($ssk, 'x'));
t('sign', fn() => sodium_crypto_sign($msg, $ssk));
t('sign_badkey', fn() => sodium_crypto_sign($msg, 'x'));
t('sign_open', fn() => sodium_crypto_sign_open(sodium_crypto_sign($msg, $ssk), $spk));
t('sign_open_fail', fn() => sodium_crypto_sign_open(str_repeat('q', 70), $spk));
t('sign_open_short', fn() => sodium_crypto_sign_open('q', $spk));
t('sign_open_badkey', fn() => sodium_crypto_sign_open('q', 'x'));
t('sign_detached', fn() => sodium_crypto_sign_detached($msg, $ssk));
t('sign_detached_badkey', fn() => sodium_crypto_sign_detached($msg, 'x'));
t('sign_verify_detached', fn() => [sodium_crypto_sign_verify_detached(sodium_crypto_sign_detached($msg, $ssk), $msg, $spk), sodium_crypto_sign_verify_detached(str_repeat("\1", 64), $msg, $spk)]);
t('sign_verify_detached_badsig', fn() => sodium_crypto_sign_verify_detached('x', $msg, $spk));
t('sign_verify_detached_badpk', fn() => sodium_crypto_sign_verify_detached(str_repeat("\1", 64), $msg, 'x'));
t('sign_sk_to_curve', fn() => sodium_crypto_sign_ed25519_sk_to_curve25519($ssk));
t('sign_pk_to_curve', fn() => sodium_crypto_sign_ed25519_pk_to_curve25519($spk));
t('sign_pk_to_curve_fail', fn() => sodium_crypto_sign_ed25519_pk_to_curve25519(str_repeat("\0", 32)));
t('sign_pk_to_curve_bad', fn() => sodium_crypto_sign_ed25519_pk_to_curve25519('x'));
t('sign_sk_to_curve_bad', fn() => sodium_crypto_sign_ed25519_sk_to_curve25519('x'));
t('sign_keypair_len', fn() => strlen(sodium_crypto_sign_keypair()));

// secretstream
t('ss_init_push_bad', fn() => sodium_crypto_secretstream_xchacha20poly1305_init_push('x'));
t('ss_init_pull_badheader', fn() => sodium_crypto_secretstream_xchacha20poly1305_init_pull('x', $k32));
t('ss_init_pull_badkey', fn() => sodium_crypto_secretstream_xchacha20poly1305_init_pull($n24, 'x'));
t('ss_pull_fixed', function () use ($n24, $k32) {
    // Decrypt a stream produced by the C implementation (header fixed).
    $st = sodium_crypto_secretstream_xchacha20poly1305_init_pull($n24, $k32);
    return [strlen($st), sodium_crypto_secretstream_xchacha20poly1305_pull($st, str_repeat("\0", 20))];
});
t('ss_push_badstate', function () { $st = 'abc'; return sodium_crypto_secretstream_xchacha20poly1305_push($st, 'x'); });
t('ss_push_badtag', function () use ($k32) { [$st] = sodium_crypto_secretstream_xchacha20poly1305_init_push($k32); return sodium_crypto_secretstream_xchacha20poly1305_push($st, 'x', '', 256); });
t('ss_push_negtag', function () use ($k32) { [$st] = sodium_crypto_secretstream_xchacha20poly1305_init_push($k32); return sodium_crypto_secretstream_xchacha20poly1305_push($st, 'x', '', -1); });
t('ss_pull_badstate', function () { $st = 'abc'; return sodium_crypto_secretstream_xchacha20poly1305_pull($st, 'x'); });
t('ss_pull_short', function () use ($n24, $k32) { $st = sodium_crypto_secretstream_xchacha20poly1305_init_pull($n24, $k32); return sodium_crypto_secretstream_xchacha20poly1305_pull($st, 'x'); });
t('ss_rekey_badstate', function () { $st = 'abc'; sodium_crypto_secretstream_xchacha20poly1305_rekey($st); return $st; });
t('ss_rekey_fixed', function () use ($n24, $k32) { $st = sodium_crypto_secretstream_xchacha20poly1305_init_pull($n24, $k32); sodium_crypto_secretstream_xchacha20poly1305_rekey($st); return $st; });

// pwhash
$salt16 = substr($k32, 0, 16);
t('pwhash_i', fn() => sodium_crypto_pwhash(32, 'password', $salt16, 3, 8192, SODIUM_CRYPTO_PWHASH_ALG_ARGON2I13));
t('pwhash_id', fn() => sodium_crypto_pwhash(16, 'password', $salt16, 1, 8192, SODIUM_CRYPTO_PWHASH_ALG_ARGON2ID13));
t('pwhash_default', fn() => sodium_crypto_pwhash(16, 'password', $salt16, 2, 16384));
t('pwhash_i_ops2', fn() => sodium_crypto_pwhash(16, 'password', $salt16, 2, 8192, SODIUM_CRYPTO_PWHASH_ALG_ARGON2I13));
t('pwhash_empty', fn() => sodium_crypto_pwhash(16, '', $salt16, 1, 8192));
t('pwhash_len0', fn() => sodium_crypto_pwhash(0, 'p', $salt16, 1, 8192));
t('pwhash_lenbig', fn() => sodium_crypto_pwhash(0xffffffff, 'p', $salt16, 1, 8192));
t('pwhash_ops0', fn() => sodium_crypto_pwhash(16, 'p', $salt16, 0, 8192));
t('pwhash_mem0', fn() => sodium_crypto_pwhash(16, 'p', $salt16, 1, 0));
t('pwhash_alg', fn() => sodium_crypto_pwhash(16, 'p', $salt16, 1, 8192, 3));
t('pwhash_alg_empty', fn() => sodium_crypto_pwhash(16, '', $salt16, 1, 8192, 3));
t('pwhash_salt', fn() => sodium_crypto_pwhash(16, '', 'salt', 1, 8192));
t('pwhash_mem_min', fn() => sodium_crypto_pwhash(16, 'p', $salt16, 1, 8191));
t('pwhash_str_prefix', fn() => substr(sodium_crypto_pwhash_str('password', 1, 8192), 0, 27));
t('pwhash_str_verify', fn() => [sodium_crypto_pwhash_str_verify(sodium_crypto_pwhash_str('password', 1, 8192), 'password'), sodium_crypto_pwhash_str_verify(sodium_crypto_pwhash_str('password', 1, 8192), 'wrong')]);
t('pwhash_str_verify_fixed', fn() => sodium_crypto_pwhash_str_verify('$argon2id$v=19$m=8,t=1,p=1$c2FsdHNhbHQ$1Gb4Cb3pPDB9AAJr3Mr1ThGqp4yOYHNGqk7bDyQcGG4', 'password'));
t('pwhash_str_verify_garbage', fn() => sodium_crypto_pwhash_str_verify('nope', ''));
t('pwhash_str_ops0', fn() => sodium_crypto_pwhash_str('p', 0, 8192));
t('pwhash_str_mem0', fn() => sodium_crypto_pwhash_str('p', 1, 0));
t('pwhash_str_mem_min', fn() => sodium_crypto_pwhash_str('p', 1, 1));
t('pwhash_str_empty_len', fn() => strlen(sodium_crypto_pwhash_str('', 1, 8192)));
t('pwhash_needs_rehash', function () {
    $h = sodium_crypto_pwhash_str('password', 1, 8192);
    return [sodium_crypto_pwhash_str_needs_rehash($h, 1, 8192), sodium_crypto_pwhash_str_needs_rehash($h, 2, 8192), sodium_crypto_pwhash_str_needs_rehash('junk', 1, 8192)];
});
$salt32 = $k32;
t('scrypt', fn() => sodium_crypto_pwhash_scryptsalsa208sha256(32, 'password', $salt32, 524288, 16777216));
t('scrypt_len0', fn() => sodium_crypto_pwhash_scryptsalsa208sha256(0, 'p', $salt32, 524288, 16777216));
t('scrypt_ops0', fn() => sodium_crypto_pwhash_scryptsalsa208sha256(1, 'p', $salt32, 0, 16777216));
t('scrypt_mem0', fn() => sodium_crypto_pwhash_scryptsalsa208sha256(1, 'p', $salt32, 1, -1));
t('scrypt_salt', fn() => sodium_crypto_pwhash_scryptsalsa208sha256(1, '', 'x', 1, 1));
t('scrypt_ops_min', fn() => sodium_crypto_pwhash_scryptsalsa208sha256(1, 'p', $salt32, 1, 1));
t('scrypt_mem_min', fn() => sodium_crypto_pwhash_scryptsalsa208sha256(1, 'p', $salt32, 524288, 1));
t('scrypt_str', function () {
    $h = sodium_crypto_pwhash_scryptsalsa208sha256_str('password', 524288, 16777216);
    return [strlen($h), substr($h, 0, 3), sodium_crypto_pwhash_scryptsalsa208sha256_str_verify($h, 'password'), sodium_crypto_pwhash_scryptsalsa208sha256_str_verify($h, 'nope')];
});
t('scrypt_str_ops0', fn() => sodium_crypto_pwhash_scryptsalsa208sha256_str('p', 0, 1));
t('scrypt_str_mem0', fn() => sodium_crypto_pwhash_scryptsalsa208sha256_str('p', 1, 0));
t('scrypt_str_ops_min', fn() => sodium_crypto_pwhash_scryptsalsa208sha256_str('', 1, 1));
t('scrypt_str_mem_min', fn() => sodium_crypto_pwhash_scryptsalsa208sha256_str('p', 524288, 1));
t('scrypt_str_verify_size', fn() => sodium_crypto_pwhash_scryptsalsa208sha256_str_verify('abc', ''));

// ristretto255
$h64 = $k64;
$p1 = sodium_crypto_core_ristretto255_from_hash($h64);
$p2 = sodium_crypto_core_ristretto255_from_hash(strrev($h64));
$s1 = sodium_crypto_core_ristretto255_scalar_reduce($h64);
$s2 = sodium_crypto_core_ristretto255_scalar_reduce(strrev($h64));
t('r_from_hash', fn() => $p1);
t('r_from_hash_bad', fn() => sodium_crypto_core_ristretto255_from_hash('x'));
t('r_is_valid', fn() => [sodium_crypto_core_ristretto255_is_valid_point($p1), sodium_crypto_core_ristretto255_is_valid_point(str_repeat("\xff", 32))]);
t('r_is_valid_bad', fn() => sodium_crypto_core_ristretto255_is_valid_point('x'));
t('r_add', fn() => sodium_crypto_core_ristretto255_add($p1, $p2));
t('r_add_invalid', fn() => sodium_crypto_core_ristretto255_add($p1, str_repeat("\xff", 32)));
t('r_add_bad1', fn() => sodium_crypto_core_ristretto255_add('x', $p2));
t('r_add_bad2', fn() => sodium_crypto_core_ristretto255_add($p1, 'x'));
t('r_sub', fn() => sodium_crypto_core_ristretto255_sub($p1, $p2));
t('r_sub_bad2', fn() => sodium_crypto_core_ristretto255_sub($p1, 'x'));
t('r_scalar_reduce', fn() => $s1);
t('r_scalar_reduce_bad', fn() => sodium_crypto_core_ristretto255_scalar_reduce($k32));
t('r_scalar_add', fn() => sodium_crypto_core_ristretto255_scalar_add($s1, $s2));
t('r_scalar_add_bad1', fn() => sodium_crypto_core_ristretto255_scalar_add('x', $s2));
t('r_scalar_sub', fn() => sodium_crypto_core_ristretto255_scalar_sub($s1, $s2));
t('r_scalar_sub_bad2', fn() => sodium_crypto_core_ristretto255_scalar_sub($s1, 'x'));
t('r_scalar_mul', fn() => sodium_crypto_core_ristretto255_scalar_mul($s1, $s2));
t('r_scalar_mul_bad2', fn() => sodium_crypto_core_ristretto255_scalar_mul($s1, 'x'));
t('r_scalar_invert', fn() => sodium_crypto_core_ristretto255_scalar_invert($s1));
t('r_scalar_invert_zero', fn() => sodium_crypto_core_ristretto255_scalar_invert(str_repeat("\0", 32)));
t('r_scalar_invert_bad', fn() => sodium_crypto_core_ristretto255_scalar_invert('x'));
t('r_scalar_negate', fn() => sodium_crypto_core_ristretto255_scalar_negate($s1));
t('r_scalar_negate_bad', fn() => sodium_crypto_core_ristretto255_scalar_negate('x'));
t('r_scalar_complement', fn() => sodium_crypto_core_ristretto255_scalar_complement($s1));
t('r_scalar_complement_bad', fn() => sodium_crypto_core_ristretto255_scalar_complement('x'));
t('r_random_valid', fn() => sodium_crypto_core_ristretto255_is_valid_point(sodium_crypto_core_ristretto255_random()));
t('r_scalar_random_len', fn() => strlen(sodium_crypto_core_ristretto255_scalar_random()));
t('r_scalarmult', fn() => sodium_crypto_scalarmult_ristretto255($s1, $p1));
t('r_scalarmult_identity', fn() => sodium_crypto_scalarmult_ristretto255(str_repeat("\0", 32), $p1));
t('r_scalarmult_bad1', fn() => sodium_crypto_scalarmult_ristretto255('x', $p1));
t('r_scalarmult_bad2', fn() => sodium_crypto_scalarmult_ristretto255($s1, 'x'));
t('r_scalarmult_base', fn() => sodium_crypto_scalarmult_ristretto255_base($s1));
t('r_scalarmult_base_zero', fn() => sodium_crypto_scalarmult_ristretto255_base(str_repeat("\0", 32)));
t('r_scalarmult_base_bad', fn() => sodium_crypto_scalarmult_ristretto255_base('x'));

// keygens
foreach (['auth', 'generichash', 'kdf', 'secretbox', 'shorthash', 'stream', 'stream_xchacha20', 'secretstream_xchacha20poly1305'] as $k) {
    t("keygen_$k", fn() => strlen(("sodium_crypto_{$k}_keygen")()));
}
// password_hash() argon2 info via the core (same output as ext/sodium's hooks)
t('password_get_info', fn() => array_values(password_get_info('$argon2id$v=19$m=1024,t=2,p=3$c2FsdHNhbHQ$abc')['options']));
t('password_get_info_partial', fn() => array_values(password_get_info('$argon2i$v=19$m=77$xx')['options']));
