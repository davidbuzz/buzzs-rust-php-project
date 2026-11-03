#!/usr/bin/env python3
"""Generates tests/data/oracle.tsv by running random requests through the
libbcmath oracle (tools/oracle, built from php-src master's libbcmath).

usage: tools/gen_fixtures.py <path-to-bcoracle> [seed]

Each output line is `<request fields>\t=>\t<response fields>` (see
tools/oracle/oracle.c for the protocol).
"""
import random
import subprocess
import sys

MODES = ["HalfAwayFromZero", "HalfTowardsZero", "HalfEven", "HalfOdd",
         "TowardsZero", "AwayFromZero", "NegativeInfinity", "PositiveInfinity"]


def digits(rng, n, kind):
    if n == 0:
        return ""
    if kind == "nines":
        return "9" * n
    if kind == "zeros":
        return "0" * n
    if kind == "sparse":
        return "".join(rng.choice("0000000001") for _ in range(n))
    if kind == "five":
        return "".join(rng.choice("0459") for _ in range(n))
    return "".join(rng.choice("0123456789") for _ in range(n))


def number(rng, max_int=30, max_frac=30, allow_sign=True):
    r = rng.random()
    if r < 0.05:
        specials = ["0", "-0", "0.000", "-0.000", "1", "-1", "1.000", "10", "0.01", "-0.001",
                    "100", "1000000", "0.5", "-0.5", "2", ".5", "5.", "", "+3", "-", "."]
        if max_frac == 0:
            specials = [x for x in specials if "." not in x or x.endswith(".")]
        return rng.choice(specials)
    kind = rng.choice(["rand", "rand", "rand", "rand", "rand", "nines", "sparse", "five", "zeros"])
    il = rng.choice([0, 1, 1, 2, 3, 5, 8, 9, 10, 16, 17, rng.randint(0, max_int)])
    fl = min(max_frac, rng.choice([0, 0, 1, 2, 3, 5, 8, 9, 10, 17, rng.randint(0, max_frac)]))
    il = min(max_int, il)
    ip = digits(rng, il, kind)
    if rng.random() < 0.1:
        ip = "0" * rng.randint(1, 3) + ip
    fp = digits(rng, fl, kind)
    if fl and max_frac and rng.random() < 0.15:
        fp += "0" * rng.randint(1, 4)
    s = ip if not fp else (ip if ip else ("0" if rng.random() < 0.8 else "")) + "." + fp
    if s == "":
        s = "0"
    if allow_sign:
        r = rng.random()
        if r < 0.4:
            s = "-" + s
        elif r < 0.45:
            s = "+" + s
    return s


def invalid(rng):
    return rng.choice([" 1", "1 ", "1e5", "1.2.3", "abc", "--1", "1,5", "0x10", "1.5a", "+-1", "1 .5", "INF"])


def operand(rng, **kw):
    return invalid(rng) if rng.random() < 0.02 else number(rng, **kw)


def scale(rng):
    return str(rng.choice([0, 0, 1, 2, 3, 5, 10, 20, rng.randint(0, 50)]))


def gen(rng):
    reqs = []
    N = 700
    for op in ["add", "sub", "mul", "div", "mod", "divmod", "comp"]:
        for _ in range(N):
            reqs.append([op, operand(rng), operand(rng), scale(rng)])
    # Divisors near powers of ten and one.
    for _ in range(300):
        d = rng.choice(["1", "-1", "10", "0.1", "0.01", "100", "1000.000", "-0.001", "1.0", "10000000000"])
        reqs.append([rng.choice(["div", "mod", "divmod"]), operand(rng), d, scale(rng)])
    for _ in range(N):
        base = operand(rng, max_int=6, max_frac=6)
        e = str(rng.choice([rng.randint(-12, 40), rng.randint(-3, 3), rng.randint(40, 120)]))
        if rng.random() < 0.05:
            e = rng.choice(["1.5", "2.0", "-0", "9223372036854775808", "-9223372036854775807", "abc"])
        reqs.append(["pow", base, e, scale(rng)])
    for _ in range(N):
        b = number(rng, max_int=25, max_frac=0)
        e = str(rng.randint(0, 10 ** rng.randint(0, 12)))
        m = number(rng, max_int=20, max_frac=0)
        r = rng.random()
        if r < 0.03:
            b += ".5"
        elif r < 0.06:
            e = "-" + e if e != "0" else "-3"
        elif r < 0.09:
            m = rng.choice(["0", "1", "-1", "0.000", "2.5"])
        reqs.append(["powmod", b, e, m, scale(rng)])
    for _ in range(N):
        reqs.append(["sqrt", operand(rng, max_int=40, max_frac=40), scale(rng)])
    for op in ["floor", "ceil"]:
        for _ in range(300):
            reqs.append([op, operand(rng)])
    for _ in range(1500):
        prec = str(rng.choice([rng.randint(-12, 12), rng.randint(-40, 40), 0]))
        reqs.append(["round", operand(rng, max_int=20, max_frac=20), prec, rng.choice(MODES)])

    # BcMath\Number methods ("-" = null scale).
    def nscale():
        return "-" if rng.random() < 0.6 else scale(rng)

    for op in ["nadd", "nsub", "nmul", "ndiv", "nmod", "ndivmod", "ncomp"]:
        for _ in range(N // 2):
            reqs.append([op, number(rng), operand(rng), nscale()])
    for _ in range(N // 2):
        e = str(rng.choice([rng.randint(-12, 30), rng.randint(-3, 3)]))
        if rng.random() < 0.05:
            e = rng.choice(["1.5", "9223372036854775808"])
        reqs.append(["npow", number(rng, max_int=5, max_frac=5), e, nscale()])
    for _ in range(N // 3):
        reqs.append(["npowmod", number(rng, max_int=20, max_frac=0), str(rng.randint(0, 10 ** 8)),
                     number(rng, max_int=12, max_frac=0), nscale()])
    for _ in range(N // 2):
        reqs.append(["nsqrt", number(rng, max_int=30, max_frac=30), nscale()])
    for _ in range(N // 2):
        reqs.append(["nround", number(rng, max_int=15, max_frac=15), str(rng.randint(-15, 15)), rng.choice(MODES)])
    for op in ["nfloor", "nceil"]:
        for _ in range(100):
            reqs.append([op, number(rng)])
    for _ in range(200):
        reqs.append(["nnew", operand(rng)])
    return reqs


def main():
    oracle = sys.argv[1]
    seed = int(sys.argv[2]) if len(sys.argv) > 2 else 20241103
    rng = random.Random(seed)
    reqs = gen(rng)
    data = "".join("\t".join(r) + "\n" for r in reqs)
    out = subprocess.run([oracle], input=data.encode(), capture_output=True, check=True).stdout.decode()
    lines = out.split("\n")[:-1]
    assert len(lines) == len(reqs), (len(lines), len(reqs))
    with open("tests/data/oracle.tsv", "w") as f:
        for r, l in zip(reqs, lines):
            f.write("\t".join(r) + "\t=>\t" + l + "\n")
    print(f"{len(reqs)} cases")


if __name__ == "__main__":
    main()
