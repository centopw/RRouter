# Docker

The image is distroless: Node builds the dashboard, Rust embeds it and builds the binary,
and the runtime has no shell or package manager. Multi-arch images are published to GHCR for
`linux/amd64` and `linux/arm64`.

## Run

```sh
docker run -d --name rrouter \
  -p 127.0.0.1:20128:20128 \
  -v rrouter-data:/data \
  ghcr.io/centopw/rrouter:latest
```

Open <http://127.0.0.1:20128/app>. The first visit asks for the setup code printed in:

```sh
docker logs rrouter | grep "Setup code"
```

Publish on loopback unless a reverse proxy with authentication is in front. A container
request reaches the gateway from the bridge network, so the admin gate needs:

```sh
-e RROUTER_ALLOW_REMOTE_ADMIN=true
```

Only do that when the port is loopback-published or the proxy authenticates `/api/*` and
`/metrics`. For a shared gateway also set `RROUTER_REQUIRE_API_KEY=true`.

## Compose

The repository includes [`docker-compose.yml`](../../docker-compose.yml) with the volume,
health check and stop grace period:

```sh
docker compose up -d
docker compose logs -f rrouter
```

## Build locally

```sh
docker build -t rrouter .
docker run --rm -p 127.0.0.1:20128:20128 -v rrouter-data:/data rrouter
```

## Health and storage

The image declares a health check that runs:

```sh
/usr/local/bin/rrouter --healthcheck
```

`GET /ready` additionally reads the store; use it for orchestrator readiness. The data
volume must be writable by uid `65532` when using a host bind mount:

```sh
sudo chown -R 65532:65532 /srv/rrouter-data
```

## Upgrade

```sh
docker pull ghcr.io/centopw/rrouter:latest
docker compose up -d
```

Migrations run at startup and are forward-only. Back up first; a rollback to an older binary
against a newer schema is not supported. See [Operations](../DEPLOY.md#4-backups) and
[Updates](../UPDATES.md).
