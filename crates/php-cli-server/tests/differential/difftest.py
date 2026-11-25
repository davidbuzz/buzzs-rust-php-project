#!/usr/bin/env python3
# buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
# Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
"""Differential test of the web SAPIs over sequences of requests.

Plays each scenario against our binary and against the C implementation and
compares every response: status line, header names/order/case/values, and the
body bytes. Only what must differ is normalised (see `normalise`).

Modes:
  server   `php -S` (one process serves every request of the scenario)
  cgi      `php-cgi` (one process per request, CGI environment, body on stdin)
  fcgi     `php-cgi -b` (one FastCGI process serves every request)

A scenario is a directory holding the document root's files plus
`scenario.json`:

  {
    "modes": ["server", "cgi", "fcgi"],      # default: ["server"]
    "args": ["-d", "precision=5"],           # extra php arguments
    "router": "router.php",                  # server mode only
    "cookies": true,                         # carry Set-Cookie to later requests
    "requests": [
      {"method": "GET", "path": "/a.php?x=1",
       "headers": [["Cookie", "a=b"]],
       "body": "text", "body_hex": "00ff",   # optional, one of them
       "raw": "GET / HTTP/1.0\\r\\n\\r\\n",  # server mode: sent verbatim
       "repeat": 3}                          # send this request n times
    ]
  }

Usage:
  difftest.py [--ours DIR] [--ref-php BIN] [--ref-cgi BIN] [-v] [scenario ...]

With no scenario arguments every directory under `scenarios/` runs.
Exit status 1 when any scenario differs. Every server started is killed
(its whole process group) when the script ends, also on failure.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_OURS = os.environ.get("DIFFTEST_OURS", "/tmp/409923/scratchpad/agent-bin")
CPU = os.environ.get("DIFFTEST_CPU", "12")

STARTED: list[subprocess.Popen] = []

# Ubuntu builds these as shared modules, which `-n` leaves out; ours has them
# built in. Loaded into the C side only.
REF_EXTENSIONS = ["mbstring", "ctype", "iconv", "tokenizer", "posix", "xml", "dom",
                  "simplexml", "xmlwriter", "xmlreader", "calendar", "bcmath", "gettext",
                  "fileinfo", "exif", "intl", "gmp", "bz2", "phar", "zip", "curl",
                  "sockets", "pdo", "pdo_sqlite", "sqlite3"]
REF_ARGS = [a for e in REF_EXTENSIONS for a in ("-d", f"extension={e}")]


def kill_all() -> None:
    for p in STARTED:
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        try:
            p.wait(timeout=2)
        except subprocess.TimeoutExpired:
            pass
    STARTED.clear()


def on_signal(signum, _frame):
    kill_all()
    sys.exit(128 + signum)


def free_port() -> int:
    for port in range(8501, 9500):
        with socket.socket() as s:
            try:
                s.bind(("127.0.0.1", port))
            except OSError:
                continue
            if port not in USED_PORTS:
                USED_PORTS.add(port)
                return port
    raise RuntimeError("no free port")


USED_PORTS: set[int] = set()


def spawn(cmd: list[str], cwd: Path, env=None, stdin=None) -> subprocess.Popen:
    p = subprocess.Popen(
        ["taskset", "-c", CPU, *cmd],
        cwd=cwd,
        stdin=stdin if stdin is not None else subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        env=env,
        start_new_session=True,
    )
    STARTED.append(p)
    return p


def wait_port(port: int, timeout: float = 5.0) -> None:
    end = time.time() + timeout
    while time.time() < end:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"server on {port} did not start")


# ---------------------------------------------------------------- HTTP client

def recv_all(sock: socket.socket, timeout: float = 5.0) -> bytes:
    sock.settimeout(timeout)
    chunks = []
    while True:
        try:
            b = sock.recv(65536)
        except socket.timeout:
            chunks.append(b"<<TIMEOUT>>")
            break
        except ConnectionResetError:
            chunks.append(b"<<RESET>>")
            break
        if not b:
            break
        chunks.append(b)
    return b"".join(chunks)


def body_of(req: dict) -> bytes:
    if "body_hex" in req:
        return bytes.fromhex(req["body_hex"])
    return req.get("body", "").encode("latin-1")


def build_http(req: dict, port: int, cookies: dict[str, str]) -> bytes:
    if "raw" in req:
        return req["raw"].replace("{PORT}", str(port)).encode("latin-1")
    method = req.get("method", "GET")
    path = req.get("path", "/")
    proto = req.get("protocol", "HTTP/1.1")
    headers = [list(h) for h in req.get("headers", [])]
    names = {h[0].lower() for h in headers}
    if "host" not in names:
        headers.insert(0, ["Host", f"127.0.0.1:{port}"])
    if cookies and "cookie" not in names:
        headers.append(["Cookie", "; ".join(f"{k}={v}" for k, v in cookies.items())])
    body = body_of(req)
    if (body or method in ("POST", "PUT")) and "content-length" not in names \
            and "transfer-encoding" not in names:
        headers.append(["Content-Length", str(len(body))])
    head = f"{method} {path} {proto}\r\n" + "".join(f"{k}: {v}\r\n" for k, v in headers) + "\r\n"
    return head.encode("latin-1") + body


def http_exchange(port: int, data: bytes) -> bytes:
    with socket.create_connection(("127.0.0.1", port), timeout=5) as s:
        s.sendall(data)
        return recv_all(s)


# ---------------------------------------------------------------- CGI client

def cgi_env(req: dict, docroot: Path, script_name_extra=None) -> tuple[dict, bytes]:
    method = req.get("method", "GET")
    path = req.get("path", "/")
    uri_path, _, query = path.partition("?")
    # Split the script from path info: the first component ending in .php.
    script, pinfo = uri_path, ""
    m = re.match(r"^(.*?\.php)(/.*)?$", uri_path)
    if m:
        script, pinfo = m.group(1), m.group(2) or ""
    body = body_of(req)
    env = {
        "PATH": "/usr/bin:/bin",
        "GATEWAY_INTERFACE": "CGI/1.1",
        "SERVER_PROTOCOL": req.get("protocol", "HTTP/1.1"),
        "SERVER_SOFTWARE": "difftest",
        "SERVER_NAME": "localhost",
        "SERVER_PORT": "80",
        "REMOTE_ADDR": "127.0.0.1",
        "REMOTE_PORT": "40000",
        "REQUEST_METHOD": method,
        "REQUEST_URI": path,
        "SCRIPT_NAME": script,
        "SCRIPT_FILENAME": str(docroot) + script,
        "DOCUMENT_ROOT": str(docroot),
        "QUERY_STRING": query,
        "REDIRECT_STATUS": "200",
    }
    if pinfo:
        env["PATH_INFO"] = pinfo
    if body or method in ("POST", "PUT"):
        env["CONTENT_LENGTH"] = str(len(body))
    for k, v in req.get("headers", []):
        kk = k.upper().replace("-", "_")
        if kk == "CONTENT_TYPE":
            env["CONTENT_TYPE"] = v
        elif kk == "CONTENT_LENGTH":
            env["CONTENT_LENGTH"] = v
        else:
            env["HTTP_" + kk] = v
    env.update(req.get("env", {}))
    return env, body


def cgi_exchange(binary: str, args: list[str], docroot: Path, req: dict,
                 cookies: dict[str, str]) -> bytes:
    req = dict(req)
    if cookies and not any(h[0].lower() == "cookie" for h in req.get("headers", [])):
        req["headers"] = [*req.get("headers", []),
                          ["Cookie", "; ".join(f"{k}={v}" for k, v in cookies.items())]]
    env, body = cgi_env(req, docroot)
    try:
        p = subprocess.run(
            ["taskset", "-c", CPU, binary, "-n", "-d", "memory_limit=1G", *args],
            input=body, env=env, cwd=docroot, capture_output=True, timeout=10,
            start_new_session=True,
        )
    except subprocess.TimeoutExpired:
        return b"<<TIMEOUT>>"
    return p.stdout + (f"\n<<exit {p.returncode}>>".encode() if p.returncode else b"")


# ---------------------------------------------------------------- FastCGI

def fcgi_record(rtype: int, rid: int, content: bytes) -> bytes:
    out = b""
    while True:
        part, content = content[:65535], content[65535:]
        pad = (8 - len(part) % 8) % 8
        out += bytes([1, rtype, rid >> 8, rid & 255, len(part) >> 8, len(part) & 255, pad, 0])
        out += part + b"\0" * pad
        if not content:
            return out


def fcgi_nv(k: bytes, v: bytes) -> bytes:
    def ln(n):
        return bytes([n]) if n < 128 else (n | 0x80000000).to_bytes(4, "big")
    return ln(len(k)) + ln(len(v)) + k + v


def fcgi_exchange(port: int, docroot: Path, req: dict, cookies: dict[str, str]) -> bytes:
    req = dict(req)
    if cookies and not any(h[0].lower() == "cookie" for h in req.get("headers", [])):
        req["headers"] = [*req.get("headers", []),
                          ["Cookie", "; ".join(f"{k}={v}" for k, v in cookies.items())]]
    env, body = cgi_env(req, docroot)
    env.pop("PATH", None)
    params = b"".join(fcgi_nv(k.encode(), v.encode("latin-1")) for k, v in env.items())
    data = fcgi_record(1, 1, bytes([0, 1, 0, 0, 0, 0, 0, 0]))  # BEGIN_REQUEST, responder
    data += fcgi_record(4, 1, params) + fcgi_record(4, 1, b"")
    if body:
        data += fcgi_record(5, 1, body)
    data += fcgi_record(5, 1, b"")
    out = b""
    err = b""
    with socket.create_connection(("127.0.0.1", port), timeout=5) as s:
        s.sendall(data)
        raw = recv_all(s)
    i = 0
    status = None
    while i + 8 <= len(raw):
        _, rtype, _, _, l1, l0, pad, _ = raw[i:i + 8]
        clen = l1 << 8 | l0
        content = raw[i + 8:i + 8 + clen]
        i += 8 + clen + pad
        if rtype == 6:
            out += content
        elif rtype == 7:
            err += content
        elif rtype == 3:
            status = int.from_bytes(content[:4], "big")
    if raw[i:]:
        out += b"<<TRAILING " + raw[i:] + b">>"
    if status:
        out += f"\n<<appStatus {status}>>".encode()
    return out


# ---------------------------------------------------------------- normalising

SESS_RE = re.compile(rb"(PHPSESSID[=\"]|\"sid\":\"|sid=|SID:)([A-Za-z0-9,-]{16,256})")
TMP_RE = re.compile(rb"/tmp/php[A-Za-z0-9]{6,}")
VER_RE = re.compile(rb"PHP/8\.\d+\.\d+(-dev)?")
DATE_RE = re.compile(rb"^(Date|Last-Modified|Expires): .*$", re.M | re.I)


class Normaliser:
    """Maps session ids and temporary names to their order of first appearance,
    so that whether an id repeats is still compared."""

    def __init__(self, port: int):
        self.port = str(port).encode()
        self.ids: dict[bytes, bytes] = {}
        self.tmps: dict[bytes, bytes] = {}

    def __call__(self, resp: bytes) -> bytes:
        resp = resp.replace(b":" + self.port, b":PORT")
        # A var_dump()ed string holding the version: its length goes with it.
        resp = re.sub(rb'string\(\d+\) "([^"\n]*PHP/8\.\d+\.\d+[^"\n]*)"',
                      lambda m: b'string(%d) "%s"' % (
                          len(VER_RE.sub(b"PHP/VER", m.group(1))),
                          VER_RE.sub(b"PHP/VER", m.group(1))), resp)
        resp = VER_RE.sub(b"PHP/VER", resp)
        # 8.5.12's server echoes the request's Host; master does not.
        resp = re.sub(rb"^Host: [^\r\n]*\r\n", b"", resp, flags=re.M)
        resp = DATE_RE.sub(lambda m: m.group(1) + b": DATE\r" if m.group(0).endswith(b"\r")
                           else m.group(1) + b": DATE", resp)

        def sid(m):
            v = m.group(2)
            if v not in self.ids:
                self.ids[v] = b"SID%d(len%d)" % (len(self.ids) + 1, len(v))
            return m.group(1) + self.ids[v]
        resp = SESS_RE.sub(sid, resp)
        # An id seen once (in a cookie) is the same id wherever it appears.
        for raw, name in self.ids.items():
            resp = resp.replace(raw, name)

        def tmp(m):
            v = m.group(0)
            if v not in self.tmps:
                self.tmps[v] = b"/tmp/phpTMP%d(len%d)" % (len(self.tmps) + 1, len(v))
            return self.tmps[v]
        return TMP_RE.sub(tmp, resp)


COOKIE_RE = re.compile(rb"^Set-Cookie: ([^=;\s]+)=([^;\r\n]*)", re.M | re.I)


def update_cookies(cookies: dict[str, str], resp: bytes) -> None:
    head = resp.split(b"\r\n\r\n", 1)[0]
    for m in COOKIE_RE.finditer(head):
        k, v = m.group(1).decode("latin-1"), m.group(2).decode("latin-1")
        if v in ("", "deleted"):
            cookies.pop(k, None)
        else:
            cookies[k] = v


def first_difference(a: bytes, b: bytes) -> str:
    al, bl = a.split(b"\n"), b.split(b"\n")
    for i in range(max(len(al), len(bl))):
        x = al[i] if i < len(al) else b"<<EOF>>"
        y = bl[i] if i < len(bl) else b"<<EOF>>"
        if x != y:
            return f"line {i + 1}:\n    C   : {x!r}\n    ours: {y!r}"
    return "identical"


# ---------------------------------------------------------------- scenarios

def expand(requests: list[dict]) -> list[dict]:
    out = []
    for r in requests:
        out.extend([r] * int(r.get("repeat", 1)))
    return out


class Side:
    def __init__(self, name: str, mode: str, php: str, cgi: str, sc: dict, docroot: Path):
        self.name, self.mode, self.sc, self.docroot = name, mode, sc, docroot
        self.php, self.cgi = php, cgi
        self.args = (REF_ARGS if name == "C" else []) + sc.get("args", [])
        self.port = None
        self.cookies: dict[str, str] = {}
        if mode == "server":
            self.port = free_port()
            cmd = [php, "-n", "-d", "memory_limit=1G", *self.args,
                   "-S", f"127.0.0.1:{self.port}", "-t", str(docroot)]
            if sc.get("router"):
                cmd.append(sc["router"])
            spawn(cmd, docroot, env={**os.environ, "PHP_CLI_SERVER_WORKERS": "1"})
            wait_port(self.port)
        elif mode == "fcgi":
            self.port = free_port()
            env = {"PATH": "/usr/bin:/bin", "PHP_FCGI_CHILDREN": "0",
                   "PHP_FCGI_MAX_REQUESTS": "1000"}
            spawn([cgi, "-n", "-d", "memory_limit=1G", *self.args, "-b",
                   f"127.0.0.1:{self.port}"], docroot, env=env)
            wait_port(self.port)
        self.norm = Normaliser(self.port or 0)

    def send(self, req: dict) -> bytes:
        if self.mode == "server":
            resp = http_exchange(self.port, build_http(req, self.port, self.cookies))
        elif self.mode == "cgi":
            resp = cgi_exchange(self.cgi, self.args, self.docroot, req, self.cookies)
        else:
            resp = fcgi_exchange(self.port, self.docroot, req, self.cookies)
        if self.sc.get("cookies"):
            update_cookies(self.cookies, resp)
        if self.mode == "server" and req.get("pause"):
            time.sleep(float(req["pause"]))
        return self.norm(resp)


def run_scenario(path: Path, opts) -> bool:
    sc = json.loads((path / "scenario.json").read_text())
    ok = True
    for mode in opts.modes or sc.get("modes", ["server"]):
        if mode not in sc.get("modes", ["server"]):
            continue
        # A fresh copy of the docroot per side, so files a script writes
        # cannot leak from one implementation to the other.
        with tempfile.TemporaryDirectory(prefix="difftest-") as tmp:
            # Both sides run, one after the other, in a fresh copy at the same
            # path: paths appear in output, files a script writes do not leak.
            root = Path(tmp) / "docroot"
            label = f"{path.name} [{mode}]"
            try:
                subprocess.run(["cp", "-a", str(path), str(root)], check=True)
                c = Side("C", mode, opts.ref_php, opts.ref_cgi, sc, root)
                c_out = [c.send(r) for r in expand(sc["requests"])]
                kill_all()
                subprocess.run(["rm", "-rf", str(root)], check=True)
                subprocess.run(["cp", "-a", str(path), str(root)], check=True)
                o = Side("ours", mode, opts.ours + "/php", opts.ours + "/php-cgi", sc, root)
                o_out = [o.send(r) for r in expand(sc["requests"])]
            finally:
                kill_all()
            for i, (a, b) in enumerate(zip(c_out, o_out)):
                if a != b:
                    ok = False
                    print(f"DIFF {label} request {i + 1}: {first_difference(a, b)}")
                    if opts.verbose:
                        print("---- C ----\n" + a.decode("latin-1"))
                        print("---- ours ----\n" + b.decode("latin-1"))
                    if not opts.every:
                        break
            if ok:
                print(f"same {label} ({len(c_out)} requests)")
    return ok


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--ours", default=DEFAULT_OURS, help="directory with php and php-cgi")
    ap.add_argument("--ref-php", default="/usr/bin/php8.5")
    ap.add_argument("--ref-cgi", default="/usr/bin/php-cgi8.5")
    ap.add_argument("--mode", dest="modes", action="append", choices=["server", "cgi", "fcgi"])
    ap.add_argument("-v", "--verbose", action="store_true")
    ap.add_argument("--every", action="store_true",
                    help="report every request that differs, not only the first")
    ap.add_argument("scenarios", nargs="*")
    opts = ap.parse_args()
    signal.signal(signal.SIGTERM, on_signal)
    signal.signal(signal.SIGINT, on_signal)
    paths = [Path(s) for s in opts.scenarios] or sorted(
        p for p in (HERE / "scenarios").iterdir() if (p / "scenario.json").exists())
    bad = 0
    try:
        for p in paths:
            if not run_scenario(p.resolve(), opts):
                bad += 1
    finally:
        kill_all()
    print(f"{len(paths) - bad}/{len(paths)} scenarios identical")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
