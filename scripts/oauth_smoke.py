#!/usr/bin/env python3
"""Exercise both OAuth flows through the real release binary."""
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ACCESS = "LIVE-ACCESS-TOKEN"
REFRESH = "LIVE-REFRESH-TOKEN"
PKCE_ACCESS = "PKCE-ACCESS-TOKEN"
PKCE_REFRESH = "PKCE-REFRESH-TOKEN"
ID_TOKEN = "eyJhbGciOiJub25lIn0.eyJlbWFpbCI6InRlc3RAZXhhbXBsZS5jb20ifQ."
RR = os.environ.get("RR", "target/release/rrouter")


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class Provider(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    polls = 0

    def log_message(self, *_args):
        pass

    def _json(self, status, value):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _text(self, status, text):
        body = text.encode()
        self.send_response(status)
        self.send_header("content-type", "text/plain")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/verify" or self.path == "/authorize":
            return self._text(200, "mock authorization page")
        if self.path == "/user":
            return self._json(200, {"login": "live-user"})
        if self.path == "/copilot_internal/v2/token":
            return self._json(200, {"token": "LIVE-COPILOT-TOKEN", "expires_at": 4102444800})
        return self._json(404, {"error": "not_found"})

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        raw = self.rfile.read(length).decode()
        form = urllib.parse.parse_qs(raw)
        if self.path == "/device":
            return self._json(200, {
                "device_code": "LIVE-DEVICE-CODE",
                "user_code": "LIVE-USER-CODE",
                "verification_uri": f"http://127.0.0.1:{self.server.server_port}/verify",
                "expires_in": 900,
                "interval": 1,
            })
        if self.path == "/token":
            if form.get("device_code") == ["LIVE-DEVICE-CODE"]:
                type(self).polls += 1
                if type(self).polls == 1:
                    return self._json(200, {"error": "authorization_pending"})
                return self._json(200, {
                    "access_token": ACCESS,
                    "refresh_token": REFRESH,
                    "id_token": ID_TOKEN,
                    "expires_in": 3600,
                })
            if form.get("grant_type") == ["authorization_code"] or "authorization_code" in raw:
                return self._json(200, {
                    "access_token": PKCE_ACCESS,
                    "refresh_token": PKCE_REFRESH,
                    "id_token": ID_TOKEN,
                    "expires_in": 3600,
                })
        return self._json(404, {"error": "not_found"})


def api(port, method, path, body=None):
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/api{path}",
        data=None if body is None else json.dumps(body).encode(),
        headers={"content-type": "application/json"},
        method=method,
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.loads(response.read() or b"{}")
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"{}")
    except urllib.error.URLError as error:
        return 0, {"error": {"message": str(error)}}


