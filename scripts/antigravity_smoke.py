#!/usr/bin/env python3
"""Offline Cloud Code Assist (Antigravity) acceptance harness.

Starts the real gateway binary against a local mock of every external surface:

* Google's OAuth token endpoint (authorization-code exchange and refresh),
* `loadCodeAssist` and `onboardUser` on the daily/production CCA hosts,
* `generateContent` and `streamGenerateContent?alt=sse`.

No Google account, no real network, no token is written to the repository. The gateway is
pointed at the mock with the documented overrides
(`RROUTER_OAUTH_OVERRIDE_GOOGLE_ANTIGRAVITY_{AUTHORIZE,TOKEN}`,
`RROUTER_ANTIGRAVITY_{PROD,DAILY}_HOST`), and OAuth is driven through the real
`/api/oauth/google-antigravity/*` endpoints and a dynamic loopback callback.

This is deliberately separate from a live OAuth run: a live sign-in needs a real Google
account and is a manual checklist item in `docs/PROVIDERS.md`.

Usage:
  RR=target/release/rrouter python3 scripts/antigravity_smoke.py
  python3 scripts/antigravity_smoke.py --check-backend   # only probe the routes

Exit codes: 0 all checks pass, 1 a check or setup failed, 3 the backend has not landed
(the `google-antigravity` OAuth routes/provider are absent).
"""

from __future__ import annotations

import argparse
import base64
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

PROVIDER = "google-antigravity"
ACCESS_PREFIX = "offline-access"
REFRESH_PREFIX = "offline-refresh"
MODEL = "gemini-3-pro"
MODEL_REF = f"{PROVIDER}/{MODEL}"
RR = os.environ.get("RR", "target/release/rrouter")


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def id_token(email: str) -> str:
    payload = base64.urlsafe_b64encode(json.dumps({"email": email}).encode()).rstrip(b"=")
    return f"eyJhbGciOiJub25lIn0.{payload.decode()}.signature"


class MockState:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.flows = 0
        self.plans: dict[str, str] = {}
        self.exchanges: list[dict[str, str]] = []
        self.refreshes: dict[str, int] = {}
        self.load_calls: list[dict] = []
        self.onboard_calls: list[dict] = []
        self.cca_calls: list[dict] = []
        self.onboard_polls: dict[str, int] = {}

    def next_flow(self, plan: str) -> tuple[str, str]:
        with self.lock:
            self.flows += 1
            number = self.flows
            access = f"{ACCESS_PREFIX}-{number}"
            refresh = f"{REFRESH_PREFIX}-{number}"
            self.plans[access] = plan
            return access, refresh


# The order matters: the script drives one sign-in per plan in this sequence.
PLANS = ["load-a", "load-b", "load-c", "onboard", "missing"]


