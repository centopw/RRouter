#!/usr/bin/env python3
"""Generate a CycloneDX 1.5 SBOM for one release artifact from `Cargo.lock`.

Why a script instead of an SBOM action: the release job must not depend on a mutable
third-party service to describe its own build, and the result must be reproducible — run
it twice on the same tag and the bytes are identical, so a published SBOM can be compared
against a fresh one (see `scripts/verify-release.sh --commit`).

The inventory is the locked dependency set (`Cargo.lock`), not just what the linker kept
for one target: that is the honest answer to "what source could be in this binary", and it
is how a CVE feed is matched against a release.

    python3 scripts/sbom.py \
        --artifact rrouter-linux-amd64 \
        --version 0.3.0 \
        --commit 0123456789abcdef... \
        --output dist/rrouter-linux-amd64.tar.gz.sbom.cdx.json

No third-party modules, no network, no timestamps (a timestamp would make the file
irreproducible; build provenance carries the time instead).
"""
import argparse
import json
import re
import sys
import uuid
from pathlib import Path
from urllib.parse import quote

SPEC_VERSION = "1.5"
TOOL_NAME = "scripts/sbom.py"
LOCKFILE = Path("Cargo.lock")

_NAME = re.compile(r'^name = "([^"]+)"$')
_VERSION = re.compile(r'^version = "([^"]+)"$')


def parse_lock(text):
    """Every `[[package]]` name/version pair, in lock order.

    Cargo.lock is a flat TOML document that has been stable for years; parsing the two
    keys we need avoids a TOML dependency and keeps this runnable on any Python 3.
    """
    packages = []
    current = None
    in_package = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped == "[[package]]":
            if current is not None:
                packages.append(current)
            current = {"name": None, "version": None}
            in_package = True
            continue
        if stripped.startswith("[") and stripped != "[[package]]":
            # A new table (`[metadata]`, a nested table) ends the package block.
            in_package = False
            continue
        if not in_package or current is None:
            continue
        match = _NAME.match(stripped)
        if match:
            current["name"] = match.group(1)
            continue
        match = _VERSION.match(stripped)
        if match:
            current["version"] = match.group(1)
    if current is not None:
        packages.append(current)

    complete = []
    for package in packages:
        if package["name"] is None or package["version"] is None:
            raise SystemExit(
                "sbom: a [[package]] block in Cargo.lock has no name or version"
            )
        complete.append((package["name"], package["version"]))
    if not complete:
        raise SystemExit("sbom: Cargo.lock contains no packages")
    return complete


def purl(name, version):
    # purl requires reserved characters in the version to be percent-encoded; crate names
    # are already `[A-Za-z0-9_-]`.
    return f"pkg:cargo/{name}@{quote(version, safe='.-_')}"


def build_bom(artifact, version, commit, packages):
    # The root is also `metadata.component`; keeping its lock entry (when present) is
    # harmless and avoids silently dropping a future package with the same name.
    components = [
        {
            "type": "library",
            "name": name,
            "version": dep_version,
            "purl": purl(name, dep_version),
        }
        for name, dep_version in sorted(packages)
    ]
    if not components:
        raise SystemExit("sbom: no dependency components were found")

    serial = uuid.uuid5(
        uuid.NAMESPACE_URL, f"rrouter:{artifact}:{version}:{commit}"
    )
    return {
        "bomFormat": "CycloneDX",
        "specVersion": SPEC_VERSION,
        "serialNumber": f"urn:uuid:{serial}",
        "version": 1,
        "metadata": {
            "tools": [{"vendor": "RRouter", "name": TOOL_NAME, "version": "1"}],
            "component": {
                "type": "application",
                "name": "rrouter",
                "version": version,
                "purl": purl("rrouter", version),
            },
            "properties": [
                {"name": "rrouter:artifact", "value": artifact},
                {"name": "rrouter:commit", "value": commit},
            ],
        },
        "components": components,
    }


def dumps(bom):
    # Fixed separators and key order: byte-identical output for identical inputs.
    return json.dumps(bom, indent=2, ensure_ascii=True) + "\n"


def validate(rendered, artifact, version):
    """Re-read what we are about to write; a malformed SBOM must never be published."""
    bom = json.loads(rendered)
    assert bom["bomFormat"] == "CycloneDX", "bomFormat"
    assert bom["specVersion"] == SPEC_VERSION, "specVersion"
    assert bom["serialNumber"].startswith("urn:uuid:"), "serialNumber"
    component = bom["metadata"]["component"]
    assert component["version"] == version, "root component version"
    assert any(
        prop["name"] == "rrouter:artifact" and prop["value"] == artifact
        for prop in bom["metadata"]["properties"]
    ), "artifact property"
    assert bom["components"], "components"
    for entry in bom["components"]:
        assert entry["type"] == "library", "component type"
        assert entry["name"] and entry["version"], "component name/version"
        assert entry["purl"].startswith("pkg:cargo/"), "component purl"


def main(argv):
    parser = argparse.ArgumentParser(
        description="Generate a CycloneDX SBOM for one RRouter release artifact."
    )
    parser.add_argument("--artifact", required=True, help="e.g. rrouter-linux-amd64")
    parser.add_argument("--version", required=True, help="release version, e.g. 0.3.0")
    parser.add_argument("--commit", required=True, help="git commit the artifact was built from")
    parser.add_argument("--output", required=True, help="path to write the SBOM to")
    parser.add_argument(
        "--lock",
        default=str(LOCKFILE),
        help="Cargo.lock to inventory (default: ./Cargo.lock)",
    )
    args = parser.parse_args(argv)

    lockfile = Path(args.lock)
    try:
        packages = parse_lock(lockfile.read_text(encoding="utf-8"))
    except OSError as error:
        raise SystemExit(f"sbom: cannot read {lockfile}: {error}")

    bom = build_bom(args.artifact, args.version, args.commit, packages)
    rendered = dumps(bom)
    validate(rendered, args.artifact, args.version)

    output = Path(args.output)
    if output.parent and not output.parent.exists():
        output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(rendered, encoding="utf-8")
    print(
        f"sbom: wrote {output} ({len(bom['components'])} components, "
        f"artifact {args.artifact}, version {args.version})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
