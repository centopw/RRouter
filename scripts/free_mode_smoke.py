#!/usr/bin/env python3
"""Acceptance harness for free mode: import, discovery, and `free` routing.

Self-contained: starts a mock OpenAI-compatible upstream and (by default) the real gateway
binary against a temporary data directory, so no provider credentials and no network are
needed. The contract it asserts:

  GET  /api/free-mode                      -> zero-config state (enabled/fallback/summary/catalogs)
  PATCH /api/free-mode                     -> toggles the pool on/off
  POST /api/free-mode/import   {document}  -> the document is applied (accounts created)
  POST /api/free-mode/discover             -> free models recorded with their source
  GET  /api/free-mode                      -> document, routing knobs, accounts, models
  DELETE /api/free-mode                    -> tables cleared

  `free/MODEL`              routes exactly that model across every free account
  `free`                    routes the selected pool (`routing.models`) only
  `free:PROVIDER`           narrows the pool to one provider
  `free:unknown`            -> 404
  `free/unknown-model`      -> 404 whose message names discovery

Spread and rate limits are asserted against what the upstream actually received: the
account that served each attempt (its API key), the model it was asked for, and a 429 on
the first account forcing the same request to finish on the second.

Usage:
  RR=target/release/rrouter python3 scripts/free_mode_smoke.py
  python3 scripts/free_mode_smoke.py --base-url http://127.0.0.1:20128 --token "$RROUTER_ADMIN_TOKEN"

Exit codes: 0 all checks pass, 1 failure, 3 free-mode backend absent, 4 gateway could not
start.
"""

from __future__ import annotations

import argparse
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

RR = os.environ.get("RR", "target/release/rrouter")
PROVIDER = "smoke-free"
PINNED_MODEL = "deepseek-v4.1-flash"
LISTED_FREE = "llama-3.3:free"
PAID_MODEL = "gpt-5-expensive"
KEY_ONE = "smoke-free-key-one"
KEY_TWO = "smoke-free-key-two"
# A third free account the upstream always answers 429. It is in the pool on purpose: the
# gateway must walk past it to a healthy account and the client must never see the 429.
THROTTLED_KEY = "smoke-free-key-three"
# How many chat requests the pinned-model phase sends.
PINNED_ROUNDS = 4


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class UpstreamState:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        # One entry per chat completion the upstream served (or refused): the API key it
        # arrived with, the model requested, and the status answered.
        self.calls: list[dict] = []
        self.throttled: set[str] = set()

    def record(self, key: str, model: str, status: int) -> None:
        with self.lock:
            self.calls.append({"key": key, "model": model, "status": status})

    def since(self, index: int) -> list[dict]:
        with self.lock:
            return list(self.calls[index:])


class Upstream(BaseHTTPRequestHandler):
    """Mock OpenAI-compatible provider: a model list plus chat completions."""

    protocol_version = "HTTP/1.1"
    state: UpstreamState  # set by main()

    def log_message(self, *_args) -> None:  # keep the harness output readable
        return

    def _json(self, status: int, payload: dict) -> None:
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _auth_key(self) -> str:
        header = self.headers.get("authorization", "")
        return header[7:] if header.lower().startswith("bearer ") else ""

    def _body(self) -> dict:
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length) if length else b"{}"
        try:
            return json.loads(raw or b"{}")
        except json.JSONDecodeError:
            return {}

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if urllib.parse.urlparse(self.path).path.rstrip("/") in ("/v1/models", "/models"):
            self._json(
                200,
                {
                    "object": "list",
                    "data": [
                        {"id": PINNED_MODEL, "object": "model"},
                        {"id": LISTED_FREE, "object": "model"},
                        {"id": PAID_MODEL, "object": "model"},
                    ],
                },
            )
            return
        self._json(404, {"error": {"message": "not found"}})

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        if not urllib.parse.urlparse(self.path).path.rstrip("/").endswith(
            "/chat/completions"
        ):
            self._json(404, {"error": {"message": "not found"}})
            return
        body = self._body()
        model = body.get("model", "")
        key = self._auth_key()
        # The throttled account always answers 429: the gateway must finish every client
        # request on another free account rather than surfacing the rate limit.
        if key == THROTTLED_KEY:
            self.state.throttled.add(key)
            self.state.record(key, model, 429)
            self._json(
                429,
                {"error": {"message": "rate limited", "type": "rate_limit_error"}},
            )
            return
        self.state.record(key, model, 200)
        answer = {
            "id": "chatcmpl-free",
            "object": "chat.completion",
            "model": model,
            "choices": [
                {
                    "index": 0,
                    "message": {"role": "assistant", "content": f"ok:{model}"},
                    "finish_reason": "stop",
                }
            ],
            "usage": {"prompt_tokens": 2, "completion_tokens": 1},
        }
        self._json(200, answer)


