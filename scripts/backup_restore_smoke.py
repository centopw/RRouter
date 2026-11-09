#!/usr/bin/env python3
"""Restore drill for `scripts/backup.sh`: prove a snapshot actually restores.

Runs two real gateways against two data directories:

  1. A: create an encrypted provider connection and a non-default setting, stop it;
  2. snapshot A with `scripts/backup.sh`, check the manifest/checksums and file set;
  3. B: copy the snapshot into a fresh data dir, start a second gateway, and assert the
     credential decrypts (`has_api_key`) and the setting survived.

A backup nobody restored is a guess. This is the cheap version: no upstream is called,
no provider tokens are spent, and the whole drill takes a couple of seconds.

    python3 scripts/backup_restore_smoke.py

Exit code is non-zero on any failed check. `RR` overrides the binary (default
`target/release/rrouter`).
"""
import hashlib
import json
import os
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

RR = os.environ.get("RR", "target/release/rrouter")
SECRET = "SECRET-RESTORE-DRILL-XYZ"
BRAND = "RestoreProof"


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def http(port, method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        data=data,
        headers={"content-type": "application/json"},
        method=method,
    )
    with urllib.request.urlopen(req, timeout=10) as resp:
        return resp.status, resp.read().decode()


def wait_up(proc, port, seconds=15):
    deadline = time.time() + seconds
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"gateway exited early with {proc.returncode}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.25):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"gateway did not listen on {port}")


def start(data_dir, port):
    env = {
        **os.environ,
        "RROUTER_PORT": str(port),
        "RROUTER_DATA_DIR": data_dir,
        "RUST_LOG": "rrouter=warn",
    }
    proc = subprocess.Popen(
        [RR], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    )
    wait_up(proc, port)
    return proc


def stop(proc):
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main():
    checks = []

    def check(name, ok, detail=""):
        checks.append(ok)
        print(("PASS  " if ok else "FAIL  ") + name + (f"  [{detail}]" if detail else ""))

    if not os.access(RR, os.X_OK):
        print(f"skip: {RR} is not built (cargo build --release)", file=sys.stderr)
        return 2

    root = tempfile.mkdtemp(prefix="rrouter-restore-")
    source = os.path.join(root, "data-a")
    snapshot = os.path.join(root, "snapshot")
    restored = os.path.join(root, "data-b")
    os.makedirs(source)
    os.makedirs(restored)

    proc = None
    try:
        # --- 1. state worth restoring ------------------------------------------------
        port = free_port()
        proc = start(source, port)
        status, body = http(
            port,
            "POST",
            "/api/connections",
            {
                "provider": "openai",
                "auth_type": "api_key",
                "name": "restore-drill",
                "priority": 5,
                "credentials": {"api_key": SECRET},
            },
        )
        check("connection created", status in (200, 201), body[:120])
        status, body = http(
            port,
            "PUT",
            "/api/settings",
            {"brand_name": BRAND, "rtk": False},
        )
        check("settings written", status == 200, body[:120])
        stop(proc)
        proc = None

        for name in ("rrouter.db", "master.key", "settings.db", "jwt.key"):
            check(f"live data dir has {name}", os.path.isfile(os.path.join(source, name)))

        # --- 2. snapshot -------------------------------------------------------------
        env = {**os.environ, "RROUTER_DATA_DIR": source}
        result = subprocess.run(
            ["sh", "scripts/backup.sh", snapshot], env=env, capture_output=True, text=True
        )
        check("backup.sh exits 0", result.returncode == 0, result.stderr.strip()[:200])

        for name in ("rrouter.db", "master.key", "settings.db", "jwt.key", "MANIFEST", "SHA256SUMS"):
            check(f"snapshot has {name}", os.path.isfile(os.path.join(snapshot, name)))

        # The manifest must name every file, and SHA256SUMS must actually match.
        manifest = open(os.path.join(snapshot, "MANIFEST"), encoding="utf-8").read()
        listed = [
            line.split("\t")[0]
            for line in manifest.splitlines()
            if line and not line.startswith("#")
        ]
        on_disk = sorted(
            name
            for name in os.listdir(snapshot)
            if name not in ("MANIFEST", "SHA256SUMS")
        )
        check("manifest lists every snapshot file", sorted(listed) == on_disk, str(listed))
        sums = {}
        for line in open(os.path.join(snapshot, "SHA256SUMS"), encoding="utf-8"):
            digest, name = line.split(None, 1)
            sums[name.strip()] = digest
        mismatches = [
            name
            for name, digest in sums.items()
            if sha256(os.path.join(snapshot, name)) != digest
        ]
        check("checksums match the files", not mismatches, str(mismatches))
        check("SHA256SUMS covers every file", sorted(sums) == on_disk)

        # A snapshot holds encrypted credentials, hashed keys and the session-signing
        # key: it must be unreadable by other local users, whatever the umask was.
        dir_mode = os.stat(snapshot).st_mode & 0o777
        check("snapshot directory is 0700", dir_mode == 0o700, oct(dir_mode))
        for name in (
            "rrouter.db",
            "settings.db",
            "master.key",
            "jwt.key",
            "MANIFEST",
            "SHA256SUMS",
        ):
            mode = os.stat(os.path.join(snapshot, name)).st_mode & 0o777
            check(f"snapshot {name} is 0600", mode == 0o600, oct(mode))
        # Nothing else in the snapshot may be readable either (ext assets, setup code).
        loose = [
            name
            for name in on_disk
            if os.stat(os.path.join(snapshot, name)).st_mode & 0o077
        ]
        check("no snapshot file is group/world accessible", not loose, str(loose))
        check(
            "plaintext credential absent from the snapshot DB",
            SECRET.encode() not in open(os.path.join(snapshot, "rrouter.db"), "rb").read(),
        )

        # --- 3. restore into a fresh data dir ---------------------------------------
        for name in on_disk:
            shutil.copy2(os.path.join(snapshot, name), os.path.join(restored, name))

        port = free_port()
        proc = start(restored, port)
        status, body = http(port, "GET", "/api/connections")
        connections = json.loads(body)["data"] if status == 200 else []
        restored_connection = next(
            (c for c in connections if c.get("name") == "restore-drill"), None
        )
        check("restored connection is present", restored_connection is not None)
        check(
            "restored credential decrypts",
            bool(restored_connection and restored_connection.get("has_api_key")),
            json.dumps(restored_connection or {})[:160],
        )
        check("restored credential is not echoed", SECRET not in body)

        status, body = http(port, "GET", "/api/settings")
        settings = json.loads(body) if status == 200 else {}
        check("restored brand_name", settings.get("brand_name") == BRAND, str(settings)[:160])
        check("restored rtk flag", settings.get("rtk") is False, str(settings)[:160])
    finally:
        if proc is not None:
            stop(proc)
        if "--keep" in sys.argv:
            print(f"kept {root}", file=sys.stderr)
        else:
            shutil.rmtree(root, ignore_errors=True)

    failures = checks.count(False)
    print(f"\nRESULT: {len(checks) - failures}/{len(checks)} checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
