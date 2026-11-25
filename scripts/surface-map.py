#!/usr/bin/env python3
"""One change, every surface it touches.

The dashboard is wired route -> page -> component -> hook -> endpoint -> handler, and the
same names appear again in docs, tests, settings and screenshots. This script derives
that chain from the source with regexes (stdlib only, no parser, no new dependency) and
writes it twice:

    docs/SURFACES.md     human map: one Mermaid graph, a table per page
    docs/surfaces.json   the same data for tools

    scripts/surface-map.py                  regenerate both files
    scripts/surface-map.py --check          exit 1 when the committed map is stale, or
                                            when a route, page, endpoint or test is not in it
    scripts/surface-map.py --affected FILE  print every surface reachable from a change;
                                            name a directory to expand it to its files

Paths are repo-relative and sorted; the output is byte-stable, so --check is a plain
diff. `.md` is generated, never hand-edited.
"""

from __future__ import annotations

import json
import os
import posixpath
import re
import subprocess
import sys
from pathlib import Path

# The repo this script ships in, or $SURFACE_MAP_ROOT for a checkout elsewhere (tests).
ROOT = Path(os.environ.get("SURFACE_MAP_ROOT") or Path(__file__).resolve().parent.parent)
UI = "ui/src"
MAIN = f"{UI}/main.tsx"
APP = f"{UI}/app.tsx"
API = f"{UI}/lib/api.ts"
SETTINGS_RS = "crates/rrouter-gateway/src/settings.rs"
GATEWAY_RS = "crates/rrouter-gateway/src/lib.rs"
ROUTES_DIR = "crates/rrouter-gateway/src/routes"
MD = "docs/SURFACES.md"
JSON = "docs/surfaces.json"

# A test is anything the repo runs on its own: Node's runner, Rust integration and unit
# tests, and the smoke scripts.
TEST_GLOBS = ("ui/tests/**/*.ts", f"{UI}/**/*.test.ts", "crates/*/tests/*.rs",
              "scripts/*_smoke.py", "scripts/test-*.sh")

# Endpoint parameters, in either spelling the two sides use: axum `{id}` / `{*path}` and a
# template literal `${...}`. Normalised to `*` so the handler table can match by pattern.
PARAM = re.compile(r"\{[^}]*\}|\$\{[^}]*\}")


# ------------------------------------------------------------------ reading the repo