def api(base: str, method: str, path: str, body=None, token: str | None = None):
    headers = {"content-type": "application/json"}
    if token:
        headers["authorization"] = f"Bearer {token}"
    request = urllib.request.Request(
        f"{base}/api{path}",
        data=None if body is None else json.dumps(body).encode(),
        headers=headers,
        method=method,
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
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


def chat(base: str, model: str, token: str | None = None):
    headers = {"content-type": "application/json"}
    if token:
        headers["authorization"] = f"Bearer {token}"
    body = {
        "model": model,
        "messages": [{"role": "user", "content": "hello"}],
        "stream": False,
    }
    request = urllib.request.Request(
        f"{base}/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers=headers,
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=40) as response:
            return response.status, json.loads(response.read() or b"{}")
    except urllib.error.HTTPError as error:
        raw = error.read()
        try:
            return error.code, json.loads(raw or b"{}")
        except json.JSONDecodeError:
            return error.code, {"raw": raw.decode("utf-8", "replace")}


class Run:
    def __init__(self) -> None:
        self.failures = 0
        self.blocked: str | None = None

    def check(self, name: str, condition: bool, detail: str = "") -> None:
        print(("PASS  " if condition else "FAIL  ") + name + (f"  [{detail}]" if detail else ""))
        if not condition:
            self.failures += 1

    def note(self, name: str, detail: str) -> None:
        print(f"BLOCKED/SKIP  {name}  [{detail}]")


def document(upstream_base: str, models: list[str]) -> dict:
    return {
        "version": 1,
        "routing": {
            "enabled": True,
            "per_account_rpm": 1000,
            "max_attempts": 4,
            "prefer": "spread",
            "models": models,
        },
        "providers": [
            {
                "provider": PROVIDER,
                "custom": {
                    "id": PROVIDER,
                    "display": "Smoke Free Provider",
                    "base_url": f"{upstream_base}/v1",
                    "dialect": "chat",
                },
                # Seeded, so a model the provider never lists is still routable: this is
                # the operator-chosen-id path `free/MODEL` exists for.
                "models": [PINNED_MODEL],
                "accounts": [
                    {"label": "smoke-account-one", "api_key": KEY_ONE, "rpm": 1000},
                    {"label": "smoke-account-two", "api_key": KEY_TWO, "rpm": 1000},
                    {"label": "smoke-account-throttled", "api_key": THROTTLED_KEY, "rpm": 1000},
                ],
            }
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default=os.environ.get("RROUTER_URL"))
    parser.add_argument("--token", default=os.environ.get("RROUTER_ADMIN_TOKEN", ""))
    parser.add_argument("--keep", action="store_true", help="leave the temp gateway/data dir")
    args = parser.parse_args()

    upstream_state = UpstreamState()
    upstream = ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
    Upstream.state = upstream_state  # type: ignore[attr-defined]
    threading.Thread(target=upstream.serve_forever, daemon=True).start()
    upstream_base = f"http://127.0.0.1:{upstream.server_port}"

    data_dir = tempfile.mkdtemp(prefix="rrouter-free-mode-")
    log_path = os.path.join(data_dir, "gateway.log")
    gateway = None
    log = None
    base = args.base_url
    run = Run()

    if not base:
        if not os.path.exists(RR):
            print(f"missing gateway binary: {RR} (cargo build --release)", file=sys.stderr)
            return 4
        port = free_port()
        base = f"http://127.0.0.1:{port}"
        log = open(log_path, "wb")
        env = {
            **os.environ,
            "RROUTER_PORT": str(port),
            "RROUTER_DATA_DIR": data_dir,
            "RROUTER_LOG_FILTER": "rrouter=debug",
        }
        gateway = subprocess.Popen([RR], env=env, stdout=log, stderr=subprocess.STDOUT)
        ready = False
        for _ in range(200):
            if gateway.poll() is not None:
                break
            try:
                with urllib.request.urlopen(f"{base}/health", timeout=1):
                    ready = True
                    break
            except (OSError, urllib.error.URLError):
                time.sleep(0.1)
        if not ready:
            print("gateway did not start", file=sys.stderr)
            if log:
                log.close()
                with open(log_path, "rb") as handle:
                    print(handle.read().decode("utf-8", "replace")[-3000:], file=sys.stderr)
            return 4
    token = args.token or None

    try:
        # ----- contract: the endpoints exist at all ---------------------------
        status, state = api(base, "GET", "/free-mode", token=token)
        if status != 200:
            # A build without free mode answers 404, or 307 to the dashboard catch-all.
            # Either way the harness makes no claim about behaviour.
            run.note(
                "free-mode API",
                f"GET /api/free-mode answered {status}: {state}",
            )
            run.blocked = "free-mode import/discovery endpoints are absent"
            return 3
        run.check("free-mode state is readable", status == 200, f"{status} {state}")

        # ----- zero-config surface: toggles, summary and catalog fields --------
        # A fresh instance (no document imported) defaults off, and the frozen
        # contract's top-level fields are present even when no catalog ran.
        for field in ("enabled", "fallback_on_quota", "summary", "catalogs", "next_fallback_model", "ref"):
            run.check(f"zero-config field {field!r} is present", field in state, str(sorted(state))[:200])
        run.check("pool defaults to off", state.get("enabled") is False, str(state.get("enabled")))

        status, toggled = api(base, "PATCH", "/free-mode", {"enabled": True}, token=token)
        run.check("PATCH toggles the pool on", status == 200, f"{status} {toggled}")
        run.check("PATCH echoes enabled", toggled.get("enabled") is True, str(toggled.get("enabled")))

        status, toggled = api(base, "PATCH", "/free-mode", {"enabled": False}, token=token)
        run.check("PATCH toggles the pool back off", status == 200 and toggled.get("enabled") is False, f"{status} {toggled}")

        # ----- import: the document creates free-mode accounts -----------------
        status, imported = api(
            base, "POST", "/free-mode/import", document(upstream_base, [PINNED_MODEL]), token=token
        )
        run.check("import applies the document", status == 200, f"{status} {imported}")
        if status != 200:
            return 1
        accounts = imported.get("accounts") or {}
        run.check(
            "import creates every account",
            accounts.get("created") == 3 and accounts.get("updated") == 0,
            str(accounts),
        )

        status, again = api(
            base, "POST", "/free-mode/import", document(upstream_base, [PINNED_MODEL]), token=token
        )
        accounts = again.get("accounts") or {}
        run.check(
            "re-import is idempotent",
            status == 200 and accounts.get("created") == 0 and accounts.get("updated") == 3,
            f"{status} {accounts}",
        )

        status, state = api(base, "GET", "/free-mode", token=token)
        routing = state.get("routing") or {}
        run.check(
            "routing knobs round-trip, including the selected models",
            status == 200 and routing.get("models") == [PINNED_MODEL],
            f"{status} {routing}",
        )
        listed = state.get("accounts") or []
        run.check("every free-mode account is listed", len(listed) == 3, str(len(listed)))
        run.check(
            "accounts are marked free mode and hide their key",
            all(a.get("free_rpm") is not None for a in listed)
            and KEY_ONE not in json.dumps(state)
            and KEY_TWO not in json.dumps(state),
            str(listed)[:300],
        )

        # ----- discovery: the provider list plus the seeded model --------------
        status, found = api(base, "POST", "/free-mode/discover", {}, token=token)
        run.check("discovery runs", status == 200, f"{status} {found}")
        status, state = api(base, "GET", "/free-mode", token=token)
        models = {row["model"]: row for row in (state.get("models") or [])}
        run.check(
            "discovery records the provider-listed free model",
            LISTED_FREE in models and models[LISTED_FREE].get("free") is True,
            str(models.get(LISTED_FREE)),
        )
        run.check(
            "the seeded model is routable with source=document",
            PINNED_MODEL in models and models[PINNED_MODEL].get("source") == "document",
            str(models.get(PINNED_MODEL)),
        )
        run.check(
            "a priced model is not marked free",
            PAID_MODEL not in models or models[PAID_MODEL].get("free") is False,
            str(models.get(PAID_MODEL)),
        )

        # ----- routing: free/MODEL spreads across accounts (and survives a 429)
        before = len(upstream_state.calls)
        served_ok = 0
        for _ in range(PINNED_ROUNDS):
            status, body = chat(base, f"free/{PINNED_MODEL}", token=token)
            if status == 200:
                served_ok += 1
        run.check(
            f"free/MODEL answers all {PINNED_ROUNDS} requests",
            served_ok == PINNED_ROUNDS,
            str(served_ok),
        )
        attempts = upstream_state.since(before)
        run.check(
            "free/MODEL always sends the pinned model",
            bool(attempts) and all(a["model"] == PINNED_MODEL for a in attempts),
            str([a["model"] for a in attempts]),
        )
        keys = {a["key"] for a in attempts}
        healthy = keys & {KEY_ONE, KEY_TWO}
        run.check(
            "free/MODEL rotates across the healthy free accounts",
            healthy == {KEY_ONE, KEY_TWO},
            f"saw {sorted(keys)}; every attempt {[(a['key'], a['status']) for a in attempts]}",
        )
        # The throttled account is in the pool, so an attempt may legitimately 429; what
        # must never happen is the client seeing one. A single healthy account taking all
        # the traffic would mean the rotation stopped, which the previous check rules out.
        run.check(
            "a 429 on a free account never reaches the client",
            served_ok == PINNED_ROUNDS
            and all(a["status"] in (200, 429) for a in attempts),
            str([(a["key"], a["status"]) for a in attempts]),
        )
        run.check(
            "the throttled account was actually exercised",
            THROTTLED_KEY in keys,
            str(sorted(keys)),
        )
        answer = body.get("choices", [{}])[0].get("message", {}).get("content")
        run.check(
            "the client sees the pinned model's answer",
            answer == f"ok:{PINNED_MODEL}",
            str(answer),
        )

        # ----- routing: bare `free` honours the selected pool -------------------
        before = len(upstream_state.calls)
        status, body = chat(base, "free", token=token)
        attempts = upstream_state.since(before)
        run.check("bare free answers", status == 200, f"{status} {body}")
        run.check(
            "bare free only used the selected model, never the other free one",
            bool(attempts) and all(a["model"] == PINNED_MODEL for a in attempts),
            str([a["model"] for a in attempts]),
        )

        # ----- routing: provider-scoped and both-narrowed refs -----------------
        status, body = chat(base, f"free:{PROVIDER}", token=token)
        run.check("free:PROVIDER answers", status == 200, f"{status} {body}")
        status, body = chat(base, f"free:{PROVIDER}/{PINNED_MODEL}", token=token)
        run.check("free:PROVIDER/MODEL answers", status == 200, f"{status} {body}")

        # ----- routing: the error cases name the fix ---------------------------
        status, body = chat(base, "free:no-such-provider", token=token)
        run.check("free:unknown-provider is a 404", status == 404, f"{status} {body}")
        status, body = chat(base, "free/no-such-model-xyz", token=token)
        detail = json.dumps(body).lower()
        run.check(
            "free/unknown-model is a 404 that names discovery",
            status == 404 and "discover" in detail,
            f"{status} {body}",
        )

        # ----- no secrets, anywhere --------------------------------------------
        status, state = api(base, "GET", "/free-mode", token=token)
        serialized = json.dumps(state)
        run.check(
            "the state read exposes no API key",
            KEY_ONE not in serialized and KEY_TWO not in serialized,
        )
        if log is not None:
            log.flush()
            with open(log_path, "r", encoding="utf-8", errors="replace") as handle:
                text = handle.read()
            run.check(
                "gateway logs expose no API key",
                KEY_ONE not in text and KEY_TWO not in text,
            )

        # ----- teardown ---------------------------------------------------------
        status, cleared = api(base, "DELETE", "/free-mode", token=token)
        run.check("teardown clears free mode", status in (200, 204), f"{status} {cleared}")
        status, state = api(base, "GET", "/free-mode", token=token)
        run.check(
            "teardown leaves no document and no discovery rows",
            status == 200
            and not state.get("document")
            and not state.get("models"),
            f"{status} {state}",
        )
    except Exception as error:  # noqa: BLE001
        run.check("harness completed without exception", False, repr(error))
    finally:
        if gateway is not None:
            gateway.terminate()
            try:
                gateway.wait(timeout=10)
            except subprocess.TimeoutExpired:
                gateway.kill()
                gateway.wait()
        if log is not None:
            log.close()
        upstream.shutdown()
        upstream.server_close()
        if args.keep:
            print(f"kept data dir: {data_dir}", file=sys.stderr)

    if run.failures:
        print(f"\nRESULT: {run.failures} FAILURE(S)")
        return 1
    if run.blocked:
        print(f"\nRESULT: BLOCKED-BY-BACKEND  {run.blocked}")
        return 3
    print("\nRESULT: ALL PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
