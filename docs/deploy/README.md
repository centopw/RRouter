# Deploy RRouter

RRouter is one process, one data directory, SQLite inside. There is no Redis, Postgres or
sidecar. The hard constraint is the same for every method:

> **One writer per database.** Run exactly one RRouter process against a data directory.
> Kubernetes/Helm therefore use one replica and `Recreate`; do not scale the Deployment up.

## Pick a method

| Method | Best for | Guide |
| --- | --- | --- |
| Docker / Compose | A single host, easiest upgrades | [Docker](docker.md) |
| Kubernetes | Existing cluster, raw manifests | [Kubernetes](kubernetes.md) |
| Helm | Cluster with release/values management | [Helm](helm.md) |
| systemd | A host without containers | [systemd](systemd.md) |
| Release binary | Any host, no runtime | [Install](../../README.md#install) and [Operations](../DEPLOY.md) |

## What every deployment needs

- A persistent directory for `rrouter.db`, `master.key` and `/ext` assets. Back it up;
  losing `master.key` makes provider credentials unrecoverable.
- A port for `/v1` and `/app`. Default `20128`; bind `127.0.0.1` behind a proxy when the
  gateway is not container-local.
- TLS and authentication for `/api/*` and `/metrics`. The admin API trusts loopback only;
  containers and pods need `RROUTER_ALLOW_REMOTE_ADMIN=true` **plus** proxy/Ingress auth or
  `RROUTER_ADMIN_TOKEN`.
- A client key on `/v1` for shared deployments: `RROUTER_REQUIRE_API_KEY=true`.
- Long-lived streaming: do not buffer or cap SSE at the proxy.

Start with [Operations](../DEPLOY.md) for exposure, backups, restore drills and failure
modes after choosing a method.
