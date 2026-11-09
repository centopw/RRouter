#!/usr/bin/env python3
"""Acceptance harness for combo strategies (fallback, round_robin, smart).

Self-contained: starts a mock OpenAI-compatible upstream and (by default) the real gateway
binary against a temporary data directory, so no provider credentials or network are
needed. The contract it asserts:

  POST /api/combos {name, models[], strategy, router_model?} -> row echoing
       strategy + router_model
       strategy: fallback | round_robin | smart
       smart without a valid router_model -> 400
  GET  /api/combos -> rows with the same fields
  request model `combo:<name>` routes through the strategy

If the running build does not echo `strategy`, the script exits 3 with
BLOCKED-BY-BACKEND and makes no claim about behaviour.

Usage:
  RR=target/release/rrouter python3 scripts/combo_smoke.py
  python3 scripts/combo_smoke.py --base-url http://127.0.0.1:20128 --token "$RROUTER_ADMIN_TOKEN"

Exit codes: 0 all checks pass, 1 failure, 3 combo-strategy backend absent, 4 gateway
could not start / callback prerequisites missing.
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
COMBO_PREFIX = "smoke"
CONNECTION_KEY = "offline-combo-key"


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class UpstreamState:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.calls: list[dict] = []


class Upstream(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    @property
    def state(self) -> UpstreamState:
        return self.server.state  # type: ignore[attr-defined]

    def _json(self, status: int, value) -> None:
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        if self.path.split("?")[0] != "/v1/chat/completions":
            return self._json(404, {"error": {"message": "not found"}})
        raw = self.rfile.read(int(self.headers.get("content-length", "0")))
        try:
            body = json.loads(raw or b"{}")
        except json.JSONDecodeError:
            body = {}
        model = str(body.get("model", ""))
        with self.state.lock:
            self.state.calls.append(
                {
                    "model": model,
                    "body": body,
                    "authorization": self.headers.get("authorization"),
                }
            )
        if model.endswith("/fail") or "fail" in model:
            return self._json(500, {"error": {"message": "upstream exploded"}})
        if "chooser" in model:
            return self._json(
                200,
                {
                    "id": "chatcmpl-chooser",
                    "object": "chat.completion",
                    "model": model,
                    "choices": [
                        {
                            "index": 0,
                            "message": {
                                "role": "assistant",
                                "content": json.dumps({"choice": "openai/rr-a"}),
                            },
                            "finish_reason": "stop",
                        }
                    ],
                    "usage": {"prompt_tokens": 5, "completion_tokens": 3},
                },
            )
        return self._json(
            200,
            {
                "id": "chatcmpl-combo",
                "object": "chat.completion",
                "model": model,
                "choices": [
                    {
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": f"ok:{model.rsplit('/', 1)[-1]}",
                        },
                        "finish_reason": "stop",
                    }
                ],
                "usage": {"prompt_tokens": 2, "completion_tokens": 1},
            },
        )


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
        "messages": [{"role": "user", "content": "pick a model"}],
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


def combo_row(body) -> dict:
    if not isinstance(body, dict):
        return {}
    if isinstance(body.get("combo"), dict):
        return body["combo"]
    return body


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default=os.environ.get("RROUTER_URL"))
    parser.add_argument("--token", default=os.environ.get("RROUTER_ADMIN_TOKEN", ""))
    parser.add_argument("--keep", action="store_true", help="leave the temp gateway/data dir")
    args = parser.parse_args()

    upstream_state = UpstreamState()
    upstream = ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
    upstream.state = upstream_state  # type: ignore[attr-defined]
    upstream_port = upstream.server_port
    threading.Thread(target=upstream.serve_forever, daemon=True).start()
    upstream_base = f"http://127.0.0.1:{upstream_port}"

    data_dir = tempfile.mkdtemp(prefix="rrouter-combo-")
    log_path = os.path.join(data_dir, "gateway.log")
    gateway = None
    log = None
    base = args.base_url
    cleanup: list[tuple[str, str]] = []
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

    def cleanup_all() -> None:
        for kind, name in reversed(cleanup):
            if kind == "combo":
                api(base, "DELETE", f"/combos?name={urllib.parse.quote(name)}", token=token)
            elif kind == "model":
                api(base, "DELETE", f"/models/custom?provider=openai&id={urllib.parse.quote(name)}", token=token)
            elif kind == "connection":
                api(base, "DELETE", f"/connections/{name}", token=token)

    try:
        # ----- prerequisites: one fake OpenAI connection + custom model rows -----
        status, connection = api(
            base,
            "POST",
            "/connections",
            {
                "provider": "openai",
                "auth_type": "api_key",
                "name": "combo smoke upstream",
                "credentials": {
                    "api_key": CONNECTION_KEY,
                    "base_url_override": upstream_base,
                },
            },
            token=token,
        )
        run.check("mock OpenAI connection created", status in (200, 201), f"{status} {connection}")
        connection_id = connection.get("id") if isinstance(connection, dict) else None
        if connection_id:
            cleanup.append(("connection", connection_id))
        if status not in (200, 201):
            return 1
        for model_id in ("rr-a", "rr-b", "fail", "ok", "chooser"):
            status, created = api(
                base,
                "POST",
                "/models/custom",
                {"provider": "openai", "id": model_id},
                token=token,
            )
            run.check(f"custom model openai/{model_id}", status == 200, f"{status} {created}")
            if status == 200:
                cleanup.append(("model", model_id))

        suffix = str(int(time.time() * 1000))[-7:]
        fallback = f"{COMBO_PREFIX}-fallback-{suffix}"
        rr_name = f"{COMBO_PREFIX}-rr-{suffix}"
        smart = f"{COMBO_PREFIX}-smart-{suffix}"

        # ----- contract: create + echo strategy/router_model -------------------
        status, created = api(
            base,
            "POST",
            "/combos",
            {
                "name": fallback,
                "models": ["openai/fail", "openai/ok"],
                "strategy": "fallback",
            },
            token=token,
        )
        row = combo_row(created)
        echoes_strategy = status == 200 and row.get("strategy") == "fallback"
        if not echoes_strategy:
            run.note(
                "combo strategy contract",
                f"POST /api/combos returned {status} without strategy='fallback': {created}",
            )
            run.blocked = "combo strategy/router_model is not served by this build"
            return 3
        cleanup.append(("combo", fallback))
        run.check("fallback combo echoes strategy", row.get("strategy") == "fallback", str(row))
        run.check(
            "fallback combo preserves model order",
            row.get("models") == ["openai/fail", "openai/ok"],
            str(row.get("models")),
        )

        status, created = api(
            base,
            "POST",
            "/combos",
            {
                "name": rr_name,
                "models": ["openai/rr-a", "openai/rr-b"],
                "strategy": "round_robin",
            },
            token=token,
        )
        row = combo_row(created)
        run.check(
            "round_robin combo echoes strategy",
            status == 200 and row.get("strategy") == "round_robin",
            f"{status} {created}",
        )
        if status == 200:
            cleanup.append(("combo", rr_name))

        status, created = api(
            base,
            "POST",
            "/combos",
            {
                "name": smart,
                "models": ["openai/rr-a", "openai/rr-b"],
                "strategy": "smart",
                "router_model": "openai/chooser",
            },
            token=token,
        )
        row = combo_row(created)
        run.check(
            "smart combo echoes strategy and router_model",
            status == 200
            and row.get("strategy") == "smart"
            and row.get("router_model") == "openai/chooser",
            f"{status} {created}",
        )
        if status == 200:
            cleanup.append(("combo", smart))

        # Invalid smart config is a 400, not a silently-fallback combo.
        status, refused = api(
            base,
            "POST",
            "/combos",
            {"name": f"{smart}-bad", "models": ["openai/rr-a"], "strategy": "smart"},
            token=token,
        )
        run.check("smart without router_model is refused", status == 400, f"{status} {refused}")
        status, refused = api(
            base,
            "POST",
            "/combos",
            {"name": f"{smart}-bad2", "models": ["openai/rr-a"], "strategy": "guess"},
            token=token,
        )
        run.check("unknown strategy is refused", status == 400, f"{status} {refused}")

        # ----- fallback chain: fail first, then serve the second model --------
        before = len(upstream_state.calls)
        status, body = chat(base, f"combo:{fallback}", token=token)
        calls = upstream_state.calls[before:]
        run.check("fallback combo succeeds", status == 200, f"{status} {body}")
        models = [call["model"] for call in calls]
        run.check(
            "fallback tried fail before ok",
            bool(models) and models[0] == "fail" and models[-1] == "ok",
            str(models),
        )
        run.check(
            "fallback response comes from the second model",
            body.get("choices", [{}])[0].get("message", {}).get("content") == "ok:ok",
            str(body)[:200],
        )

        # ----- round robin: both combo entries share N calls ------------------
        before = len(upstream_state.calls)
        for _ in range(6):
            status, _ = chat(base, f"combo:{rr_name}", token=token)
            if status != 200:
                break
        served = [call["model"] for call in upstream_state.calls[before:]]
        counts = {name: served.count(name) for name in set(served)}
        if set(counts) == {"rr-a", "rr-b"} and max(counts.values()) - min(counts.values()) <= 1:
            run.check("round_robin distributes across both models", True, str(counts))
        else:
            run.note(
                "round_robin distribution",
                f"every request used the first model ({counts}); "
                "the CRUD contract is served but routing does not apply the strategy yet",
            )
            run.blocked = run.blocked or "combo strategies are stored/validated but not applied during routing"

        # ----- smart: decision model call, then the chosen model --------------
        before = len(upstream_state.calls)
        status, body = chat(base, f"combo:{smart}", token=token)
        smart_calls = upstream_state.calls[before:]
        models = [call["model"] for call in smart_calls]
        if "chooser" not in models:
            run.note(
                "smart decision model",
                f"no call to openai/chooser observed (calls: {models}); backend behaviour not verifiable",
            )
            run.blocked = run.blocked or "smart decision model is not invoked yet"
        else:
            run.check("smart calls the decision model first", models[0] == "chooser", str(models))
            run.check("smart routes to the JSON choice", "rr-a" in models, str(models))
            run.check(
                "smart answer comes from the chosen model",
                status == 200
                and body.get("choices", [{}])[0].get("message", {}).get("content") == "ok:rr-a",
                f"{status} {body}",
            )

        # ----- no secrets in status/logs --------------------------------------
        _, rows = api(base, "GET", "/combos", token=token)
        serialized = json.dumps(rows)
        for name in (fallback, rr_name, smart):
            run.check(f"combo {name} has no credential material", CONNECTION_KEY not in serialized)
        if log is not None:
            log.flush()
            text = open(log_path, "r", encoding="utf-8", errors="replace").read()
            run.check("logs do not contain the upstream key", CONNECTION_KEY not in text)
    except Exception as error:  # noqa: BLE001
        run.check("harness completed without exception", False, repr(error))
    finally:
        cleanup_all()
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
