#!/usr/bin/env python3
"""Generate GitHub wiki pages from README.md + docs/. Usage: sync-wiki.py OUT_DIR
The wiki is a build artifact; edit docs/, never the wiki. See docs/DEVELOPMENT.md."""
import posixpath, re, sys
from pathlib import Path

REPO = "centopw/RRouter"
BLOB = f"https://github.com/{REPO}/blob/main/"
RAW = f"https://raw.githubusercontent.com/{REPO}/main/"
ROOT = Path(__file__).resolve().parent.parent
PAGES = {  # source path -> wiki page
    "README.md": "Home", "docs/README.md": "Documentation",
    "docs/deploy/README.md": "Deployment-Overview", "docs/deploy/docker.md": "Docker",
    "docs/deploy/kubernetes.md": "Kubernetes", "docs/deploy/helm.md": "Helm",
    "docs/deploy/systemd.md": "systemd", "docs/DEPLOY.md": "Operations",
    "docs/CONFIG.md": "Configuration", "docs/API.md": "API", "docs/ROUTING.md": "Routing",
    "docs/COMBOS.md": "Combos", "docs/MODELS.md": "Models", "docs/MODEL_PRESETS.md": "Model-Presets",
    "docs/KEYS.md": "Keys-and-Limits", "docs/GUARDRAILS.md": "Guardrails", "docs/CACHING.md": "Caching",
    "docs/PROVIDERS.md": "Providers", "docs/OBSERVABILITY.md": "Observability",
    "docs/SECURITY.md": "Security", "docs/TELEMETRY.md": "Telemetry", "docs/UPDATES.md": "Updates",
    "docs/ARCHITECTURE.md": "Architecture", "docs/DEVELOPMENT.md": "Development",
    "docs/RELEASES.md": "Releases", "docs/EVENTS.md": "Events", "docs/ROADMAP.md": "Roadmap",
}
SIDEBAR = """**[RRouter](Home)**

**Start**
- [Home](Home)
- [Documentation index](Documentation)

**Deploy**
- [Overview](Deployment-Overview)
- [Docker](Docker) · [Kubernetes](Kubernetes) · [Helm](Helm) · [systemd](systemd)
- [Operations](Operations)
- [Configuration](Configuration)

**Use**
- [API](API) · [Routing](Routing) · [Combos](Combos)
- [Models](Models) · [Model presets](Model-Presets)
- [Keys and limits](Keys-and-Limits) · [Guardrails](Guardrails)
- [Caching](Caching) · [Providers](Providers)
- [Observability](Observability) · [Security](Security)
- [Updates](Updates) · [Events](Events)

**Develop**
- [Architecture](Architecture) · [Development](Development)
- [Releases](Releases) · [Roadmap](Roadmap)
"""

def fix(src, text):
    base = posixpath.dirname(src)
    def link(m):
        bang, label, target = m.group(1), m.group(2), m.group(3)
        if re.match(r"(https?:|mailto:|#)", target): return m.group(0)
        path, _, frag = target.partition("#")
        full = posixpath.normpath(posixpath.join(base, path))
        if bang: return f"![{label}]({RAW}{full})"
        if full in PAGES: return f"[{label}]({PAGES[full]}{'#' + frag if frag else ''})"
        return f"[{label}]({BLOB}{full})"
    text = re.sub(r"(!?)\[([^\]]*)\]\(([^)\s]+)\)", link, text)
    return re.sub(r'(<img [^>]*src=")([^"]+)(")', lambda m: m.group(1) + (m.group(2) if m.group(2).startswith("http") else RAW + posixpath.normpath(posixpath.join(base, m.group(2)))) + m.group(3), text)

out = Path(sys.argv[1]); out.mkdir(parents=True, exist_ok=True)
for src, page in PAGES.items():
    body = fix(src, (ROOT / src).read_text())
    note = f"\n\n---\n*Generated from [`{src}`]({BLOB}{src}) — edit it there, not in the wiki.*\n"
    (out / f"{page}.md").write_text(body + note)
(out / "_Sidebar.md").write_text(SIDEBAR)
(out / "_Footer.md").write_text(f"[Repository](https://github.com/{REPO}) · [Issues](https://github.com/{REPO}/issues) · MIT\n")
print(f"{len(PAGES)} pages -> {out}")
