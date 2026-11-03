//! Test vectors generated from the C implementation (`/usr/bin/php`), see
//! `vectors.rs`.

mod vectors;

use php_hash::{
    bin2hex, hash, hash_algos, hash_hkdf, hash_hmac, hash_hmac_algos, hash_pbkdf2, HashContext,
    HashOptions, StateElem, StateValue,
};
use vectors::{CHUNKED, DIGESTS, HKDF, HMAC, INPUTS, PBKDF2, SECRET, SECRETED, SEEDED, SERIALIZED};

fn hex(algo: &str, data: &[u8], opts: &HashOptions) -> String {
    hash(algo.as_bytes(), data, opts)
        .unwrap_or_else(|e| panic!("{algo}: {e}"))
        .hex()
}

#[test]
fn algos_order_matches_php() {
    let php: Vec<&str> = DIGESTS.iter().map(|(a, _)| *a).collect();
    assert_eq!(hash_algos(), php);
    let hmac: Vec<&str> = HMAC.iter().map(|(a, _, _)| *a).collect();
    assert_eq!(hash_hmac_algos(), hmac);
}

#[test]
fn one_shot_digests() {
    let mut failures = Vec::new();
    for (algo, expected) in DIGESTS {
        for (input, want) in INPUTS.iter().zip(*expected) {
            let got = hex(algo, input, &HashOptions::default());
            if got != *want {
                failures.push(format!("{algo} len={}: got {got} want {want}", input.len()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn chunked_updates() {
    let big = INPUTS[6];
    let mut failures = Vec::new();
    for (algo, want) in CHUNKED {
        let mut ctx = HashContext::new(algo.as_bytes(), 0, b"", &HashOptions::default()).unwrap();
        let (mut p, mut n) = (0usize, 1usize);
        while p < big.len() {
            let end = (p + n).min(big.len());
            ctx.update(&big[p..end]).unwrap();
            p = end;
            n = n % 70 + 1;
        }
        let got = bin2hex(&ctx.finalize().unwrap());
        if got != *want {
            failures.push(format!("{algo}: got {got} want {want}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn hmac_one_shot_and_incremental() {
    let long_key = vec![b'k'; 300];
    for (algo, short, long) in HMAC {
        assert_eq!(
            bin2hex(&hash_hmac(algo.as_bytes(), b"data", b"key").unwrap()),
            *short,
            "{algo}"
        );
        assert_eq!(
            bin2hex(&hash_hmac(algo.as_bytes(), INPUTS[6], &long_key).unwrap()),
            *long,
            "{algo}"
        );
        let mut ctx = HashContext::new(
            algo.as_bytes(),
            php_hash::HASH_HMAC,
            b"key",
            &HashOptions::default(),
        )
        .unwrap();
        ctx.update(b"da").unwrap();
        let mut copy = ctx.try_clone().unwrap();
        ctx.update(b"ta").unwrap();
        copy.update(b"ta").unwrap();
        assert_eq!(bin2hex(&ctx.finalize().unwrap()), *short, "{algo}");
        assert_eq!(bin2hex(&copy.finalize().unwrap()), *short, "{algo} copy");
    }
}

#[test]
fn seeded() {
    let mut failures = Vec::new();
    for (i, (algo, seed, want)) in SEEDED.iter().enumerate() {
        let input = if i % 2 == 0 { INPUTS[2] } else { INPUTS[6] };
        let got = hex(algo, input, &HashOptions::with_seed(*seed));
        if got != *want {
            failures.push(format!("{algo} seed={seed}: got {got} want {want}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn xxh3_secret() {
    for (algo, short, long) in SECRETED {
        assert_eq!(
            hex(algo, INPUTS[2], &HashOptions::with_secret(SECRET)),
            *short,
            "{algo}"
        );
        assert_eq!(
            hex(algo, INPUTS[6], &HashOptions::with_secret(SECRET)),
            *long,
            "{algo}"
        );
    }
}

/// Minimal PHP `serialize()` of a state array.
fn php_serialize(state: &[StateValue]) -> Vec<u8> {
    let mut out = format!("a:{}:{{", state.len()).into_bytes();
    for (i, v) in state.iter().enumerate() {
        out.extend_from_slice(format!("i:{i};").as_bytes());
        match v {
            StateValue::Int(n) => out.extend_from_slice(format!("i:{n};").as_bytes()),
            StateValue::Bytes(b) => {
                out.extend_from_slice(format!("s:{}:\"", b.len()).as_bytes());
                out.extend_from_slice(b);
                out.extend_from_slice(b"\";");
            }
        }
    }
    out.push(b'}');
    out
}

#[test]
fn serialization_matches_php() {
    let mut failures = Vec::new();
    for (i, (algo, want, magic)) in SERIALIZED.iter().enumerate() {
        let data: &[u8] = if i % 2 == 0 {
            b"I can't remember anything"
        } else {
            INPUTS[6]
        };
        let mut ctx = HashContext::new(algo.as_bytes(), 0, b"", &HashOptions::default()).unwrap();
        ctx.update(data).unwrap();
        let s = ctx.serialize().unwrap();
        if php_serialize(&s.state) != *want || s.magic != *magic {
            failures.push(format!(
                "{algo}: got {} want {}",
                String::from_utf8_lossy(&php_serialize(&s.state)),
                String::from_utf8_lossy(want)
            ));
            continue;
        }
        // Round trip, then continue hashing.
        let elems: Vec<StateElem<'_>> = s
            .state
            .iter()
            .map(|v| match v {
                StateValue::Int(n) => StateElem::Int(*n),
                StateValue::Bytes(b) => StateElem::Bytes(b),
            })
            .collect();
        let lookup = |j: usize| elems.get(j).copied();
        let mut back =
            HashContext::unserialize(s.algo.as_bytes(), 0, s.magic, Some(&lookup)).unwrap();
        back.update(b"more").unwrap();
        ctx.update(b"more").unwrap();
        assert_eq!(back.finalize().unwrap(), ctx.finalize().unwrap(), "{algo}");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn pbkdf2() {
    for (algo, it, len, raw, want) in PBKDF2 {
        let got = hash_pbkdf2(algo.as_bytes(), b"password", b"salt", *it, *len, *raw).unwrap();
        assert_eq!(bin2hex(&got), *want, "{algo} {it} {len} {raw}");
    }
}

#[test]
fn hkdf() {
    for (algo, len, info, salt, want) in HKDF {
        let got = hash_hkdf(
            algo.as_bytes(),
            b"input key",
            *len,
            info.as_bytes(),
            salt.as_bytes(),
        )
        .unwrap();
        assert_eq!(bin2hex(&got), *want, "{algo} {len}");
    }
}

#[test]
fn xxh3_secret_phpt_vectors() {
    let secret = vec![b'a'; 256];
    let data = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit.";
    assert_eq!(
        hex("xxh3", data, &HashOptions::with_secret(secret.clone())),
        "8028aa834c03557a"
    );
    assert_eq!(
        hex("xxh128", data, &HashOptions::with_secret(secret)),
        "54279097795e7218093a05d4d781cbb9"
    );
}
