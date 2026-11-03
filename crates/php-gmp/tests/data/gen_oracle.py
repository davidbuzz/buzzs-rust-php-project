"""Generates gmp_oracle.txt by calling the system libgmp through ctypes.

Run: python3 -I gen_oracle.py > gmp_oracle.txt   (from this directory)

Numbers are written in hexadecimal ("-" prefixed when negative) to keep the
file compact.  Each line: op|arg|arg|...|result
"""
import os
import random
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import libgmp as g  # noqa: E402

R = random.Random(20261103)


def h(v):
    return ("-" if v < 0 else "") + format(abs(v), "x")


def rnd(maxbits=600):
    kind = R.random()
    bits = R.choice([R.randint(0, 64), R.randint(0, 200), R.randint(0, maxbits)])
    if kind < 0.6:
        v = R.getrandbits(bits) if bits else 0
    elif kind < 0.7:
        v = (1 << bits) - 1
    elif kind < 0.8:
        v = (1 << bits) + R.choice([-1, 0, 1])
    elif kind < 0.9:
        v = sum(1 << R.randint(0, max(bits, 1)) for _ in range(R.randint(1, 4)))
    else:
        v = R.choice([0, 1, 2, 3, 7, 2**63 - 1, 2**63, 2**64 - 1, 2**64, 2**64 + 1])
    return -v if R.random() < 0.4 else v


def nz(maxbits=600):
    while True:
        v = rnd(maxbits)
        if v:
            return v


out = []


def emit(op, *vals):
    out.append("|".join([op] + [x if isinstance(x, str) else h(x) for x in vals]))


for _ in range(1500):
    a, b = rnd(), rnd()
    emit("add", a, b, a + b)
    emit("sub", a, b, a - b)
    emit("mul", a, b, a * b)
for _ in range(25):
    a, b = rnd(9000), rnd(9000)
    emit("mul", a, b, a * b)
for _ in range(1500):
    a, d = rnd(), nz(400)
    emit("tdiv", a, d, g.tdiv_q(a, d), g.tdiv_r(a, d))
    emit("cdiv", a, d, g.cdiv_q(a, d), g.cdiv_r(a, d))
    emit("fdiv", a, d, g.fdiv_q(a, d), g.fdiv_r(a, d))
    emit("mod", a, d, g.mod(a, d))
for _ in range(20):
    a, d = rnd(12000), nz(6000)
    emit("tdiv", a, d, g.tdiv_q(a, d), g.tdiv_r(a, d))
for _ in range(600):
    d = nz(300)
    q = rnd(300)
    emit("divexact", q * d, d, g.divexact(q * d, d))
    a = rnd(400)
    emit("divexact", a, d, g.divexact(a, d))
for _ in range(800):
    a, b = rnd(), rnd()
    emit("gcd", a, b, g.gcd(a, b))
    emit("lcm", a, b, g.lcm(a, b))
    gg, s, t = g.gcdext(a, b)
    emit("gcdext", a, b, gg, s, t)
    emit("and", a, b, g.and_(a, b))
    emit("or", a, b, g.ior(a, b))
    emit("xor", a, b, g.xor(a, b))
    emit("com", a, g.com(a))
    emit("hamdist", a, b, h(g.hamdist(a, b)))
    emit("popcount", a, h(g.popcount(a)))
for _ in range(300):
    c = R.randint(1, 1000)
    a, b = R.randint(-c, c), R.randint(-c, c)
    gg, s, t = g.gcdext(a, b)
    emit("gcdext", a, b, gg, s, t)
for _ in range(800):
    a, m = rnd(), nz(300)
    inv = g.invert(a, m)
    emit("invert", a, m, "none" if inv is None else h(inv))
    emit("jacobi", a, m, h(g.jacobi(a, m)))
for _ in range(300):
    a, m = R.randint(-50, 50), R.randint(-50, 50)
    emit("jacobi", a, m, h(g.jacobi(a, m)))
    if m:
        inv = g.invert(a, m)
        emit("invert", a, m, "none" if inv is None else h(inv))
for _ in range(600):
    a = rnd()
    i = R.randint(0, 700)
    emit("bits", a, i, h(g.tstbit(a, i)), h(g.scan0(a, i)), h(g.scan1(a, i)), g.setbit(a, i), g.clrbit(a, i))
    emit("shift", a, i, g.mul_2exp(a, i), g.fdiv_q_2exp(a, i))
    emit("getsi", a, h(g.get_si(a)))
    emit("getd", a, struct.pack(">d", g.get_d(a)).hex())
