#!/usr/bin/env python3
"""Acceptance smoke for the model-preset API (`/api/model-presets`).

Run against a live gateway (admin authority required — loopback or
`RROUTER_ADMIN_TOKEN`). The script exercises the full contract the dashboard uses:

  GET    /api/model-presets                    -> { data, active_id, default_id }
  GET    /api/model-presets/{id}               -> preset (layout + revision)
  PUT    /api/model-presets/{id}/layout        -> preset  (CAS: { revision, layout })
  POST   /api/model-presets                    -> 201 preset ({ name, layout? })
  POST   /api/model-presets/{id}/duplicate     -> 201 preset ({ name })
  PATCH  /api/model-presets/{id}               -> preset  ({ name?, description? })
  DELETE /api/model-presets/{id}               -> { deleted, id }
  POST   /api/model-presets/{id}/default       -> preset  (admin; demotes the old default)
  POST   /api/model-presets/{id}/activate      -> { active_id, persisted } (per-account)
  GET    /api/model-presets/active             -> preset this caller resolves to

A layout is `{ schema_version: 1, auto_include_new, items: [{ provider, id, position,
visible }] }`; positions are dense from 0 inside each visibility group. The default preset
cannot be deleted; an admin moves the flag with `POST /{id}/default`, which demotes the
previous default in the same transaction. Activation is a per-account preference, so an
admin-token caller has none and resolves to the default.

It is read-mostly and self-cleaning: it creates one preset, verifies order/hide/restore,
revision conflicts, duplicate, activate, default protection, then restores the original
active/default and deletes what it created.

Exit codes (stable for CI):
  0  every check passed
  1  a check failed / unexpected response
  3  the API is not served yet (404 on the collection): report as blocked-by-API, not a
     product failure. The UI shows "presets unavailable" in that state.
  4  authentication/authority problem (401/403)

Usage:
  python3 scripts/model_presets_smoke.py \
      [--base-url http://127.0.0.1:20128] [--token "$RROUTER_ADMIN_TOKEN"] [--timeout 10]

Nothing here writes routing state: hiding a preset row is a layout flag and must never
touch the routing `disabled_models` set. The script asserts the registry is unchanged, but
does not call the disable endpoint (that would mutate routing).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
import uuid

EXIT_OK = 0
EXIT_FAIL = 1
EXIT_API_MISSING = 3
EXIT_AUTH = 4


class CheckError(Exception):
    pass


class Api:
    def __init__(self, base_url: str, token: str, timeout: float) -> None:
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.timeout = timeout

    def request(self, method: str, path: str, body: dict | None = None) -> tuple[int, dict | str | None]:
        url = f"{self.base_url}{path}"
        data = None if body is None else json.dumps(body).encode()
        headers = {"accept": "application/json"}
        if data is not None:
            headers["content-type"] = "application/json"
        if self.token:
            headers["authorization"] = f"Bearer {self.token}"
        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                raw = resp.read()
                return resp.status, self._decode(raw)
        except urllib.error.HTTPError as err:
            raw = err.read()
            return err.code, self._decode(raw)
        except urllib.error.URLError as err:
            raise CheckError(f"cannot reach {url}: {err.reason}") from err

    @staticmethod
    def _decode(raw: bytes) -> dict | str | None:
        if not raw:
            return None
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return raw.decode("utf-8", "replace")

    def get(self, path: str) -> tuple[int, dict | str | None]:
        return self.request("GET", path)

    def put(self, path: str, body: dict) -> tuple[int, dict | str | None]:
        return self.request("PUT", path, body)

    def post(self, path: str, body: dict | None = None) -> tuple[int, dict | str | None]:
        return self.request("POST", path, body)

    def patch(self, path: str, body: dict) -> tuple[int, dict | str | None]:
        return self.request("PATCH", path, body)

    def delete(self, path: str) -> tuple[int, dict | str | None]:
        return self.request("DELETE", path)


def ok(step: str) -> None:
    print(f"PASS  {step}")


def expect(condition: bool, step: str, detail: object = "") -> None:
    if not condition:
        raise CheckError(f"{step}: {detail}")


def expect_status(
    got: int, want: int | tuple[int, ...], step: str, body: object = None
) -> None:
    wants = (want,) if isinstance(want, int) else want
    if got not in wants:
        raise CheckError(f"{step}: expected HTTP {wants}, got {got}: {body}")


def error_code(body: object) -> str | None:
    if isinstance(body, dict):
        error = body.get("error")
        if isinstance(error, dict):
            code = error.get("code")
            return code if isinstance(code, str) else None
    return None


def get_index(api: Api) -> dict:
    status, body = api.get("/api/model-presets")
    expect_status(status, 200, "GET /api/model-presets")
    expect(isinstance(body, dict), "index is an object", body)
    for field in ("data", "active_id", "default_id"):
        expect(field in body, f"index has {field}", body)
    expect(isinstance(body["data"], list), "data is an array", body)
    return body


def get_doc(api: Api, preset_id: str) -> dict:
    status, body = api.get(f"/api/model-presets/{preset_id}")
    expect_status(status, 200, f"GET preset {preset_id}", body)
    expect(isinstance(body, dict), "preset is an object", body)
    for field in ("id", "name", "revision", "layout", "is_default"):
        expect(field in body, f"preset has {field}", body)
    expect(isinstance(body["revision"], int), "revision is an integer", body)
    layout = body["layout"]
    expect(isinstance(layout, dict), "layout is an object", body)
    expect(layout.get("schema_version") == 1, "layout schema_version is 1", layout)
    expect(isinstance(layout.get("items"), list), "layout items is an array", layout)
    return body


def make_layout(items: list[dict], auto: bool) -> dict:
    """Dense positions per visibility group, as the store validates."""
    visible = [item for item in items if item["visible"]]
    hidden = [item for item in items if not item["visible"]]
    out = []
    for group in (visible, hidden):
        for position, item in enumerate(group):
            out.append(
                {
                    "provider": item["provider"],
                    "id": item["id"],
                    "position": position,
                    "visible": item["visible"],
                }
            )
    return {"schema_version": 1, "auto_include_new": auto, "items": out}


def layout_items(doc: dict) -> list[dict]:
    return doc["layout"]["items"]


def visible_items(doc: dict) -> list[dict]:
    items = [item for item in layout_items(doc) if item.get("visible")]
    return sorted(items, key=lambda item: item.get("position", 0))


def save_layout(api: Api, preset_id: str, layout: dict, revision: int) -> dict:
    status, body = api.put(f"/api/model-presets/{preset_id}/layout", {"revision": revision, "layout": layout})
    expect_status(status, 200, f"PUT layout {preset_id}", body)
    expect(isinstance(body, dict) and isinstance(body.get("revision"), int), "layout save returns the preset", body)
    expect(body["revision"] > revision, "revision advances on save", body)
    return body


def registry_refs(api: Api) -> list[tuple[str, str, str]]:
    """(ref, provider, id) for every routable registry row."""
    status, body = api.get("/api/models")
    expect_status(status, 200, "GET /api/models")
    expect(isinstance(body, dict) and isinstance(body.get("data"), list), "registry list shape", body)
    rows = []
    for row in body["data"]:
        if not isinstance(row, dict):
            continue
        ref = row.get("ref")
        provider = row.get("provider")
        model_id = row.get("id")
        if isinstance(ref, str) and isinstance(provider, str) and isinstance(model_id, str):
            rows.append((ref, provider, model_id))
    return rows


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base-url", default=os.environ.get("RROUTER_URL", "http://127.0.0.1:20128"))
    parser.add_argument("--token", default=os.environ.get("RROUTER_ADMIN_TOKEN", ""))
    parser.add_argument("--timeout", type=float, default=10.0)
    args = parser.parse_args()

    api = Api(args.base_url, args.token, args.timeout)
    created: list[str] = []
    original_active: str | None = None
    original_default: str | None = None
    promoted_default: str | None = None

    def cleanup() -> None:
        # Best effort: restore the default this run moved, then delete what it created. The
        # default and each account's active preset are server/account state; the script
        # only ever activates a preset it created, and an admin-token caller's activation
        # is not persisted.
        if promoted_default is not None and original_default:
            try:
                api.post(f"/api/model-presets/{original_default}/default", {})
            except CheckError:
                pass
        for leftover in list(created):
            try:
                api.delete(f"/api/model-presets/{leftover}")
                created.remove(leftover)
            except CheckError:
                pass

    try:
        status, body = api.get("/api/model-presets")
        served = isinstance(body, dict) and "data" in body and "default_id" in body
        fallback = isinstance(body, str) and "<html" in body.lower()
        if status == 404 or (status == 200 and not served and fallback):
            print(
                "BLOCKED-BY-API  GET /api/model-presets is not served by this build\n"
                f"  (HTTP {status}: {'dashboard fallback' if fallback else body!r}).\n"
                "  The preset routes are not registered yet, so the dashboard reports\n"
                "  \"presets unavailable\". This script encodes the gateway/store contract\n"
                "  (schema v30) and runs unchanged once the routes are mounted.",
                file=sys.stderr,
            )
            return EXIT_API_MISSING
        if status in (401, 403):
            print(
                f"BLOCKED-BY-AUTH  GET /api/model-presets returned {status}: {body}\n"
                "  Use a loopback URL or pass --token/RROUTER_ADMIN_TOKEN with admin authority.",
                file=sys.stderr,
            )
            return EXIT_AUTH
        expect_status(status, 200, "GET /api/model-presets", body)
        index = body
        presets = index["data"]
        expect(isinstance(presets, list) and presets, "index has at least one preset", index)
        ids = [p.get("id") for p in presets if isinstance(p, dict)]
        expect(all(isinstance(i, str) and i for i in ids), "every preset has an id", ids)
        original_active = index["active_id"]
        original_default = index["default_id"]
        expect(original_default in ids, "default_id names a listed preset", index)
        expect(original_active in ids, "active_id names a listed preset", index)
        defaults = [p for p in presets if isinstance(p, dict) and p.get("is_default")]
        expect(len(defaults) == 1, "exactly one preset is marked default", defaults)
        expect(defaults[0].get("id") == original_default, "is_default and default_id agree", index)
        for preset in presets:
            expect("is_default" in preset, "every summary carries is_default", preset)
            expect("layout" not in preset, "list rows omit the layout document", preset)
        ok("index lists preset metadata with one active and one default")

        status, body = api.delete(f"/api/model-presets/{original_default}")
        expect(status == 409, "deleting the default preset is refused", (status, body))
        expect(error_code(body) == "preset_is_default", "refusal names preset_is_default", body)
        ok("default preset delete refused with preset_is_default")

        registry = registry_refs(api)
        refs_before = [row[0] for row in registry]
        # A fresh gateway can have no provider connections, so the registry is empty. The
        # preset API does not require layout refs to be routable, so the layout checks still
        # run on synthetic refs; the routing-independence assertions are skipped and
        # reported, because there is no routing surface to compare.
        registry_checks = len(registry) >= 3
        if registry_checks:
            seed_source = [(ref, provider, model_id) for ref, provider, model_id in registry[:3]]
        else:
            seed_source = [
                ("openai/gpt-4.1", "openai", "gpt-4.1"),
                ("anthropic/claude-sonnet-4-5", "anthropic", "claude-sonnet-4-5"),
                ("openai/gpt-4.1-mini", "openai", "gpt-4.1-mini"),
            ]
            print(
                "SKIP  registry has fewer than three models; using synthetic layout refs and "
                "skipping routing-independence assertions",
                file=sys.stderr,
            )

        # ----- create with an explicit seeded layout -----------------------------
        name = f"acceptance-{uuid.uuid4().hex[:8]}"
        seed = [
            {"provider": provider, "id": model_id, "visible": True}
            for _, provider, model_id in seed_source
        ]
        status, body = api.post(
            "/api/model-presets",
            {"name": name, "layout": make_layout(seed, True)},
        )
        expect_status(status, 201, "POST /api/model-presets", body)
        preset_id = body.get("id")
        expect(isinstance(preset_id, str) and preset_id, "create returns an id", body)
        created.append(preset_id)
        doc = get_doc(api, preset_id)
        expect(doc["name"] == name, "created name is echoed", doc)
        expect(len(visible_items(doc)) == 3, "seeded layout round-trips", doc)
        expect(doc["layout"]["auto_include_new"] is True, "auto_include_new round-trips", doc)
        ok(f"created preset {preset_id} with a seeded layout")

        # ----- order round trip --------------------------------------------------
        current = visible_items(doc)
        reversed_items = [
            {"provider": item["provider"], "id": item["id"], "visible": True}
            for item in reversed(current)
        ]
        hidden = [
            {"provider": item["provider"], "id": item["id"], "visible": False}
            for item in layout_items(doc)
            if not item["visible"]
        ]
        saved = save_layout(api, preset_id, make_layout(reversed_items + hidden, True), doc["revision"])
        doc = get_doc(api, preset_id)
        order = [(item["provider"], item["id"]) for item in visible_items(doc)]
        expect(order == [(i["provider"], i["id"]) for i in reversed_items], "reorder persisted", doc)
        expect(doc["revision"] == saved["revision"], "GET sees the saved revision", doc)
        ok("reorder persists through the layout PUT")

        # ----- hide and restore (layout only, never routing) ---------------------
        first = visible_items(doc)[0]
        hidden_item = {"provider": first["provider"], "id": first["id"], "visible": False}
        rest = [
            {"provider": item["provider"], "id": item["id"], "visible": True}
            for item in visible_items(doc)[1:]
        ]
        saved = save_layout(api, preset_id, make_layout(rest + [hidden_item], True), doc["revision"])
        doc = get_doc(api, preset_id)
        entry = next(
            item
            for item in layout_items(doc)
            if (item["provider"], item["id"]) == (hidden_item["provider"], hidden_item["id"])
        )
        expect(entry["visible"] is False, "hidden item reports visible=false", entry)
        if registry_checks:
            refs_after_hide = [row[0] for row in registry_refs(api)]
            expect(refs_after_hide == refs_before, "hiding does NOT change the routing registry", refs_after_hide)
            registry_ref = f"{hidden_item['provider']}/{hidden_item['id']}"
            expect(registry_ref in refs_after_hide, "hidden model stays routable in the registry", registry_ref)

        restored = [
            {"provider": item["provider"], "id": item["id"], "visible": True}
            for item in layout_items(doc)
        ]
        saved = save_layout(api, preset_id, make_layout(restored, True), doc["revision"])
        doc = get_doc(api, preset_id)
        entry = next(
            item
            for item in layout_items(doc)
            if (item["provider"], item["id"]) == (hidden_item["provider"], hidden_item["id"])
        )
        expect(entry["visible"] is True, "restore flips visible back", doc)
        ok("hide/restore is layout-only; routing registry unchanged")

        # ----- revision conflict (CAS) -------------------------------------------
        stale = doc["revision"]
        status, body = api.put(
            f"/api/model-presets/{preset_id}/layout",
            {"revision": stale - 1, "layout": doc["layout"]},
        )
        expect_status(status, 409, "stale revision is refused", body)
        expect(
            error_code(body) in ("preset_conflict", "revision_conflict"),
            "409 names a revision conflict",
            body,
        )
        expect(
            isinstance(body.get("error"), dict) and body["error"].get("current_revision") == stale,
            "409 carries current_revision",
            body,
        )
        after = get_doc(api, preset_id)
        expect(after["revision"] == stale, "a refused save does not move the revision", after)
        ok("stale revision answers 409 preset_conflict and changes nothing")

        # ----- duplicate ---------------------------------------------------------
        dup_name = f"{name}-copy"
        status, body = api.post(f"/api/model-presets/{preset_id}/duplicate", {"name": dup_name})
        expect_status(status, 201, "duplicate preset", body)
        dup_id = body.get("id")
        expect(isinstance(dup_id, str) and dup_id != preset_id, "duplicate has a new id", body)
        created.append(dup_id)
        dup = get_doc(api, dup_id)
        expect(dup["name"] == dup_name, "duplicate name is echoed", dup)
        expect(
            [(i["provider"], i["id"], i["position"], i["visible"]) for i in layout_items(dup)]
            == [(i["provider"], i["id"], i["position"], i["visible"]) for i in layout_items(doc)],
            "duplicate copies the layout in order",
            dup,
        )
        ok(f"duplicated preset {dup_id} with identical layout")

        # ----- rename + description ----------------------------------------------
        status, body = api.patch(
            f"/api/model-presets/{preset_id}",
            {"name": name + "-renamed", "description": "acceptance smoke"},
        )
        expect_status(status, 200, "PATCH preset", body)
        expect(body.get("name") == name + "-renamed", "rename applied", body)
        expect(body.get("description") == "acceptance smoke", "description applied", body)
        expect(body.get("revision") == doc["revision"], "metadata edit leaves the revision alone", body)
        ok("rename and description applied without touching the revision")

        # ----- activate (per-account preference) ---------------------------------
        status, body = api.post(f"/api/model-presets/{preset_id}/activate", {})
        expect_status(status, 200, "activate preset", body)
        expect("active_id" in body and isinstance(body.get("persisted"), bool), "activate response shape", body)
        if body["persisted"]:
            expect(body["active_id"] == preset_id, "session activation persists the choice", body)
            ok("activation persisted for the signed-in account")
        else:
            # An admin-token/loopback caller has no dashboard account: the API reports the
            # default it resolves to and stores nothing. The browser checklist covers the
            # session path (CSRF + per-account persistence).
            expect(body["active_id"] == original_default, "token caller resolves to the default", body)
            ok("activation is a no-op for a token caller (per-account preference unchanged)")

        # ----- default promotion (admin library flag) ----------------------------
        status, body = api.post(f"/api/model-presets/{preset_id}/default", {})
        expect_status(status, 200, "promote preset to default", body)
        expect(
            body.get("id") == preset_id and body.get("is_default") is True,
            "promotion returns the updated preset",
            body,
        )
        promoted_default = preset_id
        status, index = api.get("/api/model-presets")
        expect_status(status, 200, "GET index after promotion", index)
        expect(
            index.get("default_id") == preset_id,
            "index default_id follows the promotion",
            index,
        )
        defaults = [p for p in index["data"] if isinstance(p, dict) and p.get("is_default")]
        expect(
            len(defaults) == 1 and defaults[0].get("id") == preset_id,
            "exactly one default after promotion",
            defaults,
        )
        for preset in index["data"]:
            expect("entry_count" in preset and "visible_count" in preset,
                   "every summary carries entry_count/visible_count", preset)
        ok("default promoted through the API; previous default demoted")

        status, body = api.post(f"/api/model-presets/{original_default}/default", {})
        expect_status(status, 200, "restore original default", body)
        promoted_default = None
        status, index = api.get("/api/model-presets")
        expect(
            index.get("default_id") == original_default,
            "original default restored",
            index,
        )
        ok("original default restored")

        # ----- delete ------------------------------------------------------------
        status, body = api.delete(f"/api/model-presets/{dup_id}")
        expect_status(status, 200, "delete duplicate", body)
        expect(body.get("deleted") is True, "delete echoes deleted", body)
        created.remove(dup_id)
        status, _ = api.get(f"/api/model-presets/{dup_id}")
        expect_status(status, 404, "deleted preset is gone")
        ok("delete removes a non-default preset")

        if registry_checks:
            refs_after_all = [row[0] for row in registry_refs(api)]
            expect(refs_after_all == refs_before, "registry unchanged for the whole run", refs_after_all)
            ok("routing registry unchanged across the whole acceptance run")
        else:
            print("SKIP  routing registry unchanged (no registry models on this instance)", file=sys.stderr)

        print("\nALL CHECKS PASSED")
        return EXIT_OK
    except CheckError as err:
        print(f"FAIL  {err}", file=sys.stderr)
        return EXIT_FAIL
    finally:
        cleanup()


if __name__ == "__main__":
    sys.exit(main())
