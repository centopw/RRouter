#!/usr/bin/env python3
"""Live OAuth sign-in verification for RRouter.

Stands up a mock OAuth provider (device-code + PKCE token endpoints), points a
dynamic RRouter instance at it via the registry-independent device flow, and drives
the real endpoints the dashboard uses:

    POST /api/oauth/{provider}/start
    GET  /api/oauth/{provider}/status/{id}
    POST /api/oauth/{provider}/complete

Because the built-in providers point at real vendors, the flow is exercised here
with a *mock* provider registered through a test-only env override. The point is to
prove the full path — network exchange, credential persistence, encryption at rest —
against the real binary, not to sign in to a vendor account.
"""
import json
import os
import subprocess
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

RR = os.environ.get("RR", "target/release/rrouter")
DATA = "/tmp/rrouter-oauth"
UP = 21140
GW = 21141

device_polls = {"n": 0}

class Provider(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _json(self, status, obj):
        body = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        raw = self.rfile.read(n).decode()
        if self.path == "/device":
            return self._json(200, {
                "device_code": "DEV-LIVE",
                "user_code": "LIVE-CODE",
                "verification_uri": f"http://127.0.0.1:{UP}/verify",
                "expires_in": 900,
                "interval": 1,
            })
        if self.path == "/token":
            if "device_code=DEV-LIVE" in raw:
                device_polls["n"] += 1
                if device_polls["n"] < 2:
                    return self._json(200, {"error": "authorization_pending"})
                return self._json(200, {
                    "access_token": "LIVE-ACCESS-TOKEN",
                    "refresh_token": "LIVE-REFRESH-TOKEN",
                    "expires_in": 3600,
                })
            if "grant_type=authorization_code" in raw:
                return self._json(200, {
                    "access_token": "PKCE-ACCESS",
                    "refresh_token": "PKCE-REFRESH",
                    "expires_in": 3600,
                })
        return self._json(404, {"error": "not_found"})

def api(method, path, body=None):
    req = urllib.request.Request(
        f"http://127.0.0.1:{GW}/api{path}",
        data=None if body is None else json.dumps(body).encode(),
        headers={"content-type": "application/json"},
        method=method,
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")

def main():
    os.system(f"rm -rf {DATA} && mkdir -p {DATA}")
    srv = ThreadingHTTPServer(("127.0.0.1", UP), Provider)
    threading.Thread(target=srv.serve_forever, daemon=True).start()

    gw = subprocess.Popen([RR], env={
        **os.environ,
        "RROUTER_PORT": str(GW), "RROUTER_DATA_DIR": DATA, "RUST_LOG": "rrouter=warn",
        # Test-only registry override: point the github device flow at the mock.
        "RROUTER_OAUTH_OVERRIDE_GITHUB_DEVICE": f"http://127.0.0.1:{UP}/device",
        "RROUTER_OAUTH_OVERRIDE_GITHUB_TOKEN": f"http://127.0.0.1:{UP}/token",
    }, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    ok = True
    def check(name, cond, detail=""):
        nonlocal ok
        print(("PASS  " if cond else "FAIL  ") + name + (f"  [{detail}]" if detail else ""))
        ok = ok and cond

    try:
        for _ in range(100):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{GW}/health", timeout=1)
                break
            except Exception:
                time.sleep(0.05)

        # Unknown provider is rejected without side effects.
        st, body = api("POST", "/oauth/notreal/start")
        check("unknown provider -> 404", st == 404, str(body.get("error", {}).get("message", ""))[:60])

        # A provider with no login flow is refused.
        st, body = api("POST", "/oauth/openai/start")
        check("no login flow -> 400", st == 400, str(body.get("error", {}).get("message", ""))[:60])

        # Start the device flow.
        st, body = api("POST", "/oauth/github/start")
        if st != 200:
            check("device start", False, f"HTTP {st} {json.dumps(body)[:160]}")
        else:
            check("device start returns a user code", body.get("challenge", {}).get("user_code") == "LIVE-CODE", json.dumps(body.get("challenge"))[:80])
            check("start never returns a token", "access_token" not in json.dumps(body))
            login_id = body["id"]

            st, status = api("GET", f"/oauth/github/status/{login_id}")
            check("status reports pending", status.get("pending") is True, json.dumps(status))

            # Complete: the server polls until the mock hands out a token.
            st, done = api("POST", "/oauth/github/complete", {"id": login_id})
            check("complete returns a connection id", st == 200 and done.get("id"), f"HTTP {st}")
            check("complete never returns a token", "access_token" not in json.dumps(done), json.dumps(done)[:100])
            check("provider polled at least twice (pending then success)", device_polls["n"] >= 2, f"{device_polls['n']} polls")

            # Idempotent: completing again must not re-exchange.
            before = device_polls["n"]
            st2, done2 = api("POST", "/oauth/github/complete", {"id": login_id})
            check("complete is idempotent", st2 == 200 and device_polls["n"] == before,
                  f"{device_polls['n']} polls (was {before})")

            # The connection exists and never exposes tokens.
            st, conns = api("GET", "/connections")
            rows = [c for c in conns.get("data", []) if c.get("auth_type") == "oauth"]
            check("connection stored as oauth", len(rows) == 1, f"{len(rows)} oauth connections")
            if rows:
                row = rows[0]
                check("connection reports has_access_token", row.get("has_access_token") is True)
                check("connection view leaks no token", "LIVE-ACCESS-TOKEN" not in json.dumps(row))

        # Both secrets absent from the DB file on disk.
        db = open(f"{DATA}/rrouter.db", "rb").read()
        check("access token not in DB plaintext", b"LIVE-ACCESS-TOKEN" not in db)
        check("refresh token not in DB plaintext", b"LIVE-REFRESH-TOKEN" not in db)

        print("\nRESULT:", "ALL PASS" if ok else "FAILURES PRESENT")
        return 0 if ok else 1
    finally:
        gw.terminate()
        try:
            gw.wait(timeout=10)
        except Exception:
            gw.kill()
        srv.shutdown()

if __name__ == "__main__":
    sys.exit(main())
