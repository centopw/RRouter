#!/usr/bin/env python3
"""Prepare or publish a release from one version source.

Cargo.toml is authoritative. The next version always comes from the newest `v*` tag
on the current history; no version number is passed by hand. The release type is the
only choice:

  * no flag  -> derive the type from Conventional Commits since the last tag
  * `--patch` -> X.Y.(Z+1)
  * `--minor` -> X.(Y+1).0
  * `--major` / `--breaking` -> (X+1).0.0

`--breaking` is an alias for `--major`: the version rule is the same, and the
CHANGELOG still records which commits were breaking. An explicit type controls the
version; commit messages still generate the notes. Every derived carrier
(Cargo.lock, Helm appVersion, README, roadmap, Helm guide) is updated together.

Usage:
  python3 scripts/release.py --patch          # next patch from the latest tag
  python3 scripts/release.py --minor          # next minor
  python3 scripts/release.py --major          # next major
  python3 scripts/release.py --breaking       # alias for --major
  python3 scripts/release.py                  # derive the type from commits
  python3 scripts/release.py --check          # CI gate: carriers + notes
  python3 scripts/release.py --publish        # verify, commit, tag, push
  python3 scripts/release.py --patch --publish

`--publish` runs scripts/verify.sh first unless `--no-verify` is also passed.
"""

from __future__ import annotations

import argparse
import datetime
import re
import subprocess
import sys
from pathlib import Path

DEFAULT_ROOT = Path(__file__).resolve().parent.parent
SEMVER = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$")
WORKSPACE = re.compile(r"(?ms)^\[workspace\.package\]\s*$(.*?)(?=^\[|\Z)")
LOCK_MEMBER = re.compile(
    r'(?ms)(\[\[package\]\]\s*\nname = "rrouter-[^"]+"\s*\nversion = ")[^"]+(")'
)
CHART_APP = re.compile(r'(?m)^appVersion: "[^"]+"')
README_PIN = re.compile(r"RROUTER_VERSION=v[0-9][0-9A-Za-z.+-]*")
README_NEWEST = re.compile(r"`v[0-9][0-9A-Za-z.+-]*` is the newest")
ROADMAP_CURRENT = re.compile(r"\*\*[0-9][0-9A-Za-z.+-]*\*\* is the current release")
ROADMAP_CHANGELOG = re.compile(r"`CHANGELOG\.md` → `[0-9][0-9A-Za-z.+-]*`")
HELM_TAG = re.compile(r"--set image\.tag=[0-9][0-9A-Za-z.+-]*")


def normalize(raw: str) -> str:
    version = raw.strip()
    if version.startswith("v"):
        version = version[1:]
    if not SEMVER.fullmatch(version):
        raise ValueError(f"not a semver release version: {raw!r}")
    return version


CONVENTIONAL = re.compile(
    r"^(?P<type>[A-Za-z]+)(?:\((?P<scope>[^)]*)\))?(?P<breaking>!)?:\s+(?P<subject>.+)$"
)
BREAKING_FOOTER = re.compile(r"(?mi)^BREAKING[ -]CHANGE:\s*(.+)")
RELEASE_SUBJECT = re.compile(r"^(release:|chore\(release\):|chore: release\b)")
TYPE_GROUPS = {
    "feat": "Features",
    "fix": "Bug Fixes",
    "perf": "Performance",
    "docs": "Documentation",
    "refactor": "Refactoring",
    "build": "Build & CI",
    "ci": "Build & CI",
}


def git_text(root: Path, *args: str, check: bool = True) -> str:
    result = subprocess.run(
        ["git", *args], cwd=root, text=True, check=False, capture_output=True
    )
    if check and result.returncode != 0:
        raise ValueError((result.stderr or result.stdout).strip() or f"git {' '.join(args)} failed")
    return result.stdout


def last_tag(root: Path) -> tuple[str, str] | None:
    tag = git_text(root, "describe", "--tags", "--abbrev=0", "--match", "v*", check=False).strip()
    if not tag:
        return None
    try:
        return tag, normalize(tag)
    except ValueError:
        return None


