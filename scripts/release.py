#!/usr/bin/env python3
"""Prepare or publish a release from one version source.

Cargo.toml is authoritative. This tool derives the next version from Conventional
Commits since the last `v*` tag, writes every derived carrier (Cargo.lock, Helm
appVersion, README, roadmap, Helm guide), and generates the CHANGELOG section. The
version rules are strict:

  * `feat:`                         -> minor bump (X.Y.0)
  * `fix:`/`perf:`/`docs:`/...      -> patch bump (X.Y.Z)
  * `type!:`, `type(scope)!:` or a  -> major bump (X.0.0)
    `BREAKING CHANGE:` footer

A requested version that is not exactly that bump is refused.

Usage:
  python3 scripts/release.py                 # derive the bump from commits
  python3 scripts/release.py --bump patch    # insist on the commit-derived kind
  python3 scripts/release.py 0.10.0          # explicit; validated against commits
  python3 scripts/release.py --check         # CI gate: carriers + bump + notes
  python3 scripts/release.py --publish       # verify, commit, tag, push

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
    if tag and version != tag[1]:
        try:
            kind = required_kind(commits_since(root, tag[0]))
            if kind is None:
                problems.append(
                    f"{version} is ahead of {tag[1]} but there are no releasable commits"
                )
            else:
                expected = bump_version(tag[1], kind)
                if version != expected:
                    problems.append(
                        f"{version} is not a valid {kind} bump from {tag[1]}; "
                        f"expected {expected} (breaking=major, feat=minor, else patch)"
                    )
        except ValueError as error:
            problems.append(str(error))
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
    root: Path, raw_version: str | None, kind: str | None
) -> tuple[str, list[dict[str, str]]]:
    """The next version, derived from Conventional Commits since the last tag."""
    tag = last_tag(root)
    current = read_version(root)
    commits = commits_since(root, tag[0]) if tag else commits_since(root, None)
    required = required_kind(commits)
    if raw_version:
        version = normalize(raw_version)
        if tag and version != tag[1]:
            expected = bump_version(tag[1], required) if required else None
            if expected is None:
                raise ValueError(
                    f"{version} is ahead of {tag[1]} but there are no releasable commits"
                )
            if version != expected:
                raise ValueError(
                    f"{version} is not a valid {required} bump from {tag[1]}; "
                    f"expected {expected} (breaking=major, feat=minor, else patch)"
                )
        return version, commits
    if kind:
        if not tag:
            return bump_version(current, kind), commits
        if required is None:
            raise ValueError("nothing to release: no conventional commits since the last tag")
        if kind != required:
            raise ValueError(
                f"commits since {tag[0]} require a {required} bump, not {kind}"
            )
        return bump_version(tag[1], required), commits
    if not tag:
        raise ValueError("no previous v* tag; pass the first version explicitly")
    if required is None:
        raise ValueError("nothing to release: no conventional commits since the last tag")
    expected = bump_version(tag[1], required)
    if current == tag[1]:
        return expected, commits
    if current == expected:
        return current, commits
    raise ValueError(
        f"Cargo.toml is {current}; expected {tag[1]} (unreleased) or {expected} "
        f"({required} bump from {tag[1]})"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "version",
        nargs="?",
        help="release version, e.g. 0.10.0 or v0.10.0; omit to let commits decide",
    )
    parser.add_argument(
        "--bump",
        choices=["patch", "minor", "major"],
        help="requested semver bump; must match the commits since the last tag",
    )
    parser.add_argument("--root", type=Path, default=DEFAULT_ROOT)
    parser.add_argument("--check", action="store_true", help="validate only; write nothing")
    parser.add_argument("--publish", action="store_true", help="verify, commit, tag and push")
    parser.add_argument("--no-verify", action="store_true", help="skip scripts/verify.sh")
    args = parser.parse_args()
    if args.version and args.bump:
        parser.error("pass a version or --bump, not both")
    root = args.root.resolve()
    if args.check:
        try:
            version = normalize(args.version) if args.version else read_version(root)
        except ValueError as error:
            print(f"release: {error}", file=sys.stderr)
            return 1
        problems = check(root, version)
        if problems:
            print("release: metadata is stale:", file=sys.stderr)
            for problem in problems:
                print(f"  - {problem}", file=sys.stderr)
            print("release: run ./scripts/release.py (or with --bump)", file=sys.stderr)
            return 1
        print(f"release: {version} metadata is consistent")
        return 0
    try:
        version, commits = resolve_target(root, args.version, args.bump)
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
    print(f"release: {version} metadata is ready; publish with:")
    print(f"  ./scripts/release.py {version} --publish")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