def main():
    gateway_port = free_port()
    provider = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    provider_port = provider.server_port
    provider_thread = threading.Thread(target=provider.serve_forever, daemon=True)
    provider_thread.start()

    ok = True

    def check(name, condition, detail=""):
        nonlocal ok
        print(("PASS  " if condition else "FAIL  ") + name + (f"  [{detail}]" if detail else ""))
        ok = ok and bool(condition)

    with tempfile.TemporaryDirectory(prefix="rrouter-oauth-") as data_dir:
        env = {
            **os.environ,
            "RROUTER_PORT": str(gateway_port),
            "RROUTER_DATA_DIR": data_dir,
            "RROUTER_ALLOW_INSECURE_OAUTH": "1",
            "RROUTER_OAUTH_OVERRIDE_GITHUB_DEVICE": f"http://127.0.0.1:{provider_port}/device",
            "RROUTER_OAUTH_OVERRIDE_GITHUB_TOKEN": f"http://127.0.0.1:{provider_port}/token",
            "RROUTER_OAUTH_OVERRIDE_CLAUDE_AUTHORIZE": f"http://127.0.0.1:{provider_port}/authorize",
            "RROUTER_OAUTH_OVERRIDE_CLAUDE_TOKEN": f"http://127.0.0.1:{provider_port}/token",
        }
        gateway = None
        try:
            gateway = subprocess.Popen(
                [RR],
                env=env,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            ready = False
            for _ in range(120):
                if gateway.poll() is not None:
                    break
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{gateway_port}/health", timeout=1):
                        ready = True
                        break
                except (OSError, urllib.error.URLError):
                    time.sleep(0.1)
            check("release gateway starts", ready)

            device_status, device = api(gateway_port, "POST", "/oauth/github/start")
            device_challenge = device.get("challenge", {})
            check("device start succeeds", device_status == 200, f"HTTP {device_status}")
            check("device challenge has user code", device_challenge.get("user_code") == "LIVE-USER-CODE")
            check("device start never returns a token", "access_token" not in json.dumps(device))
            device_id = device.get("id")
            if device_id:
                status_code, status = api(gateway_port, "GET", f"/oauth/github/status/{device_id}")
                check("device status is pending", status_code == 200 and status.get("pending") is True)
                complete_code, complete = api(
                    gateway_port, "POST", "/oauth/github/complete", {"id": device_id}
                )
                check("device complete returns a connection id", complete_code == 200 and bool(complete.get("id")))
                check("device complete never returns a token", "access_token" not in json.dumps(complete))
                check("device provider polled pending then success", Provider.polls >= 2, f"{Provider.polls} polls")

                _, connections = api(gateway_port, "GET", "/connections")
                rows = [row for row in connections.get("data", []) if row.get("auth_type") == "oauth"]
                check("device connection is stored", len(rows) == 1, f"{len(rows)} oauth rows")
                if rows:
                    check("device connection has access token", rows[0].get("has_access_token") is True)
                    check("device connection view hides token", ACCESS not in json.dumps(rows[0]))

                before = Provider.polls
                second_code, _ = api(
                    gateway_port, "POST", "/oauth/github/complete", {"id": device_id}
                )
                check("second device complete is idempotent", second_code == 200 and Provider.polls == before)

            pkce_status, pkce = api(gateway_port, "POST", "/oauth/claude/start")
            challenge = pkce.get("challenge", {})
            authorize_url = challenge.get("authorize_url")
            redirect_uri = challenge.get("redirect_uri")
            check("PKCE start succeeds", pkce_status == 200 and bool(authorize_url))
            check("PKCE challenge has redirect URI", bool(redirect_uri))
            if authorize_url and redirect_uri and pkce.get("id"):
                query = urllib.parse.parse_qs(urllib.parse.urlparse(authorize_url).query)
                state = query.get("state", [""])[0]
                check("PKCE authorize URL has state", bool(state))
                result = {}

                def complete_pkce():
                    result["response"] = api(
                        gateway_port, "POST", "/oauth/claude/complete", {"id": pkce["id"]}
                    )

                worker = threading.Thread(target=complete_pkce)
                worker.start()
                callback = f"{redirect_uri}?code=PKCE-CODE&state={urllib.parse.quote(state)}"
                with urllib.request.urlopen(callback, timeout=30) as response:
                    check("PKCE loopback redirect succeeds", response.status == 200)
                worker.join(timeout=30)
                complete_result = result.get("response", (0, {}))
                check("PKCE complete stores a connection", complete_result[0] == 200 and bool(complete_result[1].get("id")))

            negative_status, negative = api(gateway_port, "POST", "/oauth/claude/start")
            negative_redirect = negative.get("challenge", {}).get("redirect_uri")
            _, before_negative = api(gateway_port, "GET", "/connections")
            before_negative_count = len(before_negative.get("data", []))
            if negative_status == 200 and negative_redirect and negative.get("id"):
                result = {}

                def complete_negative():
                    result["response"] = api(
                        gateway_port, "POST", "/oauth/claude/complete", {"id": negative["id"]}
                    )

                worker = threading.Thread(target=complete_negative)
                worker.start()
                bad_callback = f"{negative_redirect}?code=PKCE-CODE&state=wrong-state"
                try:
                    urllib.request.urlopen(bad_callback, timeout=30)
                    callback_status = 200
                except urllib.error.HTTPError as error:
                    callback_status = error.code
                worker.join(timeout=30)
                complete_result = result.get("response", (0, {}))
                check("wrong PKCE state returns loopback 400", callback_status == 400)
                check("wrong PKCE state fails complete", complete_result[0] >= 400)
                _, after_negative = api(gateway_port, "GET", "/connections")
                check(
                    "wrong PKCE state stores no connection",
                    len(after_negative.get("data", [])) == before_negative_count,
                )

            db_path = os.path.join(data_dir, "rrouter.db")
            try:
                db_bytes = open(db_path, "rb").read()
                check("access token absent from encrypted DB", ACCESS.encode() not in db_bytes and PKCE_ACCESS.encode() not in db_bytes)
                check("refresh token absent from encrypted DB", REFRESH.encode() not in db_bytes and PKCE_REFRESH.encode() not in db_bytes)
            except OSError as error:
                check("encrypted DB exists", False, str(error))
        except Exception as error:
            check("smoke harness completed without exception", False, repr(error))
        finally:
            if gateway is not None:
                gateway.terminate()
                try:
                    gateway.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    gateway.kill()
                    gateway.wait()
            provider.shutdown()
            provider.server_close()

    print("\nRESULT:", "ALL PASS" if ok else "FAILURES PRESENT")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
