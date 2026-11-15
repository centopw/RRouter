#!/usr/bin/env python3
"""Validate the upstream catalogs RRouter depends on, without a Rust build.

Two inputs drift independently of this repository:

* `https://models.dev/api.json` — the catalog the gateway mirrors into its `synced`
  rows (daily, and on `POST /api/models/sync`).
* `providers/community/*.json` — the reviewed manifests `crates/rrouter-auth/build.rs`
  compiles into the provider table.

The scheduled workflow `.github/workflows/upstream-sync.yml` runs this script and opens a
single issue when either drifts. It only reports: nothing here edits, merges or deploys.

Usage:
  python3 scripts/validate-upstreams.py                # fetch models.dev, check manifests
  python3 scripts/validate-upstreams.py --offline      # manifests only, no network
  python3 scripts/validate-upstreams.py --file doc.json  # validate a saved fixture

Exit status is 0 only when every check passes; each problem is one line on stderr.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
COMMUNITY_DIR = ROOT / "providers" / "community"
SCHEMA_PATH = ROOT / "providers" / "schema.json"

MODELS_DEV_URL = "https://models.dev/api.json"
# models.dev serves anonymous JSON; a named agent keeps the request identifiable.
USER_AGENT = "RRouter-upstream-validator (+https://github.com/centopw/RRouter)"

# Field names that would mean a public manifest is carrying (or inviting) a credential.
# Matched against whole key names, normalized to lowercase with `-` folded onto `_`, so
# legitimate keys such as `max_tokens_key` and `default_max_tokens` never match.
CREDENTIAL_KEYS = frozenset(
    {
        "api_key",
        "apikey",
        "authorization",
        "auth_token",
        "access_token",
        "refresh_token",
        "client_secret",
        "token",
        "secret",
        "password",
        "credentials",
        "credential",
    }
)

# URL-shaped keys must be literal https endpoints: a template would point somewhere no
# reviewer has seen, and `check_url` in `manifest.rs` refuses it at build time anyway.
TEMPLATE_CHARS = set(" \t\r\n{}<>$")


def fetch_models_dev(url: str, timeout: float) -> Any:
    request = urllib.request.Request(
        url, headers={"User-Agent": USER_AGENT, "Accept": "application/json"}
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read())


def validate_models_dev(document: Any) -> tuple[list[str], int, int]:
    """Problems, provider count and model count for one models.dev document.

    The gateway's `models_registry::validate_models_dev` applies the same rules at
    runtime, so a document this script accepts is one a sync can parse.
    """
    problems: list[str] = []
    if not isinstance(document, dict):
        return ["models.dev: document is not a JSON object"], 0, 0
    if not document:
        return ["models.dev: document lists no providers"], 0, 0

    models = 0
    for provider_id, provider in document.items():
        if not provider_id:
            problems.append("models.dev: a provider has an empty id")
        if not isinstance(provider, dict):
            problems.append(f"models.dev: provider `{provider_id}` is not an object")
            continue
        listed = provider.get("models")
        if not isinstance(listed, dict):
            problems.append(f"models.dev: provider `{provider_id}` has no `models` object")
            continue
        for model_id, entry in listed.items():
            if not model_id:
                problems.append(f"models.dev: provider `{provider_id}` has an empty model id")
            if not isinstance(entry, dict):
                problems.append(
                    f"models.dev: provider `{provider_id}` model `{model_id or '<empty>'}` "
                    "is not an object"
                )
        models += len(listed)

    if models == 0:
        problems.append("models.dev: document lists no models")
    return problems, len(document), models


def credential_problems(document: Any, label: str) -> list[str]:
    """Keys anywhere in the manifest that name a credential; manifests are public."""
    problems: list[str] = []

    def walk(value: Any, trail: str) -> None:
        if isinstance(value, dict):
            for key, inner in value.items():
                here = f"{trail}.{key}" if trail else str(key)
                normalized = str(key).strip().lower().replace("-", "_")
                if normalized in CREDENTIAL_KEYS:
                    problems.append(
                        f"{label}: `{here}` names a credential; manifests are public and "
                        "credentials belong in the connection store"
                    )
                walk(inner, here)
        elif isinstance(value, list):
            for index, item in enumerate(value):
                walk(item, f"{trail}[{index}]")

    walk(document, "")
    return problems


def check_url(key: str, value: Any) -> list[str]:
    if not isinstance(value, str):
        return [f"`{key}` must be a string URL"]
    problems = []
    if not value.startswith("https://"):
        problems.append(f"`{key}` must be an https:// URL, got `{value}`")
    if any(ch in TEMPLATE_CHARS for ch in value):
        problems.append(f"`{key}` must be a literal URL, not a template: `{value}`")
    return problems


def validate_manifest(path: Path, document: Any, schema: dict[str, Any]) -> list[str]:
    label = str(path.relative_to(ROOT))
    if not isinstance(document, dict):
        return [f"{label}: manifest is not a JSON object"]

    properties: dict[str, Any] = schema.get("properties", {})
    problems: list[str] = []

    for key in schema.get("required", []):
        if key not in document:
            problems.append(f"{label}: missing required key `{key}`")

    # `additionalProperties: false`: a key the compiler ignores is a reviewed setting
    # that silently never takes effect.
    for key in document:
        if key not in properties:
            problems.append(f"{label}: unknown key `{key}` (see providers/schema.json)")

    manifest_id = document.get("id")
    pattern = properties.get("id", {}).get("pattern")
    if "id" in document:
        if not isinstance(manifest_id, str):
            problems.append(f"{label}: `id` must be a string")
        elif pattern and not re.fullmatch(pattern, manifest_id):
            problems.append(f"{label}: `id` `{manifest_id}` must match {pattern}")

    display = document.get("display")
    if "display" in document and (not isinstance(display, str) or not display.strip()):
        problems.append(f"{label}: `display` must be a non-empty string")

    # Every URL-shaped key is an endpoint the gateway calls or links to.
    for key, value in document.items():
        if key == "base_url" or key.endswith("_url"):
            problems.extend(f"{label}: {problem}" for problem in check_url(key, value))

    base_url = document.get("base_url")
    if isinstance(base_url, str) and base_url.endswith("/"):
        problems.append(f"{label}: `base_url` must not end with a slash")

    if isinstance(document.get("base_url_requires_override"), str):
        problems.append(f"{label}: `base_url_requires_override` must be a boolean")
    if isinstance(document.get("stream_only"), str):
        problems.append(f"{label}: `stream_only` must be a boolean")

    for key in ("auth", "default_dialect", "max_tokens_key"):
        if key not in document:
            continue
        allowed = properties.get(key, {}).get("enum")
        value = document[key]
        if allowed is not None and value not in allowed:
            problems.append(
                f"{label}: `{key}` `{value}` is not one of {', '.join(map(str, allowed))}"
            )

    auth_header = document.get("auth_header")
    if "auth_header" in document:
        header_pattern = properties.get("auth_header", {}).get("pattern")
        if not isinstance(auth_header, str):
            problems.append(f"{label}: `auth_header` must be a string")
        elif header_pattern and not re.fullmatch(header_pattern, auth_header):
            problems.append(
                f"{label}: `auth_header` `{auth_header}` must match {header_pattern}"
            )
        elif document.get("auth") != "api-key":
            # Mirrors manifest.rs: the header is only meaningful for `auth: api-key`.
            problems.append(f"{label}: `auth_header` is only valid with auth: api-key")

    default_max_tokens = document.get("default_max_tokens")
    if "default_max_tokens" in document:
        if (
            not isinstance(default_max_tokens, int)
            or isinstance(default_max_tokens, bool)
            or default_max_tokens < 1
        ):
            problems.append(f"{label}: `default_max_tokens` must be a positive integer")

    passthrough = document.get("passthrough_headers")
    if "passthrough_headers" in document:
        if not isinstance(passthrough, list) or not all(
            isinstance(item, str) for item in passthrough
        ):
            problems.append(f"{label}: `passthrough_headers` must be an array of strings")
        elif len(set(passthrough)) != len(passthrough):
            problems.append(f"{label}: `passthrough_headers` must not repeat a header")

    problems.extend(credential_problems(document, label))
    return problems


def load_schema() -> dict[str, Any]:
    try:
        schema = json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"{SCHEMA_PATH.relative_to(ROOT)}: cannot read schema: {error}")
    if not isinstance(schema, dict):
        raise SystemExit(f"{SCHEMA_PATH.relative_to(ROOT)}: schema is not a JSON object")
    return schema


def validate_manifests(schema: dict[str, Any]) -> tuple[list[str], int]:
    problems: list[str] = []
    paths = sorted(COMMUNITY_DIR.glob("*.json"))
    if not paths:
        return [f"{COMMUNITY_DIR.relative_to(ROOT)}: no manifests found"], 0

    seen_ids: dict[str, str] = {}
    for path in paths:
        label = str(path.relative_to(ROOT))
        try:
            document = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            problems.append(f"{label}: cannot parse: {error}")
            continue

        problems.extend(validate_manifest(path, document, schema))
        if isinstance(document, dict):
            manifest_id = document.get("id")
            if isinstance(manifest_id, str) and manifest_id:
                if manifest_id in seen_ids:
                    problems.append(
                        f"{label}: id `{manifest_id}` collides with {seen_ids[manifest_id]}"
                    )
                else:
                    seen_ids[manifest_id] = label

    return problems, len(paths)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Validate models.dev and the reviewed provider manifests."
    )
    parser.add_argument(
        "--offline",
        action="store_true",
        help="skip the models.dev fetch and validate manifests only",
    )
    parser.add_argument(
        "--file",
        type=Path,
        metavar="PATH",
        help="validate this models.dev document instead of fetching it (fixtures, CI)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=30.0,
        help="models.dev fetch timeout in seconds (default: 30)",
    )
    args = parser.parse_args(argv)

    if args.offline and args.file:
        parser.error("--offline and --file are mutually exclusive")

    problems: list[str] = []
    models_dev_summary = "skipped (--offline)"

    if not args.offline:
        document: Any = None
        if args.file:
            try:
                document = json.loads(args.file.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError) as error:
                problems.append(f"models.dev: cannot read {args.file}: {error}")
                models_dev_summary = "unreadable fixture"
        else:
            try:
                document = fetch_models_dev(MODELS_DEV_URL, args.timeout)
            except (
                urllib.error.URLError,
                urllib.error.HTTPError,
                OSError,
                ValueError,
            ) as error:
                problems.append(f"models.dev: fetch {MODELS_DEV_URL} failed: {error}")
                models_dev_summary = "fetch failed"

        if document is not None:
            found, providers, models = validate_models_dev(document)
            problems.extend(found)
            models_dev_summary = f"{providers} providers, {models} models"
            if found:
                models_dev_summary += " (invalid)"

    schema = load_schema()
    manifest_problems, manifest_count = validate_manifests(schema)
    problems.extend(manifest_problems)

    for problem in problems:
        print(problem, file=sys.stderr)

    print(f"models.dev         : {models_dev_summary}")
    print(f"manifests validated: {manifest_count}")
    print(f"failures           : {len(problems)}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
