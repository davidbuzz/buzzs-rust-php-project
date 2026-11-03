"""Thin ctypes binding to the system libgmp, used as an oracle for php-gmp."""
import ctypes
import ctypes.util

_lib = ctypes.CDLL(ctypes.util.find_library("gmp") or "libgmp.so.10")
_libc = ctypes.CDLL(None)
_libc.free.argtypes = [ctypes.c_void_p]


class _Mpz(ctypes.Structure):
    _fields_ = [("alloc", ctypes.c_int), ("size", ctypes.c_int), ("d", ctypes.c_void_p)]


PZ = ctypes.POINTER(_Mpz)


def _f(name, res, *args):
    fn = getattr(_lib, "__g" + name)
    fn.restype = res
    fn.argtypes = list(args)
    return fn


UL = ctypes.c_ulong
SL = ctypes.c_long
_init = _f("mpz_init", None, PZ)
_set_str = _f("mpz_set_str", ctypes.c_int, PZ, ctypes.c_char_p, ctypes.c_int)
_get_str = _f("mpz_get_str", ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int, PZ)

VERSION = ctypes.c_char_p.in_dll(_lib, "__gmp_version").value.decode()


class Z:
    def __init__(self, v=0):
        self.z = _Mpz()
        _init(ctypes.byref(self.z))
        if isinstance(v, Z):
            v = int(v)
        s = str(int(v)).encode()
        assert _set_str(ctypes.byref(self.z), s, 10) == 0

    @property
    def p(self):
        return ctypes.byref(self.z)

    def __int__(self):
        ptr = _get_str(None, 10, self.p)
        s = ctypes.string_at(ptr).decode()
        _libc.free(ptr)
        return int(s)

    def get_str(self, base):
        ptr = _get_str(None, base, self.p)
        s = ctypes.string_at(ptr)
        _libc.free(ptr)
        return s


def set_str(s, base):
    z = Z(0)
    r = _set_str(z.p, s, base)
    return (r, int(z))


def _op3(name):
    fn = _f(name, None, PZ, PZ, PZ)

    def run(a, b):
        r = Z()
        fn(r.p, Z(a).p, Z(b).p)
        return int(r)
    return run


def _op2(name):
    fn = _f(name, None, PZ, PZ)

    def run(a):
        r = Z()
        fn(r.p, Z(a).p)
        return int(r)
    return run


def _pred(name, *extra):
    fn = _f(name, ctypes.c_int, PZ, *extra)

    def run(a, *e):
        return fn(Z(a).p, *e)
    return run


divexact = _op3("mpz_divexact")
gcd = _op3("mpz_gcd")
lcm = _op3("mpz_lcm")
and_ = _op3("mpz_and")
ior = _op3("mpz_ior")
xor = _op3("mpz_xor")
com = _op2("mpz_com")
sqrt = _op2("mpz_sqrt")
nextprime = _op2("mpz_nextprime")
perfect_power = _pred("mpz_perfect_power_p")
perfect_square = _pred("mpz_perfect_square_p")
probab_prime = _pred("mpz_probab_prime_p", ctypes.c_int)
_jacobi = _f("mpz_jacobi", ctypes.c_int, PZ, PZ)
_legendre = _f("mpz_legendre", ctypes.c_int, PZ, PZ)
_invert = _f("mpz_invert", ctypes.c_int, PZ, PZ, PZ)
_gcdext = _f("mpz_gcdext", None, PZ, PZ, PZ, PZ, PZ)
_prevprime = _f("mpz_prevprime", ctypes.c_int, PZ, PZ)
_get_d = _f("mpz_get_d", ctypes.c_double, PZ)
_get_si = _f("mpz_get_si", SL, PZ)
_root = _f("mpz_root", ctypes.c_int, PZ, PZ, UL)
_rootrem = _f("mpz_rootrem", None, PZ, PZ, PZ, UL)
_bin_ui = _f("mpz_bin_ui", None, PZ, PZ, UL)
_fac_ui = _f("mpz_fac_ui", None, PZ, UL)
_pow_ui = _f("mpz_pow_ui", None, PZ, PZ, UL)
_powm = _f("mpz_powm", None, PZ, PZ, PZ, PZ)
_popcount = _f("mpz_popcount", UL, PZ)
_hamdist = _f("mpz_hamdist", UL, PZ, PZ)
_scan0 = _f("mpz_scan0", UL, PZ, UL)
_scan1 = _f("mpz_scan1", UL, PZ, UL)
_sizeinbase = _f("mpz_sizeinbase", ctypes.c_size_t, PZ, ctypes.c_int)


def jacobi(a, b):
    return _jacobi(Z(a).p, Z(b).p)


def legendre(a, b):
    return _legendre(Z(a).p, Z(b).p)


def invert(a, b):
    r = Z()
    ok = _invert(r.p, Z(a).p, Z(b).p)
    return int(r) if ok else None


def gcdext(a, b):
    g, s, t = Z(), Z(), Z()
    _gcdext(g.p, s.p, t.p, Z(a).p, Z(b).p)
    return int(g), int(s), int(t)


def prevprime(a):
    r = Z()
    res = _prevprime(r.p, Z(a).p)
    return int(r), res


def get_d(a):
    return _get_d(Z(a).p)


def get_si(a):
    return _get_si(Z(a).p)


def root(a, n):
    r = Z()
    _root(r.p, Z(a).p, n)
    return int(r)


def rootrem(a, n):
    r, m = Z(), Z()
    _rootrem(r.p, m.p, Z(a).p, n)
    return int(r), int(m)


def binomial(n, k):
    r = Z()
    _bin_ui(r.p, Z(n).p, k)
    return int(r)


def fact(n):
    r = Z()
    _fac_ui(r.p, n)
    return int(r)


def powm(b, e, m):
    r = Z()
    _powm(r.p, Z(b).p, Z(e).p, Z(m).p)
    return int(r)


def popcount(a):
    return _popcount(Z(a).p)


def hamdist(a, b):
    return _hamdist(Z(a).p, Z(b).p)


def scan0(a, i):
    return _scan0(Z(a).p, i)


def scan1(a, i):
    return _scan1(Z(a).p, i)


def sizeinbase(a, b):
    return _sizeinbase(Z(a).p, b)


class _RandState(ctypes.Structure):
    _fields_ = [("seed", _Mpz), ("alg", ctypes.c_int), ("algdata", ctypes.c_void_p)]


_randinit_mt = _f("mp_randinit_mt", None, ctypes.POINTER(_RandState))
_randseed = _f("mp_randseed", None, ctypes.POINTER(_RandState), PZ)
_urandomb = _f("mpz_urandomb", None, PZ, ctypes.POINTER(_RandState), UL)
_urandomm = _f("mpz_urandomm", None, PZ, ctypes.POINTER(_RandState), PZ)


class Rand:
    def __init__(self, seed):
        self.st = _RandState()
        _randinit_mt(ctypes.byref(self.st))
        _randseed(ctypes.byref(self.st), Z(seed).p)

    def urandomb(self, bits):
        r = Z()
        _urandomb(r.p, ctypes.byref(self.st), bits)
        return int(r)

    def urandomm(self, n):
        r = Z()
        _urandomm(r.p, ctypes.byref(self.st), Z(n).p)
        return int(r)
