//! Embedded dashboard. Zero build step, zero CDN: three `include_str!` assets.
//!
//! The page is a shell. Panels are data: `GET /api/ui/panels` returns the built-ins plus
//! whatever an enterprise registered under the `panels` key of `/api/settings`, and
//! `kind: "extension"` panels load their `url` in a sandboxed iframe that receives the
//! panel token via `postMessage`. Serving `/ext/*` from `RROUTER_STATIC_DIR` is how a
//! deployment ships those extension bundles without forking this crate.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as UrlPath, State};
use axum::http::header;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::state::AppState;

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const STYLE_CSS: &str = include_str!("../assets/style.css");

/// Built-in panels, in display order.
pub fn builtin_panels() -> serde_json::Value {
    json!([
        { "id": "connections", "title": "Connections", "icon": "\u{1f517}", "order": 10, "kind": "builtin" },
        { "id": "keys", "title": "API Keys", "icon": "\u{1f511}", "order": 20, "kind": "builtin" },
        { "id": "models", "title": "Models", "icon": "\u{1f9e0}", "order": 25, "kind": "builtin" },
        { "id": "aliases", "title": "Aliases", "icon": "\u{21aa}", "order": 30, "kind": "builtin" },
        { "id": "combos", "title": "Combos", "icon": "\u{1f500}", "order": 40, "kind": "builtin" },
        { "id": "usage", "title": "Usage", "icon": "\u{1f4ca}", "order": 50, "kind": "builtin" },
        { "id": "status", "title": "Health", "icon": "\u{2764}", "order": 60, "kind": "builtin" },
    ])
}

/// `GET /api/ui/panels`: built-ins plus stored extensions, ordered, deduplicated by id.
pub async fn panels(State(state): State<Arc<AppState>>) -> Response {
    let mut list = match builtin_panels() {
        serde_json::Value::Array(items) => items,
        _ => Vec::new(),
    };
    if let Ok(stored) = crate::settings::load(&state).await {
        if let Some(extensions) = stored.get("panels").and_then(|p| p.as_array()) {
            for entry in extensions {
                let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                if list.iter().any(|p| p.get("id").and_then(|v| v.as_str()) == Some(id)) {
                    continue;
                }
                let mut panel = entry.clone();
                if let Some(obj) = panel.as_object_mut() {
                    obj.insert("kind".into(), json!("extension"));
                    obj.entry("order").or_insert(json!(100));
                    obj.entry("icon").or_insert(json!("\u{1f9e9}"));
                    obj.entry("title").or_insert(json!(id));
                }
                list.push(panel);
            }
        }
    }
    list.sort_by_key(|p| p.get("order").and_then(|v| v.as_i64()).unwrap_or(100));
    axum::Json(json!({ "data": list, "brand": crate::settings::load(&state).await.map(|s| crate::settings::brand_of(&s)).unwrap_or_else(|_| crate::settings::brand_of(&json!({}))) }))
        .into_response()
}

pub async fn index() -> Response {
    asset_response("text/html; charset=utf-8", INDEX_HTML, "no-cache")
}

/// `/{*path}` for the three embedded assets, `/ext/*` for deployment-supplied files,
/// and an SPA-style fallback to `index.html` for anything else.
pub async fn asset(
    State(state): State<Arc<AppState>>,
    UrlPath(path): UrlPath<String>,
) -> Response {
    match path.as_str() {
        "app.js" | "assets/app.js" => {
            return asset_response("text/javascript; charset=utf-8", APP_JS, "no-cache")
        }
        "style.css" | "assets/style.css" => {
            return asset_response("text/css; charset=utf-8", STYLE_CSS, "no-cache")
        }
        _ => {}
    }
    if let Some(rest) = path.strip_prefix("ext/") {
        return ext_file(&state, rest).await;
    }
    index().await
}

fn asset_response(content_type: &str, body: &'static str, cache: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache),
        ],
        body,
    )
        .into_response()
}

/// Serve `<static_dir>/<rest>` with traversal protection.
///
/// The request path is decoded by axum, so `%2e%2e` is already `..` here. Anything that
/// is not a plain relative path of normal components is a `404`, and so is a file whose
/// canonical path escapes the canonical root (symlinks included).
async fn ext_file(state: &AppState, rest: &str) -> Response {
    let Some(root) = state.config.static_dir.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let relative = match sanitize(rest) {
        Some(p) => p,
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    let Ok(canonical_root) = tokio::fs::canonicalize(&root).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let candidate = canonical_root.join(&relative);
    let Ok(canonical) = tokio::fs::canonicalize(&candidate).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !canonical.starts_with(&canonical_root) || !canonical.is_file() {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read(&canonical).await {
        Ok(bytes) => {
            let content_type = content_type_for(&canonical);
            ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `None` for anything that is not a plain relative path (no `..`, no absolute, no
/// drive/root prefixes, no empty components).
fn sanitize(rest: &str) -> Option<PathBuf> {
    if rest.is_empty() || rest.contains('\0') || rest.contains('\\') {
        return None;
    }
    let path = Path::new(rest);
    if path.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn content_type_for(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "woff2" => "font/woff2",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_forms() {
        assert!(sanitize("../Cargo.toml").is_none());
        assert!(sanitize("a/../../b").is_none());
        assert!(sanitize("/etc/passwd").is_none());
        assert!(sanitize("a\\..\\b").is_none());
        assert!(sanitize("").is_none());
        assert_eq!(sanitize("panels/acme.js"), Some(PathBuf::from("panels/acme.js")));
    }

    #[test]
    fn builtin_panel_ids_are_stable() {
        let ids: Vec<String> = builtin_panels()
            .as_array()
            .map(|a| a.iter().filter_map(|p| p["id"].as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        assert_eq!(
            ids,
            ["connections", "keys", "models", "aliases", "combos", "usage", "status"]
        );
    }
}
