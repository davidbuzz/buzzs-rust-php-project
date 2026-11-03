//! Unit tests for the public API surface (errors, helpers, edge cases).
//! Bulk digest vectors live in `tests/oracle.rs`.

use super::*;

fn elems(state: &[StateValue]) -> Vec<StateElem<'_>> {
    state
        .iter()
        .map(|v| match v {
            StateValue::Int(n) => StateElem::Int(*n),
            StateValue::Bytes(b) => StateElem::Bytes(b),
        })
        .collect()
}

#[test]
fn helpers() {
    assert_eq!(bin2hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(
        bin2hex(&sha1(b"abc")),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        crc32(b"The quick brown fox jumped over the lazy dog."),
        2_191_738_434
    );
    let mut reg = 0xFFFF_FFFF;
    reg = crc32_bulk_update(reg, b"The quick brown fox ");
    reg = crc32_bulk_update(reg, b"jumped over the lazy dog.");
    assert_eq!(!reg, 2_191_738_434);
    assert!(hash_equals(b"abc", b"abc"));
    assert!(!hash_equals(b"abc", b"abd"));
    assert!(!hash_equals(b"abc", b"ab"));
}

#[test]
fn crc32_byte_order_quirk() {
    let d = |a: &str| {
        hash(a.as_bytes(), b"abc", &HashOptions::default())
            .unwrap()
            .hex()
    };
    assert_eq!(d("crc32"), "73bb8c64");
    assert_eq!(d("crc32b"), "352441c2");
    assert_eq!(d("crc32b"), format!("{:08x}", crc32(b"abc")));
    assert_eq!(d("joaat"), "ed131f5b");
}

#[test]
fn lookup_is_case_insensitive() {
    assert_eq!(
        Algorithm::find(b"SHA256").map(Algorithm::name),
        Some("sha256")
    );
    assert_eq!(
        Algorithm::find(b"Tiger192,3").map(Algorithm::digest_size),
        Some(24)
    );
    assert!(Algorithm::find(b"sha256 ").is_none());
    assert_eq!(
        Algorithm::find(b"snefru256").map(Algorithm::ops_name),
        Some("snefru")
    );
    assert_eq!(hash_algos().len(), 60);
    assert_eq!(hash_hmac_algos().len(), 44);
}

#[test]
fn error_messages() {
    let e = hash(b"foo", b"", &HashOptions::default()).unwrap_err();
    assert_eq!(e.class, ErrorClass::ValueError);
    assert_eq!(
        e.message_for("hash"),
        "hash(): Argument #1 ($algo) must be a valid hashing algorithm"
    );

    let e = HashContext::new(b"crc32", HASH_HMAC, b"k", &HashOptions::default()).unwrap_err();
    assert_eq!(
        e.message_for("hash_init"),
        "hash_init(): Argument #1 ($algo) must be a cryptographic hashing algorithm if HMAC is requested"
    );
    let e = HashContext::new(b"md5", HASH_HMAC, b"", &HashOptions::default()).unwrap_err();
    assert_eq!(
        e.message_for("hash_init"),
        "hash_init(): Argument #3 ($key) must not be empty when HMAC is requested"
    );
    for f in ["foo", "crc32", "xxh3", "murmur3a"] {
        let e = hash_hmac(f.as_bytes(), b"", b"k").unwrap_err();
        assert_eq!(
            e.message_for("hash_hmac"),
            "hash_hmac(): Argument #1 ($algo) must be a valid cryptographic hashing algorithm"
        );
    }
    assert!(hmac_context(b"crc32", b"k").is_err());

    let e = hash_hkdf(b"sha256", b"", 0, b"", b"").unwrap_err();
    assert_eq!(
        e.message_for("hash_hkdf"),
        "hash_hkdf(): Argument #2 ($key) must not be empty"
    );
    let e = hash_hkdf(b"sha256", b"k", -1, b"", b"").unwrap_err();
    assert_eq!(
        e.message_for("hash_hkdf"),
        "hash_hkdf(): Argument #3 ($length) must be greater than or equal to 0"
    );
    let e = hash_hkdf(b"sha1", b"x", 9999, b"", b"").unwrap_err();
    assert_eq!(
        e.message_for("hash_hkdf"),
        "hash_hkdf(): Argument #3 ($length) must be less than or equal to 5100"
    );
    assert_eq!(
        hash_hkdf(b"sha1", b"x", 5100, b"", b"").unwrap().len(),
        5100
    );

    let e = hash_pbkdf2(b"md5", b"p", b"s", 0, 0, false).unwrap_err();
    assert_eq!(
        e.message_for("hash_pbkdf2"),
        "hash_pbkdf2(): Argument #4 ($iterations) must be greater than 0"
    );
    let e = hash_pbkdf2(b"md5", b"p", b"s", 1, -1, false).unwrap_err();
    assert_eq!(
        e.message_for("hash_pbkdf2"),
        "hash_pbkdf2(): Argument #5 ($length) must be greater than or equal to 0"
    );
}

#[test]
fn pbkdf2_odd_lengths() {
    assert_eq!(
        hash_pbkdf2(b"sha256", b"p", b"s", 1, 3, false).unwrap(),
        b"372"
    );
    let raw = hash_pbkdf2(b"sha1", b"p", b"s", 2, 45, true).unwrap();
    assert_eq!(
        bin2hex(&raw),
        "ea4b66d4030c711fa7c569b86856f404c929ee0a3a54b9691e65644bf909acc30aa73b150d756208a7bb2c6a2a"
    );
    assert_eq!(
        hash_pbkdf2(b"sha1", b"p", b"s", 2, 45, false).unwrap(),
        b"ea4b66d4030c711fa7c569b86856f404c929ee0a3a54b"
    );
}

#[test]
fn seed_and_secret_options() {
    let non_int = HashOptions {
        seed: Some(SeedOption::NonInt),
        secret: None,
    };
    let d = hash(b"murmur3a", b"abc", &non_int).unwrap();
    assert_eq!(
        d.bytes,
        hash(b"murmur3a", b"abc", &HashOptions::default())
            .unwrap()
            .bytes
    );
    assert_eq!(
        d.diagnostics,
        vec![Diagnostic {
            level: DiagnosticLevel::Deprecated,
            message: "Passing a seed of a type other than int is deprecated because it is the same as setting the seed to 0".into()
        }]
    );
    let d = hash(b"xxh3", b"abc", &non_int).unwrap();
    assert_eq!(
        d.diagnostics[0].message,
        "Passing a seed of a type other than int is deprecated because it is ignored"
    );
    // Options are ignored by algorithms that take none.
    assert!(hash(b"md5", b"abc", &non_int)
        .unwrap()
        .diagnostics
        .is_empty());

    let both = HashOptions {
        seed: Some(SeedOption::Int(24)),
        secret: Some(SecretOption {
            bytes: vec![b'a'; 256],
            was_string: true,
        }),
    };
    let e = HashContext::new(b"xxh128", 0, b"", &both).unwrap_err();
    assert_eq!(e.class, ErrorClass::Error);
    assert_eq!(
        e.message,
        "xxh128: Only one of seed or secret is to be passed for initialization"
    );

    let e =
        HashContext::new(b"xxh3", 0, b"", &HashOptions::with_secret(vec![b'a'; 17])).unwrap_err();
    assert_eq!(
        e.message_for("hash_init"),
        "xxh3: Secret length must be >= 136 bytes, 17 bytes passed"
    );

    let long = HashOptions::with_secret(vec![b'a'; 300]);
    let d = hash(b"xxh3", b"Lorem", &long).unwrap();
    assert_eq!(d.diagnostics[0].level, DiagnosticLevel::Warning);
    assert_eq!(
        d.diagnostics[0].message,
        "xxh3: Secret content exceeding 256 bytes discarded"
    );
    assert_eq!(
        d.bytes,
        hash(
            b"xxh3",
            b"Lorem",
            &HashOptions::with_secret(vec![b'a'; 256])
        )
        .unwrap()
        .bytes
    );

    let converted = HashOptions {
        seed: None,
        secret: Some(SecretOption {
            bytes: vec![b'1'; 200],
            was_string: false,
        }),
    };
    let d = hash(b"xxh3", b"x", &converted).unwrap();
    assert!(d.diagnostics[0]
        .message
        .starts_with("Passing a secret of a type other than string"));
}

#[test]
fn context_lifecycle() {
    let mut ctx = HashContext::new(b"sha256", 0, b"", &HashOptions::default()).unwrap();
    ctx.update(b"ab").unwrap();
    let mut copy = ctx.try_clone().unwrap();
    ctx.update(b"c").unwrap();
    copy.update(b"c").unwrap();
    let a = ctx.finalize().unwrap();
    assert_eq!(a, copy.finalize().unwrap());
    assert_eq!(
        bin2hex(&a),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(ctx.is_finalized());
    let e = ctx.update(b"x").unwrap_err();
    assert_eq!(e.class, ErrorClass::TypeError);
    assert_eq!(
        e.message_for("hash_update"),
        "hash_update(): Argument #1 ($context) must be a valid, non-finalized HashContext"
    );
    assert!(ctx.finalize().is_err());
    let e = ctx.try_clone().unwrap_err();
    assert_eq!(
        (e.class, e.message.as_str()),
        (
            ErrorClass::ValueError,
            "Cannot clone a finalized HashContext"
        )
    );
    assert!(ctx.clone().is_finalized());
    assert_eq!(ctx.algo_name(), "sha256");
}

#[test]
fn hmac_with_long_key_via_context() {
    let key = vec![7u8; 200];
    let mut ctx = hmac_context(b"sha3-512", &key).unwrap();
    ctx.update(b"payload").unwrap();
    assert_eq!(
        ctx.finalize().unwrap(),
        hash_hmac(b"sha3-512", b"payload", &key).unwrap()
    );
}

#[test]
fn serialization_errors() {
    let ctx = HashContext::new(b"md5", HASH_HMAC, b"key", &HashOptions::default()).unwrap();
    assert_eq!(
        ctx.serialize().unwrap_err().message,
        "HashContext with HASH_HMAC option cannot be serialized"
    );
    let ctx = HashContext::new(b"xxh128", 0, b"", &HashOptions::default()).unwrap();
    let e = ctx.serialize().unwrap_err();
    assert_eq!(
        (e.class, e.message.as_str()),
        (
            ErrorClass::Exception,
            "HashContext for algorithm \"xxh128\" cannot be serialized"
        )
    );

    let mut ctx = HashContext::new(b"sha1", 0, b"", &HashOptions::default()).unwrap();
    ctx.update(b"I can't remember anything").unwrap();
    let s = ctx.serialize().unwrap();
    assert_eq!(s.algo, "sha1");
    assert_eq!(s.magic, SERIALIZE_MAGIC_SPEC);
    assert_eq!(s.state.len(), 8);
    let good = elems(&s.state);

    // "serialization internals wrong": element 6 is a string -> code -1024.
    let mut bad = good.clone();
    bad[6] = StateElem::Bytes(b"");
    let lookup = |j: usize| bad.get(j).copied();
    let e = HashContext::unserialize(b"sha1", 0, 2, Some(&lookup)).unwrap_err();
    assert_eq!(
        e.message,
        "Incomplete or ill-formed serialization data (\"sha1\" code -1024)"
    );

    let lookup = |j: usize| good.get(j).copied();
    // Bad magic number.
    let e = HashContext::unserialize(b"sha1", 0, 0, Some(&lookup)).unwrap_err();
    assert_eq!(
        e.message,
        "Incomplete or ill-formed serialization data (\"sha1\" code -1)"
    );
    // State is not an array.
    let e = HashContext::unserialize(b"sha1", 0, 2, None).unwrap_err();
    assert_eq!(
        e.message,
        "Incomplete or ill-formed serialization data (\"sha1\" code -1)"
    );
    let e = HashContext::unserialize(b"xxx1", 0, 2, Some(&lookup)).unwrap_err();
    assert_eq!(e.message, "Unknown hash algorithm");
    let e = HashContext::unserialize(b"sha1", 1, 2, Some(&lookup)).unwrap_err();
    assert_eq!(
        e.message,
        "HashContext with HASH_HMAC option cannot be serialized"
    );
    let e = HashContext::unserialize(b"xxh3", 0, 2, Some(&lookup)).unwrap_err();
    assert_eq!(
        e.message,
        "Incomplete or ill-formed serialization data (\"xxh3\" code -1)"
    );

    let mut back = HashContext::unserialize(b"SHA1", 0, 2, Some(&lookup)).unwrap();
    assert_eq!(
        bin2hex(&back.finalize().unwrap()),
        "29f62a228f726cd728efa7a0ac6a2aba318baf15"
    );
}

#[test]
fn unserialize_validation_failure() {
    // xxh32 with memsize >= 16 is rejected (xxhash_unserialize_memsize.phpt).
    let ctx = HashContext::new(b"xxh32", 0, b"", &HashOptions::default()).unwrap();
    let s = ctx.serialize().unwrap();
    let mut e = elems(&s.state);
    e[10] = StateElem::Int(16);
    let lookup = |j: usize| e.get(j).copied();
    let err = HashContext::unserialize(b"xxh32", 0, 2, Some(&lookup)).unwrap_err();
    assert_eq!(
        err.message,
        "Incomplete or ill-formed serialization data (\"xxh32\" code -2000)"
    );

    // sha3 uses the Keccak magic.
    let ctx = HashContext::new(b"sha3-256", 0, b"", &HashOptions::default()).unwrap();
    let s = ctx.serialize().unwrap();
    assert_eq!(s.magic, 100);
    let e = elems(&s.state);
    let lookup = |j: usize| e.get(j).copied();
    assert!(HashContext::unserialize(b"sha3-256", 0, 2, Some(&lookup)).is_err());
    assert!(HashContext::unserialize(b"sha3-256", 0, 100, Some(&lookup)).is_ok());
}

#[test]
fn mhash_helpers() {
    let md5 = mhash(1, b"x", None).unwrap().unwrap();
    assert_eq!(bin2hex(&md5), "9dd4e461268c8034f5c8564e155c67a6");
    let mac = mhash(2, b"x", Some(b"k")).unwrap().unwrap();
    assert_eq!(bin2hex(&mac), "cb00131fcec602a8fc84413a2a70154d8c06a0d2");
    assert_eq!(mhash(4, b"x", None).unwrap(), None);
    assert_eq!(mhash(99, b"x", None).unwrap(), None);
    let key = mhash_keygen_s2k(2, b"pw", b"saltsalty", 50)
        .unwrap()
        .unwrap();
    assert_eq!(
        bin2hex(&key),
        "68778c5d46180727d701e54bafa1597852164d86ba0a266e25b7be070811097e9576d336572b036eade06f0dd73c7c642010"
    );
    assert_eq!(mhash_get_hash_name(7), Some("TIGER"));
    assert_eq!(mhash_get_block_size(7), Some(24));
    assert_eq!(mhash_count(), 41);
    assert_eq!(mhash_constants().len(), 39);
    let e = mhash_keygen_s2k(2, b"", b"", 0).unwrap_err();
    assert_eq!(
        e.message_for("mhash_keygen_s2k"),
        "mhash_keygen_s2k(): Argument #4 ($length) must be a greater than 0"
    );
}

#[test]
fn pbkdf2_huge_length_does_not_panic() {
    let size = hash_pbkdf2_alloc_size(b"sha256", i64::MAX, true).unwrap();
    assert_eq!(
        size,
        (usize::try_from(i64::MAX).unwrap() - 1) / 32 * 32 + 32
    );
    let e = hash_pbkdf2(b"sha256", b"p", b"s", 1, i64::MAX, true).unwrap_err();
    assert_eq!(e.out_of_memory, Some(size));
    assert_eq!(e.class, ErrorClass::Error);
    let e = hash_pbkdf2(b"md5", b"p", b"s", 1, i64::MAX - 1, false).unwrap_err();
    assert!(e.out_of_memory.is_some());
    assert_eq!(hash_pbkdf2_alloc_size(b"sha1", 0, false), Some(20));
    assert_eq!(hash_pbkdf2_alloc_size(b"crc32", 10, true), None);
    assert_eq!(hash_pbkdf2_alloc_size(b"sha1", -1, true), None);
}

#[test]
fn diagnostics_survive_error_path() {
    let opts = HashOptions {
        seed: None,
        secret: Some(SecretOption {
            bytes: b"short".to_vec(),
            was_string: false,
        }),
    };
    let e = HashContext::new(b"xxh3", 0, b"", &opts).unwrap_err();
    assert_eq!(
        e.message,
        "xxh3: Secret length must be >= 136 bytes, 5 bytes passed"
    );
    assert_eq!(e.diagnostics.len(), 1);
    assert_eq!(e.diagnostics[0].level, DiagnosticLevel::Deprecated);
    assert!(e.diagnostics[0]
        .message
        .starts_with("Passing a secret of a type other than string"));
    let e = hash(b"xxh128", b"x", &opts).unwrap_err();
    assert_eq!(e.diagnostics.len(), 1);
    // Errors without preceding diagnostics carry none.
    let e = HashContext::new(
        b"xxh3",
        0,
        b"",
        &HashOptions::with_secret(b"short".to_vec()),
    )
    .unwrap_err();
    assert!(e.diagnostics.is_empty());
}
