# systemd

The unit at [`deploy/rrouter.service`](../../deploy/rrouter.service) runs one process as a
dedicated user, keeps the admin API on loopback and stores state in `/var/lib/rrouter`.

```sh
sudo useradd --system --home /var/lib/rrouter --shell /usr/sbin/nologin rrouter
sudo install -d -o rrouter -g rrouter -m 0700 /var/lib/rrouter
sudo install -m 0755 target/release/rrouter /usr/local/bin/rrouter
sudo install -m 0644 deploy/rrouter.service /etc/systemd/system/rrouter.service
sudo systemctl daemon-reload
sudo systemctl enable --now rrouter
sudo journalctl -u rrouter -f
```

The first setup code is in the journal. Put Caddy/nginx in front for TLS and authenticate
`/api/*` and `/metrics`; see [Operations](../DEPLOY.md#3-exposure). Back up
`/var/lib/rrouter` with `scripts/backup.sh`.

The dashboard detects systemd and can stage a binary update, but it never applies it
automatically; an operator restarts the unit. See [Updates](../UPDATES.md).
