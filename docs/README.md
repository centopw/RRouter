# Documentation

RRouter is one Rust binary with embedded SQLite and a compiled-in dashboard. Start with the
[README](../README.md) for install and the first request, then use this index.

## Deploy

| Guide | Use it for |
| --- | --- |
| [Deployment overview](deploy/README.md) | Choosing a method, ports, volumes, the single-writer rule |
| [Docker](deploy/docker.md) | Image, Compose, volumes, health checks, multi-arch |
| [Kubernetes](deploy/kubernetes.md) | Raw manifests, PVC, probes, Ingress auth |
| [Helm](deploy/helm.md) | Chart install, values, secrets, Ingress, upgrades |
| [systemd](deploy/systemd.md) | A single-node host install |
| [Operations](DEPLOY.md) | Exposure, TLS, backups, upgrades, troubleshooting, monitoring |
| [Configuration](CONFIG.md) | Every environment variable and precedence rule |

## Use

| Guide | Use it for |
| --- | --- |
| [API](API.md) | Endpoints, dialects, usage queries |
| [Routing](ROUTING.md) | Aliases, combos, strategies, breakers, preferred providers |
| [Models](MODELS.md) | Registry, custom models, disabled models, provider coverage |
| [Model presets](MODEL_PRESETS.md) | Saved layouts, reorder/hide, revision conflicts |
| [Keys and limits](KEYS.md) | Key limits, teams, guardrails, PII redaction |
| [Caching](CACHING.md) | Exact cache, semantic cache, prompt-cache preservation |
| [Providers](PROVIDERS.md) | OAuth/API-key support, endpoints, Antigravity |
| [Observability](OBSERVABILITY.md) | Request IDs, metrics, traces, dashboard/Activity/Logs |
| [Security](SECURITY.md) | Encryption, admin gate, accounts, audit |
| [Updates](UPDATES.md) | Update checks, deployment-specific upgrade commands |

## Develop

| Guide | Use it for |
| --- | --- |
| [Architecture](ARCHITECTURE.md) | Crates and request flow |
| [Development](DEVELOPMENT.md) | Build, dashboard dev server, tests, release checks |
| [Releases](RELEASES.md) | Tagging, release assets, Docker manifests |
| [Events](EVENTS.md) | Webhook event vocabulary and delivery |
| [Guardrails](GUARDRAILS.md) | Policy semantics and limits |
| [Telemetry](TELEMETRY.md) | OTLP/HTTP spans and sampling |
| [Roadmap](ROADMAP.md) | Status and planned work |
