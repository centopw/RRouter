# Updates

Settings → **Software updates** checks the latest GitHub release, shows release notes and
can download the archive. Checking is daily by default; automatic downloading is opt-in.
Archives are SHA-256 verified and staged under `<data_dir>/updates/<version>/`.

`update_available` is a SemVer comparison, never string inequality: a last-seen release
older than the running build (for example `v0.8.0` on a `v0.11.0` pod) shows **up to date**,
not an update. Opening Settings also re-checks once when the persisted check is over a day
old or failed, so an image upgrade does not leave stale release data on screen. The sidebar
shows a compact **vX.Y.Z available** marker with the first lines of the release notes; it
links to Settings. A failed automatic check is named instead of silently keeping old data.

`RROUTER_UPDATE_TOKEN` (or `GITHUB_TOKEN`) raises GitHub's per-IP API limit; it is sent only
as a bearer header and never logged or returned. Without a token, routine checks avoid the
API and follow `releases/latest` to `/releases/tag/vX`; notes come from the last cached
check, and archives use the release-download URL with the same checksum verification. On a
`403`/`429`, the gateway records `rate_limited_until` from `x-ratelimit-reset` (+ jitter),
stops automatic API checks, keeps the last known release and shows the retry time. A restart
does not immediately re-check.

## Deployment-specific upgrades

| Deployment | Detection | Upgrade |
| --- | --- | --- |
| Binary / systemd | default, `INVOCATION_ID` | staged binary replaces the executable and restarts |
| Homebrew | executable under `/Cellar/` | `brew upgrade rrouter` |
| Docker | `/.dockerenv` or `container` | pull and recreate the container |
| Kubernetes | `KUBERNETES_SERVICE_HOST` | update the image and roll out |
| Helm | Kubernetes + `RROUTER_HELM_RELEASE` | `helm upgrade <release> <chart> --reuse-values --set image.tag=<version>` |

Containers and pods cannot replace their own image or Helm release, so the dashboard shows
the exact supervisor command instead of a button that cannot work. Set
`RROUTER_DEPLOYMENT=docker|helm|kubernetes|brew|systemd|binary` when detection cannot infer
it, and `RROUTER_UPDATE_COMMAND` for a deployment-specific command. A staged binary is never
applied automatically; an operator chooses the restart.

Release mechanics are in [Releases](RELEASES.md).
