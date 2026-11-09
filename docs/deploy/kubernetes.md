# Kubernetes

Use [`deploy/k8s.yaml`](../../deploy/k8s.yaml) for a single-replica Deployment, PVC, Service,
liveness/readiness probes and a restricted security context. For a chart with values and
Ingress support use [Helm](helm.md).

## Apply

```sh
kubectl apply -f deploy/k8s.yaml
kubectl rollout status deploy/rrouter
kubectl logs deploy/rrouter | grep "Setup code"
```

Expose the Service through an Ingress. Do not publish the pod port directly.

## Storage and replicas

- The PVC is `ReadWriteOnce` and holds `rrouter.db`, `master.key` and `/ext` assets.
- `replicas: 1` and `strategy: Recreate` are deliberate. SQLite is single-writer; scaling up
  will corrupt or lock the database.
- Back up the volume with the gateway running using `scripts/backup.sh` or `VACUUM INTO`.

## Admin authentication

The manifest sets `RROUTER_ALLOW_REMOTE_ADMIN=true` because Ingress traffic does not arrive
over pod loopback. That makes the admin API reachable to anything that can reach the pod
network. Close it with one of:

- an Ingress that authenticates `/api/*` and `/metrics` (basic auth, OIDC, IP allowlist); or
- `RROUTER_ADMIN_TOKEN` from a Secret, sent as `Authorization: Bearer` or `x-admin-token`.

The dashboard prompts for the admin token and keeps it in the tab's sessionStorage. Never
expose `/api` publicly without one of these controls.

## Master key

Prefer a Secret for `RROUTER_MASTER_KEY` in real clusters:

```yaml
env:
  - name: RROUTER_MASTER_KEY
    valueFrom:
      secretKeyRef:
        name: rrouter-master-key
        key: master-key
```

If unset, the key is generated at `/data/master.key`; back that file up with the database.

## Probes and streaming

- Liveness: `rrouter --healthcheck`.
- Readiness: `rrouter --ready` (reads the store).
- Set the Ingress read timeout for long SSE responses and disable proxy buffering.

## Upgrade

```sh
kubectl set image deploy/rrouter rrouter=ghcr.io/centopw/rrouter:<version>
kubectl rollout status deploy/rrouter
```

Back up first; migrations are forward-only. The gateway cannot replace its own pod image;
the dashboard shows the exact `kubectl` command instead of a non-working button. See
[Updates](../UPDATES.md).
