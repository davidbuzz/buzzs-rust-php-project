//! Rough timings of expensive operations (`cargo run --release --example perf`).
use std::time::Instant;

use php_bcmath::BcMath;

fn time<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    println!(
        "{label:<40} {:>10.3} ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
    r
}

fn main() {
    let bc = BcMath::new();
    let big: String = (0..20_000)
        .map(|i| char::from(b'1' + u8::try_from(i % 9).unwrap()))
        .collect();
    let big2: String = (0..9_000)
        .map(|i| char::from(b'1' + u8::try_from((i * 7) % 9).unwrap()))
        .collect();
    time("bcsqrt(2, 5000)", || bc.bcsqrt(b"2", Some(5000)).unwrap());
    time("bcsqrt(2, 20000)", || bc.bcsqrt(b"2", Some(20000)).unwrap());
    time("bcpow(2, 100000)", || {
        bc.bcpow(b"2", b"100000", None).unwrap()
    });
    time("bcpow(1.0001, 5000, 0)", || {
        bc.bcpow(b"1.0001", b"5000", Some(0)).unwrap()
    });
    time("bcmul(20k digits, 9k digits)", || {
        bc.bcmul(big.as_bytes(), big2.as_bytes(), None).unwrap()
    });
    time("bcdiv(20k digits, 9k digits, 100)", || {
        bc.bcdiv(big.as_bytes(), big2.as_bytes(), Some(100))
            .unwrap()
    });
    time("bcdiv(1, 3, 100000)", || {
        bc.bcdiv(b"1", b"3", Some(100_000)).unwrap()
    });
    let m: String = big2[..300].to_string();
    let e: String = big2[300..600].to_string();
    time("bcpowmod(300-digit)", || {
        bc.bcpowmod(b"123456789", e.as_bytes(), m.as_bytes(), None)
            .unwrap()
    });
}