class Mock(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):  # silence
        pass

    @property
    def state(self) -> MockState:
        return self.server.state  # type: ignore[attr-defined]

    def _json(self, status: int, value) -> None:
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _sse(self, frames: list[dict]) -> None:
        body = "".join(f"data: {json.dumps(frame)}\n\n" for frame in frames).encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _body(self) -> bytes:
        return self.rfile.read(int(self.headers.get("content-length", "0")))

    def _json_body(self):
        raw = self._body()
        try:
            return json.loads(raw or b"{}")
        except json.JSONDecodeError:
            return {}

    def do_GET(self):
        if self.path in ("/authorize", "/callback"):
            return self._json(200, {"ok": True})
        self._json(404, {"error": "not_found"})

    def do_POST(self):
        path = urllib.parse.urlparse(self.path).path
        if path == "/token":
            return self._token()
        if path.endswith("/v1internal:loadCodeAssist"):
            return self._load()
        if path.endswith("/v1internal:onboardUser"):
            return self._onboard()
        if path.endswith("/v1internal:generateContent"):
            return self._generate()
        if path.endswith("/v1internal:streamGenerateContent"):
            return self._stream()
        self._json(404, {"error": "not_found"})

    # ----- OAuth -------------------------------------------------------------
    def _token(self) -> None:
        form = urllib.parse.parse_qs(self._body().decode())
        grant = form.get("grant_type", [""])[0]
        if grant == "refresh_token":
            refresh = form.get("refresh_token", [""])[0]
            with self.state.lock:
                number = self.state.refreshes.get(refresh, 0) + 1
                self.state.refreshes[refresh] = number
                access = f"{ACCESS_PREFIX}-refresh-{refresh}"
            return self._json(
                200,
                {"access_token": access, "expires_in": 3600, "token_type": "Bearer"},
            )
        if grant != "authorization_code":
            return self._json(400, {"error": "unsupported_grant_type"})
        plan = PLANS[min(self.state.flows, len(PLANS) - 1)]
        access, refresh = self.state.next_flow(plan)
        with self.state.lock:
            self.state.exchanges.append({key: values[0] for key, values in form.items()})
        return self._json(
            200,
            {
                "access_token": access,
                "refresh_token": refresh,
                "id_token": id_token(f"antigravity-{plan}@example.test"),
                "expires_in": 1,
                "token_type": "Bearer",
            },
        )

    # ----- project discovery ------------------------------------------------
    def _plan(self) -> str:
        token = self.headers.get("authorization", "").removeprefix("Bearer ").strip()
        return self.state.plans.get(token, "missing")

    def _load(self) -> None:
        body = self._json_body()
        with self.state.lock:
            self.state.load_calls.append({"body": body, "authorization": self.headers.get("authorization")})
        plan = self._plan()
        if plan == "load-a":
            return self._json(200, {"cloudaicompanionProject": "proj-load-a"})
        if plan == "load-b":
            return self._json(200, {"projectId": "proj-load-b"})
        if plan == "load-c":
            return self._json(200, {"project": {"id": "proj-load-c"}})
        if plan == "onboard":
            return self._json(
                200,
                {"allowedTiers": [{"tierId": "free-tier", "isDefault": True}]},
            )
        self._json(200, {})

    def _onboard(self) -> None:
        body = self._json_body()
        token = self.headers.get("authorization", "")
        with self.state.lock:
            self.state.onboard_calls.append({"body": body, "authorization": token})
            polls = self.state.onboard_polls.get(token, 0) + 1
            self.state.onboard_polls[token] = polls
        if self._plan() != "onboard":
            return self._json(200, {"done": True})
        if polls < 2:
            return self._json(200, {"done": False})
        self._json(
            200,
            {"done": True, "response": {"cloudaicompanionProject": {"id": "proj-onboard"}}},
        )

    # ----- CCA data plane ---------------------------------------------------
    def _record_cca(self, path: str, body) -> None:
        with self.state.lock:
            self.state.cca_calls.append(
                {
                    "path": path,
                    "body": body,
                    "authorization": self.headers.get("authorization"),
                    "user_agent": self.headers.get("user-agent"),
                    "api_client": self.headers.get("x-goog-api-client"),
                }
            )

    def _generate(self) -> None:
        body = self._json_body()
        self._record_cca("generateContent", body)
        self._json(
            200,
            {
                "response": {
                    "candidates": [
                        {
                            "content": {"role": "model", "parts": [{"text": "hello from cca"}]},
                            "finishReason": "STOP",
                        }
                    ],
                    "usageMetadata": {
                        "promptTokenCount": 3,
                        "candidatesTokenCount": 2,
                        "totalTokenCount": 5,
                    },
                },
                "traceId": "trace-offline",
            },
        )

    def _stream(self) -> None:
        body = self._json_body()
        self._record_cca("streamGenerateContent", body)
        self._sse(
            [
                {"response": {"candidates": [{"content": {"parts": [{"text": "hello "}]}}]}},
                {"response": {"candidates": [{"content": {"parts": [{"text": "from stream"}]}}]}},
                {
                    "response": {
                        "candidates": [
                            {"content": {"parts": [{"text": "!"}]}, "finishReason": "STOP"}
                        ],
                        "usageMetadata": {
                            "promptTokenCount": 3,
                            "candidatesTokenCount": 4,
                            "totalTokenCount": 7,
                        },
                    }
                },
            ]
        )


def api(port: int, method: str, path: str, body=None, token: str | None = None):
    headers = {"content-type": "application/json"}
    if token:
        headers["authorization"] = f"Bearer {token}"
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/api{path}",
        data=None if body is None else json.dumps(body).encode(),
        headers=headers,
        method=method,
    )
    try:
        with urllib.request.urlopen(request, timeout=40) as response:
            raw = response.read()
            return response.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as error:
        raw = error.read()
        try:
            return error.code, json.loads(raw or b"{}")
        except json.JSONDecodeError:
            return error.code, {"raw": raw.decode("utf-8", "replace")}
    except urllib.error.URLError as error:
        return 0, {"error": {"message": str(error)}}


