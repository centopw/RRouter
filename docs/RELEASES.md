# Releasing and verifying RRouter

A release is four platform tarballs plus, for every tarball, a checksum, a dependency
inventory, and a GitHub build-provenance attestation. The release **body is rendered once**
from `CHANGELOG.md`; build legs only attach assets. This page says exactly what each piece
proves, what the installer checks, and what a human still has to do.

## 1. What a release contains

For each of `linux-amd64`, `linux-arm64`, `darwin-arm64`, `darwin-amd64`:

| Asset | Purpose |
| --- | --- |
| `rrouter-<asset>.tar.gz` | the binary with `README.md` and `LICENSE` — **names are a contract with `install.sh`** |
| `rrouter-<asset>.tar.gz.sha256` | SHA-256 in `sha256sum -c` form; the installer verifies this |
| `rrouter-<asset>.tar.gz.sbom.cdx.json` | CycloneDX 1.5 inventory of the tag's `Cargo.lock` |
| build provenance | an attestation stored by GitHub, not a file asset |

The SBOM lists the locked dependency set (currently 253 packages), not only the crates the
linker kept for one target. That is the honest answer to "what source could be in this
binary" and what a CVE feed is matched against. Its inventory is reproducible: the same
tag and commit produce byte-identical JSON, because the generator has no timestamp and
sorts its components.

## 2. What is signed, and what that proves

**Signed: the provenance attestation.** `actions/attest-build-provenance` signs a statement
with the workflow's OIDC identity (Sigstore/Fulcio) and stores it in the repository's
attestation store. It binds the tarball's digest to:

- this repository and workflow file,
- the commit that was checked out (`git rev-parse HEAD` at build time),
- the tag/release run it belongs to.

Verify it with GitHub's own verifier — no extra key, no extra tool, no secret:

```sh
gh attestation verify rrouter-linux-amd64.tar.gz --repo centopw/RRouter
```

**Not signed: the SBOM.** It is a published asset whose integrity is covered two ways: it
travels in the same release, and the `verify` job regenerates it from the tagged checkout
and compares byte for byte (`scripts/verify-release.sh --commit <sha>`), so a swapped
inventory fails CI. It is not a vulnerability report; run a scanner against it yourself.

**No detached cosign signature.** Deliberately skipped in this wave:

- the provenance attestation is already a Sigstore-signed binding of the artifact digest —
  a cosign `.sig` would duplicate that trust root;
- keyless cosign adds a second network dependency (`cosign-installer`, Fulcio, Rekor) and
  two extra assets per platform, so a Rekor hiccup would fail a release for a signature
  that proves nothing new;
- the installer does not use it.

If a consumer needs an offline-verifiable detached signature, add it as a **separate,
non-blocking job** (not inside the matrix) once it is proven stable: install
`sigstore/cosign-installer@v3`, run `cosign sign-blob --yes --output-signature X.sig
--output-certificate X.pem X.tar.gz`, upload both, and verify with `cosign verify-blob
--certificate X.pem --signature X.sig --certificate-identity-regexp
'https://github.com/centopw/RRouter/.github/workflows/release.yml@refs/tags/.*'
--certificate-oidc-issuer https://token.actions.githubusercontent.com X.tar.gz`.
Document the failure mode before making it required.

## 3. What the installer verifies

`install.sh` downloads the tarball and its `.sha256` over TLS and checks the digest before
installing. That catches corruption and a truncated proxy response; it does **not** prove
authorship, because the checksum comes from the same channel as the tarball.

For anything beyond a convenience install, verify provenance yourself (§4). The installer
deliberately does not call `gh`: it must work on a minimal host with no GitHub CLI, and it
should not require network access to the GitHub API after the download.

## 4. Verifying a release

One artifact:

```sh
gh attestation verify rrouter-linux-amd64.tar.gz --repo centopw/RRouter
shasum -a 256 -c rrouter-linux-amd64.tar.gz.sha256
```

The whole release, including completeness (all four platforms present) and the SBOMs:

```sh
./scripts/verify-release.sh <tag>
```

Offline, against a directory you downloaded yourself:

```sh
./scripts/verify-release.sh <tag> --dir ./release --skip-download --no-attestation
```

Flags: `--repo owner/name` (defaults to `$GITHUB_REPOSITORY`), `--expect
linux-amd64,linux-arm64,darwin-arm64,darwin-amd64` (update when the matrix changes),
`--commit <sha>` (regenerate and compare the SBOM against this checkout), `--skip-download`,
`--no-attestation`. It exits non-zero on the first failure.

## 5. The pipeline

`release.yml` has four jobs:

