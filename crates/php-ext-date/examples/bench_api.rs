//! Times a few `php-date` API calls (debug-build profiling aid).
use std::time::Instant;

fn main() {
    let tz = php_date::tz::get("UTC").expect("UTC");
    let (a, _) = php_date::api::date_initialize(b"2009-11-20", None, None, &tz, 0, 0);
    let (b, _) = php_date::api::date_initialize(b"2010-03-04", None, None, &tz, 0, 0);
    let (a, b) = (a.expect("a"), b.expect("b"));
    let n = 10_000;
    let t = Instant::now();
    for _ in 0..n {
        std::hint::black_box(php_date::api::interval_from_iso(b"P1Y2M3D").ok());
    }
    println!("iso   {:?}", t.elapsed() / n);
    let t = Instant::now();
    for _ in 0..n {
        std::hint::black_box(php_date::api::diff(&a, &b, false));
    }
    println!("diff  {:?}", t.elapsed() / n);
    let iv = php_date::api::interval_from_iso(b"P1Y2M3D").expect("iv");
    let t = Instant::now();
    for _ in 0..n {
        std::hint::black_box(php_date::api::add(&a, &iv));
    }
    println!("add   {:?}", t.elapsed() / n);
    let t = Instant::now();
    for _ in 0..n {
        std::hint::black_box(iv.props());
    }
    println!("props {:?}", t.elapsed() / n);
}
