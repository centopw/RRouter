#!/bin/sh
# Start an RRouter image and prove it is runnable: the entrypoint starts, /health answers,
# the /data volume is writable and the built-in healthcheck passes. The runtime image is
# distroless (no shell), so starting it is the only way to know.
#
#   scripts/image_smoke.sh <image-ref>
set -eu

ref="${1:?usage: image_smoke.sh <image-ref>}"
name="rrouter-smoke-$$"
trap 'docker rm -f "$name" >/dev/null 2>&1 || true; docker volume rm -f "$name" >/dev/null 2>&1 || true' EXIT

docker run -d --name "$name" -p 20128:20128 -v "$name:/data" \
  -e RROUTER_ALLOW_REMOTE_ADMIN=true "$ref" >/dev/null

ok=0
for _ in $(seq 1 30); do
  if curl -fsS http://127.0.0.1:20128/health >/dev/null 2>&1; then ok=1; break; fi
  sleep 1
done
if [ "$ok" -ne 1 ]; then
  echo "image never became healthy; container log:" >&2
  docker logs "$name" >&2 || true
  exit 1
fi
curl -fsS http://127.0.0.1:20128/health
echo
# A healthy server proves the SQLite file and master key were written to the volume.
docker exec "$name" /usr/local/bin/rrouter --healthcheck
echo "image smoke ok: $ref"
