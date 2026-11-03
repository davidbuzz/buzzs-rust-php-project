//! Raw declarations for the parts of libsodium 1.0.18 (`<sodium.h>`) that
//! ext/sodium uses.  Everything here is `unsafe` to call; the safe, PHP-
//! flavoured wrappers live in the other modules.
//!
//! Pointer conventions follow the C headers: `*const u8`/`*mut u8` buffers
//! whose lengths are implied by the primitive's constants unless a length
//! argument is passed explicitly.

#![allow(
    unsafe_code,
    non_camel_case_types,
    clippy::missing_docs_in_private_items
)]

use std::ffi::{c_char, c_int};

/// `unsigned long long`.
pub type ull = u64;

#[link(name = "sodium")]
unsafe extern "C" {
    // core / version / utils
    pub fn sodium_init() -> c_int;
    pub fn sodium_version_string() -> *const c_char;
    pub fn sodium_library_version_major() -> c_int;
    pub fn sodium_library_version_minor() -> c_int;
    pub fn sodium_memzero(pnt: *mut u8, len: usize);
    pub fn sodium_memcmp(b1: *const u8, b2: *const u8, len: usize) -> c_int;
    pub fn sodium_compare(b1: *const u8, b2: *const u8, len: usize) -> c_int;
    pub fn sodium_increment(n: *mut u8, nlen: usize);
    pub fn sodium_add(a: *mut u8, b: *const u8, len: usize);
    pub fn sodium_bin2hex(
        hex: *mut c_char,
        hex_maxlen: usize,
        bin: *const u8,
        bin_len: usize,
    ) -> *mut c_char;
    pub fn sodium_hex2bin(
        bin: *mut u8,
        bin_maxlen: usize,
        hex: *const c_char,
        hex_len: usize,
        ignore: *const c_char,
        bin_len: *mut usize,
        hex_end: *mut *const c_char,
    ) -> c_int;
    pub fn sodium_base64_encoded_len(bin_len: usize, variant: c_int) -> usize;
    pub fn sodium_bin2base64(
        b64: *mut c_char,
        b64_maxlen: usize,
        bin: *const u8,
        bin_len: usize,
        variant: c_int,
    ) -> *mut c_char;
    pub fn sodium_base642bin(
        bin: *mut u8,
        bin_maxlen: usize,
        b64: *const c_char,
        b64_len: usize,
        ignore: *const c_char,
        bin_len: *mut usize,
        b64_end: *mut *const c_char,
        variant: c_int,
    ) -> c_int;
    pub fn sodium_pad(
        padded_buflen_p: *mut usize,
        buf: *mut u8,
        unpadded_buflen: usize,
        blocksize: usize,
        max_buflen: usize,
    ) -> c_int;
    pub fn sodium_unpad(
        unpadded_buflen_p: *mut usize,
        buf: *const u8,
        padded_buflen: usize,
        blocksize: usize,
    ) -> c_int;
    pub fn randombytes_buf(buf: *mut u8, size: usize);

    // AEAD
    pub fn crypto_aead_aes256gcm_is_available() -> c_int;
    pub fn crypto_aead_aes256gcm_encrypt(
        c: *mut u8,
        clen_p: *mut ull,
        m: *const u8,
        mlen: ull,
        ad: *const u8,
        adlen: ull,
        nsec: *const u8,
        npub: *const u8,
        k: *const u8,
    ) -> c_int;
    pub fn crypto_aead_aes256gcm_decrypt(
        m: *mut u8,
        mlen_p: *mut ull,
        nsec: *mut u8,
        c: *const u8,
        clen: ull,
        ad: *const u8,
        adlen: ull,
        npub: *const u8,
        k: *const u8,
    ) -> c_int;
    pub fn crypto_aead_chacha20poly1305_encrypt(
        c: *mut u8,
        clen_p: *mut ull,
        m: *const u8,
        mlen: ull,
        ad: *const u8,
        adlen: ull,
        nsec: *const u8,
        npub: *const u8,
        k: *const u8,
    ) -> c_int;
    pub fn crypto_aead_chacha20poly1305_decrypt(
        m: *mut u8,
        mlen_p: *mut ull,
        nsec: *mut u8,
        c: *const u8,
        clen: ull,
        ad: *const u8,
        adlen: ull,
        npub: *const u8,
        k: *const u8,
    ) -> c_int;
    pub fn crypto_aead_chacha20poly1305_ietf_encrypt(
        c: *mut u8,
        clen_p: *mut ull,
        m: *const u8,
        mlen: ull,
        ad: *const u8,
        adlen: ull,
        nsec: *const u8,
        npub: *const u8,
        k: *const u8,
    ) -> c_int;
    pub fn crypto_aead_chacha20poly1305_ietf_decrypt(
        m: *mut u8,
        mlen_p: *mut ull,
        nsec: *mut u8,
        c: *const u8,
        clen: ull,
        ad: *const u8,
        adlen: ull,
        npub: *const u8,
        k: *const u8,
    ) -> c_int;
    pub fn crypto_aead_xchacha20poly1305_ietf_encrypt(
        c: *mut u8,
        clen_p: *mut ull,
        m: *const u8,
        mlen: ull,
        ad: *const u8,
        adlen: ull,
        nsec: *const u8,
        npub: *const u8,
        k: *const u8,
    ) -> c_int;
    pub fn crypto_aead_xchacha20poly1305_ietf_decrypt(
        m: *mut u8,
        mlen_p: *mut ull,
        nsec: *mut u8,
        c: *const u8,
        clen: ull,
        ad: *const u8,
        adlen: ull,
        npub: *const u8,
        k: *const u8,
    ) -> c_int;

    // auth / shorthash / generichash / kdf
    pub fn crypto_auth(out: *mut u8, inp: *const u8, inlen: ull, k: *const u8) -> c_int;
    pub fn crypto_auth_verify(h: *const u8, inp: *const u8, inlen: ull, k: *const u8) -> c_int;
    pub fn crypto_shorthash(out: *mut u8, inp: *const u8, inlen: ull, k: *const u8) -> c_int;
    pub fn crypto_generichash(
        out: *mut u8,
        outlen: usize,
        inp: *const u8,
        inlen: ull,
        key: *const u8,
        keylen: usize,
    ) -> c_int;
    pub fn crypto_generichash_statebytes() -> usize;
    pub fn crypto_generichash_init(
        state: *mut u8,
        key: *const u8,
        keylen: usize,
        outlen: usize,
    ) -> c_int;
    pub fn crypto_generichash_update(state: *mut u8, inp: *const u8, inlen: ull) -> c_int;
    pub fn crypto_generichash_final(state: *mut u8, out: *mut u8, outlen: usize) -> c_int;
    pub fn crypto_kdf_derive_from_key(
        subkey: *mut u8,
        subkey_len: usize,
        subkey_id: u64,
        ctx: *const c_char,
        key: *const u8,
    ) -> c_int;

    // box / scalarmult / kx helpers
    pub fn crypto_box_keypair(pk: *mut u8, sk: *mut u8) -> c_int;
    pub fn crypto_box_seed_keypair(pk: *mut u8, sk: *mut u8, seed: *const u8) -> c_int;
    pub fn crypto_box_easy(
        c: *mut u8,
        m: *const u8,
        mlen: ull,
        n: *const u8,
        pk: *const u8,
        sk: *const u8,
    ) -> c_int;
    pub fn crypto_box_open_easy(
        m: *mut u8,
        c: *const u8,
        clen: ull,
        n: *const u8,
        pk: *const u8,
        sk: *const u8,
    ) -> c_int;
    pub fn crypto_box_seal(c: *mut u8, m: *const u8, mlen: ull, pk: *const u8) -> c_int;
    pub fn crypto_box_seal_open(
        m: *mut u8,
        c: *const u8,
        clen: ull,
        pk: *const u8,
        sk: *const u8,
    ) -> c_int;
    pub fn crypto_scalarmult(q: *mut u8, n: *const u8, p: *const u8) -> c_int;
    pub fn crypto_scalarmult_base(q: *mut u8, n: *const u8) -> c_int;

    // secretbox / stream
    pub fn crypto_secretbox_easy(
        c: *mut u8,
        m: *const u8,
        mlen: ull,
        n: *const u8,
        k: *const u8,
    ) -> c_int;
    pub fn crypto_secretbox_open_easy(
        m: *mut u8,
        c: *const u8,
        clen: ull,
        n: *const u8,
        k: *const u8,
    ) -> c_int;
    pub fn crypto_stream(c: *mut u8, clen: ull, n: *const u8, k: *const u8) -> c_int;
    pub fn crypto_stream_xor(
        c: *mut u8,
        m: *const u8,
        mlen: ull,
        n: *const u8,
        k: *const u8,
    ) -> c_int;
    pub fn crypto_stream_xchacha20(c: *mut u8, clen: ull, n: *const u8, k: *const u8) -> c_int;
    pub fn crypto_stream_xchacha20_xor(
        c: *mut u8,
        m: *const u8,
        mlen: ull,
        n: *const u8,
        k: *const u8,
    ) -> c_int;
    pub fn crypto_stream_xchacha20_xor_ic(
        c: *mut u8,
        m: *const u8,
        mlen: ull,
        n: *const u8,
        ic: u64,
        k: *const u8,
    ) -> c_int;

    // secretstream
    pub fn crypto_secretstream_xchacha20poly1305_statebytes() -> usize;
    pub fn crypto_secretstream_xchacha20poly1305_init_push(
        state: *mut u8,
        header: *mut u8,
        k: *const u8,
    ) -> c_int;
    pub fn crypto_secretstream_xchacha20poly1305_push(
        state: *mut u8,
        c: *mut u8,
        clen_p: *mut ull,
        m: *const u8,
        mlen: ull,
        ad: *const u8,
        adlen: ull,
        tag: u8,
    ) -> c_int;
    pub fn crypto_secretstream_xchacha20poly1305_init_pull(
        state: *mut u8,
        header: *const u8,
        k: *const u8,
    ) -> c_int;
    pub fn crypto_secretstream_xchacha20poly1305_pull(
        state: *mut u8,
        m: *mut u8,
        mlen_p: *mut ull,
        tag_p: *mut u8,
        c: *const u8,
        clen: ull,
        ad: *const u8,
        adlen: ull,
    ) -> c_int;
    pub fn crypto_secretstream_xchacha20poly1305_rekey(state: *mut u8);

    // sign
    pub fn crypto_sign_keypair(pk: *mut u8, sk: *mut u8) -> c_int;
    pub fn crypto_sign_seed_keypair(pk: *mut u8, sk: *mut u8, seed: *const u8) -> c_int;
    pub fn crypto_sign(
        sm: *mut u8,
        smlen_p: *mut ull,
        m: *const u8,
        mlen: ull,
        sk: *const u8,
    ) -> c_int;
    pub fn crypto_sign_open(
        m: *mut u8,
        mlen_p: *mut ull,
        sm: *const u8,
        smlen: ull,
        pk: *const u8,
    ) -> c_int;
    pub fn crypto_sign_detached(
        sig: *mut u8,
        siglen_p: *mut ull,
        m: *const u8,
        mlen: ull,
        sk: *const u8,
    ) -> c_int;
    pub fn crypto_sign_verify_detached(
        sig: *const u8,
        m: *const u8,
        mlen: ull,
        pk: *const u8,
    ) -> c_int;
    pub fn crypto_sign_ed25519_sk_to_pk(pk: *mut u8, sk: *const u8) -> c_int;
    pub fn crypto_sign_ed25519_pk_to_curve25519(curve: *mut u8, ed: *const u8) -> c_int;
    pub fn crypto_sign_ed25519_sk_to_curve25519(curve: *mut u8, ed: *const u8) -> c_int;

    // pwhash
    pub fn crypto_pwhash(
        out: *mut u8,
        outlen: ull,
        passwd: *const c_char,
        passwdlen: ull,
        salt: *const u8,
        opslimit: ull,
        memlimit: usize,
        alg: c_int,
    ) -> c_int;
    pub fn crypto_pwhash_str(
        out: *mut c_char,
        passwd: *const c_char,
        passwdlen: ull,
        opslimit: ull,
        memlimit: usize,
    ) -> c_int;
    pub fn crypto_pwhash_str_alg(
        out: *mut c_char,
        passwd: *const c_char,
        passwdlen: ull,
        opslimit: ull,
        memlimit: usize,
        alg: c_int,
    ) -> c_int;
    pub fn crypto_pwhash_str_verify(
        hash: *const c_char,
        passwd: *const c_char,
        passwdlen: ull,
    ) -> c_int;
    pub fn crypto_pwhash_str_needs_rehash(
        hash: *const c_char,
        opslimit: ull,
        memlimit: usize,
    ) -> c_int;
    pub fn crypto_pwhash_scryptsalsa208sha256(
        out: *mut u8,
        outlen: ull,
        passwd: *const c_char,
        passwdlen: ull,
        salt: *const u8,
        opslimit: ull,
        memlimit: usize,
    ) -> c_int;
    pub fn crypto_pwhash_scryptsalsa208sha256_str(
        out: *mut c_char,
        passwd: *const c_char,
        passwdlen: ull,
        opslimit: ull,
        memlimit: usize,
    ) -> c_int;
    pub fn crypto_pwhash_scryptsalsa208sha256_str_verify(
        hash: *const c_char,
        passwd: *const c_char,
        passwdlen: ull,
    ) -> c_int;

    // ristretto255
    pub fn crypto_core_ristretto255_is_valid_point(p: *const u8) -> c_int;
    pub fn crypto_core_ristretto255_add(r: *mut u8, p: *const u8, q: *const u8) -> c_int;
    pub fn crypto_core_ristretto255_sub(r: *mut u8, p: *const u8, q: *const u8) -> c_int;
    pub fn crypto_core_ristretto255_from_hash(p: *mut u8, r: *const u8) -> c_int;
    pub fn crypto_core_ristretto255_random(p: *mut u8);
    pub fn crypto_core_ristretto255_scalar_random(r: *mut u8);
    pub fn crypto_core_ristretto255_scalar_invert(recip: *mut u8, s: *const u8) -> c_int;
    pub fn crypto_core_ristretto255_scalar_negate(neg: *mut u8, s: *const u8);
    pub fn crypto_core_ristretto255_scalar_complement(comp: *mut u8, s: *const u8);
    pub fn crypto_core_ristretto255_scalar_add(z: *mut u8, x: *const u8, y: *const u8);
    pub fn crypto_core_ristretto255_scalar_sub(z: *mut u8, x: *const u8, y: *const u8);
    pub fn crypto_core_ristretto255_scalar_mul(z: *mut u8, x: *const u8, y: *const u8);
    pub fn crypto_core_ristretto255_scalar_reduce(r: *mut u8, s: *const u8);
    pub fn crypto_scalarmult_ristretto255(q: *mut u8, n: *const u8, p: *const u8) -> c_int;
    pub fn crypto_scalarmult_ristretto255_base(q: *mut u8, n: *const u8) -> c_int;
}
