#!/usr/bin/env python3
"""Concurrency probe for a live RRouter: real requests, real streams, honest numbers.

    python3 scripts/load_test.py --model claude/claude-haiku-4-5-20251001 \\
        --requests 20 --concurrency 5

Reports latency, time-to-first-byte for streamed responses, status counts and token
throughput. It spends provider tokens: keep --max-tokens small and the account cheap.

No dependencies beyond the standard library, so it runs on any host that can reach the
gateway.
"""
import argparse
import json
import random
import statistics
import sys
import threading
import time
import urllib.error
import urllib.request


def one_request(args, index, results):
    body = {
        "model": args.model,
        "stream": not args.no_stream,
        "max_tokens": args.max_tokens,
        "messages": [{"role": "user", "content": f"Reply with the single word: pong ({index})"}],
    }
    req = urllib.request.Request(
        args.url.rstrip("/") + "/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers={"content-type": "application/json", **({"authorization": f"Bearer {args.key}"} if args.key else {})},
        method="POST",
    )
    started = time.perf_counter()
    first = None
    text = []
    prompt_tokens = completion_tokens = 0
    try:
        with urllib.request.urlopen(req, timeout=args.timeout) as resp:
            if args.no_stream:
                payload = json.loads(resp.read())
                first = time.perf_counter()
                text.append(payload["choices"][0]["message"]["content"])
                usage = payload.get("usage") or {}
                prompt_tokens = usage.get("prompt_tokens", 0)
                completion_tokens = usage.get("completion_tokens", 0)
            else:
                for raw in resp:
                    line = raw.decode("utf-8", "replace").strip()
                    if not line.startswith("data:"):
                        continue
                    data = line[5:].strip()
                    if data == "[DONE]":
                        break
                    chunk = json.loads(data)
                    delta = chunk.get("choices", [{}])[0].get("delta", {}).get("content")
                    if delta and first is None:
                        first = time.perf_counter()
                    if delta:
                        text.append(delta)
                    usage = chunk.get("usage") or {}
                    if usage:
                        prompt_tokens = usage.get("prompt_tokens", prompt_tokens)
                        completion_tokens = usage.get("completion_tokens", completion_tokens)
        elapsed = time.perf_counter() - started
        results.append(
            {
                "ok": True,
                "status": 200,
                "latency": elapsed,
                "ttfb": (first - started) if first else None,
                "prompt": prompt_tokens,
                "completion": completion_tokens,
                "error": None,
            }
        )
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", "replace")[:300]
        results.append({"ok": False, "status": e.code, "latency": time.perf_counter() - started, "ttfb": None, "prompt": 0, "completion": 0, "error": body})
    except Exception as e:  # noqa: BLE001 - a probe reports whatever happened
        results.append({"ok": False, "status": 0, "latency": time.perf_counter() - started, "ttfb": None, "prompt": 0, "completion": 0, "error": str(e)})


def run(args):
    results = []
    lock = threading.Lock()
    queue = list(range(args.requests))

    def worker():
        while True:
            with lock:
                if not queue:
                    return
                index = queue.pop(0)
            one_request(args, index, results)
            time.sleep(args.pause)

    threads = [threading.Thread(target=worker, daemon=True) for _ in range(args.concurrency)]
    started = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    wall = time.perf_counter() - started

    results.sort(key=lambda r: r["latency"])
    ok = [r for r in results if r["ok"]]
    errors = [r for r in results if not r["ok"]]
    latencies = [r["latency"] for r in results]
    ttfts = [r["ttfb"] for r in ok if r["ttfb"] is not None]

    def pct(values, p):
        return values[min(len(values) - 1, int(len(values) * p))] if values else 0.0

    print(f"requests {len(results)}  ok {len(ok)}  failed {len(errors)}  wall {wall:.2f}s  rps {len(results)/wall:.2f}")
    if latencies:
        print(
            f"latency  p50 {pct(latencies,0.5)*1000:.0f}ms  p95 {pct(latencies,0.95)*1000:.0f}ms  max {latencies[-1]*1000:.0f}ms"
        )
    if ttfts:
        print(f"ttfb     p50 {statistics.median(ttfts)*1000:.0f}ms  max {max(ttfts)*1000:.0f}ms")
    if ok:
        tokens = sum(r["prompt"] + r["completion"] for r in ok)
        print(f"tokens   {tokens}  ({tokens/wall:.1f}/s)")
    statuses = {}
    for r in results:
        statuses[r["status"]] = statuses.get(r["status"], 0) + 1
    print("status   " + "  ".join(f"{code}:{count}" for code, count in sorted(statuses.items())))
    for r in errors[:5]:
        print(f"error    {r['status']}: {r['error']}", file=sys.stderr)
    return 0 if not errors else 1


def main():
    parser = argparse.ArgumentParser(description="Concurrency probe for a live RRouter")
    parser.add_argument("--url", default="http://127.0.0.1:20128")
    parser.add_argument("--key", default="", help="gateway key when RROUTER_REQUIRE_API_KEY=true")
    parser.add_argument("--model", default="claude/claude-haiku-4-5-20251001")
    parser.add_argument("--requests", type=int, default=10)
    parser.add_argument("--concurrency", type=int, default=4)
    parser.add_argument("--max-tokens", type=int, default=16)
    parser.add_argument("--timeout", type=float, default=120)
    parser.add_argument("--pause", type=float, default=0.0, help="seconds between requests per worker")
    parser.add_argument("--no-stream", action="store_true")
    parser.add_argument("--seed", type=int, default=None)
    args = parser.parse_args()
    if args.seed is not None:
        random.seed(args.seed)
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