def commits_since(root: Path, since: str | None) -> list[dict[str, str]]:
    span = f"{since}..HEAD" if since else "HEAD"
    raw = git_text(root, "log", span, "--format=%H%x1f%s%x1f%b%x1e", check=False)
    commits: list[dict[str, str]] = []
    for record in raw.split("\x1e"):
        record = record.strip("\n")
        if not record:
            continue
        parts = record.split("\x1f")
        if len(parts) < 2:
            continue
        commits.append(
            {
                "sha": parts[0],
                "subject": parts[1].strip(),
                "body": parts[2] if len(parts) > 2 else "",
            }
        )
    return commits


def is_release_commit(commit: dict[str, str]) -> bool:
    subject = commit["subject"]
    return subject.startswith("Merge ") or bool(RELEASE_SUBJECT.match(subject))


def parse_commit(commit: dict[str, str]) -> dict[str, str] | None:
    if is_release_commit(commit):
        return None
    match = CONVENTIONAL.match(commit["subject"])
    breaking = bool(match and match.group("breaking")) or bool(
        BREAKING_FOOTER.search(commit["body"])
    )
    if not match:
        return {
            "type": "other",
            "scope": "",
            "subject": commit["subject"],
            "breaking": "true" if breaking else "",
            "sha": commit["sha"],
        }
    return {
        "type": match.group("type").lower(),
        "scope": match.group("scope") or "",
        "subject": match.group("subject").strip(),
        "breaking": "true" if breaking else "",
        "sha": commit["sha"],
    }


def required_kind(commits: list[dict[str, str]]) -> str | None:
    parsed = [item for item in (parse_commit(commit) for commit in commits) if item]
    if not parsed:
        return None
    if any(item["breaking"] for item in parsed):
        return "major"
    if any(item["type"] == "feat" for item in parsed):
        return "minor"
    return "patch"


def bump_version(version: str, kind: str) -> str:
    core = version.split("-", 1)[0]
    major, minor, patch = (int(part) for part in core.split("."))
    if kind == "major":
        return f"{major + 1}.0.0"
    if kind == "minor":
        return f"{major}.{minor + 1}.0"
    if kind == "patch":
        return f"{major}.{minor}.{patch + 1}"
    raise ValueError(f"unknown bump kind {kind!r}")

def newer_than(version: str, base: str) -> bool:
    """A release may be any forward SemVer move from the last tag, not only the
    commit-derived kind: the operator chooses the release type."""
    return tuple(int(part) for part in version.split("-", 1)[0].split(".")) > tuple(
        int(part) for part in base.split("-", 1)[0].split(".")
    )


def repo_url(root: Path) -> str:
    remote = git_text(root, "remote", "get-url", "origin", check=False).strip()
    if not remote:
        return ""
    remote = remote.removesuffix(".git")
    if remote.startswith("git@") and ":" in remote:
        host, path = remote[4:].split(":", 1)
        return f"https://{host}/{path}"
    if remote.startswith("ssh://git@"):
        return "https://" + remote.removeprefix("ssh://git@").replace(":", "/", 1)
    return remote


def changelog_notes(commits: list[dict[str, str]], repo: str) -> str:
    groups: dict[str, list[str]] = {}
    order = [
        "Breaking Changes",
        "Features",
        "Bug Fixes",
        "Performance",
        "Documentation",
        "Refactoring",
        "Build & CI",
        "Other Changes",
    ]
    for commit in commits:
        parsed = parse_commit(commit)
        if not parsed:
            continue
        if parsed["breaking"]:
            group = "Breaking Changes"
        else:
            group = TYPE_GROUPS.get(parsed["type"], "Other Changes")
        short = parsed["sha"][:7]
        link = f"[`{short}`]({repo}/commit/{parsed['sha']})" if repo else f"`{short}`"
        scope = f"({parsed['scope']})" if parsed["scope"] else ""
        if parsed["type"] == "other":
            line = f"- {parsed['subject']} ({link})"
        else:
            line = f"- {parsed['type']}{scope}: {parsed['subject']} ({link})"
        groups.setdefault(group, []).append(line)
    blocks = []
    for group in order:
        lines = groups.pop(group, [])
        if lines:
            blocks.append(f"### {group}\n\n" + "\n".join(lines))
    for group, lines in groups.items():
        blocks.append(f"### {group}\n\n" + "\n".join(lines))
    return "\n\n".join(blocks).strip()


