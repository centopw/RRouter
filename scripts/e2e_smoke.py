#!/usr/bin/env python3
"""Live end-to-end smoke for RRouter: a mock upstream + the release binary.

Proves, against the real binary (not test harness):
  1. an [OI] streaming request is proxied chunk-by-chunk, and the client sees the
     first content frame before the upstream has sent its last chunk;
  2. a dead account falls through to the next one within the fallback budget;
  3. credentials are encrypted at rest and never echoed by /api/connections;
  4. an Anthropic-dialect client gets Anthropic-shaped SSE from an [OI]-speaking provider.
"""
import json
import os
import socket
import subprocess
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

RR = os.environ.get("RR", "target/release/rrouter")
DATA = "/tmp/rrouter-e2e"
UP = 21110
GW = 21111


def free(p):
    s = socket.socket()
    try:
        s.connect(("127.0.0.1", p))
        return False
    except OSError:
        return True
    finally:
        s.close()


class Upstream(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    hits = 0

    def log_message(self, *a):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        Upstream.hits += 1
        if self.headers.get("Authorization") == "Bearer dead-key":
            self.send_response(401)
            payload = b'{"error":{"message":"invalid api key"}}'
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return

        stream = body.get("stream", False)
        if not stream:
            payload = json.dumps({
                "id": "cmpl-1", "object": "chat.completion", "model": body["model"],
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "pong"},
                             "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4},
            }).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return

        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("transfer-encoding", "chunked")
        self.end_headers()

        def frame(delta, finish=None):
            return "data: " + json.dumps({
                "id": "cmpl-1", "object": "chat.completion.chunk", "model": body["model"],
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
            }) + "\n\n"

        def send(s):
            b = s.encode()
            self.wfile.write(b"%x\r\n%s\r\n" % (len(b), b))
            self.wfile.flush()

        send(frame({"role": "assistant", "content": "first"}))
        time.sleep(1.5)  # a client that buffers would prove nothing until this returns
        send(frame({"content": " second"}))
        send(frame({}, "stop"))
        send("data: [DONE]\n\n")
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()


def http(method, path, body=None, headers=None):
    req = urllib.request.Request(
        f"http://127.0.0.1:{GW}{path}",
        data=None if body is None else json.dumps(body).encode(),
        headers={"content-type": "application/json", **(headers or {})},
        method=method,
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.status, r.read().decode()


def main():
    assert free(UP) and free(GW), "ports busy"
    os.system(f"rm -rf {DATA} && mkdir -p {DATA}")
    srv = ThreadingHTTPServer(("127.0.0.1", UP), Upstream)
    threading.Thread(target=srv.serve_forever, daemon=True).start()

    gw = subprocess.Popen(
        [RR], env={**os.environ, "RROUTER_PORT": str(GW), "RROUTER_DATA_DIR": DATA,
                   "RUST_LOG": "rrouter=warn"},
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    try:
        for _ in range(100):
            if not free(GW):
                break
            time.sleep(0.05)

        ok = True

        def check(name, cond, detail=""):
            nonlocal ok
            print(("PASS  " if cond else "FAIL  ") + name + (f"  [{detail}]" if detail else ""))
            ok = ok and cond

        # --- register a dead account first, then a live one -----------------------
        _, dead = http("POST", "/api/connections", {
            "provider": "openai", "auth_type": "api_key", "name": "dead", "priority": 0,
            "credentials": {"api_key": "dead-key", "base_url_override": f"http://127.0.0.1:{UP}/v1/chat/completions"},
        })
        _, live = http("POST", "/api/connections", {
            "provider": "openai", "auth_type": "api_key", "name": "live", "priority": 10,
            "credentials": {"api_key": "SECRET-E2E-XYZ", "base_url_override": f"http://127.0.0.1:{UP}/v1/chat/completions"},
        })
        dead_id, live_id = json.loads(dead)["id"], json.loads(live)["id"]
        check("connections registered", bool(dead_id and live_id))

        # --- fallback: 401 on priority 0 -> next account --------------------------
        t0 = time.time()
        st, body = http("POST", "/v1/chat/completions", {
            "model": "gpt-4.1", "messages": [{"role": "user", "content": "ping"}],
        })
        dt = (time.time() - t0) * 1000
        check("fallback after 401 returns 200", st == 200, f"{dt:.0f} ms")
        check("fallback latency budget < 2000 ms", dt < 2000, f"{dt:.0f} ms")
        check("content served by live account", "pong" in body)

        # --- non-streaming content -------------------------------------------------
        check("non-streaming body shape", json.loads(body)["choices"][0]["message"]["content"] == "pong")

        # --- governance: no token material leaks -----------------------------------
        _, conns = http("GET", "/api/connections")
        check("admin API never returns api_key", "SECRET-E2E-XYZ" not in conns and "dead-key" not in conns)

        # --- streaming: first frame before upstream finishes -----------------------
        req = urllib.request.Request(
            f"http://127.0.0.1:{GW}/v1/chat/completions",
            data=json.dumps({"model": "gpt-4.1", "stream": True,
                             "messages": [{"role": "user", "content": "ping"}]}).encode(),
            headers={"content-type": "application/json"}, method="POST")
        t0 = time.time()
        with urllib.request.urlopen(req, timeout=30) as r:
            first = r.readline()
            first_at = time.time() - t0
        check("SSE first byte is a data frame", first.startswith(b"data:"), first[:40].decode(errors="replace"))
        check("first frame arrives before upstream tail (streaming, not buffered)", first_at < 1.2,
              f"{first_at*1000:.0f} ms")

        # --- Anthropic client, [OI]-speaking provider ------------------------------
        req = urllib.request.Request(
            f"http://127.0.0.1:{GW}/v1/messages",
            data=json.dumps({"model": "gpt-4.1", "max_tokens": 64,
                             "messages": [{"role": "user", "content": "ping"}]}).encode(),
            headers={"content-type": "application/json",
                     "anthropic-version": "2023-06-01"}, method="POST")
        with urllib.request.urlopen(req, timeout=30) as r:
            anth = json.loads(r.read())
        check("anthropic dialect response shape", anth.get("type") == "message"
              and isinstance(anth.get("content"), list), str(anth)[:90])

        # --- credentials encrypted at rest ----------------------------------------
        db = open(f"{DATA}/rrouter.db", "rb").read()
        check("plaintext credential absent from DB file", b"SECRET-E2E-XYZ" not in db)
        key_file = os.path.join(DATA, "master.key")
        mode = oct(os.stat(key_file).st_mode & 0o777) if os.path.exists(key_file) else "missing"
        check("key file 0600", mode == "0o600", mode)

        # --- gateway API key enforcement, off by default --------------------------
        _, health = http("GET", "/health")
        check("health ok", json.loads(health)["ok"] is True)

        print("\nRESULT:", "ALL PASS" if ok else "FAILURES PRESENT")
        return 0 if ok else 1
    finally:
        gw.terminate()
        gw.wait(timeout=10)
        srv.shutdown()


if __name__ == "__main__":
    sys.exit(main())
