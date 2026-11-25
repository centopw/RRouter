#!/usr/bin/env bash
# Local checks for the surface map. No network, no build.
#
#   ./scripts/test-surface-map.sh
#
# The map's chain (route -> page -> hook -> endpoint -> handler) is what a change is
# judged against, so this checks the parts that can silently go empty: a page's own
# regexes, a route the router no longer holds, and an endpoint that reaches no handler.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

step() { printf '\n== %s\n' "$1"; }
pass() { printf '  PASS %s\n' "$1"; }
fail() {
  printf '  FAIL %s\n' "$1" >&2
  exit 1
}

step "syntax"
python3 -c 'import ast, sys; ast.parse(open(sys.argv[1], encoding="utf-8").read())' \
  scripts/surface-map.py || fail "surface-map.py has a syntax error"
pass "surface-map.py parses"

# Load the module once and hold it to the tree it just read.
python3 - <<'PY' || exit 1
import importlib.util

spec = importlib.util.spec_from_file_location("surface_map", "scripts/surface-map.py")
sm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sm)


def check(condition, message):
    if not condition:
        raise SystemExit(f"FAIL {message}")


# Regexes against the strings they must read.
check(sm.normalize("/api/keys/{id}") == "/api/keys/*", "axum parameter normalisation")
check(sm.normalize("/api/keys/:id") == "/api/keys/:id", "only braces are parameters")
check(sm.endpoints_in('api.get("/health")') == {"/api/health"}, "client path gets the /api base")
check(
    sm.endpoints_in('api.get(`/api/evals/suites/${id}/cases`)') == {"/api/evals/suites/*/cases"},
    "template literal hole becomes one segment",
)
check("/api/keys/*" not in sm.endpoints_in('api.get("/api/keys?team=1")'), "query string dropped")
check(sm.regex_of("/api/keys/*").match("/api/keys/7"), "endpoint pattern matches a parameter")
check(not sm.regex_of("/api/keys").match("/api/keys/7/extra"), "endpoint match is segment-bounded")

# The router: every route it parses exists at its call path, under one handler.
endpoints = sm.declared_endpoints()
check(endpoints, "declared_endpoints() read no route")
check(all(k.startswith("/api") for k in endpoints), "an endpoint escaped the /api mount")
routes = sm.rust_router("api")[0]
check(len(routes) > 50, f"rust_router(api) found only {len(routes)} routes")
check("/api/connections" in endpoints, "/api/connections is missing from the map")
missing = [h for v in endpoints.values() for h in v["handlers"] if not sm.is_file(h)]
check(not missing, f"handlers that do not exist: {missing[:3]}")
check(
    endpoints["/api/connections"]["handlers"] == ["crates/rrouter-gateway/src/routes/connections.rs"],
    "a route module is not resolved to rrouter-gateway/src",
)

# The pages: every page file exists, and the known shared component stays listed.
pages = sm.ui_pages()
check(len(pages) > 20, f"ui_pages() found only {len(pages)} routes")
check(pages.get("providers") == "ui/src/pages/providers.tsx", "providers route -> page")
check(all(sm.is_file(p) for p in pages.values()), "a route points at a missing page")
icon = sm.import_closure("ui/src/pages/providers.tsx")
check("ui/src/components/provider-icon.tsx" in icon, "provider-icon is in the providers closure")
check(
    sm.import_closure("ui/src/components/ui.tsx"),
    "the ui barrel's re-exports are followed",
)
print("ok")
PY
pass "regexes, router, pages and the providers closure"

step "the committed map matches the tree"
out=$(python3 scripts/surface-map.py --check)
printf '  %s\n' "$out" || fail "--check rejected the committed map"
pass "--check"

step "a change reports every surface it reaches"
out=$(python3 scripts/surface-map.py --affected ui/src/components/provider-icon.tsx)
grep -q "^  providers$" <<<"$out" || fail "provider-icon did not list the providers page"
grep -q "docs/screenshots/providers.png" <<<"$out" || fail "provider-icon did not list its screenshot"
grep -q "^  ui/src/components/provider-icon.tsx$" <<<"$out" || fail "provider-icon did not name itself as shared"
pass "provider-icon -> providers page, tests, docs and screenshot"

out=$(python3 scripts/surface-map.py --affected crates/rrouter-gateway/src/routes/connections.rs)
grep -q "^Endpoints:$" <<<"$out" || fail "a handler change listed no endpoint"
grep -q "/api/connections" <<<"$out" || fail "/api/connections missing from the handler's surfaces"
grep -q "^Pages:$" <<<"$out" || fail "the handler's dashboard pages are missing"
pass "connections handler -> /api/connections and the pages that call it"

step "a stale map fails"
BACKUP=$(mktemp)
cp docs/surfaces.json "$BACKUP"
python3 - <<'PY' || fail "could not make the map stale"
import json

with open("docs/surfaces.json", encoding="utf-8") as handle:
    data = json.load(handle)
data["pages"].pop("providers", None)
with open("docs/surfaces.json", "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
PY
if python3 scripts/surface-map.py --check >/dev/null 2>&1; then
  cp "$BACKUP" docs/surfaces.json
  rm -f "$BACKUP"
  fail "a map missing the providers route passed --check"
fi
cp "$BACKUP" docs/surfaces.json
rm -f "$BACKUP"
pass "--check exits 1 when a route is missing"

printf '\nsurface map: all checks passed\n'