def _tracked() -> list[str] | None:
    """Files git tracks. A page deleted in the change under review is then already out of
    the map instead of being reported as one the map is missing."""
    try:
        found = subprocess.run(
            ["git", "-C", str(ROOT), "ls-files", "-z"],
            capture_output=True, text=True, check=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    return [p for p in found.stdout.split("\0") if p]


TRACKED = _tracked()


def is_file(path: str) -> bool:
    return path in TRACKED if TRACKED is not None else (ROOT / path).is_file()


def read(path: str) -> str:
    file = ROOT / path
    return file.read_text(encoding="utf-8") if is_file(path) else ""


def repo_files(pattern: str) -> list[str]:
    found = (p.relative_to(ROOT).as_posix() for p in ROOT.glob(pattern) if is_file(p.relative_to(ROOT).as_posix()))
    return sorted(found)


def resolve(from_file: str, specifier: str) -> str | None:
    """`./x` from `ui/src/pages/a.tsx` is `ui/src/pages/x.ts[x]`; null for a package."""
    if not specifier.startswith("."):
        return None
    base = posixpath.normpath(posixpath.join(posixpath.dirname(from_file), specifier))
    for candidate in (base, f"{base}.ts", f"{base}.tsx", f"{base}/index.ts", f"{base}/index.tsx"):
        if is_file(candidate):
            return candidate
    return None


# ------------------------------------------------------------------ UI

IMPORT = re.compile(r"import\s+(?:type\s+)?[^;]*?from\s+[\"']([^\"']+)[\"']")
# `export { X } from "./x"`: a barrel (`components/ui.tsx`) is a dependency of every
# page that imports anything from it, so its targets belong to the page's closure too.
REEXPORT = re.compile(r"export\s+(?:type\s+)?\{[^}]*\}\s*from\s+[\"']([^\"']+)[\"']")
EXPORT_FN = re.compile(r"\bexport\s+(?:async\s+)?function\s+(\w+)")
EXPORT_CONST = re.compile(r"\bexport\s+const\s+(\w+)")
HOOK_CALL = re.compile(r"\b(use[A-Z]\w*)\s*\(")
# A path literal in a call: `api.get("/x")`, `list<T>(`/x/${id}`)`. The leading `/api`
# is optional because the client itself is rooted there, so `/health` means `/api/health`.
JSON_CALL = re.compile(r"([`\"])(/?(?:api/)?[\w-]+(?:/[\w.${}-]+)*)(?:\?[^`\"]*)?\1")


def endpoints_in(text: str) -> set[str]:
    """Every `/api` path a file calls, as the route spells it. A template literal keeps
    one `*` per hole, so `/api/evals/suites/${...}` is `/api/evals/suites/*`, and a
    client path without the mount (`/health`) is `/api/health`."""
    found: set[str] = set()
    for _quote, path in JSON_CALL.findall(text):
        head = normalize(re.sub(r"\$\{[^}]*\}", "*", path.split("?", 1)[0]))
        found.add(head if head.startswith("/api/") else "/api" + head)
    return found


def normalize(endpoint: str) -> str:
    return PARAM.sub("*", endpoint).rstrip("/") or "/"


def regex_of(endpoint: str) -> re.Pattern[str]:
    parts = normalize(endpoint).split("*")
    return re.compile("^" + "[^/]*".join(re.escape(p) for p in parts) + "$")


def ui_pages() -> dict[str, str]:
    """`/system/health` -> `ui/src/pages/health.tsx`, from the router in main.tsx.

    A `page(() => import(...))` const names a chunk; an entry's `element` shows the
    component it renders (`<Providers />`, or `<AdminPage><Providers /></AdminPage>`),
    so the innermost name that is a chunk is that entry's page. An entry whose element
    is only a `<Navigate>` or a `<Legacy>` is a redirect, and `children: [...]` makes
    the entry a layout: neither is a page.
    """
    main = read(MAIN)
    chunks: dict[str, str] = {}
    for const, spec in re.findall(
        r"const\s+(\w+)\s*=\s*page\(\s*\(\)\s*=>\s*import\(\s*[\"']([^\"']+)[\"']", main
    ):
        target = resolve(MAIN, spec)
        if target:
            chunks[const] = target
    for spec, name in re.findall(
        r"const\s+\w+\s*=\s*page\(\s*\(\)\s*=>\s*import\(\s*[\"']([^\"']+)[\"']\s*\)\s*\.then\(\s*\(\s*\w+\s*\)\s*=>\s*\(\{\s*default:\s*\w+\.(\w+)\s*\}\)",
        main,
    ):
        target = resolve(MAIN, spec)
        if target:
            chunks[name] = target

    # Flat scan of the router literal. `levels` holds the route prefix of each open
    # array; `children_of` is set by an entry with `children:`, so the bracket that
    # opens next continues that entry's route. A folder page (one file serving `/a`
    # and `/a/b`) names its routes with an object inside a comment, so a `{` in prose
    # is skipped: only a path that resolves to a chunk is a page.
    pages: dict[str, str] = {}
    levels: list[str] = []
    children_of: str | None = None
    start = main.index("const router = createBrowserRouter(")
    for index in range(start, len(main)):
        char = main[index]
        if char == "[":
            levels.append(children_of or "")
            children_of = None
        elif char == "]":
            if levels:
                levels.pop()
        elif char == "{":
            end = main.find("}", index)
            entry = main[index:end + 1]
            found = re.match(r"\{\s*path:\s*\"([^\"]+)\"", entry)
            if found:
                parent = levels[-1] if levels else ""
                route = f"{parent}/{found.group(1)}" if parent else found.group(1)
                if "children:" in entry:
                    children_of = route
                else:
                    element = re.search(r"\belement:\s*(.*)", entry, re.S)
                    names = re.findall(r"<([A-Z]\w*)\b", element.group(1)) if element else []
                    for name in reversed(names):
                        if name in chunks:
                            pages[route] = chunks[name]
                            break
    return dict(sorted(pages.items()))


NAV_ENTRY = re.compile(r"\{\s*to:\s*\"(/[^\"]*)\",\s*(?:memberTo:\s*\"[^\"]*\",\s*)?label:\s*\"([^\"]*)\"")


def nav_entries() -> dict[str, str]:
    return {route: label for route, label in NAV_ENTRY.findall(read(APP))}


def import_closure(page: str, limit: int = 4) -> dict[str, int]:
    """Repo-relative imports under `ui/src` up to `limit` hops, with the hop depth.

    Reachable only from the page: a file that imports itself into the graph is skipped,
    so a leaf like `molecules/Card.tsx` never drags the whole dashboard in with it.
    """
    seen: dict[str, int] = {}
    frontier = [(page, 0)]
    while frontier:
        file, depth = frontier.pop()
        if depth >= limit:
            continue
        for spec in IMPORT.findall(read(file)) + REEXPORT.findall(read(file)):
            target = resolve(file, spec)
            if not target or not target.startswith(UI):
                continue
            if target in (page, *seen):
                continue
            seen[target] = depth + 1
            frontier.append((target, depth + 1))
    seen.pop(page, None)
    return dict(sorted(seen.items()))


def exported(path: str) -> list[str]:
    text = read(path)
    return sorted(set(EXPORT_FN.findall(text)) | set(EXPORT_CONST.findall(text)))


def hook_bodies() -> dict[str, tuple[str, str]]:
    """`useX` -> (`ui/src/lib/...`, body) for every exported hook in the lib.

    The body is the hook's own `{...}`, so an endpoint a sibling hook in the same file
    calls is not charged to this one.
    """
    bodies: dict[str, tuple[str, str]] = {}
    for path in repo_files(f"{UI}/lib/*.ts"):
        text = read(path)
        for name in exported(path):
            if not name.startswith("use"):
                continue
            match = re.search(rf"\b(?:export\s+)?(?:async\s+)?function\s+{name}\b[^{{]*\{{", text)
            if not match:
                continue
            depth = 0
            for index in range(match.end() - 1, len(text)):
                if text[index] == "{":
                    depth += 1
                elif text[index] == "}":
                    depth -= 1
                    if depth == 0:
                        bodies[name] = (path, text[match.start():index + 1])
                        break
    return bodies


def hooks_and_calls(page: str, closure: dict[str, int], bodies: dict[str, tuple[str, str]]) -> dict[str, list[str]]:
    """Hooks whose call is in the page's own files, with the file that calls them.

    Anything the page imports counts — that is what a change to the endpoint has to keep
    working — but a dependency is charged only when *it* calls the hook, never because a
    sibling module in the same library does.
    """
    used: dict[str, str] = {}
    queue = [page]
    while queue:
        file = queue.pop(0)
        for name in HOOK_CALL.findall(read(file)):
            if name in bodies and name not in used and file != bodies[name][0]:
                used[name] = file
        # Follow this file's own imports, so a call in `components/table.tsx` is not
        # charged to the page through some unrelated shared file.
        for spec in IMPORT.findall(read(file)):
            target = resolve(file, spec)
            if target and target in closure and target not in queue and target != page:
                queue.append(target)
    return {name: [used[name]] for name in sorted(used)}


# ------------------------------------------------------------------ backend

ROUTE_VERBS = ("get", "post", "put", "patch", "delete")
VERB_HANDLER = re.compile(r"\b(get|post|put|patch|delete)\(\s*((?:\w+::)*\w+)")


def snake(module: str) -> str:
    """`rrouter_gateway` -> `rrouter-gateway`: Cargo's directory name for a module."""
    return module.replace("_", "-")


def rust_router(variable: str) -> tuple[list[tuple[str, str, str]], list[tuple[str, str]]]:
    """(endpoint, verb, route module) and (prefix, variable) for one `let {var} = ...`.

    Read positionally: every `.route(` owns the text up to the next one, so each handler
    under it is that path's, with its verb and its defining module (the `::` path, whose
    first segment is the crate, mapped to the crate's directory). The router ends at the
    statement that owns it, so a free function below cannot be read as a handler.
    """
    text = read(GATEWAY_RS)
    start = re.search(rf"\blet\s+{variable}\s*=", text)
    if not start:
        return [], []
    text = text[start.end():]
    owns = re.search(r"\n\s{0,8}(?:let|pub fn|fn|impl|struct|mod|#\[cfg\(test\)\])", text)
    if owns:
        text = text[:owns.start()]
    # A route whose path sits on its own line (`/settings/retention/sweep`) follows a
    # line break after `.route(`; the `\s` covers the path line too.
    calls = [m for m in re.finditer(r"\.route\(\s*\"([^\"]+)\"\s*,", text, re.S)]
    routes: list[tuple[str, str, str]] = []
    for index, call in enumerate(calls):
        end = calls[index + 1].start() if index + 1 < len(calls) else len(text)
        body = text[call.end():end]
        for verb, handler in VERB_HANDLER.findall(body):
            pieces = handler.split("::")
            if verb not in ROUTE_VERBS or len(pieces) < 2:
                continue  # `headers.get(AUTHORIZATION)` is a header lookup, not a route.
            # `routes::usage::list` lives in `src/routes/usage.rs`, `ui::panels` in
            # `src/ui.rs`: the first module path segment is the file (or folder) either
            # way, so nothing has to be guessed from the route's own path.
            handler_file = f"crates/rrouter-gateway/src/{pieces[0]}.rs"
            if len(pieces) >= 3:
                handler_file = f"crates/rrouter-gateway/src/{pieces[0]}/{pieces[1]}.rs"
            routes.append((normalize(call.group(1)), verb.upper(), handler_file))
    nested: list[tuple[str, str]] = []
    for prefix, var in re.findall(r"\.nest\(\s*\"([^\"]*)\"\s*,\s*(\w+)", text):
        nested.append((normalize(prefix), var))
    return routes, nested


def declared_endpoints() -> dict[str, dict]:
    """Endpoint -> {handlers, verbs} for every route the gateway mounts.

    The gateway nests routers (`/api` into the app router, a team router under a
    prefix); walk that graph so each route is reported at the path a client calls.
    """
    routers: dict[str, tuple] = {}
    endpoints: dict[str, dict] = {}

    def visit(variable: str, prefix: str) -> None:
        if variable not in routers:
            routers[variable] = rust_router(variable)
        routes, nested = routers[variable]
        for path, verb, handler in routes:
            row = endpoints.setdefault(prefix + path, {"handlers": [], "verbs": []})
            row["handlers"] = sorted(set(row["handlers"]) | {handler})
            row["verbs"] = sorted(set(row["verbs"]) | {verb})
        for nested_prefix, child in nested:
            visit(child, prefix + nested_prefix)

    visit("api", "/api")
    return dict(sorted(endpoints.items()))


# `const RETENTION_KEYS: &[&str] = &["a", "b"];` — the key lists this crate validates a
# settings patch against. BRAND_KEYS is a `pub const` of the same shape.
SETTINGS_LIST = re.compile(r"const\s+\w*KEYS\s*:\s*&\[&str\]\s*=\s*&\[(.*?)\];", re.S)


def settings_keys() -> list[str]:
    return sorted({key for block in SETTINGS_LIST.findall(read(SETTINGS_RS)) for key in re.findall(r"\"(\w+)\"", block)})


def settings_surfaces(keys: list[str]) -> dict[str, dict[str, list[str]]]:
    """Which settings key is named where: the settings route, the dashboard files that
    carry it, and any doc that spells it."""
    surfaces: dict[str, dict[str, list[str]]] = {}
    routes_settings = f"{ROUTES_DIR}/settings.rs"
    docs = [d for d in repo_files("docs/**/*.md") + ["README.md"] if d != MD]
    ui_files = repo_files(f"{UI}/**/*.tsx") + repo_files(f"{UI}/**/*.ts")
    for key in keys:
        word = re.compile(rf"\b{re.escape(key)}\b")
        hits = {"handler": [], "ui": [], "docs": []}
        if word.search(read(routes_settings)):
            hits["handler"].append(routes_settings)
        hits["ui"] = [p for p in ui_files if word.search(read(p))]
        hits["docs"] = [p for p in docs if word.search(read(p))]
        surfaces[key] = {kind: sorted(set(paths)) for kind, paths in hits.items()}
    return surfaces


# ------------------------------------------------------------------ tests and docs

def test_sources() -> list[str]:
    files: set[str] = set()
    for pattern in TEST_GLOBS:
        files.update(repo_files(pattern))
    return sorted(files)


def reference(path: str, needles: list[str]) -> bool:
    text = read(path)
    return any(re.search(n, text) for n in needles)


def needles_for(page: str, data: dict) -> list[str]:
    """How a test names a page: its own file name and the files under it.

    Not a shared hook name — `useConnections` is called from a dozen pages, so a test
    that mentions it is not evidence about this page.
    """
    stem = posixpath.basename(page).rsplit(".", 1)[0]
    words = {posixpath.basename(posixpath.dirname(page)) if stem == "index" else stem}
    directory = posixpath.dirname(page) + "/"
    for component in data["components"]:
        # Only the page's own subtree: `pages/free-mode/model.ts` is not `pages/health.tsx`.
        if component.startswith(directory) and posixpath.basename(component) not in ("model.ts", "index.ts"):
            words.add(posixpath.basename(component).rsplit(".", 1)[0])
    return [rf"\b{re.escape(w)}\b" for w in sorted(words) if len(w) > 3]


def endpoint_hits(path: str, endpoint: str) -> bool:
    """Is this endpoint named in the file: its path, or the same shape with `{id}`,
    `:id`, `${x}` or any one segment where the parameter sits? A bounding check keeps
    `/api/connections` from matching inside `/api/connections/extra`."""
    pattern = regex_of(endpoint)
    text = read(path)
    for match in re.finditer(r"/api/[^\s`\"'<>\)\]]*", text):
        if pattern.match(normalize(match.group(0))):
            return True
    return False


def screenshot(row: dict) -> list[str]:
    """`docs/screenshots/<word>.png` for the page's name, its directory, and its route."""
    page = posixpath.basename(row["page"]).rsplit(".", 1)[0]
    words = {page} if page != "index" else {posixpath.basename(posixpath.dirname(row["page"]))}
    words |= {w for w in row["route"].split("/") if w}
    return sorted(s for s in repo_files("docs/screenshots/*.png")
                  if any(re.escape(w) in posixpath.basename(s) for w in words))


# ------------------------------------------------------------------ the map

def build() -> dict:
    routes = ui_pages()
    nav = nav_entries()
    endpoints = declared_endpoints()
    keys = settings_keys()
    docs = [d for d in repo_files("docs/**/*.md") + ["README.md"] if d != MD]
    tests = test_sources()
    bodies = hook_bodies()
    warnings: list[str] = []

    hook_eps = {name: sorted(ep for ep in endpoints_in(body) if ep in endpoints)
                for name, (_file, body) in bodies.items()}
    pages = {}
    for route, page in routes.items():
        pages[route] = {
            "route": route,
            "nav": nav.get(route),
            "page": page,
            "components": import_closure(page),
        }

    for route, row in pages.items():
        # hook -> the file in the page's closure that calls it; the endpoint it asks is
        # looked up in `hook_endpoints` (one hook, one endpoint set).
        # What the page asks through a hook, plus any `api.*` path spelled in its own
        # file (a log view streams its endpoint without a hook). A dependency's calls
        # are not charged to the page: `settings/general.tsx` knows nothing of quotas.
        row["hooks"] = hooks_and_calls(row["page"], row["components"], bodies)
        row["endpoints"] = sorted({
            ep for hook in row["hooks"] for ep in hook_eps.get(hook, ())
        } | {ep for ep in endpoints_in(read(row["page"])) if ep in endpoints})

    for route, row in pages.items():
        if not row["endpoints"]:
            warnings.append(f"{route} reaches no /api hook (read fine, or the page is static)")
    if not pages:
        warnings.append(f"no route parsed out of {MAIN}; the map would be empty")

    callers: dict[str, list[str]] = {}
    for route, row in pages.items():
        for endpoint in row["endpoints"]:
            callers.setdefault(endpoint, []).append(route)

    entries = {}
    for endpoint, row in endpoints.items():
        if not endpoint.startswith("/api"):
            continue  # `/v1`, the dashboard and the ops router are not dashboard surfaces.
        entries[endpoint] = {
            "handler": row["handlers"][0] if row["handlers"] else None,
            "handlers": row["handlers"],
            "verbs": row["verbs"],
            "pages": sorted(callers.get(endpoint, [])),
        }

    doc_names = {d: read(d) for d in docs}
    for route, row in pages.items():
        words = needles_for(row["page"], row)
        route_word = rf"(?<![\w/-]){re.escape(route.lstrip('/'))}(?![\w/-])"
        # A doc that names the route, or one that spells an endpoint the page calls.
        row["docs"] = sorted(
            {d for d, text in doc_names.items() if re.search(route_word, text)}
            | {d for d in doc_names if any(endpoint_hits(d, ep) for ep in row["endpoints"])}
        )
        row["tests"] = sorted(
            t for t in tests
            if any(re.search(w, read(t)) for w in words)
            or any(ep.startswith("/api/") and endpoint_hits(t, ep) for ep in row["endpoints"])
        )
        row["screenshots"] = screenshot(row)
        # A shared component, a molecule or a hook file: named loosely, because a test
        # for `provider-icon` or a doc for `refusal` is about the file, not the page.
        row["shared"] = sorted(set(row["components"]) | {bodies[h][0] for h in row["hooks"]})

    return {
        "pages": pages,
        "endpoints": entries,
        "unused_endpoints": sorted(set(entries) - set(callers)),
        # A hook that calls an endpoint in the handler table. One calling `api.ts`'s
        # `/api/auth/refresh` or a chat path is outside it and would otherwise list
        # every such path as a surface for whichever page calls the hook.
        "hook_endpoints": {name: hook_eps[name] for name in sorted(hook_eps)},
        "settings": settings_surfaces(keys),
        "tests": tests,
        "docs": docs,
        "warnings": warnings,
    }


# ------------------------------------------------------------------ affected

CHANGE_KINDS = ("pages", "shared files", "endpoints", "handlers", "settings keys",
                "docs", "tests", "screenshots", "no surface follows")


def affected(changed: list[str], data: dict) -> dict[str, list[str]]:
    out: dict[str, set[str]] = {kind: set() for kind in CHANGE_KINDS}
    pages = data["pages"]
    for item in changed:
        path = Path(item)
        if path.is_dir():
            files = [posixpath.join(item, p.relative_to(path).as_posix()) for p in sorted(path.rglob("*")) if p.is_file()]
        else:
            files = [item]
        for file in files:
            hit = False
            # A page or anything it imports: every page on the other side of the file.
            for route, row in pages.items():
                if file in (row["page"], *row["components"]):
                    hit = True
                    out["pages"].add(route)
                    if file != row["page"]:
                        out["shared files"].add(file)
                        # Which hook, and so which endpoint, the shared file keeps alive.
                        for hook, (caller,) in row["hooks"].items():
                            if caller == file:
                                out["endpoints"].update(data["hook_endpoints"][hook])
                    else:
                        out["endpoints"].update(row["endpoints"])
                    out["docs"].update(row["docs"])
                    out["tests"].update(row["tests"])
                    out["screenshots"].update(row["screenshots"])
            # A hook changed: its endpoint, and every page calling it.
            for hook, eps in data["hook_endpoints"].items():
                if file.endswith(".ts") and file.startswith(f"{UI}/lib/") and re.search(
                    rf"\b{re.escape(hook)}\b", read(file)
                ):
                    out["endpoints"].update(eps)
                    for route, row in pages.items():
                        if hook in row["hooks"]:
                            out["pages"].add(route)
                            hit = True
            # An endpoint's handler (or the router that declares it) changed.
            for endpoint, entry in data["endpoints"].items():
                if file in (*entry["handlers"], GATEWAY_RS):
                    hit = True
                    out["endpoints"].add(endpoint)
                    out["handlers"].update(entry["handlers"])
                    out["pages"].update(entry["pages"])
                    for doc in data["docs"]:
                        if endpoint_hits(doc, endpoint):
                            out["docs"].add(doc)
                    for test in data["tests"]:
                        if endpoint_hits(test, endpoint):
                            out["tests"].add(test)
            # A settings key: the file that declares or writes it, its readers and docs.
            for key, where in data["settings"].items():
                named = re.search(rf"\b{re.escape(key)}\b", read(file)) if is_file(file) else None
                if file in (SETTINGS_RS, f"{ROUTES_DIR}/settings.rs") and named:
                    hit = True
                    out["settings keys"].add(key)
                    out["docs"].update(where["docs"])
                    out["tests"].update(
                        t for t in data["tests"] if re.search(rf"\b{re.escape(key)}\b", read(t))
                    )
                elif file in where["ui"]:
                    hit = True
                    out["settings keys"].add(key)
                    out["docs"].update(where["docs"])
            # A test changed: the pages it already covers.
            for route, row in pages.items():
                if file in row["tests"]:
                    hit = True
                    out["pages"].add(route)
                    out["docs"].update(row["docs"])
            if not hit:
                out["no surface follows"].add(file)
    return {kind: sorted(values) for kind, values in out.items()}


# ------------------------------------------------------------------ rendering

def render_markdown(data: dict) -> str:
    rows = data["pages"]
    lines = [
        "# Surface map",
        "",
        "Generated by `scripts/surface-map.py` — do not edit. It reads the dashboard routes,",
        "pages, imports, hooks, endpoints, settings keys, docs, tests and screenshots, and",
        "`--check` fails when this file is stale or a route, page, endpoint or test is",
        "missing from it.",
        "",
        "```sh",
        "scripts/surface-map.py --affected <files>   # every surface a change reaches",
        "scripts/surface-map.py                      # regenerate this file and docs/surfaces.json",
        "scripts/surface-map.py --check              # what CI and scripts/verify.sh run",
        "```",
        "",
        "## Graph",
        "",
        "Route → page → endpoint → handler. Shared files and hooks are per page below; drawing",
        "them here would cross every route into one knot.",
        "",
        "```mermaid",
        "graph LR",
    ]
    nodes: list[str] = []
    edges: list[str] = []
    seen: set[str] = set()

    def node(prefix: str, label: str) -> str:
        ident = re.sub(r"[^A-Za-z0-9_]", "_", f"{prefix}_{label}")
        if ident not in seen:
            seen.add(ident)
            nodes.append(f'  {ident}["{label.replace(chr(34), chr(39))}"]')
        return ident

    for route, row in rows.items():
        page = posixpath.basename(row["page"]).rsplit(".", 1)[0]
        edges.append(f"  {node('r', route)} --> {node('p', page)}")
        for endpoint in row["endpoints"]:
            shape = normalize(endpoint)
            edges.append(f"  {node('p', page)} --> {node('e', shape)}")
            handler = data["endpoints"].get(shape, {}).get("handler")
            if handler:
                edges.append(f"  {node('e', shape)} --> {node('h', posixpath.basename(handler))}")
    lines += sorted(set(nodes)) + sorted(set(edges)) + ["```", ""]

    labels = {
        "components": "Shared files (hops from the page)",
        "hooks": "Query hooks",
        "endpoints": "Endpoints",
        "docs": "Docs",
        "tests": "Tests",
        "screenshots": "Screenshots",
    }
    for route, row in rows.items():
        nav = f" — sidebar **{row['nav']}**" if row["nav"] else ""
        lines += [f"### `{route}`{nav}", "", f"Page: `{row['page']}`", ""]
        for kind, label in labels.items():
            value = row[kind]
            if kind == "hooks":
                text = ", ".join(f"`{h}` → {', '.join(f'`{e}`' for e in eps)}" for h, eps in value.items())
            elif isinstance(value, dict):
                text = ", ".join(f"`{p}` ({d})" for p, d in value.items())
            else:
                text = ", ".join(f"`{p}`" for p in value)
            lines.append(f"- {label}: {text or '—'}")
        lines.append("")

    lines += ["## Backend endpoints", "", "| endpoint | verbs | handler | dashboard pages |", "| --- | --- | --- | --- |"]
    for endpoint, entry in data["endpoints"].items():
        pages = ", ".join(f"`{p}`" for p in entry["pages"]) or "—"
        lines.append(f"| `{endpoint}` | {' '.join(entry['verbs'])} | `{entry['handler']}` | {pages} |")
    if data["unused_endpoints"]:
        lines += ["", "Registered with no dashboard caller: "
                  + ", ".join(f"`{e}`" for e in data["unused_endpoints"])]

    lines += ["", "## Settings keys", "", "| key | handler | dashboard | docs |", "| --- | --- | --- | --- |"]
    for key, where in data["settings"].items():
        cells = [" ".join(f"`{p}`" for p in where[kind]) or "—" for kind in ("handler", "ui", "docs")]
        lines.append(f"| `{key}` | {cells[0]} | {cells[1]} | {cells[2]} |")

    owned = {t for row in rows.values() for t in row["tests"]}
    lines += ["", "## Tests", "",
              "Reachable from a page: " + (", ".join(f"`{t}`" for t in sorted(owned)) or "—"), "",
              "Map-wide, no page: " + (", ".join(f"`{t}`" for t in data["tests"] if t not in owned) or "—"),
              ""]
    if data["warnings"]:
        lines += ["", "## Warnings", ""] + [f"- {w}" for w in data["warnings"]]
    return "\n".join(lines)


def render_json(data: dict) -> str:
    return json.dumps(data, indent=2, sort_keys=True) + "\n"


# ------------------------------------------------------------------ commands

def regenerate() -> int:
    data = build()
    for warning in data["warnings"]:
        print(f"surface-map: {warning}", file=sys.stderr)
    (ROOT / MD).write_text(render_markdown(data), encoding="utf-8")
    (ROOT / JSON).write_text(render_json(data), encoding="utf-8")
    print(f"surface-map: {len(data['pages'])} pages, {len(data['endpoints'])} endpoints -> {MD}, {JSON}")
    return 0


def check() -> int:
    data = build()
    failures: list[str] = []
    if not (ROOT / MD).is_file():
        failures.append(f"{MD} is missing")
    elif (ROOT / MD).read_text(encoding="utf-8") != render_markdown(data):
        failures.append(f"{MD} is stale (run scripts/surface-map.py)")
    if not (ROOT / JSON).is_file():
        failures.append(f"{JSON} is missing")
    else:
        on_disk = json.loads((ROOT / JSON).read_text(encoding="utf-8"))
        for route, row in data["pages"].items():
            committed = on_disk["pages"].get(route)
            if committed is None:
                failures.append(f"route {route} is missing from {JSON}")
                continue
            for field in ("page", "endpoints"):
                if row[field] != committed.get(field):
                    failures.append(f"{route}: {field} changed (run scripts/surface-map.py)")
        for route in set(on_disk["pages"]) - set(data["pages"]):
            failures.append(f"{route} is in {JSON} but no longer routes anywhere")
        for endpoint, entry in data["endpoints"].items():
            if endpoint not in on_disk["endpoints"]:
                failures.append(f"endpoint {endpoint} is missing from {JSON}")
            elif row_pages := entry["pages"]:
                if on_disk["endpoints"][endpoint].get("pages") != row_pages:
                    failures.append(f"endpoint {endpoint}: dashboard caller changed")
        for endpoint in set(on_disk["endpoints"]) - set(data["endpoints"]):
            failures.append(f"{endpoint} is in {JSON} but nothing registers it")
        for test in data["tests"]:
            if test not in on_disk["tests"]:
                failures.append(f"test {test} is missing from {JSON}")
    for failure in failures:
        print(f"surface-map: {failure}", file=sys.stderr)
    if failures:
        return 1
    print(f"surface-map: {MD} and {JSON} up to date ({len(data['pages'])} pages, "
          f"{len(data['endpoints'])} endpoints, {len(data['tests'])} tests)")
    return 0


def print_affected(changed: list[str]) -> int:
    if not changed:
        print("surface-map: --affected needs at least one file or directory", file=sys.stderr)
        return 2
    # Read the sources, not the committed map: the answers are for the tree as it is, so
    # a change of paths and the map edit that follows it report in one pass.
    data = build()
    missing = [p for p in changed if not (ROOT / p).exists()]
    if missing:
        print("surface-map: no such file: " + ", ".join(missing) + " (paths are repo-relative)",
              file=sys.stderr)
        return 2
    result = affected(changed, data)
    for kind, items in result.items():
        if not items:
            continue
        if kind == "no surface follows":
            print("No surface in the map reaches (nothing to update):")
        else:
            print(f"{kind.capitalize()}:")
        for item in items:
            print(f"  {item}")
        print()
    return 0


def main(argv: list[str]) -> int:
    flags = [w for w in argv if w.startswith("-")]
    if not flags:
        return regenerate()
    if flags == ["-h"] or flags == ["--help"]:
        print(__doc__.strip())
        return 0
    if flags[0] in ("--affected", "--affected="):
        if flags[0] == "--affected":
            return print_affected(argv[argv.index("--affected") + 1:])
        return print_affected([argv[argv.index("--affected"):][0].split("=", 1)[1], *argv[1:]])
    if flags == ["--check"] and len(argv) == 1:
        return check()
    print(f"surface-map: unexpected args {' '.join(argv)} (try --help)", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))