def chat(port: int, stream: bool, api_key: str | None = None, model: str = MODEL_REF):
    headers = {"content-type": "application/json"}
    if api_key:
        headers["authorization"] = f"Bearer {api_key}"
    body = {
        "model": model,
        "messages": [{"role": "user", "content": "hello"}],
        "stream": stream,
    }
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers=headers,
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=40) as response:
            return response.status, response.headers, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.headers, error.read()


def drive_oauth(port: int):
    """Start the PKCE flow, complete the fixed loopback callback, return the completion."""
    status, body = api(port, "POST", f"/oauth/{PROVIDER}/start")
    if status == 404:
        return 3, body
    if status != 200:
        return status, body
    challenge = body.get("challenge", {})
    login_id = body.get("id")
    authorize_url = challenge.get("authorize_url") or ""
    redirect_uri = challenge.get("redirect_uri") or ""
    if not login_id or not authorize_url or not redirect_uri:
        return 0, body
    state = urllib.parse.parse_qs(urllib.parse.urlparse(authorize_url).query).get("state", [""])[0]
    result: dict = {}

    def complete():
        result["value"] = api(port, "POST", f"/oauth/{PROVIDER}/complete", {"id": login_id})

    worker = threading.Thread(target=complete)
    worker.start()
    deadline = time.time() + 30
    callback = f"{redirect_uri}?code=OFFLINE-CODE&state={urllib.parse.quote(state)}"
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(callback, timeout=5) as response:
                response.read()
            break
        except (urllib.error.URLError, OSError):
            time.sleep(0.1)
    worker.join(timeout=30)
    return result.get("value", (0, {}))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-backend", action="store_true", help="only probe the OAuth route")
    args = parser.parse_args()
    if not os.path.exists(RR):
        print(f"missing gateway binary: {RR} (build with cargo build --release)", file=sys.stderr)
        return 1
    gateway_port = free_port()
    mock_state = MockState()
    mock = ThreadingHTTPServer(("127.0.0.1", 0), Mock)
    mock.state = mock_state  # type: ignore[attr-defined]
    mock_port = mock.server_port
    threading.Thread(target=mock.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{mock_port}"

    data_dir = tempfile.mkdtemp(prefix="rrouter-antigravity-")
    log_path = os.path.join(data_dir, "gateway.log")
    log = open(log_path, "wb")
    env = {
        **os.environ,
        "RROUTER_PORT": str(gateway_port),
        "RROUTER_DATA_DIR": data_dir,
        "RROUTER_LOG_FILTER": "rrouter=debug",
        "RROUTER_ALLOW_INSECURE_OAUTH": "1",
        "RROUTER_OAUTH_OVERRIDE_GOOGLE_ANTIGRAVITY_AUTHORIZE": f"{base}/authorize",
        "RROUTER_OAUTH_OVERRIDE_GOOGLE_ANTIGRAVITY_TOKEN": f"{base}/token",
        "RROUTER_ANTIGRAVITY_PROD_HOST": base,
        "RROUTER_ANTIGRAVITY_DAILY_HOST": base,
    }
    gateway = subprocess.Popen([RR], env=env, stdout=log, stderr=subprocess.STDOUT)
    failures = 0

    def check(name: str, condition: bool, detail: str = "") -> None:
        nonlocal failures
        print(("PASS  " if condition else "FAIL  ") + name + (f"  [{detail}]" if detail else ""))
        if not condition:
            failures += 1

    try:
        ready = False
        for _ in range(150):
            if gateway.poll() is not None:
                break
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{gateway_port}/health", timeout=1):
                    ready = True
                    break
            except (OSError, urllib.error.URLError):
                time.sleep(0.1)
        check("gateway starts", ready)
        if not ready:
            log.close()
            with open(log_path, "rb") as handle:
                print(handle.read().decode("utf-8", "replace")[-4000:], file=sys.stderr)
            return 1

        if args.check_backend:
            status, body = api(gateway_port, "POST", f"/oauth/{PROVIDER}/start")
            if status == 404:
                print(
                    "BLOCKED-BY-BACKEND  /api/oauth/google-antigravity/start returned 404.\n"
                    "  The google-antigravity provider/flow has not landed in this build; the\n"
                    "  harness is ready and runs unchanged once it is registered.",
                    file=sys.stderr,
                )
                return 3
            check("oauth start exposes PKCE challenge", status == 200 and bool(body.get("challenge", {}).get("authorize_url")), f"{status} {body}")
            check("oauth start never returns a token", "offline-access" not in json.dumps(body))
            if body.get("id"):
                api(gateway_port, "DELETE", f"/oauth/{PROVIDER}/{body['id']}")
            print("backend routes present")
            return 0 if failures == 0 else 1

        # ----- plan A: project in loadCodeAssist as a bare id -----------------
        status, complete = drive_oauth(gateway_port)
        if status == 3:
            print(
                "BLOCKED-BY-BACKEND  /api/oauth/google-antigravity/start returned 404.\n"
                "  The google-antigravity provider/flow has not landed in this build; the\n"
                "  harness is ready and runs unchanged once it is registered.",
                file=sys.stderr,
            )
            return 3
        check("sign-in completes for load-a", status == 200 and complete.get("id"), f"{status} {complete}")
        check("sign-in never returns tokens", "offline-access" not in json.dumps(complete))
        exchange = next((e for e in mock_state.exchanges if e.get("grant_type") == "authorization_code"), {})
        check(
            "token exchange shape",
            exchange.get("grant_type") == "authorization_code"
            and exchange.get("code") == "OFFLINE-CODE"
            and bool(exchange.get("client_id"))
            and bool(exchange.get("client_secret"))
            and bool(exchange.get("code_verifier"))
            and bool(exchange.get("redirect_uri")),
            json.dumps({k: ("<set>" if v else "") for k, v in exchange.items()}),
        )

        # Model rows for this provider are operator-defined (no /models upstream).
        status, custom = api(gateway_port, "POST", "/models/custom", {"provider": PROVIDER, "id": MODEL})
        check("custom model row created", status == 200, f"{status} {custom}")

        # Force the access token to expire so the first request exercises refresh.
        time.sleep(2.2)
        results: list[tuple] = []

        def send():
            results.append(chat(gateway_port, stream=False))

        workers = [threading.Thread(target=send) for _ in range(2)]
        for worker in workers:
            worker.start()
        for worker in workers:
            worker.join()
        check("both requests succeed after refresh", all(status == 200 for status, _, _ in results), str([r[0] for r in results]))
        refresh_total = sum(mock_state.refreshes.values())
        check("refresh is single-flight across concurrent requests", refresh_total == 1, f"{refresh_total} refreshes")
        cca = [call for call in mock_state.cca_calls if call["path"] == "generateContent"]
        check("cca generateContent was called", len(cca) >= 1, f"{len(cca)} calls")
        if cca:
            call = cca[-1]
            env_body = call["body"]
            check(
                "envelope carries project/userAgent/requestType/requestId/request",
                env_body.get("project") == "proj-load-a"
                and env_body.get("userAgent") == "antigravity"
                and env_body.get("requestType") == "agent"
                and str(env_body.get("requestId", "")).startswith("agent-")
                and isinstance(env_body.get("request"), dict)
                and "model" not in env_body["request"],
                json.dumps({k: env_body.get(k) for k in ("project", "userAgent", "requestType")}),
            )
            parts = (
                env_body.get("request", {})
                .get("contents", [{}])[0]
                .get("parts", [])
            )
            check(
                "envelope wraps the canonical request contents",
                any(part.get("text") == "hello" for part in parts),
                json.dumps(parts)[:160],
            )
            check("cca headers name the Antigravity client", call["user_agent"] == "antigravity" and bool(call["api_client"]))
            check("cca Authorization uses the refreshed bearer token", str(call["authorization"]).startswith("Bearer offline-access"))
        response = json.loads(results[-1][2]) if results and results[-1][0] == 200 else {}
        check("response envelope unwrapped to client text", response.get("choices", [{}])[0].get("message", {}).get("content") == "hello from cca", str(response)[:200])
        check("usage copied from usageMetadata", response.get("usage", {}).get("prompt_tokens") == 3 and response.get("usage", {}).get("completion_tokens") == 2, str(response.get("usage")))

        # ----- streaming SSE unwrap ------------------------------------------
        stream_status, _, stream_body = chat(gateway_port, stream=True)
        check("stream request succeeds", stream_status == 200, str(stream_status))
        raw_text = stream_body.decode("utf-8", "replace")
        frames = []
        for line in raw_text.splitlines():
            if line.startswith("data: ") and line != "data: [DONE]":
                try:
                    frames.append(json.loads(line.removeprefix("data: ")))
                except json.JSONDecodeError:
                    pass
        streamed = "".join(
            frame.get("choices", [{}])[0].get("delta", {}).get("content", "")
            for frame in frames
            if isinstance(frame, dict)
        )
        check("SSE response unwraps response.candidates", streamed == "hello from stream!", repr(streamed))
        usage_frames = [frame.get("usage") for frame in frames if isinstance(frame.get("usage"), dict)]
        check(
            "stream usage arrives from usageMetadata",
            any((usage or {}).get("prompt_tokens") == 3 for usage in usage_frames),
            str(usage_frames),
        )
        stream_calls = [call for call in mock_state.cca_calls if call["path"] == "streamGenerateContent"]
        check("stream envelope reaches CCA", bool(stream_calls) and stream_calls[-1]["body"].get("project") == "proj-load-a")

        # ----- remaining project variants, one at a time ---------------------
        for plan, expected in (("load-b", "proj-load-b"), ("load-c", "proj-load-c")):
            status, complete = drive_oauth(gateway_port)
            check(f"sign-in completes for {plan}", status == 200 and complete.get("id"), f"{status} {complete}")
            # Deactivate older accounts so routing has exactly one active candidate.
            _, connections = api(gateway_port, "GET", "/connections")
            for row in connections.get("data", []):
                if row.get("provider") == PROVIDER and row.get("id") != complete.get("id") and row.get("is_active"):
                    api(gateway_port, "PATCH", f"/connections/{row['id']}", {"is_active": False})
            status, _, raw = chat(gateway_port, stream=False)
            call = mock_state.cca_calls[-1] if mock_state.cca_calls else {}
            check(
                f"project variant {plan} reaches the envelope",
                status == 200 and call.get("body", {}).get("project") == expected,
                f"{status} {call.get('body', {}).get('project')}",
            )

        # Onboarding polls `onboardUser` until done; the mock answers done on poll 2.
        status, complete = drive_oauth(gateway_port)
        check("sign-in completes via onboarding", status == 200 and complete.get("id"), f"{status} {complete}")
        check("onboarding polled more than once", len([c for c in mock_state.onboard_calls]) >= 2, f"{len(mock_state.onboard_calls)} calls")
        _, connections = api(gateway_port, "GET", "/connections")
        for row in connections.get("data", []):
            if row.get("provider") == PROVIDER and row.get("id") != complete.get("id") and row.get("is_active"):
                api(gateway_port, "PATCH", f"/connections/{row['id']}", {"is_active": False})
        status, _, _ = chat(gateway_port, stream=False)
        call = mock_state.cca_calls[-1] if mock_state.cca_calls else {}
        check(
            "onboarded project reaches the envelope",
            status == 200 and call.get("body", {}).get("project") == "proj-onboard",
            f"{status} {call.get('body', {}).get('project')}",
        )
        onboard_body = mock_state.onboard_calls[0]["body"] if mock_state.onboard_calls else {}
        check("onboarding uses the default tier id", onboard_body.get("tierId") == "free-tier", str(onboard_body))

        # Missing project is a clear configuration error, not a panic or empty connect.
        _, before = api(gateway_port, "GET", "/connections")
        before_count = len(before.get("data", []))
        status, complete = drive_oauth(gateway_port)
        check("missing project sign-in is refused", status >= 400, f"{status} {complete}")
        message = json.dumps(complete)
        check("missing project message names the fix", "no Cloud project" in message or "Cloud project" in message, message[:240])
        _, after = api(gateway_port, "GET", "/connections")
        check("missing project stores no connection", len(after.get("data", [])) == before_count, f"{before_count} -> {len(after.get('data', []))}")

        # ----- no secret in logs, API bodies or the database ------------------
        log.flush()
        log_text = open(log_path, "r", encoding="utf-8", errors="replace").read()
        check("gateway log was captured", "listening on" in log_text, f"{len(log_text)} bytes")
        for secret in ("offline-access", "offline-refresh"):
            check(f"logs do not contain {secret}", secret not in log_text)
        db_path = os.path.join(data_dir, "rrouter.db")
        try:
            db = open(db_path, "rb").read()
            for secret in ("offline-access", "offline-refresh"):
                check(f"database does not store {secret} in clear", secret.encode() not in db)
        except OSError as error:
            check("database exists", False, str(error))
    except Exception as error:  # noqa: BLE001 - report the harness failure, not a traceback
        check("harness completed without exception", False, repr(error))
    finally:
        gateway.terminate()
        try:
            gateway.wait(timeout=10)
        except subprocess.TimeoutExpired:
            gateway.kill()
            gateway.wait()
        log.close()
        mock.shutdown()
        mock.server_close()

    print("\nRESULT:", "ALL PASS" if failures == 0 else f"{failures} FAILURE(S)")
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
