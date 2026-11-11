# Helm

The chart lives at [`deploy/helm/rrouter`](../../deploy/helm/rrouter). It deploys one
replica, a `Recreate` strategy, a PVC, probes, a Service and an optional Ingress.

## Install

```sh
helm install rrouter deploy/helm/rrouter \
  --namespace rrouter --create-namespace \
  --set admin.value='choose-a-long-admin-token' \
  --set allowRemoteAdmin=true
```

`allowRemoteAdmin` defaults to `false`, so the admin API stays loopback-only. Enabling it
without a token makes the admin API reachable to anything on the pod network.

Get the first-run setup code:

```sh
kubectl -n rrouter logs deploy/rrouter-rrouter | grep "Setup code"
```

Then open the Service through port-forward or your Ingress:

```sh
kubectl -n rrouter port-forward svc/rrouter-rrouter 20128:80
# http://127.0.0.1:20128/app
```

The chart name plus release name produces `rrouter-rrouter` unless you override
`fullnameOverride`.

## Values that matter

| Value | Default | Purpose |
| --- | --- | --- |
| `image.repository` | `ghcr.io/centopw/rrouter` | Image |
| `image.tag` | chart `appVersion` | Pin a release |
| `persistence.size` | `1Gi` | PVC for SQLite/master key |
| `persistence.existingClaim` | empty | Use an existing PVC |
| `admin.value` / `admin.existingSecret` | empty | `RROUTER_ADMIN_TOKEN`; without it authenticate at the Ingress |
| `masterKey.value` / `masterKey.existingSecret` | empty | Prefer a Secret in production |
| `requireApiKey` | `true` | Require `sk-rr-…` on `/v1` |
| `allowRemoteAdmin` | `false` | Enable only with `admin.value`/`admin.existingSecret` or authenticating Ingress |
| `ingress.enabled` | `false` | Enable the Ingress template |

Create secrets instead of putting values in the release:

```sh
kubectl -n rrouter create secret generic rrouter-admin --from-literal=admin-token='…'
kubectl -n rrouter create secret generic rrouter-master --from-literal=master-key='…'
helm upgrade --install rrouter deploy/helm/rrouter -n rrouter \
  --set admin.existingSecret=rrouter-admin \
  --set masterKey.existingSecret=rrouter-master
```

The chart uses key `admin-token` for the admin secret and `master-key` for the master key;
override `admin.existingSecretKey` / `masterKey.existingSecretKey` if yours differ.

## Ingress

```yaml
ingress:
  enabled: true
  className: nginx
  annotations:
    nginx.ingress.kubernetes.io/proxy-buffering: "off"
    nginx.ingress.kubernetes.io/proxy-read-timeout: "900"
  hosts:
    - host: rrouter.example.com
      paths:
        - path: /
          pathType: Prefix
  tls:
    - secretName: rrouter-tls
      hosts: [rrouter.example.com]
```

Do not enable an Ingress without admin authentication or `admin.value`/`admin.existingSecret`. Enabling an Ingress also requires `allowRemoteAdmin=true`.

## Upgrade and uninstall

```sh
helm upgrade rrouter deploy/helm/rrouter -n rrouter --reuse-values \
  --set image.tag=0.11.0

helm uninstall rrouter -n rrouter
```

Back up the PVC before upgrading. The PVC is not deleted by `helm uninstall`; remove it
deliberately. See [Operations](../DEPLOY.md#4-backups).