for _ in range(500):
    a = abs(rnd(1200))
    emit("sqrtrem", a, g.sqrt(a), a - g.sqrt(a) ** 2)
    n = R.choice([1, 2, 3, 4, 5, 7, 10, 33, 64, 65, 200])
    s = -1 if (n % 2 and R.random() < 0.5) else 1
    r, m = g.rootrem(s * a, n)
    emit("rootrem", s * a, h(n), r, m)
    emit("perfect", s * a, h(g.perfect_square(s * a)), h(g.perfect_power(s * a)))
for _ in range(300):
    b = R.randint(2, 50) * R.choice([-1, 1])
    e = R.choice([2, 3, 5, 7, 11, 13, 6, 9, 15])
    v = b ** e
    emit("perfect", v, h(g.perfect_square(v)), h(g.perfect_power(v)))
    emit("perfect", v + 1, h(g.perfect_square(v + 1)), h(g.perfect_power(v + 1)))
for _ in range(400):
    b, e, m = rnd(), abs(rnd(300)), nz(300)
    emit("powm", b, e, m, g.powm(b, e, m))
    if m % 2 and e > 0:
        emit("powm", b, e, m, g.powm_sec(b, e, m))
for _ in range(300):
    a = rnd(100)
    e = R.randint(0, 40)
    emit("pow", a, h(e), a ** e)
for n in list(range(0, 60)) + [100, 255, 1000, 2000]:
    emit("fact", h(n), g.fact(n))
for _ in range(300):
    n = rnd(100) if R.random() < 0.5 else R.randint(-60, 200)
    k = R.randint(0, 70)
    emit("binomial", n, h(k), g.binomial(n, k))
for _ in range(800):
    a = R.choice([R.randint(-2000, 3_000_000), rnd(80), rnd(200), g.nextprime(R.getrandbits(R.randint(20, 120)))])
    reps = R.randint(-5, 50)
    emit("prime", a, h(reps), h(g.probab_prime(a, reps)))
for _ in range(300):
    a = R.choice([R.randint(-100, 3_000_000), rnd(90)])
    emit("nextprime", a, g.nextprime(a))
    if a > 2:
        p, res = g.prevprime(a)
        emit("prevprime", a, p, h(res))
for k in range(18, 70):
    for v in (g.nextprime(1 << k), g.prevprime(1 << k)[0]):
        emit("prime", v, h(10), h(g.probab_prime(v, 10)))
        emit("prevprime", v + 2, *g.prevprime(v + 2)[:1], h(g.prevprime(v + 2)[1]))
for _ in range(600):
    a = rnd(800)
    base = R.choice(list(range(2, 63)) + list(range(-36, -1)))
    emit("getstr", a, h(base), g.Z(a).get_str(base).hex())
alphabet = "0123456789abcdefxXbBoOzZ -\t\n_+GgYy"
for _ in range(1500):
    s = "".join(R.choice(alphabet) for _ in range(R.randint(0, 12)))
    if R.random() < 0.3:
        s = R.choice(["0x", "0b", "0", "-0x", " -", "-", "00", "0X1f", "  12 34", "1\x0b2"]) + s
    base = R.choice([0, 0, 2, 8, 10, 16, 36, 37, 62, R.randint(2, 62)])
    rc, v = g.set_str(s.encode(), base)
    emit("setstr", s.encode().hex(), h(base), "err" if rc else h(v))
for _ in range(400):
    a = abs(rnd(300))
    size = R.choice([1, 2, 3, 4, 7, 8, 9, 16])
    order = R.choice([1, -1])
    endian = R.choice([1, -1, 0])
    data = g.export(a, order, size, endian)
    emit("export", a, h(size), h(order), h(endian), data.hex())
    raw = bytes(R.getrandbits(8) for _ in range(size * R.randint(0, 6)))
    emit("import", raw.hex(), h(size), h(order), h(endian), g.import_(raw, order, size, endian))
for seed in [0, -100, 100, 1, 2, -1, 12345678901234567890, -(1 << 512), 1 << 512, (1 << 19937) - 20027, 1 << 19937, R.getrandbits(30000)]:
    st = g.Rand(seed)
    vals = []
    for _ in range(12):
        if R.random() < 0.5:
            bits = R.choice([1, 10, 31, 32, 33, 63, 64, 65, 100, 1000])
            vals.append("b" + h(bits) + "=" + h(st.urandomb(bits)))
        else:
            n = R.choice([1, 2, 3, 10001, 20001, 1 << 64, (1 << 64) + 1, (1 << 200) - 1, 1 << 200])
            vals.append("m" + h(n) + "=" + h(st.urandomm(n)))
    emit("rand", seed, ",".join(vals))

print("\n".join(out))
