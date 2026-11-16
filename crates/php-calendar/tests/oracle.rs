// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Compares every function against outputs recorded from the C implementation
//! (`tests/data/gen_oracle.php`, PHP 8.3 with ext/calendar).

use php_calendar as cal;
use std::fmt::Write as _;

fn enc_str(s: &[u8]) -> String {
    if s.iter()
        .all(|b| (b' '..=b'~').contains(b) && *b != b'|' && *b != b';')
    {
        format!("s:{}", String::from_utf8_lossy(s))
    } else {
        let mut o = String::from("h:");
        for b in s {
            let _ = write!(o, "{b:02x}");
        }
        o
    }
}

fn enc_res<T>(r: Result<T, cal::CalError>, func: &str, f: impl FnOnce(T) -> String) -> String {
    match r {
        Ok(v) => f(v),
        Err(e) => format!("E:{}", e.message(func)),
    }
}

fn enc_info(i: &cal::CalInfo) -> String {
    let list = |v: &[(i64, &str)]| {
        v.iter()
            .map(|(k, n)| format!("{k}={}", enc_str(n.as_bytes())))
            .collect::<Vec<_>>()
            .join(";")
    };
    format!(
        "months=a:{};abbrevmonths=a:{};maxdaysinmonth=i:{};calname={};calsymbol={}",
        list(&i.months),
        list(&i.abbrevmonths),
        i.maxdaysinmonth,
        enc_str(i.calname.as_bytes()),
        enc_str(i.calsymbol.as_bytes())
    )
}

fn run(func: &str, a: &[i64]) -> String {
    let int = |v: i64| format!("i:{v}");
    let s = |v: &str| enc_str(v.as_bytes());
    match func {
        "jdtogregorian" => s(&cal::jdtogregorian(a[0])),
        "jdtojulian" => s(&cal::jdtojulian(a[0])),
        "jdtofrench" => s(&cal::jdtofrench(a[0])),
        "jdtojewish" => enc_res(
            cal::jdtojewish(
                a[0],
                a.get(1).is_some_and(|v| *v != 0),
                a.get(2).copied().unwrap_or(0),
            ),
            func,
            |v| enc_str(&v),
        ),
        "jddayofweek" => match cal::jddayofweek(a[0], a[1]) {
            cal::DayOfWeek::Number(n) => int(n),
            cal::DayOfWeek::Name(n) => s(n),
        },
        "jdmonthname" => s(cal::jdmonthname(a[0], a[1])),
        "cal_from_jd" => enc_res(cal::cal_from_jd(a[0], a[1]), func, |r| {
            format!(
                "a:date={};month=i:{};day=i:{};year=i:{};dow={};abbrevdayname={};dayname={};abbrevmonth={};monthname={}",
                s(&r.date),
                r.month,
                r.day,
                r.year,
                r.dow.map_or_else(|| "n".to_string(), int),
                s(r.abbrevdayname),
                s(r.dayname),
                s(r.abbrevmonth),
                s(r.monthname)
            )
        }),
        "gregoriantojd" => int(cal::gregoriantojd(a[0], a[1], a[2])),
        "juliantojd" => int(cal::juliantojd(a[0], a[1], a[2])),
        "frenchtojd" => int(cal::frenchtojd(a[0], a[1], a[2])),
        "jewishtojd" => enc_res(cal::jewishtojd(a[0], a[1], a[2]), func, int),
        "cal_to_jd" => enc_res(cal::cal_to_jd(a[0], a[1], a[2], a[3]), func, int),
        "cal_days_in_month" => enc_res(cal::cal_days_in_month(a[0], a[1], a[2]), func, int),
        "easter_days" => enc_res(cal::easter_days(a[0], a[1]), func, int),
        "jdtounix" => enc_res(cal::jdtounix(a[0]), func, int),
        "cal_info" => enc_res(cal::cal_info(a[0]), func, |v| {
            if a[0] == -1 {
                let parts: Vec<String> = v
                    .iter()
                    .enumerate()
                    .map(|(i, x)| format!("{i}=a:{}", enc_info(x)))
                    .collect();
                format!("a:{}", parts.join(";"))
            } else {
                format!("a:{}", enc_info(&v[0]))
            }
        }),
        other => panic!("unknown function {other}"),
    }
}

#[test]
fn matches_php_oracle() {
    let data = include_str!("data/calendar_oracle.txt");
    let mut failures = Vec::new();
    let mut count = 0;
    for line in data.lines() {
        let mut parts = line.splitn(3, '|');
        let (Some(func), Some(args), Some(expected)) = (parts.next(), parts.next(), parts.next())
        else {
            panic!("bad oracle line {line}");
        };
        let args: Vec<i64> = args
            .split(',')
            .map(|x| x.parse().expect("int arg"))
            .collect();
        let got = run(func, &args);
        count += 1;
        if got != expected {
            failures.push(format!("{func}({args:?}): expected {expected}, got {got}"));
        }
    }
    assert!(count > 40_000, "oracle too small: {count}");
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
