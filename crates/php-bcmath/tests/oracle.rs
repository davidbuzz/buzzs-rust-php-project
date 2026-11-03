//! Replays `tests/data/oracle.tsv`: random requests answered by php-src
//! master's libbcmath (see `tools/oracle` and `tools/gen_fixtures.py`).

use php_bcmath::{BcError, BcMath, ErrorKind, Number, Operand, RoundingMode};

/// Renders an error in the oracle's `ERR\t<kind>\t<message>` format.
fn render_err(e: &BcError) -> String {
    match e.kind {
        ErrorKind::DivisionByZeroError => format!("ERR\tZ\t{}", e.message),
        ErrorKind::ValueError => {
            // "func(): Argument #N ($name) msg" -> A<N> msg
            if let Some(rest) = e.message.split_once("(): Argument #").map(|(_, r)| r) {
                let (num, rest) = rest.split_once(' ').expect("argument number");
                let msg = rest.split_once(") ").expect("argument name").1;
                format!("ERR\tA{num}\t{msg}")
            } else {
                format!("ERR\tV\t{}", e.message)
            }
        }
        ErrorKind::OutOfMemory => "ERR\tM\tOOM".to_string(),
        _ => format!("ERR\t?\t{}", e.message),
    }
}

fn ok(s: impl std::fmt::Display) -> String {
    format!("OK\t{s}")
}

fn number_ok(n: &Number) -> String {
    format!("OK\t{}\t{}", n.value(), n.scale())
}

fn scale_arg(s: &str) -> Option<i64> {
    if s == "-" {
        None
    } else {
        Some(s.parse().expect("scale"))
    }
}

fn run(bc: BcMath, req: &[&str]) -> String {
    let b = |i: usize| req[i].as_bytes();
    let sc = |i: usize| Some(req[i].parse::<i64>().expect("scale"));
    let res: Result<String, BcError> = match req[0] {
        "add" => bc.bcadd(b(1), b(2), sc(3)).map(ok),
        "sub" => bc.bcsub(b(1), b(2), sc(3)).map(ok),
        "mul" => bc.bcmul(b(1), b(2), sc(3)).map(ok),
        "div" => bc.bcdiv(b(1), b(2), sc(3)).map(ok),
        "mod" => bc.bcmod(b(1), b(2), sc(3)).map(ok),
        "divmod" => bc
            .bcdivmod(b(1), b(2), sc(3))
            .map(|(q, r)| format!("OK\t{q}\t{r}")),
        "comp" => bc.bccomp(b(1), b(2), sc(3)).map(ok),
        "pow" => bc.bcpow(b(1), b(2), sc(3)).map(ok),
        "powmod" => bc.bcpowmod(b(1), b(2), b(3), sc(4)).map(ok),
        "sqrt" => bc.bcsqrt(b(1), sc(2)).map(ok),
        "floor" => bc.bcfloor(b(1)).map(ok),
        "ceil" => bc.bcceil(b(1)).map(ok),
        "round" => bc
            .bcround(
                b(1),
                req[2].parse().expect("precision"),
                RoundingMode::from_name(req[3]).expect("mode"),
            )
            .map(ok),
        "nnew" => Number::new(Operand::Str(b(1))).map(|n| number_ok(&n)),
        op => {
            let this = Number::new(Operand::Str(b(1))).expect("receiver");
            match op {
                "nadd" => this
                    .add(Operand::Str(b(2)), scale_arg(req[3]))
                    .map(|n| number_ok(&n)),
                "nsub" => this
                    .sub(Operand::Str(b(2)), scale_arg(req[3]))
                    .map(|n| number_ok(&n)),
                "nmul" => this
                    .mul(Operand::Str(b(2)), scale_arg(req[3]))
                    .map(|n| number_ok(&n)),
                "ndiv" => this
                    .div(Operand::Str(b(2)), scale_arg(req[3]))
                    .map(|n| number_ok(&n)),
                "nmod" => this
                    .modulo(Operand::Str(b(2)), scale_arg(req[3]))
                    .map(|n| number_ok(&n)),
                "npow" => this
                    .pow(Operand::Str(b(2)), scale_arg(req[3]))
                    .map(|n| number_ok(&n)),
                "ndivmod" => this
                    .divmod(Operand::Str(b(2)), scale_arg(req[3]))
                    .map(|(q, r)| {
                        format!(
                            "OK\t{}\t{}\t{}\t{}",
                            q.value(),
                            q.scale(),
                            r.value(),
                            r.scale()
                        )
                    }),
                "ncomp" => this.compare(Operand::Str(b(2)), scale_arg(req[3])).map(ok),
                "npowmod" => this
                    .powmod(Operand::Str(b(2)), Operand::Str(b(3)), scale_arg(req[4]))
                    .map(|n| number_ok(&n)),
                "nsqrt" => this.sqrt(scale_arg(req[2])).map(|n| number_ok(&n)),
                "nround" => this
                    .round(
                        req[2].parse().expect("precision"),
                        RoundingMode::from_name(req[3]).expect("mode"),
                    )
                    .map(|n| number_ok(&n)),
                "nfloor" => Ok(number_ok(&this.floor())),
                "nceil" => Ok(number_ok(&this.ceil())),
                _ => panic!("unknown op {op}"),
            }
        }
    };
    res.unwrap_or_else(|e| render_err(&e))
}

#[test]
fn libbcmath_oracle() {
    // BCMATH_ORACLE_FILE replays an alternative (e.g. larger) fixture.
    let data = std::env::var("BCMATH_ORACLE_FILE").map_or_else(
        |_| include_str!("data/oracle.tsv").to_string(),
        |p| std::fs::read_to_string(p).expect("fixture"),
    );
    let bc = BcMath::new();
    let mut failures = Vec::new();
    let mut total = 0;
    for line in data.lines() {
        let (req, expected) = line.split_once("\t=>\t").expect("separator");
        let req: Vec<&str> = req.split('\t').collect();
        total += 1;
        let got = run(bc, &req);
        if got != expected {
            failures.push(format!(
                "{}\n  expected: {expected}\n  got:      {got}",
                req.join(" | ")
            ));
        }
    }
    if std::env::var_os("BCMATH_ORACLE_FILE").is_none() {
        assert!(total > 10_000, "fixture too small: {total}");
    }
    assert!(
        failures.is_empty(),
        "{} of {total} oracle cases failed:\n{}",
        failures.len(),
        failures
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