def read_version(root: Path) -> str:
    text = (root / "Cargo.toml").read_text(encoding="utf-8")
    match = WORKSPACE.search(text)
    if not match:
        raise ValueError("Cargo.toml has no [workspace.package] section")
    version = re.search(r'(?m)^version\s*=\s*"([^"]+)"', match.group(1))
    if not version:
        raise ValueError("Cargo.toml [workspace.package] has no version")
    return version.group(1)


def write_version(root: Path, version: str) -> None:
    path = root / "Cargo.toml"
    text = path.read_text(encoding="utf-8")
    match = WORKSPACE.search(text)
    if not match:
        raise ValueError("Cargo.toml has no [workspace.package] section")
    body, count = re.subn(
        r'(?m)^version\s*=\s*"[^"]+"',
        f'version = "{version}"',
        match.group(1),
        count=1,
    )
    if count != 1:
        raise ValueError("Cargo.toml [workspace.package] has no version")
    path.write_text(text[: match.start(1)] + body + text[match.end(1) :], encoding="utf-8")


def update_lock(root: Path, version: str) -> None:
    path = root / "Cargo.lock"
    text = path.read_text(encoding="utf-8")
    text, count = LOCK_MEMBER.subn(rf'\g<1>{version}\g<2>', text)
    if count == 0:
        raise ValueError("Cargo.lock has no rrouter-* workspace packages")
    path.write_text(text, encoding="utf-8")


def replace_file(root: Path, name: str, pattern: re.Pattern[str], replacement: str) -> None:
    path = root / name
    text = path.read_text(encoding="utf-8")
    text, count = pattern.subn(replacement, text)
    if count == 0:
        raise ValueError(f"{name} has no version carrier to update")
    path.write_text(text, encoding="utf-8")


def update_derived(root: Path, version: str) -> None:
    update_lock(root, version)
    replace_file(
        root,
        "deploy/helm/rrouter/Chart.yaml",
        CHART_APP,
        f'appVersion: "{version}"',
    )
    replace_file(root, "README.md", README_PIN, f"RROUTER_VERSION=v{version}")
    replace_file(root, "README.md", README_NEWEST, f"`v{version}` is the newest")
    replace_file(
        root,
        "docs/ROADMAP.md",
        ROADMAP_CURRENT,
        f"**{version}** is the current release",
    )
    replace_file(
        root,
        "docs/ROADMAP.md",
        ROADMAP_CHANGELOG,
        f"`CHANGELOG.md` → `{version}`",
    )
    replace_file(
        root,
        "docs/deploy/helm.md",
        HELM_TAG,
        f"--set image.tag={version}",
    )


def write_changelog(
    root: Path, version: str, commits: list[dict[str, str]], repo: str
) -> bool:
    """Write a conventional-commit section when the version has no notes yet."""
    notes = changelog_notes(commits, repo)
    if not notes:
        raise ValueError(
            "no releasable commits since the last tag; use a conventional commit message "
            "(feat/fix/perf/docs/...)"
        )
    section = f"## {version} — {datetime.date.today().isoformat()}\n\n{notes}\n"
    path = root / "CHANGELOG.md"
    text = path.read_text(encoding="utf-8")
    heading = re.search(
        rf"(?m)^##\s+\[?v?{re.escape(version)}\]?(?:\s|$).*$", text
    )
    if heading:
        rest = text[heading.end() :]
        next_heading = re.search(r"(?m)^##\s+", rest)
        end = heading.end() + next_heading.start() if next_heading else len(text)
        if changelog_has_notes(text[heading.end() : end]):
            return False
        tail = text[end:]
        path.write_text(
            text[: heading.start()] + section + ("\n" + tail if tail else ""),
            encoding="utf-8",
        )
        return True
    first = re.search(r"(?m)^##\s+", text)
    if not first:
        raise ValueError("CHANGELOG.md has no release headings to insert before")
    path.write_text(
        text[: first.start()] + section + "\n" + text[first.start() :], encoding="utf-8"
    )
    return True


