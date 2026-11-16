// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Rough timing of the 4096-bit MODP operations used by the cryptography phpt tests.
#![allow(clippy::many_single_char_names)]

use php_gmp::Mpz;
use std::time::Instant;

fn main() {
    let p = &Mpz::pow2(4096) - &Mpz::from(1_234_567);
    let a = &Mpz::pow2(4095) + &Mpz::from(987_654_321);
    let t = Instant::now();
    let r = Mpz::from(2).powm(&a, &p);
    println!("powm 4096: {:?} ({} bits)", t.elapsed(), r.bit_len());
    let q = Mpz::parse(b"FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA051015728E5A8AACAA68FFFFFFFFFFFFFFFF", 16).unwrap();
    let t = Instant::now();
    println!("prob_prime 2048: {} {:?}", q.probab_prime(10), t.elapsed());
    let t = Instant::now();
    let f = Mpz::factorial(20_000);
    println!("fact 20000: {:?}", t.elapsed());
    let t = Instant::now();
    let s = f.to_str_radix(10);
    println!("to_str {} digits: {:?}", s.len(), t.elapsed());
    let t = Instant::now();
    let back = Mpz::parse(s.as_bytes(), 10).unwrap();
    println!("parse: {:?} eq={}", t.elapsed(), back == f);
}
