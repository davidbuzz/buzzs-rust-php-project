//! php-runtime hot-path micro-benchmarks, named after the php-src function
//! each one mirrors (Zend/zend_hash.c, Zend/zend_string.h, Zend/zend_operators.c).

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use php_runtime::error::CollectingContext;
use php_runtime::{ops, ArrayKey, HashTable, Value, ZStr};

fn packed(n: i64) -> HashTable {
    let mut t = HashTable::new();
    for i in 0..n {
        let _ = t.push(Value::Int(i));
    }
    t
}

fn keys(n: usize) -> Vec<ZStr> {
    (0..n).map(|i| ZStr::from(format!("key_{i}"))).collect()
}

fn hashed(ks: &[ZStr]) -> HashTable {
    let mut t = HashTable::new();
    for (i, k) in ks.iter().enumerate() {
        t.insert(ArrayKey::Str(k.clone()), Value::Int(i64::try_from(i).unwrap_or(0)));
    }
    t
}

fn zend_hash(c: &mut Criterion) {
    let mut g = c.benchmark_group("zend_hash");
    g.bench_function("next_index_insert_packed_1k", |b| {
        b.iter(|| black_box(packed(1000)));
    });
    let p = packed(1024);
    g.bench_function("index_find_packed", |b| {
        let mut i = 0i64;
        b.iter(|| {
            i = (i + 7) & 1023;
            black_box(p.get(&ArrayKey::Int(i)))
        });
    });
    g.bench_function("foreach_packed_1k", |b| {
        b.iter(|| {
            let mut s = 0i64;
            for (_, v) in p.iter() {
                if let Value::Int(x) = v {
                    s += *x;
                }
            }
            black_box(s)
        });
    });
    let ks = keys(1000);
    g.bench_function("add_new_str_1k", |b| {
        b.iter(|| black_box(hashed(&ks)));
    });
    let h = hashed(&ks);
    let lookups: Vec<ArrayKey> = ks.iter().map(|k| ArrayKey::Str(k.clone())).collect();
    g.bench_function("find_known_hash", |b| {
        let mut i = 0usize;
        b.iter(|| {
            i = (i + 7) % lookups.len();
            black_box(h.get(&lookups[i]))
        });
    });
    g.bench_function("find_fresh_string", |b| {
        // A key built at runtime: hash computed on first use (zend_string_hash_val).
        b.iter_batched(
            || ArrayKey::Str(ZStr::from(format!("key_{}", 517))),
            |k| black_box(h.get(&k).is_some()),
            BatchSize::SmallInput,
        );
    });
    g.bench_function("foreach_hash_1k", |b| {
        b.iter(|| black_box(h.iter().count()));
    });
    g.bench_function("dup_and_write_cow_1k", |b| {
        // Separation on write of a shared array (zend_array_dup).
        b.iter(|| {
            let mut t = h.clone();
            t.insert(ArrayKey::Int(1), Value::Null);
            black_box(t)
        });
    });
    g.finish();
}

fn zend_string(c: &mut Criterion) {
    let mut g = c.benchmark_group("zend_string");
    let a = ZStr::from("hello, ");
    let bb = ZStr::from("world");
    g.bench_function("concat_short", |b| {
        b.iter(|| black_box(ZStr::concat(a.as_bytes(), bb.as_bytes())));
    });
    g.bench_function("extend_append_1k", |b| {
        b.iter(|| {
            let mut s = ZStr::from("");
            for _ in 0..1000 {
                s.append(b"x");
            }
            black_box(s)
        });
    });
    let x1 = ZStr::from("a fairly long string used for equality, 48 bytes");
    let x2 = ZStr::from("a fairly long string used for equality, 48 bytes");
    g.bench_function("equal_content_distinct_ptrs", |b| {
        b.iter(|| black_box(x1 == x2));
    });
    g.bench_function("hash_val_fresh", |b| {
        b.iter_batched(
            || ZStr::from("some_identifier_name"),
            |s| black_box(s.hash_value()),
            BatchSize::SmallInput,
        );
    });
    g.bench_function("intern_existing", |b| {
        let _keep = ZStr::intern(b"strlen");
        b.iter(|| black_box(ZStr::intern(b"strlen")));
    });
    g.bench_function("clone_drop", |b| {
        b.iter(|| black_box(a.clone()));
    });
    g.finish();
}

fn zend_operators(c: &mut Criterion) {
    let mut g = c.benchmark_group("zend_operators");
    let mut ctx = CollectingContext::new();
    let i1 = Value::Int(40);
    let i2 = Value::Int(2);
    let f1 = Value::Float(1.5);
    let s1 = Value::Str(ZStr::from("42"));
    let s2 = Value::Str(ZStr::from("abc"));
    g.bench_function("add_long_long", |b| {
        b.iter(|| black_box(ops::add(black_box(&i1), black_box(&i2), &mut ctx)));
    });
    g.bench_function("add_long_double", |b| {
        b.iter(|| black_box(ops::add(black_box(&i1), black_box(&f1), &mut ctx)));
    });
    g.bench_function("add_numeric_string", |b| {
        b.iter(|| black_box(ops::add(black_box(&s1), black_box(&i2), &mut ctx)));
    });
    g.bench_function("concat_str_str", |b| {
        b.iter(|| black_box(ops::concat(black_box(&s1), black_box(&s2), &mut ctx)));
    });
    g.bench_function("concat_str_long", |b| {
        b.iter(|| black_box(ops::concat(black_box(&s2), black_box(&i1), &mut ctx)));
    });
    g.bench_function("is_smaller_long", |b| {
        b.iter(|| black_box(ops::is_smaller(black_box(&i1), black_box(&i2), &mut ctx)));
    });
    g.bench_function("is_equal_str_long", |b| {
        b.iter(|| black_box(ops::is_equal(black_box(&s1), black_box(&i1), &mut ctx)));
    });
    g.bench_function("compare_str_str", |b| {
        b.iter(|| black_box(ops::compare(black_box(&s1), black_box(&s2), &mut ctx)));
    });
    g.bench_function("value_clone_drop_str", |b| {
        b.iter(|| black_box(s2.clone()));
    });
    g.finish();
}

criterion_group!(benches, zend_hash, zend_string, zend_operators);
criterion_main!(benches);