def changelog_body(root: Path, version: str) -> str | None:
    text = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    heading = re.search(
        rf"(?m)^##\s+\[?v?{re.escape(version)}\]?(?:\s|$).*$", text
    )
    if not heading:
        return None
    rest = text[heading.end() :]
    next_heading = re.search(r"(?m)^##\s+", rest)
    body = rest[: next_heading.start()] if next_heading else rest
    return re.sub(r"<!--.*?-->", "", body, flags=re.DOTALL)


def changelog_has_notes(body: str | None) -> bool:
    if body is None:
        return False
    for line in body.splitlines():
        stripped = line.strip()
        if stripped and not stripped.startswith("#") and len(stripped) > 2:
            return True
    return False


def release_notes(root: Path, version: str) -> tuple[bool, str]:
    result = subprocess.run(
        [sys.executable, str(root / "scripts/release-notes.py"), version, "--check"],
        cwd=root,
        text=True,
        capture_output=True,
    )
    return result.returncode == 0, (result.stderr or result.stdout).strip()


def expected_files(version: str) -> dict[str, str]:
    return {
        "Cargo.lock": version,
        "deploy/helm/rrouter/Chart.yaml": f'appVersion: "{version}"',
        "README.md": f"v{version}",
        "docs/ROADMAP.md": version,
        "docs/deploy/helm.md": f"image.tag={version}",
    }


def check(root: Path, version: str) -> list[str]:
    problems: list[str] = []
    actual = read_version(root)
    if actual != version:
        problems.append(f"Cargo.toml is {actual}, expected {version}")
    lock = (root / "Cargo.lock").read_text(encoding="utf-8")
    members = re.findall(r'(?ms)\[\[package\]\]\s*\nname = "rrouter-[^"]+"\s*\nversion = "([^"]+)"', lock)
    for found in sorted(set(members)):
        if found != version:
            problems.append(f"Cargo.lock rrouter-* packages are {found}, expected {version}")
    for name, needle in expected_files(version).items():
        path = root / name
        if not path.exists():
            problems.append(f"{name} is missing")
        elif needle not in path.read_text(encoding="utf-8"):
            problems.append(f"{name} does not mention {needle}")
    ok, message = release_notes(root, version)
    if not ok:
        problems.append(message or f"CHANGELOG has no usable '## {version}' section")
    elif not changelog_has_notes(changelog_body(root, version)):
        problems.append(
            f"CHANGELOG '## {version}' section has no notes yet; add at least one line"
        )
    tag = last_tag(root)
    if tag:
        if version == tag[1]:
            if required_kind(commits_since(root, tag[0])) is not None:
                problems.append(
                    f"Cargo.toml is still {tag[1]}; bump the release version before tagging"
                )
        elif not newer_than(version, tag[1]):
            problems.append(
                f"{version} is not newer than the last tag {tag[1]}; "
                "choose the next forward SemVer version"
            )
    return problems


