#!/usr/bin/env python3
"""Extract one release's Markdown notes from CHANGELOG.md.

The release workflow calls this once, in a single `create-release` job, so a matrix of
build legs cannot publish four different bodies (or four duplicated "Full Changelog"
blocks). The section is the `## <version>` block up to the next level-2 heading, with HTML
comments removed.

Usage:
  python3 scripts/release-notes.py v0.4.1            # write the body to stdout
  python3 scripts/release-notes.py v0.4.1 --check    # validate only; no body
  python3 scripts/release-notes.py 0.4.1 --changelog path/to/CHANGELOG.md

Exit codes: 0 success, 1 I/O or usage error, 2 no matching section / empty body.
`--check` is the local dry run used before pushing a tag.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CHANGELOG = REPO_ROOT / "CHANGELOG.md"
COMMENT = re.compile(r"<!--.*?-->", re.DOTALL)


def normalize_version(raw: str) -> str:
    version = raw.strip()
    if version.startswith("v"):
        version = version[1:]
    if not re.fullmatch(r"[0-9][0-9A-Za-z.+-]*", version):
        raise ValueError(f"not a release version: {raw!r}")
    return version


def extract(changelog: str, version: str) -> str:
    heading = re.compile(
        rf"^##\s+\[?v?{re.escape(version)}\]?(?:\s|$).*$",
        re.MULTILINE,
    )
    matches = list(heading.finditer(changelog))
    if not matches:
        raise LookupError(f"CHANGELOG has no '## {version}' section")
    if len(matches) > 1:
        raise LookupError(f"CHANGELOG has {len(matches)} '## {version}' sections")
    start = matches[0].end()
    next_heading = re.compile(r"^##\s+", re.MULTILINE).search(changelog, start)
    end = next_heading.start() if next_heading else len(changelog)
    body = changelog[start:end]
    body = COMMENT.sub("", body)
    lines = [line.rstrip() for line in body.splitlines()]
    while lines and not lines[0].strip():
        lines.pop(0)
    while lines and not lines[-1].strip():
        lines.pop()
    cleaned: list[str] = []
    blank = 0
    for line in lines:
        if line.strip():
            blank = 0
            cleaned.append(line)
        else:
            blank += 1
            if blank <= 1:
                cleaned.append("")
    body = "\n".join(cleaned).strip()
    if not body:
        raise LookupError(f"the '## {version}' section is empty after removing comments")
    return body + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="release tag or version, e.g. v0.4.1 or 0.4.1")
    parser.add_argument("--changelog", type=Path, default=DEFAULT_CHANGELOG)
    parser.add_argument(
        "--check",
        action="store_true",
        help="validate the section exists and is non-empty; do not print it",
    )
    args = parser.parse_args()

    try:
        version = normalize_version(args.version)
    except ValueError as error:
        print(f"release-notes: {error}", file=sys.stderr)
        return 1
    try:
        changelog = args.changelog.read_text(encoding="utf-8")
    except OSError as error:
        print(f"release-notes: cannot read {args.changelog}: {error}", file=sys.stderr)
        return 1
    try:
        body = extract(changelog, version)
    except LookupError as error:
        print(f"release-notes: {error}", file=sys.stderr)
        return 2

    if args.check:
        lines = body.count("\n")
        print(f"ok: {version} ({lines} lines, {len(body)} bytes) in {args.changelog}")
        return 0
    sys.stdout.write(body)
    return 0


if __name__ == "__main__":
    sys.exit(main())