| Job | Permissions | Steps |
| --- | --- | --- |
| `prepare` | none | validate the tag is `v*`; publish `tag`/`version` outputs |
| `create-release` (once) | `contents: write` | checkout the tag → `python3 scripts/release-notes.py <tag> --check` → render `.release-notes.md` → create/update the release with `body_path` and `generate_release_notes: false` |
| `binaries` (matrix, 4 targets) | `contents: write`, `id-token: write`, `attestations: write` | checkout the tag → `cargo build --release --locked` → package (stable names) → SBOM → attest → `gh release upload --clobber` (assets only) |
| `verify` (after the matrix) | `contents: read`, `attestations: read` | download the published assets → completeness, checksums, SBOM shape, SBOM reproducibility, provenance |

The body has exactly one writer. Before this change every matrix leg called
`generate_release_notes: true`, so a release contained one “Full Changelog” comparison
block per leg (four for `v0.4.1`). Now the matrix never touches the body.

### 5.1 Release notes from the changelog

`scripts/release-notes.py` takes the `## <version>` section of `CHANGELOG.md` up to the next
level-2 heading, removes HTML comments, trims blank lines, and prints Markdown:

```sh
python3 scripts/release-notes.py v0.4.1 --check   # validate before pushing a tag
python3 scripts/release-notes.py v0.4.1           # body on stdout
```

Exit codes: `0` ok, `2` missing/empty section, `1` I/O or bad version. `--check` prints a
line count/byte summary instead of the body, so it is safe in CI and locally.

### 5.2 Docker images (native, no QEMU)

`docker.yml` builds each architecture on its own runner: `linux/amd64` on
`ubuntu-24.04`, `linux/arm64` on `ubuntu-24.04-arm`. Each leg pushes an image **by
digest** (`provenance: false`, GHA cache scoped per architecture); a single `merge` job
downloads the two digest files and runs `docker buildx imagetools create` for the version,
major-minor, branch/PR, short-sha and `latest` tags. Pull requests build both platforms
without pushing and skip the merge. The `smoke` job builds a local amd64 image from the
same GHA cache scope and runs the existing health/volume checks.

Measured vs expected:

| Flow | Wall clock |
| --- | --- |
| v0.4.1 (old): one QEMU job, `linux/amd64,linux/arm64` | **31m54s** |
| New: two native legs in parallel (~8–12 min cold each, overlapping) + merge inspection (~1 min) + cache-hit smoke (~2–4 min) | expected ~12–16 min cold; lower with warm GHA cache. Measure the next tag and update this row. |

Workflow files are linted by `actionlint` in CI (`.github/workflows/ci.yml`, pinned image
`rhysd/actionlint:1.7.12`); run it locally before pushing a workflow change.

Notes:

- **No compiled-artifact cache.** `Swatinem/rust-cache` runs with `cache-targets: "false"`,
  so it caches downloaded crates but never restores `target/` output into a job that
  produces a signed artifact. A poisoned cache cannot reach a release.
- **Actions are pinned to major tags** (`@v4`, `@v2`, …) like the rest of this repository;
  Dependabot updates `github-actions` monthly. If you tighten that policy further, use
  commit SHAs and update Dependabot accordingly.
- **Re-runs.** `workflow_dispatch` with a tag builds and publishes that tag. Note that
  GitHub's provenance records the workflow run's source ref: for a dispatch from a branch
  that is the branch commit, while the SBOM records the commit actually checked out. A
  tag push is the authoritative path; prefer re-pushing the tag for a clean provenance.
  The job's tooling comes from the checked-out tag, so a tag created before
  `scripts/sbom.py` existed cannot be re-published by this workflow without re-tagging.
- **Local checks for the tooling:** `./scripts/test-release-tools.sh` (no network, no tag)
  exercises SBOM determinism and the verifier's checks and failure modes.
- A failing `verify` job means the published release is not what the workflow intended —
  fix the release, do not silence the job.

## 6. What remains manual

1. **Cut the tag** after `CHANGELOG.md` is final and `./scripts/verify.sh` is green:
   `git tag vX.Y.Z && git push origin vX.Y.Z`.
2. **Read the release notes** the workflow generated and confirm all four tarballs, four
   checksums and four SBOMs are attached.
3. **Container image.** `docker.yml` builds and pushes the multi-arch image on the same tag,
   but this workflow does not attest image digests. Verifying/publishing an attested image
   digest is a separate decision — until then, treat the image as unverified and prefer a
   tarball for high-assurance installs.
4. **Homebrew tap / installer.** Formula bumps and any mirror updates are still manual.
5. **Vulnerability review.** The SBOM is an inventory; schedule a scanner (or Dependabot
   alerts) against it; neither is enforced by this workflow.
6. **Attestations on private forks.** GitHub artifact attestations are on by default for
   public repositories; a private or internal fork must enable them in repository
   settings before the `binaries` job can store provenance.
7. **Bit-for-bit reproducibility.** Not claimed. Provenance proves *which workflow, commit
   and repository* built an artifact; rebuilding the same tag locally is expected to
   produce a functionally identical binary, not an identical file (Rust builds embed paths
   and timestamps). Closing that gap needs `--remap-path-prefix` and a coordinated
   toolchain pin — a separate project.