def git(root: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", *args], cwd=root, text=True, check=check, capture_output=True)


def publish(root: Path, version: str, verify: bool) -> int:
    if check(root, version):
        return 1
    status = git(root, "status", "--porcelain").stdout.splitlines()
    allowed = {
        "Cargo.toml",
        "Cargo.lock",
        "CHANGELOG.md",
        "README.md",
        "deploy/helm/rrouter/Chart.yaml",
        "docs/ROADMAP.md",
        "docs/deploy/helm.md",
    }
    untracked = [line[3:] for line in status if line.startswith("??")]
    if untracked:
        print(
            "release: untracked files are not part of the release; add them or ignore them:",
            file=sys.stderr,
        )
        for path in untracked:
            print(f"  {path}", file=sys.stderr)
        return 1
    unexpected = [
        line for line in status if not line.startswith("??") and line[3:] not in allowed
    ]
    if unexpected:
        print("release: commit or stash unrelated changes first:", file=sys.stderr)
        for line in unexpected:
            print(f"  {line}", file=sys.stderr)
        return 1
    if verify:
        print("release: running ./scripts/verify.sh", file=sys.stderr)
        if subprocess.run(["./scripts/verify.sh"], cwd=root).returncode != 0:
            print("release: verification failed; tag not created", file=sys.stderr)
            return 1
    existing = git(root, "tag", "--list", f"v{version}").stdout.strip()
    if existing:
        print(f"release: tag v{version} already exists", file=sys.stderr)
        return 1
    git(root, "add", "-u", *sorted(allowed))
    dirty = git(root, "diff", "--cached", "--quiet", check=False).returncode != 0
    if dirty:
        git(root, "commit", "-m", f"chore(release): v{version}")
    git(root, "tag", "-a", f"v{version}", "-m", f"v{version}")
    git(root, "push", "origin", "main")
    git(root, "push", "origin", f"v{version}")
    print(f"release: pushed v{version}")
    return 0


def resolve_target(
    root: Path, kind: str | None
) -> tuple[str, list[dict[str, str]], str]:
    """Return the next version, its commits, and the release type.

    The latest `v*` tag is the version base. An explicit type always wins; without
    one, Conventional Commits choose the type. A Cargo.toml that already holds the
    expected next version is left as-is, so re-running a prepare is idempotent.
    """
    tag = last_tag(root)
    current = read_version(root)
    commits = commits_since(root, tag[0]) if tag else commits_since(root, None)
    inferred = required_kind(commits)
    release_kind = kind or inferred

    if inferred is None:
        raise ValueError("nothing to release: no conventional commits since the last tag")
    if release_kind is None:
        raise ValueError(
            "no previous v* tag; pass --patch, --minor, --major, or --breaking"
        )

    if not tag:
        return bump_version(current, release_kind), commits, release_kind

    base = tag[1]
    if current == base:
        return bump_version(base, release_kind), commits, release_kind
    expected = bump_version(base, release_kind)
    if current == expected:
        return current, commits, release_kind
    raise ValueError(
        f"Cargo.toml is {current}; expected {base} (unreleased) or {expected} "
        f"({release_kind} bump from {base})"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    bump = parser.add_mutually_exclusive_group()
    bump.add_argument(
        "--patch",
        dest="kind",
        action="store_const",
        const="patch",
        help="patch bump from the latest tag: X.Y.(Z+1)",
    )
    bump.add_argument(
        "--minor",
        dest="kind",
        action="store_const",
        const="minor",
        help="minor bump from the latest tag: X.(Y+1).0",
    )
    bump.add_argument(
        "--major",
        dest="kind",
        action="store_const",
        const="major",
        help="major bump from the latest tag: (X+1).0.0",
    )
    bump.add_argument(
        "--breaking",
        dest="kind",
        action="store_const",
        const="major",
        help="alias for --major; breaking commits are still marked in the CHANGELOG",
    )
    parser.add_argument("--root", type=Path, default=DEFAULT_ROOT)
    parser.add_argument("--check", action="store_true", help="validate only; write nothing")
    parser.add_argument("--publish", action="store_true", help="verify, commit, tag and push")
    parser.add_argument("--no-verify", action="store_true", help="skip scripts/verify.sh")
    args = parser.parse_args()
    root = args.root.resolve()
    if args.check:
        try:
            version = read_version(root)
        except ValueError as error:
            print(f"release: {error}", file=sys.stderr)
            return 1
        problems = check(root, version)
        if problems:
            print("release: metadata is stale:", file=sys.stderr)
            for problem in problems:
                print(f"  - {problem}", file=sys.stderr)
            print(
                "release: run ./scripts/release.py --patch|--minor|--major "
                "(or --breaking)",
                file=sys.stderr,
            )
            return 1
        print(f"release: {version} metadata is consistent")
        return 0
    try:
        version, commits, kind = resolve_target(root, args.kind)
    except ValueError as error:
        print(f"release: {error}", file=sys.stderr)
        return 1
    if args.publish:
        return publish(root, version, verify=not args.no_verify)
    try:
        write_version(root, version)
        update_derived(root, version)
        generated = write_changelog(root, version, commits, repo_url(root))
    except (OSError, ValueError) as error:
        print(f"release: {error}", file=sys.stderr)
        return 1
    if generated:
        print(f"release: generated conventional notes for {version} in CHANGELOG.md")
    problems = check(root, version)
    if problems:
        print("release: version carriers are updated; finish these:", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 2
    print(f"release: {version} ({kind}) metadata is ready; publish with:")
    print(f"  ./scripts/release.py --{kind} --publish")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
